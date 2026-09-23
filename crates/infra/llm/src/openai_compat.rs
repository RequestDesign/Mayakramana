//! Клиент к API формата OpenAI chat completions.
//!
//! Одной реализацией закрываются оба нужных провайдера: и OpenAI, и x.ai (Grok)
//! говорят на этом протоколе. Различия — адрес, набор моделей и цены — живут в
//! конфигурации и каталоге, а не в коде.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use synthforge_ports::{
    Effort, Message, PortResult, Role, TextModel, TextRequest, TextResponse,
};

use crate::catalog::Catalog;
use crate::error::{Error, Result};

#[derive(Clone)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Исходящий прокси. С российского IP без него OpenAI отвечает
    /// `unsupported_country_region_territory`, а x.ai не отвечает вовсе.
    pub proxy: Option<String>,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl ProviderConfig {
    pub fn xai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: "xai".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key: api_key.into(),
            model: model.into(),
            proxy: None,
            timeout: Duration::from_secs(180),
            max_retries: 4,
        }
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: api_key.into(),
            model: model.into(),
            proxy: None,
            timeout: Duration::from_secs(180),
            max_retries: 4,
        }
    }

    pub fn with_proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy = proxy.filter(|p| !p.trim().is_empty());
        self
    }
}

/// Ключ не печатается: конфигурация провайдера попадает в логи и в сообщения
/// об ошибках, а секрет в логах — это секрет, который утёк.
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &redacted(&self.api_key))
            .field("proxy", &self.proxy.as_deref().unwrap_or("нет"))
            .finish()
    }
}

/// Оставляем последние четыре символа: их хватает, чтобы понять, какой ключ
/// подставился, и не хватает, чтобы им воспользоваться.
pub(crate) fn redacted(key: &str) -> String {
    let n = key.chars().count();
    if n <= 4 {
        "…".to_string()
    } else {
        format!("…{}", key.chars().skip(n - 4).collect::<String>())
    }
}

pub struct OpenAiCompatText {
    http: reqwest::Client,
    cfg: ProviderConfig,
    catalog: Arc<Catalog>,
}

impl std::fmt::Debug for OpenAiCompatText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatText").field("cfg", &self.cfg).finish()
    }
}

impl OpenAiCompatText {
    pub fn new(cfg: ProviderConfig, catalog: Arc<Catalog>) -> Result<Self> {
        // Модель обязана быть в каталоге: иначе расход посчитается нулём и
        // бюджетный потолок прогона не сработает.
        catalog.get(&cfg.model)?;

        let mut builder = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(20));

        if let Some(p) = &cfg.proxy {
            let proxy = reqwest::Proxy::all(p)
                .map_err(|e| Error::Config(format!("прокси «{p}»: {e}")))?;
            builder = builder.proxy(proxy);
        }

        let http = builder.build().map_err(|source| Error::Transport {
            provider: cfg.name.clone(),
            source,
        })?;

