//! Проверка клиента на живом Nexorium — **только чтение**.
//!
//! Пробник (`probe`) смотрит сырой JSON мимо клиента. Этот пример идёт через
//! сам клиент: разбирает ли он живые ответы в свои типы — список коллекций,
//! страницу записей со счётчиком, подсчёт, экспорт. Ничего не создаёт и не
//! меняет, поэтому его можно запускать с ключом чужого пространства.
//!
//! ```
//! set NEXORIUM_SPACE=<uuid>
//! set NEXORIUM_KEY=nxr_...
//! cargo run -p synthforge-nexorium --example read_check -- [slug]
//! ```

use synthforge_nexorium::{Config, Nexorium, Query, Sort};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let space = std::env::var("NEXORIUM_SPACE").map_err(|_| "не задан NEXORIUM_SPACE")?;
    let key = std::env::var("NEXORIUM_KEY").map_err(|_| "не задан NEXORIUM_KEY")?;
    let mut cfg = Config::external(space, key)?;
    if let Ok(base) = std::env::var("NEXORIUM_BASE") {
        cfg = Config::at(&base, cfg.space, cfg.api_key)?;
    }
    let nx = Nexorium::new(cfg)?;

    let cols = nx.collections().await?;
    println!("коллекций: {}", cols.len());
    let slug = std::env::args().nth(1);
    let col = match &slug {
        Some(s) => nx.collection_by_slug(s).await?.ok_or(format!("нет коллекции «{s}»"))?,
        None => cols.first().cloned().ok_or("в пространстве нет коллекций")?,
    };
    println!("коллекция «{}» ({}) → {}", col.name, col.slug, col.id);

    let page = nx.list(col.id, &Query::new(Sort::asc("id")).per_page(2)).await?;
    println!(
        "страница: {} записей, всего {} (page {}, per_page {})",
        page.records.len(),
        page.pagination.total,
        page.pagination.page,
        page.pagination.per_page
    );
    for r in &page.records {
        let keys: Vec<&String> = r.data.as_object().map(|m| m.keys().collect()).unwrap_or_default();
        println!("  запись {} · поля {:?} · двойная обёртка: {}", r.id, keys, r.is_double_wrapped());
    }

    let n = nx.count(col.id, &[]).await?;
    println!("count: {n}");

    let all = nx.export(col.id).await?;
    println!("export: {} записей, id в экспорте: {}", all.len(), all.iter().any(|v| v.get("id").is_some()));

    if n as usize != all.len() {
        println!("⚠ count и export разошлись: {n} против {}", all.len());
    } else {
        println!("\nКлиент разбирает живые ответы: всё сходится.");
    }
    Ok(())
}
