//! Аннотации и порождённые файлы.
//!
//! Цикл, ради которого всё это: генерация → наблюдение → пометки → доработка
//! словаря → генерация. На старте он и есть основной рабочий режим.

use serde::{Deserialize, Serialize};

use crate::{now_millis, Error, Result, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Good,
    Bad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Text,
    Image,
    Entity,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Annotation {
    pub id: i64,
    pub session_id: String,
    pub job_id: Option<i64>,
    pub target: Target,
    pub asset: Option<String>,
    pub verdict: Verdict,
    pub tags: String,
    pub comment: Option<String>,
    pub author: Option<String>,
    pub created_at: i64,
}

impl Annotation {
    pub fn tag_list(&self) -> Vec<String> {
        serde_json::from_str(&self.tags).unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct NewAnnotation {
    pub job_id: Option<i64>,
    pub target: Target,
    pub asset: Option<String>,
    pub verdict: Verdict,
    pub tags: Vec<String>,
    pub comment: Option<String>,
    pub author: Option<String>,
}

impl NewAnnotation {
    pub fn bad(target: Target, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            job_id: None,
            target,
            asset: None,
            verdict: Verdict::Bad,
            tags: tags.into_iter().map(Into::into).collect(),
            comment: None,
            author: None,
        }
    }

    pub fn good(target: Target) -> Self {
        Self {
            job_id: None,
            target,
            asset: None,
            verdict: Verdict::Good,
            tags: Vec::new(),
            comment: None,
            author: None,
        }
    }

    pub fn on_job(mut self, job_id: i64) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn on_asset(mut self, path: impl Into<String>) -> Self {
        self.asset = Some(path.into());
        self
    }

    pub fn comment(mut self, c: impl Into<String>) -> Self {
        self.comment = Some(c.into());
        self
    }

    pub fn by(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }
}

/// Сколько раз встретилась метка. Это и есть сигнал к доработке словаря.
#[derive(Debug, Clone, Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FeedbackSummary {
    pub good: i64,
    pub bad: i64,
    /// Метки по убыванию частоты.
    pub tags: Vec<TagCount>,
}

impl FeedbackSummary {
    pub fn total(&self) -> i64 {
        self.good + self.bad
    }

    /// Доля брака. Пока она высока, запускать массовый прогон рано.
    pub fn reject_rate(&self) -> f64 {
        if self.total() == 0 {
            0.0
        } else {
            self.bad as f64 / self.total() as f64
        }
    }

    /// Метки, встретившиеся достаточно часто, чтобы считать их системными.
    ///
    /// Разовое «не нравится» — вкусовщина. Двадцать раз одно и то же — дефект
    /// словаря, который надо чинить параметром, а не перегенерацией.
    /// Сколько раз замечание должно повториться, чтобы считаться системным.
    ///
    /// Одной доли мало: на шести пометках любое единичное замечание даёт
    /// шестнадцать процентов и выглядит закономерностью. Абсолютный порог
    /// отделяет вкусовщину от дефекта словаря.
    pub const MIN_SYSTEMIC_COUNT: i64 = 3;

    pub fn systemic(&self, min_share: f64) -> Vec<&TagCount> {
        let total = self.total().max(1) as f64;
        self.tags
            .iter()
            .filter(|t| t.count >= Self::MIN_SYSTEMIC_COUNT)
            .filter(|t| t.count as f64 / total >= min_share)
            .collect()
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Asset {
    pub id: i64,
    pub session_id: String,
    pub entity_key: String,
    pub job_id: Option<i64>,
    pub role: String,
    pub path: String,
    pub bytes: i64,
    pub cost_usd: f64,
    pub created_at: i64,
}

impl Store {
    // ------------------------------------------------------------- аннотации

    pub async fn annotate(&self, session_id: &str, a: NewAnnotation) -> Result<i64> {
        let tags = serde_json::to_string(&a.tags)?;
        let row: (i64,) = sqlx::query_as(
            "insert into annotations
               (session_id, job_id, target, asset, verdict, tags, comment, author, created_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             returning id",
        )
        .bind(session_id)
        .bind(a.job_id)
        .bind(a.target)
        .bind(&a.asset)
        .bind(a.verdict)
        .bind(&tags)
        .bind(&a.comment)
        .bind(&a.author)
        .bind(now_millis())
        .fetch_one(self.pool())
        .await?;
        Ok(row.0)
    }

    pub async fn annotations(&self, session_id: &str) -> Result<Vec<Annotation>> {
        Ok(sqlx::query_as::<_, Annotation>(
            "select * from annotations where session_id = ?1 order by created_at desc",
        )
        .bind(session_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Сводка обратной связи: доля брака и частота меток.
    pub async fn feedback_summary(&self, session_id: &str) -> Result<FeedbackSummary> {
        let rows = sqlx::query_as::<_, (Verdict, i64)>(
            "select verdict, count(*) from annotations where session_id = ?1 group by verdict",
        )
        .bind(session_id)
        .fetch_all(self.pool())
        .await?;

        let mut summary = FeedbackSummary::default();
        for (v, n) in rows {
            match v {
                Verdict::Good => summary.good = n,
                Verdict::Bad => summary.bad = n,
            }
        }

        // Метки лежат json-массивом, поэтому считаем на стороне приложения:
        // на сотнях аннотаций это дешевле, чем городить разбор json в SQLite.
        let tag_rows = sqlx::query_as::<_, (String,)>(
            "select tags from annotations where session_id = ?1",
        )
        .bind(session_id)
        .fetch_all(self.pool())
        .await?;

        let mut counts: std::collections::BTreeMap<String, i64> = Default::default();
        for (json,) in tag_rows {
            for tag in serde_json::from_str::<Vec<String>>(&json).unwrap_or_default() {
                *counts.entry(tag).or_default() += 1;
            }
        }

        summary.tags = counts
            .into_iter()
            .map(|(tag, count)| TagCount { tag, count })
            .collect();
        summary.tags.sort_by(|a, b| b.count.cmp(&a.count).then(a.tag.cmp(&b.tag)));

        Ok(summary)
    }

    // ----------------------------------------------------------------- файлы

    #[allow(clippy::too_many_arguments)]
    pub async fn record_asset(
        &self,
        session_id: &str,
        entity_key: &str,
        job_id: i64,
        role: &str,
        path: &str,
        bytes: i64,
        cost_usd: f64,
    ) -> Result<()> {
        sqlx::query(
            "insert or replace into assets
               (session_id, entity_key, job_id, role, path, bytes, cost_usd, created_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(session_id)
        .bind(entity_key)
        .bind(job_id)
        .bind(role)
        .bind(path)
        .bind(bytes)
        .bind(cost_usd)
        .bind(now_millis())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Файлы одной сущности. Используется при сборке записи: текст и картинки
    /// сходятся здесь.
    pub async fn assets_for_entity(&self, entity_key: &str) -> Result<Vec<Asset>> {
        Ok(
            sqlx::query_as::<_, Asset>("select * from assets where entity_key = ?1 order by role")
                .bind(entity_key)
                .fetch_all(self.pool())
                .await?,
        )
    }

    pub async fn assets(&self, session_id: &str) -> Result<Vec<Asset>> {
        Ok(sqlx::query_as::<_, Asset>(
            "select * from assets where session_id = ?1 order by id",
        )
        .bind(session_id)
        .fetch_all(self.pool())
        .await?)
    }


    /// Готовые результаты заданий одного вида.
    ///
    /// Нужны для восстановления индекса уникальности: после перезапуска
    /// прогона новые тексты должны сверяться и с тем, что уже было принято до
    /// падения, иначе уникальность держалась бы только внутри одного запуска.
    ///
    /// `session_id = None` — по всем прогонам: так проверяется уникальность
    /// относительно всего, что когда-либо сгенерировано этого вида.
    pub async fn done_results_of_kind(
        &self,
        kind: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as::<_, (String, String)>(
            "select natural_key, result from jobs
              where kind = ?1 and status = 'done' and result is not null
                and (?2 is null or session_id = ?2)
              order by id",
        )
        .bind(kind)
        .bind(session_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Задание по естественному ключу. Нужно, чтобы пометить результат, зная
    /// только его адрес в интерфейсе.
    pub async fn job_by_natural_key(&self, key: &str) -> Result<Option<crate::Job>> {
        Ok(
            sqlx::query_as::<_, crate::Job>("select * from jobs where natural_key = ?1")
                .bind(key)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    pub async fn job(&self, id: i64) -> Result<crate::Job> {
        sqlx::query_as::<_, crate::Job>("select * from jobs where id = ?1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(Error::JobNotFound(id))
    }
}
