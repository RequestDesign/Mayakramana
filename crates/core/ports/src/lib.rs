//! Порты ядра: контракты, которые реализует внешний слой.
//!
//! Зависимости направлены внутрь. Ядро объявляет, что ему нужно от мира, а
//! инфраструктура это реализует. Поэтому генератор не знает, что хранилище —
//! Nexorium, а модель — Grok, и смена того или другого не трогает ни одного
//! генератора.
//!
//! Два способа обращения к модели — прямой HTTP и агент по протоколу — это
//! просто две реализации [`TextModel`], а не развилка внутри генераторов.

use async_trait::async_trait;

mod generator;
mod image;
mod text;

pub use generator::{
    AcceptedText, EntityDescriptor, GenMode, GenSpec, Generator, RejectReason, Scope, Uniqueness,
    UniquenessLevel,
};
pub use image::{ImageRequest, ImageResponse, ImageSize};
pub use text::{Effort, Message, Role, TextRequest, TextResponse, Usage};

#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("{0}")]
    Unavailable(String),

    #[error("{0}")]
    Rejected(String),

    /// Повтор бесполезен, работу надо останавливать: кончились деньги, закрыт
    /// регион, неверный ключ.
    #[error("{0}")]
    Fatal(String),
}

impl PortError {
    pub fn is_fatal(&self) -> bool {
        matches!(self, PortError::Fatal(_))
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, PortError::Unavailable(_))
    }
}

pub type PortResult<T> = Result<T, PortError>;

/// Порт текстовой модели.
#[async_trait]
pub trait TextModel: Send + Sync {
    fn id(&self) -> &str;
    fn provider(&self) -> &str;
    async fn complete(&self, req: TextRequest) -> PortResult<TextResponse>;
}

/// Порт модели изображений.
#[async_trait]
pub trait ImageModel: Send + Sync {
    fn id(&self) -> &str;
    fn provider(&self) -> &str;
    async fn render(&self, req: ImageRequest) -> PortResult<ImageResponse>;
}

/// Исход записи пачки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Committed,
    /// Соединение оборвалось, исход неизвестен — нужна сверка по метке пачки.
    /// Слепой повтор создаст дубликаты.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchVerdict {
    Committed,
    Absent,
    Partial { found: u64 },
}

/// Порт хранилища готового контента.
///
/// Запись идёт пачками и только вперёд: точечная правка записей не
/// предусмотрена намеренно. Перегенерация — это удаление пачки и вставка новой.
#[async_trait]
pub trait ContentStore: Send + Sync {
    /// Убедиться, что коллекция и её служебные поля существуют.
    async fn ensure_collection(&self, slug: &str, title: &str) -> PortResult<()>;

    async fn write_batch(
        &self,
        collection: &str,
        batch_id: &str,
        records: &[serde_json::Value],
    ) -> PortResult<WriteOutcome>;

    /// Разрешить неопределённый исход записи.
    async fn verify_batch(
        &self,
        collection: &str,
        batch_id: &str,
        expected: u64,
    ) -> PortResult<BatchVerdict>;

    /// Откатить частично легшую пачку.
    async fn delete_batch(&self, collection: &str, batch_id: &str) -> PortResult<usize>;

    async fn count(&self, collection: &str) -> PortResult<u64>;
}

/// Порт хранилища файлов.
///
/// Отделён от [`ContentStore`], потому что у них разный профиль: записей —
/// тысячи мелких пачками, файлов — десятки тысяч по мегабайту поштучно.
/// Сегодня это диск, завтра может стать объектное хранилище, и генераторов
/// это не коснётся.
#[async_trait]
pub trait AssetStore: Send + Sync {
    /// Сохранить файл и вернуть его адрес.
    async fn put(
        &self,
        session_id: &str,
        entity_key: &str,
        role: &str,
        bytes: &[u8],
    ) -> PortResult<String>;
}

#[derive(Debug, Clone)]
pub struct Neighbour {
    pub id: String,
    pub score: f32,
    pub text: String,
}

/// Порт поиска похожего.
///
/// Нужен для второго режима генерации — уникальности относительно базы.
/// Сравнение «каждый с каждым» на десяти тысячах сущностей это пятьдесят
/// миллионов пар, поэтому сверяемся только с ближайшими соседями.
#[async_trait]
pub trait SimilarityIndex: Send + Sync {
    /// Ближайшие к тексту записи в пределах коллекции.
    async fn nearest(&self, collection: &str, text: &str, k: usize)
        -> PortResult<Vec<Neighbour>>;

    /// Добавить текст в индекс.
    async fn index(&self, collection: &str, id: &str, text: &str) -> PortResult<()>;
}
