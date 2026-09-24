use serde::{Deserialize, Serialize};

use crate::text::Usage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSize {
    Square,
    /// Портреты персонала.
    Portrait,
    /// Интерьеры, территория, здания.
    Landscape,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRequest {
    /// Назначение снимка: `portrait`, `room`, `territory`.
    ///
    /// У сущности их несколько, и помечать при приёмке надо каждый по
    /// отдельности: «портреты хорошие, а территория вся одинаковая».
    #[serde(default = "default_role")]
    pub role: String,
    pub prompt: String,
    #[serde(default = "default_size")]
    pub size: ImageSize,
    /// Опорное изображение.
    ///
    /// Несколько ракурсов одного здания или портрет одного человека в разных
    /// контекстах текстовым промптом не получить — выйдут разные объекты.
    /// Референс задаёт, что это тот же самый объект.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_png: Option<Vec<u8>>,
    /// Назначение уже сделанного снимка той же сущности, который надо взять
    /// опорой: территория снимается с опорой на фасад, чтобы на обоих был один
    /// и тот же дом. Сам снимок подставляет движок — генератор знает только,
    /// на что опираться.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_role: Option<String>,
    /// Чего на изображении быть не должно. Хранится отдельно от промпта:
    /// одни провайдеры принимают отрицания в тексте, другие — отдельным полем.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid: Vec<String>,
}

fn default_size() -> ImageSize {
    ImageSize::Square
}

fn default_role() -> String {
    "image".to_string()
}

impl ImageRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            role: default_role(),
            prompt: prompt.into(),
            size: ImageSize::Square,
            reference_png: None,
            reference_role: None,
            avoid: Vec::new(),
        }
    }

    pub fn based_on(mut self, role: impl Into<String>) -> Self {
        self.reference_role = Some(role.into());
        self
    }

    pub fn role(mut self, r: impl Into<String>) -> Self {
        self.role = r.into();
        self
    }

    pub fn size(mut self, s: ImageSize) -> Self {
        self.size = s;
        self
    }

    pub fn reference(mut self, png: Vec<u8>) -> Self {
        self.reference_png = Some(png);
        self
    }

    pub fn avoid(mut self, items: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.avoid.extend(items.into_iter().map(Into::into));
        self
    }

    /// Промпт вместе с запретами — для провайдеров, у которых нет отдельного поля.
    pub fn full_prompt(&self) -> String {
        if self.avoid.is_empty() {
            self.prompt.clone()
        } else {
            format!("{}\n\nНе должно быть: {}.", self.prompt, self.avoid.join("; "))
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageResponse {
    pub png: Vec<u8>,
    pub model: String,
    pub usage: Usage,
}
