#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("справочник некорректен:\n{}", .0.join("\n"))]
    Invalid(Vec<String>),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("файл {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
