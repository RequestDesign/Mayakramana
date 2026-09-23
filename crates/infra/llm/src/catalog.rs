use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use synthforge_ports::Usage;

/// Запись каталога моделей.
///
/// Цены **не зашиты в код**: они меняются, и захардкоженное число незаметно
/// разъедется с реальностью, а счёт за прогон на тысячи сущностей заметят
/// поздно. Каталог — отдельный файл, который обновляется независимо от сборки.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Идентификатор модели в API провайдера.
    pub id: String,
    pub provider: String,
    /// Цена за миллион входных токенов, USD.
    pub input_per_mtok: f64,
    /// Цена за миллион выходных токенов, USD.
    pub output_per_mtok: f64,
    /// Для моделей изображений: цена за одно изображение, USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_image: Option<f64>,
    /// Поддерживает ли управление интеллектуальным усилием.
    #[serde(default)]
    pub supports_effort: bool,
    /// Поддерживает ли структурированный вывод по JSON-схеме.
    #[serde(default)]
    pub supports_schema: bool,
    /// Откуда взяты цифры и когда сверены. Пустое поле — повод не доверять.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub verified: String,
}

impl ModelInfo {
    pub fn cost(&self, tokens_in: u32, tokens_out: u32) -> f64 {
        (tokens_in as f64 / 1_000_000.0) * self.input_per_mtok
            + (tokens_out as f64 / 1_000_000.0) * self.output_per_mtok
    }

    pub fn usage(&self, tokens_in: u32, tokens_out: u32) -> Usage {
        Usage {
            tokens_in,
            tokens_out,
            cost_usd: self.cost(tokens_in, tokens_out),
        }
    }

    pub fn image_usage(&self, count: u32) -> Usage {
        Usage {
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: self.per_image.unwrap_or(0.0) * count as f64,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub models: BTreeMap<String, ModelInfo>,
}

impl Catalog {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("каталог моделей {}: {e}", path.display())))?;
        let c: Self = serde_json::from_str(&src)
            .map_err(|e| Error::Config(format!("каталог моделей {}: {e}", path.display())))?;
        Ok(c)
    }

    pub fn get(&self, model: &str) -> Result<&ModelInfo> {
        self.models
            .get(model)
            .ok_or_else(|| Error::UnknownModel(model.to_string()))
    }

    pub fn insert(&mut self, info: ModelInfo) {
        self.models.insert(info.id.clone(), info);
    }

    /// Модели, у которых не проставлено поле `verified`.
    ///
    /// Вызывается при старте: неподтверждённая цена означает, что учёт расхода
    /// будет врать, а бюджетный потолок не сработает.
    pub fn unverified(&self) -> Vec<&str> {
        self.models
            .values()
            .filter(|m| m.verified.trim().is_empty())
            .map(|m| m.id.as_str())
            .collect()
    }
}
