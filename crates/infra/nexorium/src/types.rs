use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Идентификатор коллекции.
///
/// Отдельный тип, потому что записи адресуются **идентификатором коллекции, а не
/// слагом**: `/collections/{uuid}/records`. Слаг участвует только в поиске самой
/// коллекции. Раньше это место принимало `&str`, и слаг подставлялся молча,
/// давая 404 вместо внятной ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CollectionId(pub Uuid);

impl std::fmt::Display for CollectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Общая обёртка всех ответов Nexorium: полезная нагрузка лежит под `data`,
/// рядом могут быть служебные ключи вроде `pagination`.
#[derive(Debug, Deserialize)]
pub(crate) struct Envelope<T> {
    pub data: T,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Number,
    Boolean,
    Date,
    Datetime,
    Email,
    Url,
    Phone,
    Select,
    MultiSelect,
    Json,
    Relation,
    File,
    Image,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldSpec {
    pub name: String,
    pub slug: String,
    pub field_type: FieldType,
    /// Сервер отклоняет запись с уже существующим значением поля (мягко
    /// удалённые не в счёт).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_unique: bool,
}

impl FieldSpec {
    pub fn new(name: impl Into<String>, slug: impl Into<String>, field_type: FieldType) -> Self {
        Self { name: name.into(), slug: slug.into(), field_type, is_unique: false }
    }

    pub fn unique(mut self) -> Self {
        self.is_unique = true;
        self
    }
}

/// Запись коллекции. Полезная нагрузка всегда под `data`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Record {
    pub id: Uuid,
    #[serde(default)]
    pub data: serde_json::Value,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Record {
    pub fn field(&self, key: &str) -> Option<&serde_json::Value> {
        self.data.get(key)
    }

    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(|v| v.as_str())
    }

    /// Признак порчи: поля легли на уровень `data.data.*` — так бывает, если
    /// элементы bulk обернули так же, как тело одиночного создания. Записи
    /// создаются, ответ успешный, но их не находит ни один фильтр.
    pub fn is_double_wrapped(&self) -> bool {
        self.data.get("data").is_some_and(|v| v.is_object())
    }

    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.data.clone())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Pagination {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub per_page: u32,
}

/// Страница выдачи: записи под `data`, счётчики — соседним ключом.
#[derive(Debug, Clone, Deserialize)]
pub struct Page {
    #[serde(rename = "data")]
    pub records: Vec<Record>,
    #[serde(default)]
    pub pagination: Pagination,
}

/// Попадание встроенного поиска Nexorium (полнотекстовый + семантический).
#[derive(Debug, Clone, Deserialize)]
pub struct SearchHit {
    pub id: Uuid,
    #[serde(default)]
    pub data: serde_json::Value,
    #[serde(default)]
    pub collection_slug: Option<String>,
    #[serde(default)]
    pub rank: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkRequest<'a, T> {
    pub operation: BulkOp,
    pub records: &'a [T],
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkOp {
    Create,
    Update,
    Delete,
}

/// Обёртка одиночного создания и обновления. Существует только чтобы форма тела
/// задавалась типом, а не памятью разработчика.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SingleEnvelope<'a, T> {
    pub data: &'a T,
}
