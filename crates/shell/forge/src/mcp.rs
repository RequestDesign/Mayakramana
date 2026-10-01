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
    /// Сколько сущностей можно заказать за один вызов. Агенту — немного:
    /// его ошибка в постановке не должна размножиться на тысячи записей.
    /// Человеку в панели — больше: он видит оценку стоимости до запуска.
    max_per_call: u64,
}

/// Предел для панели: пакетный прогон человек запускает осознанно.
pub const PANEL_MAX_PER_CALL: u64 = 2000;

impl Server {
    pub fn new(store: Store) -> Self {
        Self { store, max_per_call: MAX_PER_CALL }
    }

    pub fn for_panel(store: Store) -> Self {
        Self { store, max_per_call: PANEL_MAX_PER_CALL }
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

        match self.tool(name, &args).await {
            Ok(v) => json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                "structuredContent": v,
            }),
            Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
        }
    }

    /// Инструмент по имени. Общий для агента (MCP) и веб-панели: у человека в
    /// браузере и у агента одни и те же рычаги и одни и те же ограничения.
    pub async fn tool(&self, name: &str, args: &Value) -> Result<Value, String> {
        let args = args.clone();
        match name {
            "kinds" => Ok(kinds()),
            "plan" => self.plan(&args),
            "dictionary" => dictionary(&args),
            "distribution" => self.distribution(&args),
            "estimate" => self.estimate(&args).await,
            "sessions" => self.sessions().await,
            "progress" => self.progress(&args).await,
            "activity" => self.activity(&args).await,
            "run" => self.run(&args).await,
            "pause" => self.pause(&args).await,
            "resume" => self.resume(&args).await,
            "show" => self.show(&args).await,
            "mark" => self.mark(&args).await,
            "annotations" => self.annotations(&args).await,
            "feedback" => self.feedback(&args).await,
            "similarity" => self.similarity(&args).await,
            "storage" => storage().await,
            "upload" => {
                let wanted: Vec<String> = args
                    .get("collections")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                let wanted: Vec<&str> = wanted.iter().map(String::as_str).collect();
                crate::upload(&wanted).await.map(|lines| json!({ "report": lines }))
            }
            "site" => crate::build_site()
                .map(|n| json!({ "pages": n, "index": "/out/site/index.html" }))
                .map_err(|e| e.to_string()),
            other => Err(format!("инструмента «{other}» нет")),
        }
    }

    async fn sessions(&self) -> Result<Value, String> {
        let mut list = Vec::new();
        for s in self.store.sessions().await.map_err(str_err)? {
            let p = self.store.progress(&s.id).await.map_err(str_err)?;
            let spec = s.spec_json().unwrap_or(Value::Null);
            list.push(json!({
                "session": s.id,
                "kind": s.kind,
                "dictionary": spec.get("dictionary").and_then(|v| v.as_str()).unwrap_or(&s.kind),
                "status": status(s.status),
                "count": spec.get("count"),
                "brief": spec.get("brief"),
                "mode": mode_label(&spec),
                "with_images": spec.get("with_images").and_then(|v| v.as_bool()).unwrap_or(false),
                "done": p.done,
                "dead": p.dead,
                "percent": (p.percent() * 10.0).round() / 10.0,
                "spent_usd": s.spent_usd,
                "budget_usd": s.budget_usd,
                "created_at": s.created_at,
                "updated_at": s.updated_at,
            }));
        }
        Ok(json!({ "sessions": list }))
    }

    async fn progress(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let s = self.store.session(session).await.map_err(str_err)?.ok_or("сессия не найдена")?;
        let p = self.store.progress(session).await.map_err(str_err)?;
        let spec = s.spec_json().unwrap_or(Value::Null);
        // Прогноз по фактическому темпу: сколько готово за прошедшее время.
        let elapsed = (s.updated_at - s.created_at).max(0) as f64 / 1000.0;
        let finished = (p.done + p.dead) as f64;
        let left = (p.pending + p.running) as f64;
        let eta = (finished > 0.0 && left > 0.0).then(|| elapsed / finished * left);
        Ok(json!({
            "session": session,
            "kind": s.kind,
            "dictionary": spec.get("dictionary").and_then(|v| v.as_str()).unwrap_or(&s.kind),
            "brief": spec.get("brief"),
            "mode": mode_label(&spec),
            "cohorts": spec.get("cohorts"),
            "constraints": spec.get("constraints"),
            "seed": spec.get("seed"),
            "with_images": spec.get("with_images").and_then(|v| v.as_bool()).unwrap_or(false),
            "tokens_in": p.tokens_in,
            "tokens_out": p.tokens_out,
            "elapsed_s": elapsed,
            "eta_s": eta,
            "created_at": s.created_at,
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
        let count = self.count_arg(args)?;
        let budget = args
            .get("budget_usd")
            .and_then(|v| v.as_f64())
            .filter(|b| *b > 0.0)
            .ok_or("нужен budget_usd больше нуля: прогон тратит деньги")?;

        let g = load_generator(kind).map_err(|e| e.to_string())?;
        let mut spec = spec_from_args(kind, count, args, g.model())?;
        spec.budget_usd = Some(budget);

        let catalog = Arc::new(
            synthforge_llm::Catalog::load("config/model-catalog.json").map_err(|e| e.to_string())?,
        );
        let engine = text_engine(self.store.clone(), catalog.clone()).map_err(|e| e.to_string())?;

        // Снимки проверяем до постановки: без ключа прогон со снимками
        // застрял бы на половине.
        let images = if spec.with_images {
            Some(crate::image_pipeline(self.store.clone(), catalog).map_err(|e| e.to_string())?)
        } else {
            None
        };

        let session = engine.start_session(&*g, &spec).await.map_err(str_err)?;
        spawn_run(engine, images, g, session.clone(), spec.brief.clone());

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
        let spec = s.spec_json().unwrap_or(Value::Null);
        let kind = args
            .get("kind")
            .and_then(|v| v.as_str())
            .or_else(|| spec.get("dictionary").and_then(|v| v.as_str()))
            .unwrap_or(&s.kind)
            .to_string();
        let g = load_generator(&kind).map_err(|e| e.to_string())?;

        let catalog = Arc::new(
            synthforge_llm::Catalog::load("config/model-catalog.json").map_err(|e| e.to_string())?,
        );
        let engine = text_engine(self.store.clone(), catalog.clone()).map_err(|e| e.to_string())?;
        let images = if spec.get("with_images").and_then(|v| v.as_bool()).unwrap_or(false) {
            Some(crate::image_pipeline(self.store.clone(), catalog).map_err(|e| e.to_string())?)
        } else {
            None
        };
        engine.resume(&session).await.map_err(str_err)?;

        let brief = spec.get("brief").and_then(|b| b.as_str()).map(str::to_string);
        spawn_run(engine, images, g, session.clone(), brief);
        Ok(json!({ "session": session, "status": "running" }))
    }

    /// Готовые записи сессии — то, что агент будет принимать или браковать.
    async fn show(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5).min(self.max_per_call) as usize;
        let s = self.store.session(session).await.map_err(str_err)?.ok_or("сессия не найдена")?;

        let path = format!("{OUT_DIR}/{}s.jsonl", s.kind);
        let records: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(session))
            .take(limit)
            .collect();

        // Снимки, сделанные отдельно командой `photos`, лежат в своей карте —
        // подмешиваем их к записям, как это делают демо-страницы.
        let photos = crate::read_photo_map(&format!("{OUT_DIR}/photos.json"));
        let records: Vec<Value> = records
            .into_iter()
            .map(|mut r| {
                let key = r.get("natural_key").and_then(|v| v.as_str()).map(str::to_string);
                if let (Some(shots), Some(o)) = (key.and_then(|k| photos.get(&k)), r.as_object_mut()) {
                    o.entry("images").or_insert_with(|| Value::Array(shots.clone()));
                }
                r
            })
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

        // Снимок помечается отдельно от текста: «портрет хороший, текст — нет».
        let image = args.get("image_path").and_then(|v| v.as_str()).filter(|p| !p.is_empty());
        let target = if key.contains("#img") || image.is_some() { Target::Image } else { Target::Text };
        let mut a = match verdict {
            "good" => NewAnnotation::good(target),
            "bad" if !tags.is_empty() => NewAnnotation::bad(target, tags),
            "bad" => return Err("брак без меток бесполезен для доработки: укажите tags".into()),
            other => return Err(format!("verdict «{other}»: ожидается good или bad")),
        };
        // Кто поставил пометку: агент по протоколу или человек из панели.
        let author = args.get("author").and_then(|v| v.as_str()).unwrap_or("agent");
        a = a.on_job(job.id).by(author);
        if let Some(p) = image {
            a = a.on_asset(p);
        }
        if let Some(c) = args.get("comment").and_then(|v| v.as_str()) {
            a = a.comment(c);
        }

        let id = self.store.annotate(session, a).await.map_err(str_err)?;
        Ok(json!({ "annotation": id }))
    }

    /// Спланировать без вызова модели.
    fn plan(&self, args: &Value) -> Result<Value, String> {
        let kind = str_arg(args, "kind")?;
        let count = self.count_arg(args)?;
        let g = load_generator(kind).map_err(|e| e.to_string())?;
        let spec = spec_from_args(kind, count, args, g.model())?;
        let rows = g.plan(&spec).map_err(|e| e.to_string())?;
        let rows: Vec<Value> = rows.iter().map(|r| r.to_json()).collect();
        Ok(json!({ "kind": kind, "count": rows.len(), "rows": rows }))
    }

    /// Как распределятся параметры на выборке — до того, как потрачены деньги.
    ///
    /// Это и есть проверка невидимого слоя человеком: видно, сколько в
    /// популяции выздоравливающих, какие стажи, нет ли перекосов. Когорты и
    /// ограничения постановки учитываются — видно, что именно они сдвинут.
    fn distribution(&self, args: &Value) -> Result<Value, String> {
        let kind = str_arg(args, "kind")?;
        let n = args.get("sample").and_then(|v| v.as_u64()).unwrap_or(500).clamp(10, 5000) as usize;
        let g = load_generator(kind).map_err(|e| e.to_string())?;
        let mut spec = spec_from_args(kind, 1, args, g.model())?;
        spec.count = n;
        let rows = g.plan(&spec).map_err(|e| e.to_string())?;
        Ok(json!({
            "kind": kind,
            "sample": rows.len(),
            "params": param_stats(g.model(), &rows),
        }))
    }

    /// Оценка стоимости до запуска: средняя цена готового задания этого вида
    /// по прошлым прогонам плюс снимки по каталогу цен.
    async fn estimate(&self, args: &Value) -> Result<Value, String> {
        let kind = str_arg(args, "kind")?;
        let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as f64;
        let with_images = args.get("with_images").and_then(|v| v.as_bool()).unwrap_or(false);
        let g = load_generator(kind).map_err(|e| e.to_string())?;
        let d = g.descriptor();

        let text = self
            .store
            .mean_cost_of_kind(&synthforge_engine::text_kind(&d.kind))
            .await
            .map_err(str_err)?;
        let (per_text, basis) = match text {
            Some((mean, n)) => (mean, format!("средняя по {n} готовым текстам прошлых прогонов, включая повторы после брака")),
            None => (0.0006, "прогонов этого вида ещё не было — ориентир $0.0006 за текст".to_string()),
        };

        let catalog = synthforge_llm::Catalog::load("config/model-catalog.json").map_err(|e| e.to_string())?;
        let per_image = catalog
            .get(crate::IMAGE_MODEL)
            .ok()
            .and_then(|m| m.per_image)
            .unwrap_or(0.04);
        let images_per_entity = if with_images { d.images_per_entity as f64 } else { 0.0 };

        let per_entity = per_text + images_per_entity * per_image;
        Ok(json!({
            "kind": kind,
            "count": count,
            "per_text_usd": per_text,
            "per_image_usd": per_image,
            "images_per_entity": images_per_entity,
            "per_entity_usd": per_entity,
            "total_usd": per_entity * count,
            "basis": basis,
            "images_note": if with_images {
                "цена снимка в каталоге не сверена; по опыту реальная выше"
            } else { "" },
        }))
    }

    /// Что происходит в прогоне прямо сейчас: последние готовые записи и
    /// проблемы с причинами.
    async fn activity(&self, args: &Value) -> Result<Value, String> {
        use synthforge_store::JobStatus;
        let session = str_arg(args, "session")?;
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(15).clamp(1, 200);

        let done = self
            .store
            .recent_jobs(session, &[JobStatus::Done], limit)
            .await
            .map_err(str_err)?;
        let done: Vec<Value> = done
            .iter()
            .map(|j| {
                let r: Value = j
                    .result
                    .as_deref()
                    .and_then(|t| serde_json::from_str(t).ok())
                    .unwrap_or(Value::Null);
                json!({
                    "key": j.natural_key,
                    "kind": j.kind,
                    "title": (["full_name", "center_name", "title", "role"]
                        .iter()
                        .find_map(|k| r.get(*k).and_then(|v| v.as_str()))
                        .unwrap_or(&j.natural_key)),
                    "attempts": j.attempts,
                    "cost_usd": j.cost_usd,
                    "at": j.updated_at,
                    "retried_because": if j.attempts > 1 { j.last_error.clone() } else { None },
                })
            })
            .collect();

        let bad = self
            .store
            .recent_jobs(session, &[JobStatus::Dead, JobStatus::Pending, JobStatus::Failed], limit * 3)
            .await
            .map_err(str_err)?;
        let bad: Vec<Value> = bad
            .iter()
            .filter(|j| j.last_error.is_some())
            .take(limit as usize)
            .map(|j| {
                json!({
                    "key": j.natural_key,
                    "status": format!("{:?}", j.status).to_lowercase(),
                    "attempts": j.attempts,
                    "max_attempts": j.max_attempts,
                    "error": j.last_error,
                    "at": j.updated_at,
                })
            })
            .collect();

        Ok(json!({ "done": done, "problems": bad }))
    }

    /// Все пометки сессии: кто, что и почему пометил.
    async fn annotations(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let list = self.store.annotations(session).await.map_err(str_err)?;
        let mut out = Vec::with_capacity(list.len());
        for a in list {
            let key = match a.job_id {
                Some(id) => self.store.job(id).await.ok().map(|j| j.natural_key),
                None => None,
            };
            out.push(json!({
                "id": a.id,
                "key": key,
                "target": format!("{:?}", a.target).to_lowercase(),
                "image_path": a.asset,
                "verdict": format!("{:?}", a.verdict).to_lowercase(),
                "tags": a.tag_list(),
                "comment": a.comment,
                "author": a.author,
                "at": a.created_at,
            }));
        }
        Ok(json!({ "annotations": out }))
    }

    /// Насколько тексты прогона похожи друг на друга.
    async fn similarity(&self, args: &Value) -> Result<Value, String> {
        let session = str_arg(args, "session")?;
        let s = self.store.session(session).await.map_err(str_err)?.ok_or("сессия не найдена")?;
        let path = format!("{OUT_DIR}/{}s.jsonl", s.kind);
        let docs: Vec<(String, String)> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(session))
            .filter_map(|r| {
                let name = ["full_name", "center_name", "title", "natural_key"]
                    .iter()
                    .find_map(|k| r.get(*k).and_then(|v| v.as_str()))?
                    .to_string();
                let text: Vec<&str> = ["biography", "professional_path", "about", "approach_text", "description"]
                    .iter()
                    .filter_map(|k| r.get(*k).and_then(|v| v.as_str()))
                    .collect();
                (!text.is_empty()).then(|| (name, text.join(" ")))
            })
            .collect();
        if docs.len() < 2 {
            return Ok(json!({ "records": docs.len(), "note": "для сравнения нужно хотя бы две записи" }));
        }

        let mut idx = synthforge_textsim::LexicalIndex::new();
        for (name, t) in &docs {
            idx.insert(name.clone(), t);
        }
        let mut best: Vec<(f32, String, String)> = docs
            .iter()
            .filter_map(|(name, t)| {
                idx.nearest(t, 2)
                    .into_iter()
                    .find(|m| &m.id != name)
                    .map(|m| (m.score, name.clone(), m.id))
            })
            .collect();
        best.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut scores: Vec<f32> = best.iter().map(|b| b.0).collect();
        let mean = scores.iter().sum::<f32>() / scores.len().max(1) as f32;
        // Пара «А ↔ Б» и «Б ↔ А» — одна и та же: в списке похожих оставляем раз.
        let mut seen = std::collections::HashSet::new();
        best.retain(|(_, a, b)| {
            let key = if a < b { (a.clone(), b.clone()) } else { (b.clone(), a.clone()) };
            seen.insert(key)
        });
        scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = scores.get(scores.len() / 2).copied().unwrap_or(0.0);

        Ok(json!({
            "records": docs.len(),
            "mean": mean,
            "median": median,
            "max": best.first().map(|b| b.0),
            "top_pairs": best.iter().take(5).map(|(s, a, b)| json!({ "score": s, "a": a, "b": b })).collect::<Vec<_>>(),
        }))
    }

    fn count_arg(&self, args: &Value) -> Result<u64, String> {
        let n = args.get("count").and_then(|v| v.as_u64()).ok_or("нужен параметр «count»")?;
        if n == 0 || n > self.max_per_call {
            return Err(format!(
                "count от 1 до {}: больше за один раз не запускается",
                self.max_per_call
            ));
        }
        Ok(n)
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
    let pool = |c: &str| crate::load_pool(c).len();
    let row = |kind: &str, what: &str, collection: &str, legacy: bool| {
        json!({
            "kind": kind,
            "what": what,
            "collection": collection,
            "draft": legacy,
            "legacy": legacy,
            "dictionary": dictionary_file(kind),
            "in_pool": pool(collection),
        })
    };
    json!({
        "kinds": [
            row("doctor", "Врач", "doctors", false),
            row("consultant", "Консультант по зависимостям", "consultants", false),
            row("psychologist", "Психолог", "psychologists", false),
            row("director", "Руководитель центра", "directors", false),
            row("place", "Здание с территорией", "places", false),
            row("program", "Программа лечения", "programs", false),
            row("center", "Центр — сборка из готовых людей, здания и программы", "centers", false),
            row("doctor-v1", "Врач, прежняя модель", "doctors", true),
            row("consultant-v1", "Консультант, прежняя модель", "consultants", true),
            row("psychologist-v1", "Психолог, прежняя модель", "psychologists", true),
            row("director-v1", "Руководитель, прежняя модель", "directors", true),
            row("place-v1", "Здание, прежняя модель", "places", true),
            row("program-v1", "Программа, прежняя модель", "programs", true),
        ],
        "note": "основные модели — v2 (расширенные); прежние — с суффиксом -v1; центр собирается только из готовых записей",
    })
}

