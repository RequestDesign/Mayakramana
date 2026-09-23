#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sql(#[from] sqlx::Error),

    #[error("миграция: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("сессия {0} не найдена")]
    SessionNotFound(String),

    #[error("задание {0} не найдено")]
    JobNotFound(i64),
}

pub type Result<T> = std::result::Result<T, Error>;
