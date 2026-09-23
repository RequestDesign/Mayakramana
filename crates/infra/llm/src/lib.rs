//! Порт к моделям и его реализации.
//!
//! Генератор не знает, кто на том конце: прямой HTTP к провайдеру или агент,
//! поднятый на своём сервере. Он знает только [`TextModel`] и [`ImageModel`].
//! Второй способ обращения поэтому не требует правки ни одного генератора —
//! это отдельная реализация в слое инфраструктуры.
//!
//! # Разделение по задачам
//!
//! Тексты и изображения идут к разным провайдерам не по вкусу, а по деньгам:
//! объём текстов огромен, и разница в цене токена определяет, состоится прогон
//! или нет. Кто именно куда идёт — задаётся [`Routing`], а не кодом.
//!
//! # Почему модель обязана быть в каталоге
//!
//! [`Catalog`] — единственный источник цен, и создание клиента к модели, которой
//! в нём нет, падает сразу. Иначе расход посчитался бы нулём, бюджетный потолок
//! не сработал бы, и это выяснилось бы по счёту, а не в интерфейсе.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

mod catalog;
mod error;
mod images;
mod openai_compat;

pub use catalog::{Catalog, ModelInfo};
pub use error::{Error, Result};
pub use images::OpenAiImages;
pub use openai_compat::{generation_request, OpenAiCompatText, ProviderConfig};

// Контракты и типы обращения к моделям объявлены в ядре, здесь только
// реализация. Реэкспорт — чтобы потребителю не приходилось тянуть два крейда.
pub use synthforge_ports::{
    Effort, ImageModel, ImageRequest, ImageResponse, ImageSize, Message, Role, TextModel,
    TextRequest, TextResponse, Usage,
};

/// Выбор модели под конкретный вид задания.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChoice {
    pub model: String,
    #[serde(default)]
    pub effort: Effort,
}

impl ModelChoice {
    pub fn new(model: impl Into<String>, effort: Effort) -> Self {
        Self { model: model.into(), effort }
    }
}

/// Маршрутизация: какой вид задания какой моделью и с каким усилием.
///
/// Вынесено в конфигурацию, потому что это главный рычаг стоимости прогона.
/// Разбор постановки — один вызов, его не жалко сделать дорогой моделью.
/// Массовая генерация биографий — десять тысяч вызовов, там важна цена токена.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    pub default_text: ModelChoice,
    pub default_image: ModelChoice,
    /// Переопределения по виду задания: `"brief"`, `"biography"`, `"portrait"`.
    #[serde(default)]
    pub by_task: BTreeMap<String, ModelChoice>,
}

impl Routing {
    pub fn text_for(&self, task: &str) -> &ModelChoice {
        self.by_task.get(task).unwrap_or(&self.default_text)
    }

    pub fn image_for(&self, task: &str) -> &ModelChoice {
        self.by_task.get(task).unwrap_or(&self.default_image)
    }
}

/// Сборка провайдеров по конфигурации.
pub struct Providers {
    pub text: Arc<dyn TextModel>,
    pub image: Arc<dyn ImageModel>,
    pub catalog: Arc<Catalog>,
}

/// Что нужно, чтобы поднять провайдеров.
#[derive(Debug, Clone)]
pub struct ProvidersConfig {
    pub xai_key: String,
    pub openai_key: String,
    pub text_model: String,
    pub image_model: String,
    /// Общий исходящий прокси. Пусто — ходим напрямую.
    pub proxy: Option<String>,
}