/// Постановка из аргументов вызова: режим, вводная, когорты, ограничения.
///
/// Когорты и ограничения проверяются по словарю до запуска: опечатка в имени
/// параметра иначе молча не сработала бы, и прогон ушёл бы не той постановкой.
fn spec_from_args(
    kind: &str,
    count: u64,
    args: &Value,
    model: &synthforge_params::ParamModel,
) -> Result<synthforge_ports::GenSpec, String> {
    use synthforge_ports::{GenMode, Scope, Uniqueness, UniquenessLevel};

    let mut spec = synthforge_ports::GenSpec::new(count as usize)
        .seed(args.get("seed").and_then(|v| v.as_u64()).unwrap_or(2026));
    spec.dictionary = Some(kind.to_string());
    spec.with_images = args.get("with_images").and_then(|v| v.as_bool()).unwrap_or(false);
    if let Some(b) = args.get("brief").and_then(|v| v.as_str()).map(str::trim).filter(|b| !b.is_empty()) {
        spec = spec.brief(b);
    }

    let unique = args.get("mode").and_then(|v| v.as_str()) == Some("unique")
        || args.get("unique_threshold").is_some();
    if unique {
        let threshold = args.get("unique_threshold").and_then(|v| v.as_f64()).unwrap_or(0.25);
        if !(0.05..=0.95).contains(&threshold) {
            return Err("порог уникальности — от 0.05 до 0.95 (доля общих оборотов)".into());
        }
        let scope = match args.get("unique_scope").and_then(|v| v.as_str()) {
            Some("session") => Scope::Session,
            _ => Scope::Collection,
        };
        spec.mode = GenMode::UniqueAgainstBase(Uniqueness {
            level: UniquenessLevel::Lexical { max_ngram_overlap: threshold as f32 },
            scope,
            max_retries: args.get("max_retries").and_then(|v| v.as_u64()).unwrap_or(3).clamp(1, 10) as u32,
        });
    }

    let keys: std::collections::HashSet<&str> = model.params.iter().map(|p| p.key.as_str()).collect();
    let check = |biases: &[synthforge_params::Bias], whose: &str| -> Result<(), String> {
        for b in biases {
            if !keys.contains(b.target()) {
                return Err(format!("{whose}: параметра «{}» в словаре «{kind}» нет", b.target()));
            }
        }
        Ok(())
    };

    if let Some(c) = args.get("constraints").filter(|v| !v.is_null()) {
        let list: Vec<synthforge_params::Bias> =
            serde_json::from_value(c.clone()).map_err(|e| format!("ограничения не разобрались: {e}"))?;
        check(&list, "ограничение")?;
        spec.constraints = list;
    }
    if let Some(c) = args.get("cohorts").filter(|v| !v.is_null()) {
        let list: Vec<synthforge_params::Cohort> =
            serde_json::from_value(c.clone()).map_err(|e| format!("когорты не разобрались: {e}"))?;
        let total: f64 = list.iter().map(|c| c.share).sum();
        if list.iter().any(|c| c.share <= 0.0) || total > 1.0001 {
            return Err(format!("доли когорт должны быть больше нуля и в сумме не больше 100% (сейчас {:.0}%)", total * 100.0));
        }
        for c in &list {
            check(&c.biases, &format!("когорта «{}»", c.name))?;
        }
        spec.cohorts = list;
    }
    Ok(spec)
}