        Ok(Self { http, cfg, catalog })
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    fn body(&self, req: &TextRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role {
                        Role::System => "system",
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    "content": m.content,
                })
            })
            .collect();

        let mut body = json!({ "model": self.cfg.model, "messages": messages });
        let obj = body.as_object_mut().expect("объект");

        if let Some(n) = req.max_tokens {
            obj.insert("max_tokens".into(), json!(n));
        }
        if let Some(t) = req.temperature {
            obj.insert("temperature".into(), json!(t));
        }

        let info = self.catalog.get(&self.cfg.model).ok();

        if info.is_some_and(|i| i.supports_effort) && req.effort != Effort::Low {
            obj.insert("reasoning_effort".into(), json!(req.effort.as_str()));
        }

        if let Some(schema) = &req.schema {
            if info.is_some_and(|i| i.supports_schema) {
                obj.insert(
                    "response_format".into(),
                    json!({
                        "type": "json_schema",
                        "json_schema": {
                            "name": "result",
                            "strict": true,
                            "schema": schema,
                        }
                    }),
                );
            } else {
                // Модель схему не держит — просим структуру словами. Разбор
                // всё равно пройдёт через TextResponse::json с очисткой обёртки.
                obj.insert("response_format".into(), json!({"type": "json_object"}));
            }
        }

        body
    }

    async fn call_once(&self, body: &serde_json::Value) -> Result<TextResponse> {
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.api_key)
            .json(body)
            .send()
            .await
            .map_err(|source| Error::Transport {
                provider: self.cfg.name.clone(),
                source,
            })?;

        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|source| Error::Transport {
            provider: self.cfg.name.clone(),
            source,
        })?;

        if status != 200 {
            return Err(classify(&self.cfg.name, status, &text));
        }

        let parsed: ChatResponse =
            serde_json::from_str(&text).map_err(|source| Error::Decode {
                provider: self.cfg.name.clone(),
                source,
            })?;

        let content = parsed
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        let u = parsed.usage.unwrap_or_default();
        let info = self.catalog.get(&self.cfg.model)?;

        Ok(TextResponse {
            text: content,
            model: self.cfg.model.clone(),
            usage: info.usage(u.prompt_tokens, u.completion_tokens),
        })
    }
}

#[async_trait]
impl TextModel for OpenAiCompatText {
    fn id(&self) -> &str {
        &self.cfg.model
    }

    fn provider(&self) -> &str {
        &self.cfg.name
    }

    async fn complete(&self, req: TextRequest) -> PortResult<TextResponse> {
        Ok(self.complete_inner(req).await?)
    }
}

impl OpenAiCompatText {
    /// Внутренний вызов с полной классификацией ошибок.
    ///
    /// Наружу через порт уходит схлопнутая версия, но решение о повторе
    /// принимается здесь, где ещё известно, что именно случилось.
    pub async fn complete_inner(&self, req: TextRequest) -> Result<TextResponse> {
        let body = self.body(&req);
        let mut last: Option<Error> = None;

        for attempt in 0..=self.cfg.max_retries {
            match self.call_once(&body).await {
                Ok(r) => return Ok(r),
                Err(e) => {
                    // Фатальное не повторяем: кончились деньги, закрыт регион,
                    // неверный ключ. Повторы только сожгут время прогона.
                    if e.is_fatal() || !e.is_retryable() || attempt == self.cfg.max_retries {
                        return Err(e);
                    }
                    let backoff = Duration::from_millis(700u64 << attempt.min(5));
                    tracing::warn!(
                        provider = %self.cfg.name, attempt, error = %e, ?backoff,
                        "повтор запроса к модели"
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

pub(crate) fn classify(provider: &str, status: u16, body: &str) -> Error {
    let lower = body.to_lowercase();

    if lower.contains("unsupported_country")
        || lower.contains("country, region, or territory not supported")
        || (status == 403 && lower.contains("not allowed"))
    {
        return Error::RegionBlocked { provider: provider.to_string() };
    }
    if lower.contains("insufficient_quota") || lower.contains("billing") {
        return Error::Quota { provider: provider.to_string() };
    }
    if status == 429 {
        return Error::RateLimited { provider: provider.to_string() };
    }

    Error::Api {
        provider: provider.to_string(),
        status,
        body: body.chars().take(600).collect(),
    }
}

// ------------------------------------------------------------- форма ответа

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize, Default)]
struct TokenUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

/// Удобный конструктор запроса на генерацию по параметрам.
pub fn generation_request(system: &str, params_block: &str, brief: Option<&str>) -> TextRequest {
    let mut user = format!("Параметры:\n{params_block}");
    if let Some(b) = brief.filter(|b| !b.trim().is_empty()) {
        user.push_str(&format!("\n\nДополнительно: {b}"));
    }
    TextRequest::new(vec![Message::system(system), Message::user(user)])
}
