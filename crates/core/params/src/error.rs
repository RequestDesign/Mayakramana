#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("разбор выражения: {0}")]
    Parse(String),

    #[error("вычисление выражения: {0}")]
    Eval(String),

    #[error("словарь параметров некорректен:\n{}", .0.join("\n"))]
    Invalid(Vec<String>),

    #[error("параметр «{0}» не объявлен в словаре")]
    UnknownParam(String),

    #[error("не удалось собрать строку за {attempts} попыток; нарушается правило «{rule}»")]
    Unsatisfiable { attempts: u32, rule: String },

    #[error("не удалось добиться уникальности за {attempts} попыток — пространство параметров слишком тесное для {wanted} сущностей")]
    ExhaustedUniqueness { attempts: u32, wanted: usize },

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
