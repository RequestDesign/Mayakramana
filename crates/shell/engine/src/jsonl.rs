//! Локальный приёмник записей: файл вместо Nexorium.
//!
//! Нужен для отладки и приёмки: прогон должен работать без доступа к
//! хранилищу, а результат — лежать в файле, который можно открыть и посмотреть.
//! Реализует тот же порт, поэтому движок не отличает его от настоящего.
//!
//! Воспроизводит и неприятную часть поведения: запись пачками, метка пачки,
//! сверка. Иначе отладка шла бы в тепличных условиях, а в проде вылезало бы
//! именно то, чего в отладке не было.

use std::path::PathBuf;

use async_trait::async_trait;
use synthforge_ports::{BatchVerdict, ContentStore, PortError, PortResult, WriteOutcome};
use tokio::io::AsyncWriteExt;

pub struct JsonlStore {
    root: PathBuf,
}

impl JsonlStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, collection: &str) -> PathBuf {
        self.root.join(format!("{collection}.jsonl"))
    }

    async fn read_all(&self, collection: &str) -> PortResult<Vec<serde_json::Value>> {
        let path = self.path(collection);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| PortError::Unavailable(format!("чтение {}: {e}", path.display())))?;
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }
}

#[async_trait]
impl ContentStore for JsonlStore {
    async fn ensure_collection(&self, _slug: &str, _title: &str) -> PortResult<()> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| PortError::Fatal(format!("каталог {}: {e}", self.root.display())))
    }

    async fn write_batch(
        &self,
        collection: &str,
        batch_id: &str,
        records: &[serde_json::Value],
    ) -> PortResult<WriteOutcome> {
        let path = self.path(collection);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| PortError::Unavailable(format!("открытие {}: {e}", path.display())))?;

        let mut buf = String::new();
        for r in records {
            let mut r = r.clone();
            if let Some(o) = r.as_object_mut() {
                o.insert("batch_id".into(), serde_json::Value::String(batch_id.into()));
            }
            buf.push_str(&serde_json::to_string(&r).unwrap_or_default());
            buf.push('\n');
        }

        file.write_all(buf.as_bytes())
            .await
            .map_err(|e| PortError::Unavailable(format!("запись {}: {e}", path.display())))?;
        file.flush()
            .await
            .map_err(|e| PortError::Unavailable(format!("сброс {}: {e}", path.display())))?;

        Ok(WriteOutcome::Committed)
    }

    async fn verify_batch(
        &self,
        collection: &str,
        batch_id: &str,
        expected: u64,
    ) -> PortResult<BatchVerdict> {
        let found = self
            .read_all(collection)
            .await?
            .iter()
            .filter(|r| r.get("batch_id").and_then(|v| v.as_str()) == Some(batch_id))
            .count() as u64;

        Ok(if found == 0 {
            BatchVerdict::Absent
        } else if found >= expected {
            BatchVerdict::Committed
        } else {
            BatchVerdict::Partial { found }
        })
    }

    async fn delete_batch(&self, collection: &str, batch_id: &str) -> PortResult<usize> {
        let all = self.read_all(collection).await?;
        let before = all.len();

        let kept: Vec<String> = all
            .into_iter()
            .filter(|r| r.get("batch_id").and_then(|v| v.as_str()) != Some(batch_id))
            .map(|r| serde_json::to_string(&r).unwrap_or_default())
            .collect();

        let path = self.path(collection);
        tokio::fs::write(&path, kept.join("\n") + "\n")
            .await
            .map_err(|e| PortError::Unavailable(format!("перезапись {}: {e}", path.display())))?;

        Ok(before - kept.len())
    }

    async fn count(&self, collection: &str) -> PortResult<u64> {
        Ok(self.read_all(collection).await?.len() as u64)
    }
}
