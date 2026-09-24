//! Режим «агент по протоколу»: MCP-сервер поверх stdio.
//!
//! Агент получает те же рычаги, что человек в консоли: спланировать сущности,
//! запустить прогон, следить за ним, поставить на паузу, посмотреть готовое и
//! оставить пометку приёмки. Пометки агента ложатся в ту же базу, что и
//! пометки людей, — это тот же вход для доработки словаря.
//!
//! Протокол: JSON-RPC 2.0, по сообщению в строке. В stdout не пишется ничего,
//! кроме ответов: журнал уходит в stderr, иначе клиент получит мусор вместо
//! JSON.
//!
//! Всё, что тратит деньги, требует явного бюджета: агент не может запустить
//! прогон «на сколько получится».

use std::sync::Arc;

use serde_json::{json, Value};
use synthforge_store::{NewAnnotation, SessionStatus, Store, Target};

use crate::{load_generator, text_engine, OUT_DIR};

/// Версия протокола, если клиент не назвал свою.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Сколько сущностей агент может заказать за один вызов.
///
/// Массовые прогоны — дело оператора: ошибка агента в постановке не должна
/// размножиться на тысячи записей раньше, чем её увидит человек.
const MAX_PER_CALL: u64 = 50;

pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let server = Server::new(crate::open_store().await?);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => server.handle(req).await,
            Err(e) => Some(error(Value::Null, -32700, &format!("не JSON: {e}"))),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

pub struct Server {
    store: Store,
}