/// Прогон в фоне: тексты, затем снимки, затем запись в хранилище.
fn spawn_run(
    engine: synthforge_engine::Engine,
    images: Option<synthforge_engine::ImagePipeline>,
    g: Arc<dyn synthforge_ports::Generator>,
    sid: String,
    brief: Option<String>,
) {
    tokio::spawn(async move {
        if let Err(e) = engine.run(g.clone(), &sid, brief.as_deref()).await {
            tracing::error!(session = %sid, error = %e, "прогон остановлен");
        }
        if let Some(images) = images {
            let kind = synthforge_engine::image_kind(&g.descriptor().kind);
            if let Err(e) = images.run(g.clone(), &sid, &kind).await {
                tracing::error!(session = %sid, error = %e, "снимки остановлены");
            }
        }
        if let Err(e) = engine.flush(&*g, &sid).await {
            tracing::error!(session = %sid, error = %e, "запись результата не удалась");
        }
    });
}

/// Режим прогона словами.
fn mode_label(spec: &Value) -> String {
    let m = spec.get("mode");
    match m.and_then(|m| m.get("mode")).and_then(|v| v.as_str()) {
        Some("unique_against_base") => {
            let t = m
                .and_then(|m| m.get("level"))
                .and_then(|l| l.get("max_ngram_overlap"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let scope = match m.and_then(|m| m.get("scope")).and_then(|v| v.as_str()) {
                Some("session") => "внутри прогона",
                _ => "со всей базой",
            };
            format!("уникально {scope}, порог {:.0}%", t * 100.0)
        }
        _ => "случайно".into(),
    }
}

/// Файл словаря для вида.
fn dictionary_file(kind: &str) -> String {
    crate::dictionary_path(kind)
}

/// Словарь целиком: параметры с диапазонами, жёсткие правила и корреляции.
fn dictionary(args: &Value) -> Result<Value, String> {
    let kind = str_arg(args, "kind")?;
    let file = dictionary_file(kind);
    let model = synthforge_params::ParamModel::load(&file).map_err(|e| e.to_string())?;
    let mut v = serde_json::to_value(&model).map_err(|e| e.to_string())?;
    if let Some(o) = v.as_object_mut() {
        o.insert("file".into(), json!(file));
        o.insert("issues".into(), json!(model.issues().iter().map(|i| format!("{i:?}")).collect::<Vec<_>>()));
    }
    Ok(v)
}

/// Распределение каждого параметра на выборке.
fn param_stats(model: &synthforge_params::ParamModel, rows: &[synthforge_params::ParamRow]) -> Vec<Value> {
    use synthforge_params::Value as P;
    let n = rows.len().max(1) as f64;
    let mut out = Vec::new();
    for p in &model.params {
        let domain = serde_json::to_value(&p.domain).unwrap_or(Value::Null);
        let dkind = domain.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if dkind == "derived" {
            continue;
        }
        let vals: Vec<&P> = rows.iter().filter_map(|r| r.get(&p.key)).collect();

        let stats = if vals.iter().all(|v| matches!(v, P::Int(_) | P::Float(_) | P::Null)) && vals.iter().any(|v| !v.is_null()) {
            let mut xs: Vec<f64> = vals.iter().filter_map(|v| v.as_f64()).collect();
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let q = |f: f64| xs[((xs.len() - 1) as f64 * f).round() as usize];
            let (lo, hi) = (xs[0], xs[xs.len() - 1]);
            let bins = if hi > lo { 10usize.min((hi - lo) as usize + 1).max(2) } else { 1 };
            let mut hist = vec![0usize; bins];
            for x in &xs {
                let i = if hi > lo { (((x - lo) / (hi - lo)) * (bins as f64 - 1.0)).round() as usize } else { 0 };
                hist[i.min(bins - 1)] += 1;
            }
            json!({
                "type": "number",
                "min": lo, "p10": q(0.1), "median": q(0.5), "p90": q(0.9), "max": hi,
                "mean": xs.iter().sum::<f64>() / xs.len() as f64,
                "histogram": { "from": lo, "to": hi, "counts": hist },
            })
        } else if vals.iter().all(|v| matches!(v, P::Bool(_))) {
            let t = vals.iter().filter(|v| matches!(v, P::Bool(true))).count() as f64;
            json!({ "type": "bool", "true_share": t / n })
        } else if vals.iter().any(|v| matches!(v, P::List(_))) {
            let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
            let mut total_len = 0usize;
            for v in &vals {
                if let P::List(items) = v {
                    total_len += items.len();
                    for i in items {
                        *counts.entry(i.clone()).or_default() += 1;
                    }
                }
            }
            let mut items: Vec<(String, usize)> = counts.into_iter().collect();
            items.sort_by(|a, b| b.1.cmp(&a.1));
            json!({
                "type": "list",
                "mean_len": total_len as f64 / n,
                "values": items.iter().map(|(v, c)| json!({ "value": v, "share": *c as f64 / n })).collect::<Vec<_>>(),
            })
        } else {
            let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
            for v in &vals {
                *counts.entry(v.render()).or_default() += 1;
            }
            let mut items: Vec<(String, usize)> = counts.into_iter().collect();
            items.sort_by(|a, b| b.1.cmp(&a.1));
            json!({
                "type": "category",
                "distinct": items.len(),
                "values": items.iter().take(25).map(|(v, c)| json!({ "value": v, "share": *c as f64 / n })).collect::<Vec<_>>(),
            })
        };

        out.push(json!({
            "key": p.key,
            "title": p.title,
            "group": p.group,
            "domain_kind": dkind,
            "usage": p.usage,
            "mention": p.mention,
            "identity": p.identity,
            "stats": stats,
        }));
    }
    out
}

/// Сколько записей в каждой коллекции: локально и в Nexorium.
async fn storage() -> Result<Value, String> {
    let mut local = serde_json::Map::new();
    for (c, _) in crate::COLLECTIONS {
        local.insert(c.to_string(), json!(crate::load_pool(c).len()));
    }

    // Ошибку сразу в строку: иначе она живёт через await, и обработчик
    // панели перестаёт быть переносимым между потоками.
    let client = crate::nexorium().map_err(|e| e.to_string());
    let nexorium = match client {
        Ok(Some(nx)) => match nx.collections().await {
            Ok(cols) => {
                let mut counts = serde_json::Map::new();
                for c in cols {
                    let n = nx.count(c.id, &[]).await.ok();
                    counts.insert(c.slug.clone(), json!({ "title": c.name, "count": n }));
                }
                json!({ "configured": true, "collections": counts })
            }
            Err(e) => json!({ "configured": true, "error": e.to_string() }),
        },
        Ok(None) => json!({ "configured": false }),
        Err(e) => json!({ "configured": false, "error": e.to_string() }),
    };

    Ok(json!({ "out_dir": OUT_DIR.path(), "local": local, "nexorium": nexorium }))
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

    /// Невидимый слой из панели: словарь целиком и распределение с учётом
    /// когорт и условий постановки.
    #[tokio::test]
    async fn dictionary_and_distribution_follow_the_spec() {
        let s = server().await;
        let d = call(&s, "dictionary", json!({ "kind": "director-v2" })).await;
        assert!(d.get("isError").is_none(), "{d}");
        let d = &d["structuredContent"];
        assert!(d["params"].as_array().unwrap().len() > 50);
        assert!(d["hard"].as_array().unwrap().len() > 10);
        assert_eq!(d["issues"].as_array().unwrap().len(), 0);

        let r = call(&s, "distribution", json!({
            "kind": "consultant-v2", "sample": 300,
            "constraints": [{ "kind": "range", "param": "age", "min": 45 }],
            "cohorts": [{ "name": "созависимые", "share": 0.5,
                          "biases": [{ "kind": "force", "param": "recovery_background",
                                       "value": "созависимый, прошёл программу для родственников" }] }],
        })).await;
        assert!(r.get("isError").is_none(), "{r}");
        let params = r["structuredContent"]["params"].as_array().unwrap().clone();
        let age = params.iter().find(|p| p["key"] == "age").unwrap();
        assert!(age["stats"]["min"].as_f64().unwrap() >= 45.0, "условие не применилось");
        let bg = params.iter().find(|p| p["key"] == "recovery_background").unwrap();
        let co = bg["stats"]["values"].as_array().unwrap().iter()
            .find(|v| v["value"] == "созависимый, прошёл программу для родственников").unwrap();
        assert!(co["share"].as_f64().unwrap() >= 0.5, "когорта не легла: {co}");
    }

    /// Опечатка в условии и неверные доли когорт не должны молча запустить
    /// прогон не той постановкой.
    #[tokio::test]
    async fn bad_spec_is_rejected_before_anything_runs() {
        let s = server().await;
        let typo = call(&s, "plan", json!({ "kind": "doctor", "count": 2,
            "constraints": [{ "kind": "range", "param": "experiance_years", "min": 5 }] })).await;
        assert_eq!(typo["isError"], true);
        assert!(typo["content"][0]["text"].as_str().unwrap().contains("experiance_years"));

        let over = call(&s, "plan", json!({ "kind": "doctor", "count": 2,
            "cohorts": [{ "name": "a", "share": 0.7 }, { "name": "b", "share": 0.6 }] })).await;
        assert_eq!(over["isError"], true);

        let threshold = call(&s, "plan", json!({ "kind": "doctor", "count": 2, "mode": "unique", "unique_threshold": 2 })).await;
        assert_eq!(threshold["isError"], true);
    }

    /// Панели можно больше за раз, агенту — нет.
    #[tokio::test]
    async fn panel_limit_is_wider_than_agent_limit() {
        at_root();
        let agent = Server::new(Store::open_memory().await.unwrap());
        let panel = Server::for_panel(Store::open_memory().await.unwrap());
        let args = json!({ "kind": "doctor", "count": 200 });
        assert!(agent.tool("plan", &args).await.is_err());
        assert!(panel.tool("plan", &args).await.is_ok());
    }
}
