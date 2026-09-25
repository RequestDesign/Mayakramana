//! Реализация порта хранилища контента поверх Nexorium.
//!
//! Здесь и только здесь живёт знание о том, что коллекции адресуются
//! идентификатором, а не слагом, что запись идёт пачками по 250 и что обрыв на
//! POST оставляет исход неизвестным. Наружу, в порт, всё это не протекает.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use synthforge_ports::{
    BatchVerdict as PortVerdict, ContentStore, PortError, PortResult, WriteOutcome,
};

use crate::{BatchVerdict, CollectionId, Error, Nexorium, BULK_MAX};

pub struct NexoriumStore {
    api: Arc<Nexorium>,
    /// Слаг → идентификатор. Разрешается один раз: список коллекций меняется
    /// редко, а запрос на каждую пачку съедал бы лимит в тысячу в минуту.
    ids: RwLock<HashMap<String, CollectionId>>,
}

impl NexoriumStore {
    pub fn new(api: Arc<Nexorium>) -> Self {
        Self { api, ids: RwLock::new(HashMap::new()) }
    }

    async fn resolve(&self, slug: &str) -> PortResult<CollectionId> {
        if let Some(id) = self.ids.read().await.get(slug) {
            return Ok(*id);
        }

        let found = self
            .api
            .collection_by_slug(slug)
            .await
            .map_err(to_port)?
            .ok_or_else(|| {
                PortError::Fatal(format!(
                    "коллекции «{slug}» нет — вызовите ensure_collection до записи"
                ))
            })?;

        self.ids.write().await.insert(slug.to_string(), found.id);
        Ok(found.id)
    }
}

#[async_trait]
impl ContentStore for NexoriumStore {
    async fn ensure_collection(&self, slug: &str, title: &str) -> PortResult<()> {
        if self.resolve(slug).await.is_ok() {
            return Ok(());
        }

        let created = self.api.create_collection(title, slug).await.map_err(to_port)?;

        // Служебные поля нужны для сверки пачек и дедупликации. Без batch_id
        // обрыв соединения оставил бы записи, о которых мы не знаем.
        for spec in crate::meta::required_fields() {
            if let Err(e) = self.api.create_field(created.id, &spec).await {
                // Поле могло уже существовать — это не повод падать.
                tracing::debug!(field = %spec.slug, error = %e, "поле не создано");
            }
        }

        self.ids.write().await.insert(slug.to_string(), created.id);
        Ok(())
    }

    async fn write_batch(
        &self,
        collection: &str,
        batch_id: &str,
        records: &[serde_json::Value],
    ) -> PortResult<WriteOutcome> {
        if records.len() > BULK_MAX {
            return Err(PortError::Rejected(format!(
                "пачка из {} записей при пределе {BULK_MAX}",
                records.len()
            )));
        }

        let id = self.resolve(collection).await?;

        // Метка пачки проставляется здесь, а не вызывающим: она нужна именно
        // для сверки, и забыть её нельзя.
        let stamped: Vec<serde_json::Value> = records
            .iter()
            .map(|r| {
                let mut r = r.clone();
                if let Some(o) = r.as_object_mut() {
                    o.insert(
                        crate::meta::BATCH_ID.to_string(),
                        serde_json::Value::String(batch_id.to_string()),
                    );
                }
                r
            })
            .collect();

        match self.api.create_bulk(id, &stamped).await {
            Ok(_) => Ok(WriteOutcome::Committed),
            Err(e) if e.is_unique_conflict() => {
                // Часть сущностей уже в базе — например, пачку повторили после
                // обрыва, или заливку запустили второй раз. Сервер отверг пачку
                // целиком; дописываем только тех, кого в базе ещё нет.
                let mut fresh = Vec::with_capacity(stamped.len());
                for r in stamped {
                    let key = r.get(crate::meta::NATURAL_KEY).and_then(|v| v.as_str());
                    let exists = match key {
                        Some(k) => self
                            .api
                            .count(id, &[(crate::meta::NATURAL_KEY.to_string(), k.to_string())])
                            .await
                            .map_err(to_port)?
                            > 0,
                        None => false,
                    };
                    if !exists {
                        fresh.push(r);
                    }
                }
                tracing::info!(
                    batch = %batch_id,
                    already = records.len() - fresh.len(),
                    fresh = fresh.len(),
                    "часть пачки уже в базе"
                );
                if fresh.is_empty() {
                    return Ok(WriteOutcome::Committed);
                }
                match self.api.create_bulk(id, &fresh).await {
                    Ok(_) => Ok(WriteOutcome::Committed),
                    Err(e) if e.is_indeterminate() => Ok(WriteOutcome::Unknown),
                    Err(e) => Err(to_port(e)),
                }
            }
            Err(e) if e.is_indeterminate() => {
                // Обрыв или 5xx: сервер мог запрос выполнить, мог не выполнить.
                // Повторять вслепую нельзя — это создало бы дубликаты.
                tracing::warn!(batch = %batch_id, error = %e, "исход записи пачки неизвестен");
                Ok(WriteOutcome::Unknown)
            }
            Err(e) => Err(to_port(e)),
        }
    }

    async fn verify_batch(
        &self,
        collection: &str,
        batch_id: &str,
        expected: u64,
    ) -> PortResult<PortVerdict> {
        let id = self.resolve(collection).await?;
        let verdict = self
            .api
            .verify_batch(id, batch_id, expected)
            .await
            .map_err(to_port)?;

        Ok(match verdict {
            BatchVerdict::Committed => PortVerdict::Committed,
            BatchVerdict::Absent => PortVerdict::Absent,
            BatchVerdict::Partial { found } => PortVerdict::Partial { found },
        })
    }

    async fn delete_batch(&self, collection: &str, batch_id: &str) -> PortResult<usize> {
        let id = self.resolve(collection).await?;
        self.api.delete_batch(id, batch_id).await.map_err(to_port)
    }

    async fn count(&self, collection: &str) -> PortResult<u64> {
        let id = self.resolve(collection).await?;
        self.api.count(id, &[]).await.map_err(to_port)
    }
}

fn to_port(e: Error) -> PortError {
    match e.status() {
        // Ключ без нужных прав или неверный: повторы не помогут.
        Some(401) | Some(403) => PortError::Fatal(e.to_string()),
        _ if e.is_indeterminate() || e.is_definitely_rejected() => {
            PortError::Unavailable(e.to_string())
        }
        _ => PortError::Rejected(e.to_string()),
    }
}
