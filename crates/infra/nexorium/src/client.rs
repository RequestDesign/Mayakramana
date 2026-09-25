use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;
use uuid::Uuid;

use crate::error::{Error, Idempotency, Result};
use crate::query::{Query, Sort};
use crate::types::*;

/// Предел одной bulk-операции.
pub const BULK_MAX: usize = 250;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: Url,
    pub space: String,
    pub api_key: String,
    pub requests_per_minute: u32,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Config {
    /// Адрес изнутри Docker: минует Caddy, TLS и внешний канал.
    /// Именно этот вариант нужен, когда сервис живёт на том же сервере.
    pub fn internal(space: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        Self::at("http://nexorium:8200/api/v1", space, api_key)
    }

    /// Адрес снаружи. Медленнее и подвержен обрывам канала.
    pub fn external(space: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        Self::at("https://nexorium.trger.ru/api/v1", space, api_key)
    }

    pub fn at(base: &str, space: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let base_url = Url::parse(base).map_err(|e| Error::Config(format!("base_url: {e}")))?;
        Ok(Self {
            base_url,
            space: space.into(),
            api_key: api_key.into(),
            requests_per_minute: 900, // с запасом от лимита в 1000
            timeout: Duration::from_secs(120),
            max_retries: 5,
        })
    }
}

/// Вердикт сверки пачки после неопределённого исхода POST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchVerdict {
    /// Легла целиком — считать успехом, ничего не делать.
    Committed,
    /// Не легла — POST можно безопасно повторить.
    Absent,
    /// Легла частично — удалить по `batch_id` и повторить целиком.
    Partial { found: u64 },
}

pub struct Nexorium {
    http: reqwest::Client,
    cfg: Config,
    limiter: Limiter,
}

impl std::fmt::Debug for Nexorium {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nexorium")
            .field("base_url", &self.cfg.base_url.as_str())
            .field("space", &self.cfg.space)
            .finish_non_exhaustive()
    }
}

impl Nexorium {
    pub fn new(cfg: Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(Error::Transport)?;

        let rpm = NonZeroU32::new(cfg.requests_per_minute.max(1))
            .ok_or_else(|| Error::Config("requests_per_minute = 0".into()))?;
        let limiter = RateLimiter::direct(Quota::per_minute(rpm));

        Ok(Self { http, cfg, limiter })
    }

    fn url(&self, tail: &str) -> Result<Url> {
        let full = format!(
            "{}/spaces/{}/{}",
            self.cfg.base_url.as_str().trim_end_matches('/'),
            self.cfg.space,
            tail.trim_start_matches('/')
        );
        Url::parse(&full).map_err(|e| Error::Config(format!("url «{full}»: {e}")))
    }

    // ------------------------------------------------------------ коллекции

    pub async fn collections(&self) -> Result<Vec<Collection>> {
        let url = self.url("collections")?;
        let v = self.send(Idempotency::Safe, || self.http.get(url.clone())).await?;
        unwrap_data(v)
    }

    /// Найти коллекцию по слагу. Записи адресуются идентификатором, а не слагом,
    /// поэтому без этого шага не обойтись.
    pub async fn collection_by_slug(&self, slug: &str) -> Result<Option<Collection>> {
        Ok(self.collections().await?.into_iter().find(|c| c.slug == slug))
    }

    /// Требует ключ с правами `admin` — с `write` вернётся 403.
    pub async fn create_collection(
        &self,
        name: impl Into<String>,
        slug: impl Into<String>,
    ) -> Result<Collection> {
        #[derive(Serialize)]
        struct Body {
            name: String,
            slug: String,
        }
        let url = self.url("collections")?;
        let body = Body { name: name.into(), slug: slug.into() };
        let v = self
            .send(Idempotency::Unsafe, || self.http.post(url.clone()).json(&body))
            .await?;
        unwrap_data(v)
    }

    /// Требует ключ с правами `admin`.
    pub async fn create_field(
        &self,
        collection: CollectionId,
        spec: &FieldSpec,
    ) -> Result<serde_json::Value> {
        let url = self.url(&format!("collections/{collection}/fields"))?;
        self.send(Idempotency::Unsafe, || self.http.post(url.clone()).json(spec))
            .await
    }

    /// Удалить коллекцию вместе с полями и записями. Требует `admin`.
    pub async fn delete_collection(&self, collection: CollectionId) -> Result<()> {
        let url = self.url(&format!("collections/{collection}"))?;
        self.send(Idempotency::Safe, || self.http.delete(url.clone())).await?;
        Ok(())
    }

    // --------------------------------------------------------------- чтение

    pub async fn list(&self, collection: CollectionId, q: &Query) -> Result<Page> {
        let url = self.url(&format!("collections/{collection}/records"))?;
        let params = q.params();
        let v = self
            .send(Idempotency::Safe, || self.http.get(url.clone()).query(&params))
            .await?;
        serde_json::from_value(v).map_err(Error::Decode)
    }

