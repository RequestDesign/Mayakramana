#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("очередь: {0}")]
    Store(#[from] synthforge_store::Error),

    #[error("план: {0}")]
    Plan(#[from] synthforge_params::Error),

    #[error("{0}")]
    Port(synthforge_ports::PortError),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("сессия {0} не найдена")]
    SessionNotFound(String),
}

impl Error {
    /// Прогон надо останавливать: кончились деньги, закрыт регион, неверный
    /// ключ. Продолжать — значит жечь время на заведомо неудачные попытки.
    pub fn is_fatal(&self) -> bool {
        match self {
            Error::Port(p) => p.is_fatal(),
            Error::SessionNotFound(_) => true,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
