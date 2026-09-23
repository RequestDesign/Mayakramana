//! Типизированный клиент Nexorium.
//!
//! Задача крейта — сделать известные ловушки API невозможными конструктивно,
//! а не «мы про них помним». Каждая закрыта типом или сигнатурой:
//!
//! | Ловушка | Чем закрыта |
//! |---|---|
//! | одиночное создание ждёт `{"data": …}`, bulk — сырые объекты | разные методы [`Nexorium::create_one`] / [`Nexorium::create_bulk`], форма задаётся типом |
//! | любой неизвестный query-параметр — это фильтр по полю | [`Query::filter`] отвергает зарезервированные имена |
//! | без явного `sort` пагинация неустойчива | [`Query::new`] требует [`Sort`] обязательным аргументом |
//! | сортировка по JSONB-полю крайне медленная | [`Nexorium::export`] как штатный способ полного чтения |
//! | `POST` повторять нельзя | [`Idempotency`] управляет политикой повторов; [`Nexorium::verify_batch`] разрешает неопределённость |
//! | `PATCH` порождает ревизию и может заменять запись целиком | метод назван [`Nexorium::replace`] и требует полный набор полей |
//! | массовое обновление ~100 записей в минуту | в документации метода; штатный путь — удалить пачку и вставить заново |

mod client;
mod error;
mod query;
mod store;
mod types;

pub use client::{BatchVerdict, Config, Nexorium, BULK_MAX};
pub use store::NexoriumStore;
pub use error::{Error, Idempotency, Result};
pub use query::{Dir, Query, Sort};
pub use types::{
    BulkOp, BulkRequest, Collection, CollectionId, FieldSpec, FieldType, Page, Pagination, Record,
    SearchHit,
};

/// Служебные поля, которыми помечается каждая контентная запись.
///
/// `batch_id` делает возможной сверку после обрыва на POST,
/// `natural_key` — сверку и дедупликацию при потере локального состояния.
pub mod meta {
    pub const BATCH_ID: &str = "batch_id";
    pub const NATURAL_KEY: &str = "natural_key";
    pub const SESSION_ID: &str = "session_id";
    pub const GENERATED_AT: &str = "generated_at";

    /// Поля, которые должны существовать в любой генерируемой коллекции.
    pub fn required_fields() -> Vec<crate::FieldSpec> {
        use crate::{FieldSpec, FieldType};
        vec![
            FieldSpec::new("Пачка", BATCH_ID, FieldType::Text),
            FieldSpec::new("Естественный ключ", NATURAL_KEY, FieldType::Text),
            FieldSpec::new("Сессия", SESSION_ID, FieldType::Text),
            FieldSpec::new("Сгенерировано", GENERATED_AT, FieldType::Datetime),
        ]
    }
}