    /// Одна запись по точному значению поля.
    ///
    /// Совпадение перепроверяется на клиенте: серверный фильтр может сматчить
    /// шире, чем ожидается, и вернуть соседнюю запись.
    pub async fn find_one(
        &self,
        collection: CollectionId,
        field: &str,
        value: &str,
    ) -> Result<Option<Record>> {
        let q = Query::new(Sort::asc(field))
            .per_page(5)
            .filter(field, value)?;
        let page = self.list(collection, &q).await?;
        Ok(page
            .records
            .into_iter()
            .find(|r| r.str_field(field) == Some(value)))
    }

    /// Количество записей без выкачивания.
    ///
    /// Единственное чтение, где `sort` не нужен: итог не зависит от порядка, а
    /// страница берётся размером в одну запись только ради `pagination.total`.
    pub async fn count(
        &self,
        collection: CollectionId,
        filters: &[(String, String)],
    ) -> Result<u64> {
        let url = self.url(&format!("collections/{collection}/records"))?;
        let mut params: Vec<(String, String)> = vec![("per_page".into(), "1".into())];
        params.extend(filters.iter().cloned());
        let v = self
            .send(Idempotency::Safe, || self.http.get(url.clone()).query(&params))
            .await?;
        let page: Page = serde_json::from_value(v).map_err(Error::Decode)?;
        Ok(page.pagination.total)
    }

    /// Чтение коллекции целиком: один запрос, согласованный снимок, секунды.
    ///
    /// Постраничный обход с сортировкой по JSONB-полю на коллекции в сотню тысяч
    /// записей идёт больше 25 минут, потому что Postgres пересортировывает всё
    /// заново на каждой странице.
    ///
    /// Оговорка: экспорт отдаёт только полезную нагрузку, **без `id`**. Если
    /// записи потом надо править, идентификаторы берите через [`Self::ids_where`].
    pub async fn export(&self, collection: CollectionId) -> Result<Vec<serde_json::Value>> {
        let url = self.url(&format!("collections/{collection}/records/export"))?;
        let params = [("format".to_string(), "json".to_string())];
        let v = self
            .send(Idempotency::Safe, || self.http.get(url.clone()).query(&params))
            .await?;
        match v {
            serde_json::Value::Array(a) => Ok(a),
            other => unwrap_data(other),
        }
    }

    /// Встроенный поиск Nexorium: полнотекстовый плюс семантический по вектору.
    ///
    /// Это штатный способ осмысленного подбора уже созданных сущностей —
    /// например, персонала под специализацию центра.
    pub async fn search(
        &self,
        q: &str,
        collection: Option<CollectionId>,
        per_page: u32,
    ) -> Result<Vec<SearchHit>> {
        let url = self.url("search")?;
        let mut params = vec![
            ("q".to_string(), q.to_string()),
            ("lang".to_string(), "russian".to_string()),
            ("per_page".to_string(), per_page.to_string()),
        ];
        if let Some(c) = collection {
            params.push(("collection_id".to_string(), c.to_string()));
        }
        let v = self
            .send(Idempotency::Safe, || self.http.get(url.clone()).query(&params))
            .await?;
        unwrap_data(v)
    }

    // --------------------------------------------------------------- запись

    /// Создание **одной** записи. Тело обёрнуто в `{"data": …}`.
    ///
    /// Форма отличается от bulk и задаётся типом намеренно: если обернуть так же
    /// элементы bulk, поля лягут на уровень `data.data.*`, ответ будет успешный,
    /// а найти записи не сможет ни один фильтр.
    pub async fn create_one<T: Serialize>(
        &self,
        collection: CollectionId,
        record: &T,
    ) -> Result<Record> {
        let url = self.url(&format!("collections/{collection}/records"))?;
        let body = SingleEnvelope { data: record };
        let v = self
            .send(Idempotency::Unsafe, || self.http.post(url.clone()).json(&body))
            .await?;
        unwrap_data(v)
    }

    /// Создание пачки. Элементы уходят **сырыми объектами**, без обёртки `data`.
    ///
    /// При неопределённом исходе (обрыв канала, 5xx) повторять вслепую нельзя —
    /// вызывайте [`Self::verify_batch`].
    pub async fn create_bulk<T: Serialize>(
        &self,
        collection: CollectionId,
        records: &[T],
    ) -> Result<serde_json::Value> {
        if records.len() > BULK_MAX {
            return Err(Error::BatchTooLarge { got: records.len(), max: BULK_MAX });
        }
        if records.is_empty() {
            return Ok(serde_json::json!({ "data": [] }));
        }
        let url = self.url(&format!("collections/{collection}/records/bulk"))?;
        let body = BulkRequest { operation: BulkOp::Create, records };
        self.send(Idempotency::Unsafe, || self.http.post(url.clone()).json(&body))
            .await
    }

