use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Pending,
    Planning,
    Running,
    Paused,
    Failed,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    /// Временная неудача, задание вернулось в очередь.
    Failed,
    /// Попытки исчерпаны, задание больше не берётся.
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    Pending,
    /// POST отправлен, ответа ещё нет.
    Inflight,
    /// Соединение оборвалось. Исход неизвестен, нужна сверка по `batch_id`.
    Unknown,
    Committed,
    Failed,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Session {
    pub id: String,
    pub kind: String,
    pub spec: String,
    pub status: SessionStatus,
    pub seed: i64,
    pub budget_usd: Option<f64>,
    pub spent_usd: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Session {
    pub fn spec_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.spec)
    }

    /// Бюджет исчерпан — сессию надо остановить, не начиная новых заданий.
    pub fn over_budget(&self) -> bool {
        self.budget_usd.is_some_and(|b| self.spent_usd >= b)
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Job {
    pub id: i64,
    pub session_id: String,
    pub kind: String,
    pub natural_key: String,
    pub payload: String,
    pub status: JobStatus,
    pub attempts: i64,
    pub max_attempts: i64,
    pub locked_by: Option<String>,
    pub locked_at: Option<i64>,
    pub last_error: Option<String>,
    pub result: Option<String>,
    pub batch_id: Option<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Job {
    pub fn payload_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.payload)
    }

    pub fn result_json(&self) -> Option<Result<serde_json::Value, serde_json::Error>> {
        self.result.as_deref().map(serde_json::from_str)
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Batch {
    pub id: String,
    pub session_id: String,
    pub collection: String,
    pub expected: i64,
    pub status: BatchStatus,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewSession {
    pub kind: String,
    pub spec: serde_json::Value,
    pub seed: i64,
    pub budget_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: String,
    /// Детерминированный ключ. Повторная постановка того же задания в очередь
    /// не создаст дубликата — планировщик можно перезапускать свободно.
    pub natural_key: String,
    pub payload: serde_json::Value,
    pub max_attempts: i64,
}

impl NewJob {
    pub fn new(
        kind: impl Into<String>,
        natural_key: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            kind: kind.into(),
            natural_key: natural_key.into(),
            payload,
            max_attempts: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub pending: i64,
    pub running: i64,
    pub done: i64,
    pub failed: i64,
    pub dead: i64,
    pub spent_usd: f64,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

impl Progress {
    pub fn total(&self) -> i64 {
        self.pending + self.running + self.done + self.failed + self.dead
    }

    /// Работы не осталось: всё либо готово, либо признано безнадёжным.
    pub fn is_settled(&self) -> bool {
        self.pending == 0 && self.running == 0 && self.failed == 0
    }

    pub fn percent(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            0.0
        } else {
            (self.done + self.dead) as f64 * 100.0 / t as f64
        }
    }
}
