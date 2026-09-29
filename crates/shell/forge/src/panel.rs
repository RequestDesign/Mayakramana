//! Веб-панель наблюдения и приёмки: `forge panel`.
//!
//! Постановка требовала видеть прогон в реальном времени, ставить на паузу и
//! помечать результат. В консоли это есть, но руководителю нужна страница.
//!
//! Панель — тонкая оболочка над тем же набором инструментов, что у агента по
//! протоколу ([`crate::mcp::Server::tool`]): одни рычаги, одни ограничения,
//! никакой второй реализации. Слушает только `127.0.0.1` — наружу не видна.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::mcp::Server;
use crate::OUT_DIR;

/// Инструменты, доступные из браузера. Запуск прогона (`run`) сюда не входит:
/// он тратит деньги, а у страницы нет подтверждения бюджета, как у агента.
const ALLOWED: &[&str] = &["kinds", "sessions", "progress", "pause", "resume", "show", "mark", "feedback"];

pub async fn serve(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let server = Arc::new(Server::new(crate::open_store().await?));
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("panel.html")) }))
        .route("/api/:tool", post(tool))
        .route("/file", get(file))
        .with_state(server);

    // По умолчанию только эта машина. В контейнере слушаем все интерфейсы
    // (PANEL_BIND=0.0.0.0), а наружу порт публикуется лишь на 127.0.0.1
    // сервера: входа в панели нет, и в интернет она смотреть не должна.
    let ip: std::net::IpAddr = std::env::var("PANEL_BIND")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));
    let addr = SocketAddr::new(ip, port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("Панель: http://{addr}  (Ctrl+C — остановить)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn tool(
    State(server): State<Arc<Server>>,
    Path(name): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    if !ALLOWED.contains(&name.as_str()) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": format!("«{name}» из панели недоступен") })))
            .into_response();
    }
    let mut args = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    // Пометки из панели ставит человек, а не агент.
    if name == "mark" {
        if let Some(o) = args.as_object_mut() {
            o.entry("author").or_insert_with(|| json!("panel"));
        }
    }
    match server.tool(&name, &args).await {
        Ok(v) => Json(json!({ "data": v })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct FileQuery {
    path: String,
}

/// Снимок из каталога результатов. Всё, что за его пределами, не отдаётся:
/// иначе через параметр можно было бы прочитать любой файл, включая `.env`.
async fn file(Query(q): Query<FileQuery>) -> Response {
    let Some(path) = within(std::path::Path::new(&OUT_DIR.path()), std::path::Path::new(&q.path)) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => return StatusCode::FORBIDDEN.into_response(),
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Путь, если он ведёт внутрь `root` (после разрешения `..` и ссылок).
fn within(root: &std::path::Path, candidate: &std::path::Path) -> Option<std::path::PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    let path = std::fs::canonicalize(candidate).ok()?;
    path.starts_with(&root).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::within;

    #[test]
    fn files_outside_the_output_dir_are_refused() {
        let base = std::env::temp_dir().join(format!("sf-panel-{}", std::process::id()));
        let out = base.join("out");
        std::fs::create_dir_all(out.join("photos")).unwrap();
        std::fs::write(out.join("photos/a.png"), b"x").unwrap();
        std::fs::write(base.join(".env"), b"secret").unwrap();

        assert!(within(&out, &out.join("photos/a.png")).is_some());
        assert!(within(&out, &base.join(".env")).is_none(), "файл рядом с каталогом");
        assert!(within(&out, &out.join("../.env")).is_none(), "выход через ..");
        assert!(within(&out, &out.join("photos/../../.env")).is_none());
        assert!(within(&out, &out.join("нет-такого.png")).is_none());

        let _ = std::fs::remove_dir_all(&base);
    }
}