    /// Сверка пачки после неопределённого исхода POST.
    ///
    /// Обрыв соединения не означает, что сервер запрос не выполнил. Один GET по
    /// `batch_id` разрешает неопределённость.
    pub async fn verify_batch(
        &self,
        collection: CollectionId,
        batch_id: &str,
        expected: u64,
    ) -> Result<BatchVerdict> {
        let filters = [(crate::meta::BATCH_ID.to_string(), batch_id.to_string())];
        let found = self.count(collection, &filters).await?;
        Ok(if found == 0 {
            BatchVerdict::Absent
        } else if found >= expected {
            BatchVerdict::Committed
        } else {
            BatchVerdict::Partial { found }
        })
    }

    /// Полная замена записи.
    ///
    /// Названо `replace`, а не `patch`, умышленно: документация не уточняет,
    /// сливает `PATCH` поля или заменяет запись целиком, поэтому слать надо
    /// полный набор полей — это корректно при обеих семантиках.
    ///
    /// Каждый вызов порождает ревизию, а пропускная способность порядка
    /// **100 записей в минуту**. Для массовой правки удаляйте пачку и вставляйте
    /// заново.
    pub async fn replace<T: Serialize>(
        &self,
        collection: CollectionId,
        id: Uuid,
        full_record: &T,
    ) -> Result<Record> {
        let url = self.url(&format!("collections/{collection}/records/{id}"))?;
        let body = SingleEnvelope { data: full_record };
        let v = self
            .send(Idempotency::Safe, || self.http.patch(url.clone()).json(&body))
            .await?;
        unwrap_data(v)
    }

    pub async fn delete_one(&self, collection: CollectionId, id: Uuid) -> Result<()> {
        let url = self.url(&format!("collections/{collection}/records/{id}"))?;
        self.send(Idempotency::Safe, || self.http.delete(url.clone())).await?;
        Ok(())
    }

    /// Идентификаторы записей под фильтр. Нужны, потому что `export` их не
    /// отдаёт, а удаление и правка работают по `id`.
    pub async fn ids_where(
        &self,
        collection: CollectionId,
        filters: &[(String, String)],
        sort: Sort,
    ) -> Result<Vec<Uuid>> {
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let mut q = Query::new(sort.clone()).page(page).per_page(250);
            for (k, v) in filters {
                q = q.filter(k.clone(), v.clone())?;
            }
            let p = self.list(collection, &q).await?;
            if p.records.is_empty() {
                break;
            }
            let got = p.records.len();
            out.extend(p.records.into_iter().map(|r| r.id));
            if got < 250 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Удаление всех записей пачки. Идемпотентно, поэтому применимо для отката
    /// частично легшей пачки.
    pub async fn delete_batch(&self, collection: CollectionId, batch_id: &str) -> Result<usize> {
        let filters = [(crate::meta::BATCH_ID.to_string(), batch_id.to_string())];
        let ids = self
            .ids_where(collection, &filters, Sort::asc(crate::meta::BATCH_ID))
            .await?;
        let n = ids.len();
        for id in ids {
            self.delete_one(collection, id).await?;
        }
        Ok(n)
    }

    // ------------------------------------------------------------- нутрянка

    async fn send(
        &self,
        idem: Idempotency,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<serde_json::Value> {
        let mut last: Option<Error> = None;

        for attempt in 0..=self.cfg.max_retries {
            self.limiter.until_ready().await;

            let res = make()
                .header("X-Api-Key", &self.cfg.api_key)
                .header("Accept", "application/json")
                .send()
                .await;

            let err = match res {
                Ok(resp) => match Self::classify(resp).await {
                    Ok(v) => return Ok(v),
                    Err(e) => e,
                },
                Err(e) => Error::Transport(e),
            };

            let may_retry = match idem {
                // Повтор даёт тот же результат — повторяем на всём преходящем.
                Idempotency::Safe => err.is_indeterminate() || err.is_definitely_rejected(),
                // POST создаёт дубликат. Единственный безопасный случай — 429:
                // запрос отвергли, не начав выполнять.
                Idempotency::Unsafe => err.is_definitely_rejected(),
            };

            if !may_retry || attempt == self.cfg.max_retries {
                tracing::debug!(%idem, attempt, error = %err, "запрос не удался, повтор невозможен");
                return Err(err);
            }

            let backoff = Duration::from_millis(500u64 << attempt.min(6));
            tracing::warn!(%idem, attempt, error = %err, ?backoff, "повтор запроса");
            last = Some(err);
            tokio::time::sleep(backoff).await;
        }

        Err(Error::Exhausted {
            attempts: self.cfg.max_retries + 1,
            last: Box::new(last.unwrap_or(Error::Config("нет попыток".into()))),
        })
    }

    async fn classify(resp: reqwest::Response) -> Result<serde_json::Value> {
        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(Error::RateLimited);
        }
        let body = resp.text().await.map_err(Error::Transport)?;
        if !status.is_success() {
            return Err(Error::Api { status: status.as_u16(), body });
        }
        if body.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_str(&body).map_err(Error::Decode)
    }
}

/// Достать полезную нагрузку из общей обёртки `{"data": …}`.
fn unwrap_data<T: DeserializeOwned>(v: serde_json::Value) -> Result<T> {
    let env: Envelope<T> = serde_json::from_value(v).map_err(Error::Decode)?;
    Ok(env.data)
}
