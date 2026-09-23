use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::json;

use synthforge_ports::{ImageModel, ImageRequest, ImageResponse, PortResult};

use crate::catalog::Catalog;
use crate::error::{Error, Result};
use crate::openai_compat::{classify, ProviderConfig};

pub struct OpenAiImages {
    http: reqwest::Client,
    cfg: ProviderConfig,
    catalog: Arc<Catalog>,
}

/// Размеры — деталь конкретного провайдера, поэтому маппинг живёт здесь,
/// а не в порту.
fn openai_size(s: synthforge_ports::ImageSize) -> &'static str {
    use synthforge_ports::ImageSize::*;
    match s {
        Square => "1024x1024",
        Portrait => "1024x1536",
        Landscape => "1536x1024",
    }
}

impl std::fmt::Debug for OpenAiImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiImages").field("cfg", &self.cfg).finish()
    }
}

impl OpenAiImages {
    pub fn new(cfg: ProviderConfig, catalog: Arc<Catalog>) -> Result<Self> {
        catalog.get(&cfg.model)?;

        let mut builder = reqwest::Client::builder()
            .timeout(cfg.timeout.max(Duration::from_secs(300)))
            .connect_timeout(Duration::from_secs(20));

        if let Some(p) = &cfg.proxy {
            let proxy =
                reqwest::Proxy::all(p).map_err(|e| Error::Config(format!("прокси «{p}»: {e}")))?;
            builder = builder.proxy(proxy);
        }

        let http = builder.build().map_err(|source| Error::Transport {
            provider: cfg.name.clone(),
            source,
        })?;

        Ok(Self { http, cfg, catalog })
    }

    /// Качество снимка — главный рычаг цены у этой модели.
    ///
    /// Без явного значения провайдер выбирает сам и, судя по расходу на живом
    /// прогоне, выбирает дорогое: баланс кончился заметно быстрее, чем следовало
    /// из оценки в каталоге. Задаётся переменной `IMAGE_QUALITY`
    /// (`low` / `medium` / `high`), по умолчанию `medium`.
    fn quality(&self) -> String {
        std::env::var("IMAGE_QUALITY")
            .ok()
            .filter(|q| matches!(q.as_str(), "low" | "medium" | "high"))
            .unwrap_or_else(|| "medium".to_string())
    }

    /// Генерация с нуля по текстовому промпту.
    async fn generate(&self, req: &ImageRequest) -> Result<Vec<u8>> {
        let url = format!("{}/images/generations", self.cfg.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.cfg.model,
            "prompt": req.full_prompt(),
            "size": openai_size(req.size),
            "quality": self.quality(),
            "n": 1,
        });

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|source| Error::Transport { provider: self.cfg.name.clone(), source })?;

        self.decode(resp).await
    }

    /// Генерация с опорой на референс.
    ///
    /// Нужна для согласованности: несколько ракурсов одного здания или портрет
    /// одного человека в разных контекстах текстовым промптом не получить —
    /// выйдут разные объекты. Референс задаёт, что это тот же самый объект.
    async fn edit(&self, req: &ImageRequest, reference: &[u8]) -> Result<Vec<u8>> {
        let url = format!("{}/images/edits", self.cfg.base_url.trim_end_matches('/'));

        let part = reqwest::multipart::Part::bytes(reference.to_vec())
            .file_name("reference.png")
            .mime_str("image/png")
            .map_err(|e| Error::Config(format!("референс: {e}")))?;

        let form = reqwest::multipart::Form::new()
            .text("model", self.cfg.model.clone())
            .text("prompt", req.full_prompt())
            .text("size", openai_size(req.size))
            .text("quality", self.quality())
            .text("n", "1")
            .part("image", part);

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|source| Error::Transport { provider: self.cfg.name.clone(), source })?;

        self.decode(resp).await
    }

    async fn decode(&self, resp: reqwest::Response) -> Result<Vec<u8>> {
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|source| Error::Transport {
            provider: self.cfg.name.clone(),
            source,
        })?;

        if status != 200 {
            return Err(classify(&self.cfg.name, status, &text));
        }

        let parsed: ImagesResponse =
            serde_json::from_str(&text).map_err(|source| Error::Decode {
                provider: self.cfg.name.clone(),
                source,
            })?;

        let b64 = parsed
            .data
            .first()
            .and_then(|d| d.b64_json.clone())
            .ok_or_else(|| Error::NotStructured("ответ без изображения".into()))?;

        base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| Error::NotStructured(format!("изображение не декодируется: {e}")))
    }
}

#[async_trait]
impl ImageModel for OpenAiImages {
    fn id(&self) -> &str {
        &self.cfg.model
    }

    fn provider(&self) -> &str {
        &self.cfg.name
    }

    async fn render(&self, req: ImageRequest) -> PortResult<ImageResponse> {
        Ok(self.render_inner(req).await?)
    }
}

impl OpenAiImages {
    pub async fn render_inner(&self, req: ImageRequest) -> Result<ImageResponse> {
        let mut last: Option<Error> = None;

        for attempt in 0..=self.cfg.max_retries {
            let res = match &req.reference_png {
                Some(r) => self.edit(&req, r).await,
                None => self.generate(&req).await,
            };

            match res {
                Ok(png) => {
                    let info = self.catalog.get(&self.cfg.model)?;
                    return Ok(ImageResponse {
                        png,
                        model: self.cfg.model.clone(),
                        usage: info.image_usage(1),
                    });
                }
                Err(e) => {
                    if e.is_fatal() || !e.is_retryable() || attempt == self.cfg.max_retries {
                        return Err(e);
                    }
                    let backoff = Duration::from_millis(1000u64 << attempt.min(5));
                    tracing::warn!(
                        provider = %self.cfg.name, attempt, error = %e, ?backoff,
                        "повтор генерации изображения"
                    );
                    last = Some(e);
                    tokio::time::sleep(backoff).await;
                }
            }
        }

        Err(Error::Exhausted {
            attempts: self.cfg.max_retries + 1,
            last: Box::new(last.unwrap_or(Error::Config("нет попыток".into()))),
        })
    }
}

#[derive(Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    data: Vec<ImageItem>,
}

#[derive(Deserialize)]
struct ImageItem {
    #[serde(default)]
    b64_json: Option<String>,
}
