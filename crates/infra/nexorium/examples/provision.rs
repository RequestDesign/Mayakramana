//! Подготовка пространства synthforge в Nexorium.
//!
//! ```
//! cargo run -p synthforge-nexorium --example provision -- smoke   — проверка записи на временной коллекции
//! cargo run -p synthforge-nexorium --example provision -- create  — коллекции всех видов сущностей
//! ```
//!
//! Доступы берутся из `.env` в корне проекта (или из окружения): нужен ключ
//! своего пространства с правами `admin`.

use std::sync::Arc;

use serde_json::json;
use synthforge_nexorium::{Config, Nexorium, NexoriumStore, Query, Sort};
use synthforge_ports::{BatchVerdict, ContentStore, WriteOutcome};

/// Коллекции, в которые пишут генераторы.
const COLLECTIONS: &[(&str, &str)] = &[
    ("doctors", "Врачи"),
    ("consultants", "Консультанты"),
    ("psychologists", "Психологи"),
    ("directors", "Руководители"),
    ("places", "Здания"),
    ("programs", "Программы"),
    ("centers", "Центры"),
];

const SMOKE: &str = "smoke-test";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();
    let space = std::env::var("NEXORIUM_SPACE").map_err(|_| "не задан NEXORIUM_SPACE")?;
    let key = std::env::var("NEXORIUM_KEY").map_err(|_| "не задан NEXORIUM_KEY")?;
    let base = std::env::var("NEXORIUM_BASE")
        .unwrap_or_else(|_| "https://nexorium.trger.ru/api/v1".into());
    let api = Arc::new(Nexorium::new(Config::at(&base, space, key)?)?);
    let store = NexoriumStore::new(api.clone());

    match std::env::args().nth(1).as_deref() {
        Some("smoke") => smoke(&api, &store).await,
        Some("create") => {
            for (slug, title) in COLLECTIONS {
                store.ensure_collection(slug, title).await?;
                println!("✓ {slug} ({title})");
            }
            Ok(())
        }
        _ => Err("режим: smoke или create".into()),
    }
}

/// Запись и чтение на временной коллекции, которая потом удаляется.
async fn smoke(api: &Nexorium, store: &NexoriumStore) -> Result<(), Box<dyn std::error::Error>> {
    store.ensure_collection(SMOKE, "Проверка записи").await?;
    let col = api.collection_by_slug(SMOKE).await?.ok_or("коллекция не создалась")?;
    println!("коллекция создана: {}", col.id);

    let result = async {
        // Записи с полями, которых нет в схеме, и вложенным списком — как
        // настоящие карточки: поля параметров в схему не заводятся.
        let records = vec![
            json!({ "natural_key": "smoke:1", "full_name": "Проверка Один", "age": 44,
                    "images": [{"role": "portrait", "path": "x.png"}] }),
            json!({ "natural_key": "smoke:2", "full_name": "Проверка Два", "age": 51 }),
        ];
        let outcome = store.write_batch(SMOKE, "batch-smoke-1", &records).await?;
        println!("запись пачки: {outcome:?}");
        if !matches!(outcome, WriteOutcome::Committed) {
            return Err::<(), Box<dyn std::error::Error>>("пачка не подтверждена".into());
        }

        let verdict = store.verify_batch(SMOKE, "batch-smoke-1", 2).await?;
        println!("сверка по batch_id: {verdict:?}");
        if !matches!(verdict, BatchVerdict::Committed) {
            return Err("сверка не нашла пачку".into());
        }

        let page = api.list(col.id, &Query::new(Sort::asc("natural_key")).per_page(10)).await?;
        for r in &page.records {
            println!(
                "  {} · поля {:?} · двойная обёртка: {}",
                r.str_field("natural_key").unwrap_or("?"),
                r.data.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                r.is_double_wrapped()
            );
        }
        let first = page.records.first().ok_or("записи не читаются")?;
        if first.is_double_wrapped() || first.field("full_name").is_none() {
            return Err("поля легли не туда".into());
        }

        // Повтор того же natural_key должен отбиваться сервером.
        let dup = vec![json!({ "natural_key": "smoke:1", "full_name": "Дубль" })];
        match store.write_batch(SMOKE, "batch-smoke-2", &dup).await {
            Ok(WriteOutcome::Committed) => {
                let n = api.count(col.id, &[("natural_key".into(), "smoke:1".into())]).await?;
                println!("⚠ дубль принят, записей с ключом smoke:1: {n} — уникальность не работает");
            }
            other => println!("дубль отклонён: {other:?}"),
        }
        Ok(())
    }
    .await;

    api.delete_collection(col.id).await?;
    println!("временная коллекция удалена");
    result
}

/// `.env` из корня проекта: пример запускается из любой папки.
fn load_dotenv() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../.env");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !v.is_empty() && std::env::var(k).is_err() {
                std::env::set_var(k, v);
            }
        }
    }
}
