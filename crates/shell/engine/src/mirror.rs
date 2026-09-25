//! Запись в основное хранилище с локальной копией.
//!
//! Основное хранилище — Nexorium. Локальная копия (`out/*.jsonl`) нужна
//! инструментам, которые работают с готовым: сборке центров, страницам
//! приёмки, съёмке. В копию попадает только то, что точно легло в основное
//! хранилище: иначе локально были бы записи, которых нет в базе.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use synthforge_ports::{BatchVerdict, ContentStore, PortResult, WriteOutcome};
use tokio::sync::Mutex;

pub struct MirrorStore {
    primary: Arc<dyn ContentStore>,
    mirror: Arc<dyn ContentStore>,
    /// Пачки с неизвестным исходом: в копию они уйдут, если сверка покажет,
    /// что легли.
    pending: Mutex<HashMap<String, (String, Vec<serde_json::Value>)>>,
}

impl MirrorStore {
    pub fn new(primary: Arc<dyn ContentStore>, mirror: Arc<dyn ContentStore>) -> Self {
        Self { primary, mirror, pending: Mutex::new(HashMap::new()) }
    }

    async fn copy(&self, collection: &str, batch_id: &str, records: &[serde_json::Value]) {
        // Ошибка копии не должна ронять запись: основное хранилище уже приняло
        // данные, а копию можно восстановить выгрузкой.
        if let Err(e) = self.mirror.write_batch(collection, batch_id, records).await {
            tracing::warn!(batch = %batch_id, error = %e, "локальная копия не записана");
        }
    }
}

#[async_trait]
impl ContentStore for MirrorStore {
    async fn ensure_collection(&self, slug: &str, title: &str) -> PortResult<()> {
        self.primary.ensure_collection(slug, title).await?;
        self.mirror.ensure_collection(slug, title).await
    }

    async fn write_batch(
        &self,
        collection: &str,
        batch_id: &str,
        records: &[serde_json::Value],
    ) -> PortResult<WriteOutcome> {
        let outcome = self.primary.write_batch(collection, batch_id, records).await?;
        match outcome {
            WriteOutcome::Committed => self.copy(collection, batch_id, records).await,
            WriteOutcome::Unknown => {
                self.pending
                    .lock()
                    .await
                    .insert(batch_id.to_string(), (collection.to_string(), records.to_vec()));
            }
        }
        Ok(outcome)
    }

    async fn verify_batch(
        &self,
        collection: &str,
        batch_id: &str,
        expected: u64,
    ) -> PortResult<BatchVerdict> {
        let verdict = self.primary.verify_batch(collection, batch_id, expected).await?;
        let pending = self.pending.lock().await.remove(batch_id);
        if let (BatchVerdict::Committed, Some((col, records))) = (&verdict, pending) {
            self.copy(&col, batch_id, &records).await;
        }
        Ok(verdict)
    }

    async fn delete_batch(&self, collection: &str, batch_id: &str) -> PortResult<usize> {
        let _ = self.mirror.delete_batch(collection, batch_id).await;
        self.primary.delete_batch(collection, batch_id).await
    }

    async fn count(&self, collection: &str) -> PortResult<u64> {
        self.primary.count(collection).await
    }
}
