use serde::{Deserialize, Serialize};

/// Интеллектуальное усилие.
///
/// Существует ради расхода: тратить дорогой ризонинг там, где надо переписать
/// набор параметров человеческим языком, — значит умножить счёт на порядок без
/// выигрыша в качестве.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Массовая генерация по готовым параметрам.
    #[default]
    Low,
    /// Составные сущности, подбор, разбор постановки.
    Medium,
    /// Проектирование словарей, разбор сложных постановок.
    High,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Self {
        Self { role: Role::System, content: c.into() }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self { role: Role::User, content: c.into() }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: c.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextRequest {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub effort: Effort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// JSON-схема ответа. Если задана, модель обязана вернуть структуру —
    /// это убирает целый класс проблем с разбором свободного текста.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

impl TextRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self { messages, effort: Effort::Low, max_tokens: None, temperature: None, schema: None }
    }

    pub fn effort(mut self, e: Effort) -> Self {
        self.effort = e;
        self
    }

    pub fn schema(mut self, s: serde_json::Value) -> Self {
        self.schema = Some(s);
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }
}

/// Расход по одному вызову. Считается сразу, а не постфактум по логам:
/// бюджет прогона должен быть виден в реальном времени.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub cost_usd: f64,
}

impl Usage {
    pub fn total_tokens(&self) -> u32 {
        self.tokens_in + self.tokens_out
    }
}

impl std::ops::Add for Usage {
    type Output = Usage;
    fn add(self, o: Usage) -> Usage {
        Usage {
            tokens_in: self.tokens_in + o.tokens_in,
            tokens_out: self.tokens_out + o.tokens_out,
            cost_usd: self.cost_usd + o.cost_usd,
        }
    }
}

impl std::iter::Sum for Usage {
    fn sum<I: Iterator<Item = Usage>>(iter: I) -> Usage {
        iter.fold(Usage::default(), |a, b| a + b)
    }
}

#[derive(Debug, Clone)]
pub struct TextResponse {
    pub text: String,
    pub model: String,
    pub usage: Usage,
}

impl TextResponse {
    /// Разобрать ответ как JSON.
    ///
    /// Модели любят оборачивать структуру в тройные кавычки с пометкой языка —
    /// снимаем обёртку, прежде чем разбирать.
    pub fn json(&self) -> Result<serde_json::Value, String> {
        let t = self.text.trim();
        let cleaned = t
            .strip_prefix("```json")
            .or_else(|| t.strip_prefix("```"))
            .map(|s| s.trim_start())
            .and_then(|s| s.strip_suffix("```"))
            .map(|s| s.trim())
            .unwrap_or(t);

        serde_json::from_str(cleaned)
            .map_err(|e| format!("ответ не разобрался как JSON ({e}): {}", head(&self.text, 300)))
    }
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