impl Providers {
    pub fn build(cfg: &ProvidersConfig, catalog: Arc<Catalog>) -> Result<Self> {
        if cfg.xai_key.trim().is_empty() {
            return Err(Error::Config("не задан ключ x.ai".into()));
        }
        if cfg.openai_key.trim().is_empty() {
            return Err(Error::Config("не задан ключ OpenAI".into()));
        }

        let text = OpenAiCompatText::new(
            ProviderConfig::xai(&cfg.xai_key, &cfg.text_model).with_proxy(cfg.proxy.clone()),
            catalog.clone(),
        )?;

        let image = OpenAiImages::new(
            ProviderConfig::openai(&cfg.openai_key, &cfg.image_model)
                .with_proxy(cfg.proxy.clone()),
            catalog.clone(),
        )?;

        Ok(Self {
            text: Arc::new(text),
            image: Arc::new(image),
            catalog,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Arc<Catalog> {
        let mut c = Catalog::default();
        c.insert(ModelInfo {
            id: "test-text".into(),
            provider: "xai".into(),
            input_per_mtok: 0.20,
            output_per_mtok: 0.50,
            per_image: None,
            supports_effort: true,
            supports_schema: true,
            verified: "тест".into(),
        });
        c.insert(ModelInfo {
            id: "test-image".into(),
            provider: "openai".into(),
            input_per_mtok: 0.0,
            output_per_mtok: 0.0,
            per_image: Some(0.04),
            supports_effort: false,
            supports_schema: false,
            verified: "тест".into(),
        });
        Arc::new(c)
    }

    #[test]
    fn cost_is_computed_from_catalog() {
        let c = catalog();
        let info = c.get("test-text").unwrap();
        let u = info.usage(1_000_000, 200_000);
        assert!((u.cost_usd - (0.20 + 0.10)).abs() < 1e-9, "{}", u.cost_usd);

        let img = c.get("test-image").unwrap();
        assert!((img.image_usage(25).cost_usd - 1.0).abs() < 1e-9);
    }

    /// Модель без цены не должна тихо считаться бесплатной.
    #[test]
    fn unknown_model_is_rejected_at_construction() {
        let cfg = ProviderConfig::xai("key", "модель-которой-нет");
        let err = OpenAiCompatText::new(cfg, catalog()).unwrap_err();
        assert!(matches!(err, Error::UnknownModel(_)), "{err}");
    }

    #[test]
    fn unverified_prices_are_reported() {
        let mut c = Catalog::default();
        c.insert(ModelInfo {
            id: "неподтверждённая".into(),
            provider: "x".into(),
            input_per_mtok: 1.0,
            output_per_mtok: 1.0,
            per_image: None,
            supports_effort: false,
            supports_schema: false,
            verified: String::new(),
        });
        assert_eq!(c.unverified(), vec!["неподтверждённая"]);
    }

    #[test]
    fn region_block_is_recognised_not_buried_in_403() {
        let e = openai_compat::classify(
            "openai",
            403,
            r#"{"error":{"code":"unsupported_country_region_territory",
                "message":"Country, region, or territory not supported"}}"#,
        );
        assert!(matches!(e, Error::RegionBlocked { .. }), "{e}");
        assert!(e.is_fatal(), "гео-блок нельзя лечить повторами");
        assert!(!e.is_retryable());
    }

    #[test]
    fn quota_is_fatal_rate_limit_is_not() {
        let quota = openai_compat::classify("openai", 429, r#"{"error":{"code":"insufficient_quota"}}"#);
        assert!(quota.is_fatal(), "кончившиеся деньги повторами не лечатся");

        let rl = openai_compat::classify("xai", 429, r#"{"error":"Too Many Requests"}"#);
        assert!(rl.is_retryable(), "обычный лимит надо переждать");
        assert!(!rl.is_fatal());
    }

    #[test]
    fn server_errors_retry_client_errors_do_not() {
        assert!(openai_compat::classify("x", 503, "upstream").is_retryable());
        assert!(!openai_compat::classify("x", 400, "bad request").is_retryable());
    }

    #[test]
    fn json_is_extracted_from_markdown_fence() {
        let r = TextResponse {
            text: "```json\n{\"name\": \"Иванов\", \"age\": 45}\n```".into(),
            model: "test-text".into(),
            usage: Usage::default(),
        };
        let v = r.json().unwrap();
        assert_eq!(v["name"], "Иванов");
        assert_eq!(v["age"], 45);
    }

    #[test]
    fn non_json_answer_reports_what_came_back() {
        let r = TextResponse {
            text: "Извините, не могу выполнить".into(),
            model: "test-text".into(),
            usage: Usage::default(),
        };
        match r.json() {
            Err(msg) => assert!(
                msg.contains("Извините"),
                "ошибка должна показывать, что именно вернула модель: {msg}"
            ),
            other => panic!("ожидалась внятная ошибка, получено {other:?}"),
        }
    }

    #[test]
    fn routing_falls_back_to_default() {
        let r = Routing {
            default_text: ModelChoice::new("дешёвая", Effort::Low),
            default_image: ModelChoice::new("картинки", Effort::Low),
            by_task: BTreeMap::from([(
                "brief".to_string(),
                ModelChoice::new("умная", Effort::High),
            )]),
        };
        assert_eq!(r.text_for("brief").model, "умная");
        assert_eq!(r.text_for("brief").effort, Effort::High);
        assert_eq!(r.text_for("biography").model, "дешёвая");
    }

    #[test]
    fn usage_accumulates() {
        let a = Usage { tokens_in: 100, tokens_out: 50, cost_usd: 0.001 };
        let b = Usage { tokens_in: 20, tokens_out: 10, cost_usd: 0.0002 };
        let s = a + b;
        assert_eq!(s.tokens_in, 120);
        assert_eq!(s.total_tokens(), 180);
        assert!((s.cost_usd - 0.0012).abs() < 1e-12);
    }
}
