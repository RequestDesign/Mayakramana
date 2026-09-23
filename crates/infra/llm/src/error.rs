#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("транспорт ({provider}): {source}")]
    Transport {
        provider: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("{provider} вернул {status}: {body}")]
    Api {
        provider: String,
        status: u16,
        body: String,
    },

    /// Отдельно от прочих 4xx: чинится деньгами или ключом, а не повтором.
    #[error("{provider}: недостаточно квоты или средств")]
    Quota { provider: String },

    /// Гео-блокировка. С российского IP это штатный ответ OpenAI и Anthropic,
    /// поэтому сообщение должно быть однозначным, а не «ошибка 403».
    #[error("{provider}: доступ из этой страны запрещён — нужен исходящий прокси")]
    RegionBlocked { provider: String },

    #[error("{provider}: превышен лимит запросов")]
    RateLimited { provider: String },

    #[error("{provider}: ответ не разобрался: {0}", source)]
    Decode {
        provider: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("модель вернула текст вместо ожидаемой структуры: {0}")]
    NotStructured(String),

    #[error("модель «{0}» не найдена в каталоге — неизвестна цена, генерация заблокирована")]
    UnknownModel(String),

    #[error("исчерпаны попытки ({attempts}): {last}")]
    Exhausted { attempts: u32, last: Box<Error> },

    #[error("конфигурация: {0}")]
    Config(String),
}

impl Error {
    /// Повтор имеет смысл: сбой преходящий.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Transport { .. } | Error::RateLimited { .. } => true,
            Error::Api { status, .. } => *status >= 500,
            Error::Exhausted { last, .. } => last.is_retryable(),
            _ => false,
        }
    }

    /// Повтор бесполезен и сессию надо останавливать: кончились деньги,
    /// закрыт регион, неверный ключ. Дальнейшие задания только сожгут время.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Error::Quota { .. } | Error::RegionBlocked { .. } | Error::Config(_)
        ) || matches!(self, Error::Api { status: 401, .. } | Error::Api { status: 403, .. })
    }

    pub fn provider(&self) -> Option<&str> {
        match self {
            Error::Transport { provider, .. }
            | Error::Api { provider, .. }
            | Error::Quota { provider }
            | Error::RegionBlocked { provider }
            | Error::RateLimited { provider }
            | Error::Decode { provider, .. } => Some(provider),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Схлопывание в порт: наружу уходит только «преходящее / отказ / фатальное».
///
/// Подробная классификация нужна внутри — для решения о повторе. Движку выше
/// достаточно знать, останавливать ли сессию.
impl From<Error> for synthforge_ports::PortError {
    fn from(e: Error) -> Self {
        use synthforge_ports::PortError as P;
        if e.is_fatal() {
            P::Fatal(e.to_string())
        } else if e.is_retryable() {
            P::Unavailable(e.to_string())
        } else {
            P::Rejected(e.to_string())
        }
    }
}