impl Server {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// Ответ на одно сообщение. Уведомления ответа не получают.
    pub async fn handle(&self, req: Value) -> Option<Value> {
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        // Уведомление: без id, отвечать нельзя.
        let id = id?;

        let result: Value = match method {
            "initialize" => json!({
                "protocolVersion": params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSION),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "synthforge", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Генерация параметрического контента. Сначала plan — бесплатно и \
                    показывает, какими будут сущности. run тратит деньги и требует budget_usd. \
                    Прогон идёт в фоне: следите через progress, останавливайте через pause.",
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => self.call(&params).await,
            other => return Some(error(id, -32601, &format!("метод «{other}» не поддерживается"))),
        };

        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    /// Вызов инструмента. Ошибка инструмента — не ошибка протокола: агент
    /// должен её увидеть и поправить вызов, поэтому она идёт в `isError`.
    async fn call(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));

        let out = match name {
            "kinds" => Ok(kinds()),
            "plan" => plan(&args),
            "sessions" => self.sessions().await,
            "progress" => self.progress(&args).await,
            "run" => self.run(&args).await,
            "pause" => self.pause(&args).await,
            "resume" => self.resume(&args).await,
            "show" => self.show(&args).await,
            "mark" => self.mark(&args).await,
            "feedback" => self.feedback(&args).await,
            other => Err(format!("инструмента «{other}» нет")),
        };

        match out {
            Ok(v) => json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                "structuredContent": v,
            }),
            Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
        }
    }

    async fn sessions(&self) -> Result<Value, String> {
        let mut list = Vec::new();
        for s in self.store.sessions().await.map_err(str_err)? {
            let p = self.store.progress(&s.id).await.map_err(str_err)?;
            list.push(json!({
                "session": s.id,
                "kind": s.kind,
                "status": status(s.status),
                "percent": (p.percent() * 10.0).round() / 10.0,
                "spent_usd": s.spent_usd,
                "budget_usd": s.budget_usd,
            }));
        }
        Ok(json!({ "sessions": list }))
    }

    async fn progress(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let s = self.store.session(session).await.map_err(str_err)?.ok_or("сессия не найдена")?;
        let p = self.store.progress(session).await.map_err(str_err)?;
        Ok(json!({
            "session": session,
            "status": status(s.status),
            "done": p.done,
            "running": p.running,
            "pending": p.pending,
            "dead": p.dead,
            "percent": (p.percent() * 10.0).round() / 10.0,
            "settled": p.is_settled(),
            "spent_usd": p.spent_usd,
            "budget_usd": s.budget_usd,
        }))
    }

    /// Запустить прогон в фоне и сразу вернуть сессию.
    ///
    /// Прогон на сотню сущностей идёт минуты — держать вызов открытым всё это
    /// время нельзя. Агент следит через `progress`, как человек через `watch`.
    async fn run(&self, args: &Value) -> Result<Value, String> {
        let kind = str_arg(args, "kind")?;
        let count = count_arg(args)?;
        let budget = args
            .get("budget_usd")
            .and_then(|v| v.as_f64())
            .filter(|b| *b > 0.0)
            .ok_or("нужен budget_usd больше нуля: прогон тратит деньги")?;

        let g = load_generator(kind).map_err(|e| e.to_string())?;
        let mut spec = synthforge_ports::GenSpec::new(count as usize)
            .seed(args.get("seed").and_then(|v| v.as_u64()).unwrap_or(2026));
        spec.budget_usd = Some(budget);
        if let Some(b) = args.get("brief").and_then(|v| v.as_str()) {
            spec = spec.brief(b);
        }
        if let Some(t) = args.get("unique_threshold").and_then(|v| v.as_f64()) {
            spec.mode = synthforge_ports::GenMode::UniqueAgainstBase(synthforge_ports::Uniqueness {
                level: synthforge_ports::UniquenessLevel::Lexical { max_ngram_overlap: t as f32 },
                scope: synthforge_ports::Scope::Collection,
                max_retries: 3,
            });
        }

        let catalog = Arc::new(
            synthforge_llm::Catalog::load("config/model-catalog.json").map_err(|e| e.to_string())?,
        );
        let engine = text_engine(self.store.clone(), catalog).map_err(|e| e.to_string())?;
        let session = engine.start_session(&*g, &spec).await.map_err(str_err)?;

        let brief = spec.brief.clone();
        let sid = session.clone();
        tokio::spawn(async move {
            match engine.run(g.clone(), &sid, brief.as_deref()).await {
                Ok(_) => {
                    if let Err(e) = engine.flush(&*g, &sid).await {
                        tracing::error!(session = %sid, error = %e, "запись результата не удалась");
                    }
                }
                Err(e) => tracing::error!(session = %sid, error = %e, "прогон остановлен"),
            }
        });

        Ok(json!({
            "session": session,
            "started": true,
            "hint": "прогон идёт в фоне; следите через progress, остановить — pause",
        }))
    }

    async fn pause(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        self.store.session(session).await.map_err(str_err)?.ok_or("сессия не найдена")?;
        self.store
            .set_session_status(session, SessionStatus::Paused)
            .await
            .map_err(str_err)?;
        Ok(json!({ "session": session, "status": "paused",
                    "hint": "прогон остановится после текущей пачки" }))
    }

    /// Снять с паузы и доделать оставшееся в фоне.
    async fn resume(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?.to_string();
        let s = self.store.session(&session).await.map_err(str_err)?.ok_or("сессия не найдена")?;
        let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or(&s.kind).to_string();
        let g = load_generator(&kind).map_err(|e| e.to_string())?;

        let catalog = Arc::new(
            synthforge_llm::Catalog::load("config/model-catalog.json").map_err(|e| e.to_string())?,
        );
        let engine = text_engine(self.store.clone(), catalog).map_err(|e| e.to_string())?;
        engine.resume(&session).await.map_err(str_err)?;

        let brief = s
            .spec_json()
            .ok()
            .and_then(|v| v.get("brief").and_then(|b| b.as_str()).map(str::to_string));
        let sid = session.clone();
        tokio::spawn(async move {
            if engine.run(g.clone(), &sid, brief.as_deref()).await.is_ok() {
                let _ = engine.flush(&*g, &sid).await;
            }
        });
        Ok(json!({ "session": session, "status": "running" }))
    }

    /// Готовые записи сессии — то, что агент будет принимать или браковать.
    async fn show(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5).min(MAX_PER_CALL) as usize;
        let s = self.store.session(session).await.map_err(str_err)?.ok_or("сессия не найдена")?;

        let path = format!("{OUT_DIR}/{}s.jsonl", s.kind);
        let records: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(session))
            .take(limit)
            .collect();

        Ok(json!({ "session": session, "records": records }))
    }

    async fn mark(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let key = str_arg(args, "entity_key")?;
        let verdict = str_arg(args, "verdict")?;
        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect();

        let job = self
            .store
            .job_by_natural_key(key)
            .await
            .map_err(str_err)?
            .ok_or_else(|| format!("задания с ключом «{key}» нет"))?;

        let target = if key.contains("#img") { Target::Image } else { Target::Text };
        let mut a = match verdict {
            "good" => NewAnnotation::good(target),
            "bad" if !tags.is_empty() => NewAnnotation::bad(target, tags),
            "bad" => return Err("брак без меток бесполезен для доработки: укажите tags".into()),
            other => return Err(format!("verdict «{other}»: ожидается good или bad")),
        };
        a = a.on_job(job.id).by("agent");
        if let Some(c) = args.get("comment").and_then(|v| v.as_str()) {
            a = a.comment(c);
        }

        let id = self.store.annotate(session, a).await.map_err(str_err)?;
        Ok(json!({ "annotation": id }))
    }

    async fn feedback(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let s = self.store.feedback_summary(session).await.map_err(str_err)?;
        let tags: Vec<Value> = s.tags.iter().map(|t| json!({ "tag": t.tag, "count": t.count })).collect();
        let systemic: Vec<&str> = s.systemic(0.15).iter().map(|t| t.tag.as_str()).collect();
        Ok(json!({
            "good": s.good,
            "bad": s.bad,
            "reject_rate": s.reject_rate(),
            "tags": tags,
            "systemic": systemic,
        }))
    }
}

