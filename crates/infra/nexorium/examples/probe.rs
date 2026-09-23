//! Пробник форм ответа Nexorium.
//!
//! Клиент разбирает ответы в типы, и часть обёрток (`records` / `data` / `items`)
//! взята по догадке — в инструкции они не зафиксированы. Пробник ходит к живому
//! серверу **мимо** клиента и печатает сырой JSON, чтобы догадки проверить.
//!
//! ```
//! set NEXORIUM_BASE=https://nexorium.trger.ru/api/v1
//! set NEXORIUM_SPACE=<uuid>
//! set NEXORIUM_KEY=nxr_...
//! cargo run -p synthforge-nexorium --example probe -- <collection-slug>
//! ```

use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = env::var("NEXORIUM_BASE")
        .unwrap_or_else(|_| "https://nexorium.trger.ru/api/v1".to_string());
    let space = env::var("NEXORIUM_SPACE").map_err(|_| "не задан NEXORIUM_SPACE")?;
    let key = env::var("NEXORIUM_KEY").map_err(|_| "не задан NEXORIUM_KEY")?;
    let collection = env::args().nth(1);

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let get = |url: String| {
        let http = http.clone();
        let key = key.clone();
        async move {
            let resp = http.get(&url).header("X-Api-Key", &key).send().await?;
            let status = resp.status();
            let body = resp.text().await?;
            Ok::<_, reqwest::Error>((status, body))
        }
    };

    let show = |label: &str, status: reqwest::StatusCode, body: &str| {
        println!("\n=== {label} ===");
        println!("HTTP {status}");
        match serde_json::from_str::<serde_json::Value>(body) {
            Ok(v) => {
                println!("ключи верхнего уровня: {:?}", top_keys(&v));
                let pretty = serde_json::to_string_pretty(&truncate(&v, 3)).unwrap_or_default();
                println!("{}", head(&pretty, 60));
            }
            Err(_) => println!("{}", head(body, 20)),
        }
    };

    let (st, body) = get(format!("{base}/spaces/{space}/collections")).await?;
    show("GET /collections", st, &body);

    let Some(col) = collection else {
        println!("\nСлаг коллекции не передан — дальше не иду.");
        println!("Запуск: cargo run -p synthforge-nexorium --example probe -- <slug>");
        return Ok(());
    };

    let (st, body) = get(format!(
        "{base}/spaces/{space}/collections/{col}/records?per_page=2&sort=id:asc"
    ))
    .await?;
    show("GET /records?per_page=2&sort=id:asc", st, &body);
    println!("^ проверить: есть ли pagination.total, и под каким ключом лежит массив");
    println!("^ проверить: поля записи лежат под data.*, а НЕ под data.data.*");

    let (st, body) = get(format!(
        "{base}/spaces/{space}/collections/{col}/records/export?format=json"
    ))
    .await?;
    show("GET /records/export?format=json", st, &body);
    println!("^ проверить: массив ли это верхнего уровня и правда ли в нём нет id");

    Ok(())
}

fn top_keys(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::Object(m) => m.keys().cloned().collect(),
        serde_json::Value::Array(a) => vec![format!("<массив из {}>", a.len())],
        other => vec![format!("<{}>", kind(other))],
    }
}

fn kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Обрезает длинные массивы, чтобы не залить терминал сотней записей.
fn truncate(v: &serde_json::Value, keep: usize) -> serde_json::Value {
    match v {
        serde_json::Value::Array(a) => {
            let mut out: Vec<_> = a.iter().take(keep).map(|x| truncate(x, keep)).collect();
            if a.len() > keep {
                out.push(serde_json::json!(format!("… ещё {} шт.", a.len() - keep)));
            }
            serde_json::Value::Array(out)
        }
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.iter().map(|(k, x)| (k.clone(), truncate(x, keep))).collect(),
        ),
        other => other.clone(),
    }
}

fn head(s: &str, lines: usize) -> String {
    let mut out: Vec<&str> = s.lines().take(lines).collect();
    if s.lines().count() > lines {
        out.push("…");
    }
    out.join("\n")
}
