//! Очередь изображений.
//!
//! Отдельная линия конвейера, а не часть текстовой. Причины три: другая
//! стоимость (единицы центов против долей), другие лимиты провайдера и другой
//! профиль отказов. Смешивать их в одной очереди значит потерять возможность
//! управлять параллелизмом и бюджетом по отдельности.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use synthforge_ports::{AssetStore, ImageModel, PortError, PortResult};
use synthforge_store::{Store, Usage};

use crate::{Error, Result};

/// Полезная нагрузка задания на изображение.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageJobPayload {
    /// Ключ сущности: по нему картинка сойдётся с текстом при сборке записи.
    pub entity_key: String,
    /// Какой по счёту снимок этой сущности.
    pub index: usize,
    /// Параметры сущности — из них строится промпт.
    pub row: synthforge_params::ParamRow,
}

/// Хранилище файлов на диске.
pub struct FsAssetStore {
    root: PathBuf,
}

impl FsAssetStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl AssetStore for FsAssetStore {
    async fn put(
        &self,
        session_id: &str,
        entity_key: &str,
        role: &str,
        bytes: &[u8],
    ) -> PortResult<String> {
        let dir = self.root.join(session_id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| PortError::Unavailable(format!("каталог {}: {e}", dir.display())))?;

        // Ключ сущности содержит двоеточия — в имени файла они недопустимы.
        let safe: String = entity_key
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
            .collect();

        let path = dir.join(format!("{safe}-{role}.png"));
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| PortError::Unavailable(format!("запись {}: {e}", path.display())))?;

        Ok(path.display().to_string())
    }
}

/// Обработчик очереди изображений.
pub struct ImagePipeline {
    store: Store,
    model: Arc<dyn ImageModel>,
    assets: Arc<dyn AssetStore>,
    /// Сколько изображений генерить одновременно.
    ///
    /// Реальные лимиты провайдера заранее неизвестны, поэтому настройка.
    pub concurrency: usize,
}

impl ImagePipeline {
    pub fn new(store: Store, model: Arc<dyn ImageModel>, assets: Arc<dyn AssetStore>) -> Self {
        Self { store, model, assets, concurrency: 2 }
    }

    pub fn concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Обработать все задания на изображения в сессии.
    pub async fn run(
        &self,
        generator: Arc<dyn synthforge_ports::Generator>,
        session_id: &str,
        kind: &str,
    ) -> Result<usize> {
        let worker = format!("img-{}", std::process::id());
        let mut produced = 0usize;

        loop {
            let session = self
                .store
                .session(session_id)
                .await?
                .ok_or_else(|| Error::SessionNotFound(session_id.to_string()))?;

            if session.over_budget() {
                tracing::warn!(session = %session_id, "бюджет исчерпан, генерация изображений остановлена");
                break;
            }

            // Захватываем только задания на изображения: текстовые обрабатывает
            // другая линия, с другим параллелизмом.
            let claimed = self
                .store
                .claim_of_kind(session_id, &worker, kind, self.concurrency as i64)
                .await?;

            if claimed.is_empty() {
                break;
            }

            let mut tasks = Vec::with_capacity(claimed.len());
            for job in claimed {
                let generator = generator.clone();
                let model = self.model.clone();
                let assets = self.assets.clone();
                let store = self.store.clone();

                tasks.push(tokio::spawn(async move {
                    render_one(&*generator, &*model, &*assets, &store, job).await
                }));
            }

            for t in tasks {
                match t.await {
                    Ok(Ok(())) => produced += 1,
                    Ok(Err(e)) if e.is_fatal() => {
                        tracing::error!(error = %e, "фатальная ошибка при генерации изображений");
                        return Err(e);
                    }
                    Ok(Err(e)) => tracing::warn!(error = %e, "изображение не получено"),
                    Err(e) => tracing::error!(error = %e, "воркер изображений упал"),
                }
            }
        }

        Ok(produced)
    }
}

async fn render_one(
    generator: &dyn synthforge_ports::Generator,
    model: &dyn ImageModel,
    assets: &dyn AssetStore,
    store: &Store,
    job: synthforge_store::Job,
) -> Result<()> {
    let payload: ImageJobPayload = serde_json::from_str(&job.payload)?;

    let requests = generator.image_requests(&payload.row);
    let Some(request) = requests.into_iter().nth(payload.index) else {
        store
            .fail(job.id, "генератор не предложил такого снимка", Usage::default())
            .await?;
        return Ok(());
    };

    let role = request.role.clone();

    let rendered = match model.render(request).await {
        Ok(r) => r,
        Err(e) => {
            store.fail(job.id, &e.to_string(), Usage::default()).await?;
            return Err(Error::Port(e));
        }
    };

    let usage = Usage {
        tokens_in: rendered.usage.tokens_in as i64,
        tokens_out: rendered.usage.tokens_out as i64,
        cost_usd: rendered.usage.cost_usd,
    };

    let path = assets
        .put(&job.session_id, &payload.entity_key, &role, &rendered.png)
        .await
        .map_err(Error::Port)?;

    store
        .record_asset(
            &job.session_id,
            &payload.entity_key,
            job.id,
            &role,
            &path,
            rendered.png.len() as i64,
            rendered.usage.cost_usd,
        )
        .await?;

    store
        .complete(
            job.id,
            &serde_json::json!({ "role": role, "path": path }),
            usage,
        )
        .await?;

    Ok(())
}