fn kinds() -> Value {
    json!({
        "kinds": [
            { "kind": "doctor", "what": "врач" },
            { "kind": "consultant", "what": "консультант по зависимостям" },
            { "kind": "psychologist", "what": "психолог" },
            { "kind": "director", "what": "руководитель центра" },
            { "kind": "place", "what": "здание с территорией" },
            { "kind": "program", "what": "программа лечения" },
        ],
        "drafts": "те же виды с суффиксом -v2 — расширенные модели на согласовании",
    })
}

/// План бесплатен: модель не вызывается, решается только, какими будут
/// сущности. Агенту стоит смотреть план до того, как тратить бюджет.
fn plan(args: &Value) -> Result<Value, String> {
    let kind = str_arg(args, "kind")?;
    let count = count_arg(args)?;
    let g = load_generator(kind).map_err(|e| e.to_string())?;
    let spec = synthforge_ports::GenSpec::new(count as usize)
        .seed(args.get("seed").and_then(|v| v.as_u64()).unwrap_or(2026));
    let rows = g.plan(&spec).map_err(|e| e.to_string())?;
    let rows: Vec<Value> = rows.iter().map(|r| r.to_json()).collect();
    Ok(json!({ "kind": kind, "count": rows.len(), "rows": rows }))
}

fn tools() -> Value {
    let session = json!({ "type": "object", "properties": {
        "session": { "type": "string", "description": "идентификатор сессии" } },
        "required": ["session"] });
    json!([
        { "name": "kinds", "description": "Какие виды сущностей можно генерировать.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "plan", "description": "Спланировать сущности без вызова модели: параметры каждой. Бесплатно.",
          "inputSchema": { "type": "object", "properties": {
              "kind": { "type": "string" },
              "count": { "type": "integer", "minimum": 1, "maximum": MAX_PER_CALL },
              "seed": { "type": "integer" } },
            "required": ["kind", "count"] } },
        { "name": "run", "description": "Запустить прогон в фоне. Тратит деньги; бюджет обязателен.",
          "inputSchema": { "type": "object", "properties": {
              "kind": { "type": "string" },
              "count": { "type": "integer", "minimum": 1, "maximum": MAX_PER_CALL },
              "budget_usd": { "type": "number", "exclusiveMinimum": 0 },
              "seed": { "type": "integer" },
              "brief": { "type": "string", "description": "вводная к генерации" },
              "unique_threshold": { "type": "number", "description": "режим «уникально относительно базы»: доля общих оборотов, выше которой текст — повтор" } },
            "required": ["kind", "count", "budget_usd"] } },
        { "name": "sessions", "description": "Все прогоны со статусом и расходом.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "progress", "description": "Ход прогона: готово, в работе, брак, расход.", "inputSchema": session },
        { "name": "pause", "description": "Поставить прогон на паузу; встанет после текущей пачки.", "inputSchema": session },
        { "name": "resume", "description": "Снять с паузы и доделать оставшееся в фоне.",
          "inputSchema": { "type": "object", "properties": {
              "session": { "type": "string" },
              "kind": { "type": "string", "description": "вид со словарём, по которому планировалась сессия (для -v2)" } },
            "required": ["session"] } },
        { "name": "show", "description": "Готовые записи сессии.",
          "inputSchema": { "type": "object", "properties": {
              "session": { "type": "string" },
              "limit": { "type": "integer", "minimum": 1, "maximum": MAX_PER_CALL } },
            "required": ["session"] } },
        { "name": "mark", "description": "Пометка приёмки. Брак — обязательно с метками: они вход для доработки словаря.",
          "inputSchema": { "type": "object", "properties": {
              "session": { "type": "string" },
              "entity_key": { "type": "string", "description": "natural_key записи; для снимка — с суффиксом #imgN" },
              "verdict": { "type": "string", "enum": ["good", "bad"] },
              "tags": { "type": "array", "items": { "type": "string" } },
              "comment": { "type": "string" } },
            "required": ["session", "entity_key", "verdict"] } },
        { "name": "feedback", "description": "Сводка приёмки: доля брака и системные дефекты.", "inputSchema": session },
    ])
}

fn status(s: SessionStatus) -> String {
    format!("{s:?}").to_lowercase()
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("нужен параметр «{name}»"))
}

