use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Обрыв канала, таймаут, DNS. Ключевой случай: если это произошло на POST,
    /// **неизвестно**, выполнил сервер запрос или нет. Слепой повтор создаст дубль.
    #[error("транспорт: {0}")]
    Transport(#[source] reqwest::Error),

    #[error("Nexorium {status}: {body}")]
    Api { status: u16, body: String },

    /// 429 — запрос отвергли, не начав выполнять. Повторять безопасно даже POST.
    #[error("лимит запросов исчерпан")]
    RateLimited,

    #[error("разбор ответа: {0}")]
    Decode(#[source] serde_json::Error),

    #[error("превышен размер пачки: {got} при максимуме {max}")]
    BatchTooLarge { got: usize, max: usize },

    #[error("конфигурация: {0}")]
    Config(String),

    #[error("исчерпаны попытки ({attempts}), последняя ошибка: {last}")]
    Exhausted { attempts: u32, last: Box<Error> },
}

impl Error {
    /// Исход запроса неизвестен: сервер мог его выполнить, мог не выполнить.
    ///
    /// Для POST это означает «нельзя повторять вслепую, нужна сверка по `batch_id`».
    pub fn is_indeterminate(&self) -> bool {
        match self {
            Error::Transport(_) => true,
            // 5xx: сервер мог упасть уже после записи
            Error::Api { status, .. } => *status >= 500,
            Error::Exhausted { last, .. } => last.is_indeterminate(),
            _ => false,
        }
    }

    /// Повтор безопасен независимо от идемпотентности метода:
    /// сервер гарантированно ничего не сделал.
    pub fn is_definitely_rejected(&self) -> bool {
        matches!(self, Error::RateLimited)
    }

    /// Сервер отверг запись, потому что значение уникального поля уже есть.
    /// Для нас это значит «запись уже в базе», а не ошибка.
    pub fn is_unique_conflict(&self) -> bool {
        match self {
            Error::Api { status, body } => {
                (*status == 400 || *status == 409) && body.contains("must be unique")
            }
            Error::Exhausted { last, .. } => last.is_unique_conflict(),
            _ => false,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            Error::RateLimited => Some(429),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Класс идемпотентности запроса. Определяет политику повторов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    /// GET, PATCH, DELETE — повтор даёт тот же результат, повторяем свободно.
    Safe,
    /// POST — повтор создаёт дубликат. Повторяем **только** при 429.
    Unsafe,
}

impl fmt::Display for Idempotency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Idempotency::Safe => write!(f, "safe"),
            Idempotency::Unsafe => write!(f, "unsafe"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Текст — дословно из ответа живого сервера на повтор natural_key.
    #[test]
    fn unique_conflict_is_recognized() {
        let e = Error::Api {
            status: 400,
            body: r#"{"error":{"code":"BAD_REQUEST","message":"Bulk create failed at record 1: Conflict: Field 'Естественный ключ': value must be unique"}}"#.into(),
        };
        assert!(e.is_unique_conflict());
        assert!(!e.is_indeterminate(), "дубль — не неизвестный исход");

        let other = Error::Api { status: 400, body: "bad field".into() };
        assert!(!other.is_unique_conflict());
    }
}