fn count_arg(args: &Value) -> Result<u64, String> {
    let n = args.get("count").and_then(|v| v.as_u64()).ok_or("нужен параметр «count»")?;
    if n == 0 || n > MAX_PER_CALL {
        return Err(format!("count от 1 до {MAX_PER_CALL}: массовые прогоны запускает оператор"));
    }
    Ok(n)
}

fn str_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Словари и справочники читаются по путям от корня проекта.
    fn at_root() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            std::env::set_current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../..")).unwrap();
        });
    }

    async fn server() -> Server {
        at_root();
        Server::new(Store::open_memory().await.unwrap())
    }

    async fn call(s: &Server, name: &str, args: Value) -> Value {
        let r = s
            .handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                            "params": { "name": name, "arguments": args } }))
            .await
            .unwrap();
        r["result"].clone()
    }

    #[tokio::test]
    async fn handshake_and_tool_list() {
        let s = server().await;
        let init = s
            .handle(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize",
                            "params": { "protocolVersion": "2025-03-26" } }))
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(init["result"]["capabilities"]["tools"].is_object());

        // Уведомление — без ответа.
        assert!(s.handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.is_none());

        let list = s.handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await.unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for n in ["plan", "run", "progress", "pause", "mark", "feedback"] {
            assert!(names.contains(&n), "нет инструмента {n}: {names:?}");
        }

        let unknown = s.handle(json!({ "jsonrpc": "2.0", "id": 3, "method": "nope" })).await.unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn plan_is_free_and_deterministic() {
        let s = server().await;
        let a = call(&s, "plan", json!({ "kind": "director-v2", "count": 3, "seed": 5 })).await;
        let b = call(&s, "plan", json!({ "kind": "director-v2", "count": 3, "seed": 5 })).await;
        assert!(a.get("isError").is_none(), "{a}");
        assert_eq!(a["structuredContent"]["count"], 3);
        assert_eq!(a["structuredContent"], b["structuredContent"], "один сид — один план");
        assert!(a["structuredContent"]["rows"][0]["full_name"].is_string());
    }

    /// Ограничения, которые защищают бюджет и базу от ошибки агента.
    #[tokio::test]
    async fn guards_against_costly_mistakes() {
        let s = server().await;

        let no_budget = call(&s, "run", json!({ "kind": "doctor", "count": 5 })).await;
        assert_eq!(no_budget["isError"], true);
        assert!(no_budget["content"][0]["text"].as_str().unwrap().contains("budget_usd"));

        let too_many = call(&s, "plan", json!({ "kind": "doctor", "count": 5000 })).await;
        assert_eq!(too_many["isError"], true);

        let unknown = call(&s, "plan", json!({ "kind": "astronaut", "count": 1 })).await;
        assert_eq!(unknown["isError"], true);
    }

    /// Пауза и пометки агента ложатся в ту же базу, что и действия человека.
    #[tokio::test]
    async fn pause_and_marks_reach_the_store() {
        let s = server().await;
        let store = s.store.clone();

        let session = store
            .create_session(synthforge_store::NewSession {
                kind: "doctor".into(),
                spec: json!({}),
                seed: 1,
                budget_usd: Some(1.0),
            })
            .await
            .unwrap()
            .id;
        store
            .enqueue(&session, &[synthforge_store::NewJob::new("doctor_text", "k:1", json!({}))])
            .await
            .unwrap();

        let r = call(&s, "pause", json!({ "session": session })).await;
        assert!(r.get("isError").is_none(), "{r}");
        assert_eq!(store.session(&session).await.unwrap().unwrap().status, SessionStatus::Paused);

        let bare = call(&s, "mark", json!({ "session": session, "entity_key": "k:1", "verdict": "bad" })).await;
        assert_eq!(bare["isError"], true, "брак без меток не принимается");

        let ok = call(&s, "mark", json!({ "session": session, "entity_key": "k:1", "verdict": "bad",
                                           "tags": ["штамп"], "comment": "«с душой» в каждом абзаце" })).await;
        assert!(ok.get("isError").is_none(), "{ok}");

        let fb = call(&s, "feedback", json!({ "session": session })).await;
        assert_eq!(fb["structuredContent"]["bad"], 1);
        assert_eq!(fb["structuredContent"]["tags"][0]["tag"], "штамп");
    }
}
