//! Командная оболочка.
//!
//! ```text
//! forge plan     doctor 500                — параметры, модель не вызывается
//! forge run      doctor 20 [--images]      — прогон сессии
//! forge sessions                           — список прогонов
//! forge watch    <сессия>                  — наблюдение в реальном времени
//! forge show     <сессия> [n]              — посмотреть готовое
//! forge mark     <сессия> <ключ> bad фон угрюмый --comment "…"
//! forge feedback <сессия>                  — сводка приёмки
//! ```
//!
//! Это слой оболочки: здесь и только здесь генератор встречается с
//! инфраструктурой. Сам генератор о существовании Grok и OpenAI не знает.

mod site;

use std::sync::Arc;
use std::time::{Duration, Instant};

use synthforge_engine::{
    image_kind, Engine, EngineConfig, FsAssetStore, ImagePipeline, JsonlStore,
};
use synthforge_gen_person::PersonGenerator;
use synthforge_llm::{Catalog, OpenAiCompatText, OpenAiImages, ProviderConfig};
use synthforge_params::{ParamModel, Usage as PromptUsage};
use synthforge_ports::{GenSpec, Generator};
use synthforge_store::{NewAnnotation, Store, Target};

const TEXT_MODEL: &str = "grok-4-fast";
const IMAGE_MODEL: &str = "gpt-image-1";
const OUT_DIR: &str = "out";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into()))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");

    let result = match cmd {
        "plan" => cmd_plan(&args).await,
        "run" => cmd_run(&args).await,
        "sessions" => cmd_sessions().await,
        "watch" => cmd_watch(&args).await,
        "show" => cmd_show(&args).await,
        "mark" => cmd_mark(&args).await,
        "feedback" => cmd_feedback(&args).await,
        "similarity" => cmd_similarity(&args).await,
        "compose" => cmd_compose(&args).await,
        "center" => cmd_center(&args).await,
        "site" => cmd_site().await,
        "photos" => cmd_photos(&args).await,
        _ => {
            usage();
            return;
        }
    };

    if let Err(e) = result {
        eprintln!("\n⨯ {e}");
        std::process::exit(1);
    }
}

fn usage() {
    eprintln!(
        "forge plan     <вид> <сколько> [--seed N]\n\
         forge run      <вид> <сколько> [--seed N] [--brief «…»] [--images] [--budget 5.0]\n\
         forge sessions\n\
         forge watch    <сессия>\n\
         forge show     <сессия> [сколько]\n\
         forge mark     <сессия> <ключ> good|bad [метки…] [--comment «…»]\n\
         forge feedback <сессия>\n\n\
         Виды: doctor, consultant"
    );
}

type R = Result<(), Box<dyn std::error::Error>>;

// ------------------------------------------------------------------- общее

fn arg(args: &[String], i: usize) -> Option<&str> {
    args.get(i).map(String::as_str)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

async fn open_store() -> Result<Store, Box<dyn std::error::Error>> {
    let path = std::env::var("SYNTHFORGE_DB").unwrap_or_else(|_| "synthforge.db".into());
    Ok(Store::open(path).await?)
}

fn load_generator(kind: &str) -> Result<Arc<dyn Generator>, Box<dyn std::error::Error>> {
    // Объекты — отдельный генератор со своей логикой: без имён и биографий,
    // зато с несколькими снимками разного назначения.
    match kind {
        "place" => {
            let m = ParamModel::load("dictionaries/object-place.json")?;
            return Ok(Arc::new(synthforge_gen_object::ObjectGenerator::place(m)));
        }
        "program" => {
            let m = ParamModel::load("dictionaries/object-program.json")?;
            return Ok(Arc::new(synthforge_gen_object::ObjectGenerator::program(m)));
        }
        _ => {}
    }

    let model = ParamModel::load(format!("dictionaries/role-{kind}.json"))?;

    let mut g = match kind {
        "doctor" => PersonGenerator::doctor(model),
        // Черновик расширенной модели врача, на согласовании.
        "doctor-v2" => PersonGenerator::doctor(model),
        "consultant" => PersonGenerator::consultant(model),
        "psychologist" => PersonGenerator::psychologist(model),
        "director" => PersonGenerator::director(model),
        other => return Err(format!("неизвестный вид «{other}»").into()),
    };

    // Без справочника имя выдумает модель — полторы сотни разных имён на всю
    // популяцию, без связи с годом рождения.
    match synthforge_refdata::NameBook::load("reference/names-ru.json") {
        Ok(book) => g = g.with_names(book),
        Err(e) => eprintln!("⚠ Справочник имён не загрузился ({e}); имена выдумает модель."),
    }

    Ok(Arc::new(g))
}

fn spec_from(args: &[String]) -> GenSpec {
    let count: usize = arg(args, 2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let mut spec = GenSpec::new(count).seed(
        flag(args, "--seed")
            .and_then(|s| s.parse().ok())
            .unwrap_or(2026),
    );
    if let Some(b) = flag(args, "--brief") {
        spec = spec.brief(b);
    }
    spec.budget_usd = flag(args, "--budget").and_then(|s| s.parse().ok());
    spec.with_images = args.iter().any(|a| a == "--images");

    // Режим «уникально относительно базы»: порог — доля общих оборотов, выше
    // которой текст считается повтором уже сгенерированного.
    if let Some(t) = flag(args, "--unique").and_then(|s| s.parse::<f32>().ok()) {
        spec.mode = synthforge_ports::GenMode::UniqueAgainstBase(synthforge_ports::Uniqueness {
            level: synthforge_ports::UniquenessLevel::Lexical { max_ngram_overlap: t },
            scope: synthforge_ports::Scope::Collection,
            max_retries: 3,
        });
    }
    spec
}

// -------------------------------------------------------------------- план

async fn cmd_plan(args: &[String]) -> R {
    let kind = arg(args, 1).unwrap_or("doctor");
    let g = load_generator(kind)?;
    let spec = spec_from(args);
    let rows = g.plan(&spec)?;

    println!(
        "Спланировано {} сущностей вида «{kind}». Модель не вызывалась — \
         к этому моменту уже решено, каким будет каждый.\n",
        rows.len()
    );

    for (i, row) in rows.iter().take(3).enumerate() {
        println!("── {} ──", i + 1);
        print!("{}", row.prompt_block(g.model(), PromptUsage::Text));
        println!();
    }
    if rows.len() > 3 {
        println!("… и ещё {}", rows.len() - 3);
    }
    Ok(())
}

// ------------------------------------------------------------------ прогон

async fn cmd_run(args: &[String]) -> R {
    let kind = arg(args, 1).unwrap_or("doctor");
    let with_images = args.iter().any(|a| a == "--images");

    let g = load_generator(kind)?;
    let spec = spec_from(args);
    let store = open_store().await?;

    let catalog = Arc::new(Catalog::load("config/model-catalog.json")?);
    if !catalog.unverified().is_empty() {
        eprintln!(
            "⚠ Цены не сверены: {}. Учёт расхода приблизительный.\n",
            catalog.unverified().join(", ")
        );
    }

    let proxy = std::env::var("OUTBOUND_PROXY").ok().filter(|p| !p.trim().is_empty());
    let xai = std::env::var("XAI_API_KEY").map_err(|_| "не задан XAI_API_KEY")?;

    let text = Arc::new(OpenAiCompatText::new(
        ProviderConfig::xai(&xai, TEXT_MODEL).with_proxy(proxy.clone()),
        catalog.clone(),
    )?);

    let content = Arc::new(JsonlStore::new(OUT_DIR));
    let engine = Engine::new(store.clone(), content, text).with_config(EngineConfig {
        concurrency: 4,
        claim_size: 16,
        stale_after: Duration::from_secs(900),
    });

    let session = engine.start_session(&*g, &spec).await?;
    println!("Сессия {session}\n");

    let started = Instant::now();
    let progress = engine.run(g.clone(), &session, spec.brief.as_deref()).await?;

    println!(
        "Тексты: готово {} · отбраковано {} · безнадёжных {}",
        progress.done, progress.failed, progress.dead
    );

    if with_images {
        let key = std::env::var("OPENAI_API_KEY").map_err(|_| "не задан OPENAI_API_KEY")?;
        let model = Arc::new(OpenAiImages::new(
            ProviderConfig::openai(&key, IMAGE_MODEL).with_proxy(proxy),
            catalog.clone(),
        )?);
        let assets = Arc::new(FsAssetStore::new(OUT_DIR));

        println!("\nИзображения…");
        let made = ImagePipeline::new(store.clone(), model, assets)
            .concurrency(2)
            .run(g.clone(), &session, &image_kind(&g.descriptor().kind))
            .await?;
        println!("Снимков получено: {made}");
    }

    let written = engine.flush(&*g, &session).await?;
    let final_progress = engine.progress(&session).await?;

    println!("\n──────────────────────────────");
    println!("Записано в хранилище: {written}");
    println!(
        "Токенов {}→{} · ${:.4} · {:.1} с",
        final_progress.tokens_in,
        final_progress.tokens_out,
        final_progress.spent_usd,
        started.elapsed().as_secs_f64()
    );
    if written > 0 {
        let per = final_progress.spent_usd / written as f64;
        println!(
            "На сущность ${per:.5} · в пересчёте на 10 000: ${:.2}",
            per * 10_000.0
        );
    }
    println!("\nРезультат: {OUT_DIR}/{}.jsonl", g.descriptor().collection);
    println!("Посмотреть: forge show {session}");

    Ok(())
}

// -------------------------------------------------------------- наблюдение

async fn cmd_sessions() -> R {
    let store = open_store().await?;
    let sessions = store.sessions().await?;

    if sessions.is_empty() {
        println!("Прогонов пока не было.");
        return Ok(());
    }

    println!("{:<38} {:<12} {:<10} {:>7} {:>10}", "сессия", "вид", "статус", "готово", "потрачено");
    for s in sessions {
        let p = store.progress(&s.id).await?;
        println!(
            "{:<38} {:<12} {:<10} {:>6.0}% {:>9.4}$",
            s.id,
            s.kind,
            format!("{:?}", s.status).to_lowercase(),
            p.percent(),
            s.spent_usd
        );
    }
    Ok(())
}

/// Наблюдение за прогоном.
///
/// Основная задача интерфейса — именно наблюдать: видеть, что происходит,
/// сколько сделано и сколько потрачено. Запускать прогон можно и командой.
async fn cmd_watch(args: &[String]) -> R {
    let session = arg(args, 1).ok_or("нужен идентификатор сессии")?;
    let store = open_store().await?;

    loop {
        let p = store.progress(session).await?;
        let s = store.session(session).await?.ok_or("сессия не найдена")?;

        let width = 40usize;
        let filled = ((p.percent() / 100.0) * width as f64).round() as usize;
        let bar = "█".repeat(filled) + &"░".repeat(width - filled);

        print!(
            "\r{bar} {:>5.1}%  готово {:>5}  в работе {:>3}  ждут {:>5}  брак {:>3}  ${:.4}  ",
            p.percent(),
            p.done,
            p.running,
            p.pending,
            p.dead,
            p.spent_usd
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();

        if p.is_settled() {
            println!("\n\nПрогон завершён. Статус: {:?}", s.status);
            break;
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    Ok(())
}

async fn cmd_show(args: &[String]) -> R {
    let session = arg(args, 1).ok_or("нужен идентификатор сессии")?;
    let limit: usize = arg(args, 2).and_then(|s| s.parse().ok()).unwrap_or(3);

    let store = open_store().await?;
    let s = store.session(session).await?.ok_or("сессия не найдена")?;
    let assets = store.assets(session).await?;

    let path = format!("{OUT_DIR}/{}s.jsonl", s.kind);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("не читается {path}: {e}"))?;

    // В коллекции лежат записи всех прогонов — показываем только этот.
    let records: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(session))
        .collect();

    if records.is_empty() {
        println!("В коллекции нет записей этой сессии. Прогон ещё не отправлен или шёл до версии с метками сессий.");
        return Ok(());
    }

    for (i, r) in records.iter().take(limit).enumerate() {
        println!("══════════ {} ══════════", i + 1);

        if let Some(n) = r.get("full_name").and_then(|v| v.as_str()) {
            println!("{n}");
        }
        // Подзаголовок берём из того поля, которое есть у этой роли: у врача
        // это специальность, у консультанта — путь в профессию.
        let subtitle = ["specialty", "path_to_work", "education", "background"]
            .iter()
            .find_map(|k| r.get(*k).and_then(|v| v.as_str()))
            .unwrap_or("—");
        println!(
            "{} лет · {subtitle}",
            r.get("age").and_then(|v| v.as_i64()).unwrap_or(0)
        );

        for field in ["biography", "professional_path", "quote"] {
            if let Some(t) = r.get(field).and_then(|v| v.as_str()) {
                println!("\n{}", wrap(t, 92));
            }
        }

        if let Some(images) = r.get("images").and_then(|v| v.as_array()) {
            for img in images {
                println!(
                    "\n[{}] {}",
                    img["role"].as_str().unwrap_or("?"),
                    img["path"].as_str().unwrap_or("?")
                );
            }
        }
        println!();
    }

    if !assets.is_empty() {
        println!("Снимков в сессии: {}", assets.len());
    }
    Ok(())
}

// ----------------------------------------------------------------- приёмка

/// Пометить результат.
///
/// Пометки уходят в базу, а не в переписку: они вход для доработки словаря.
/// Замечание, повторённое двадцать раз, — измеримый сигнал добавить параметр.
async fn cmd_mark(args: &[String]) -> R {
    let session = arg(args, 1).ok_or("нужен идентификатор сессии")?;
    let key = arg(args, 2).ok_or("нужен ключ сущности")?;
    let verdict = arg(args, 3).unwrap_or("bad");

    let tags: Vec<String> = args
        .iter()
        .skip(4)
        .take_while(|a| !a.starts_with("--"))
        .cloned()
        .collect();

    let store = open_store().await?;
    let job = store
        .job_by_natural_key(key)
        .await?
        .ok_or_else(|| format!("задания с ключом «{key}» нет"))?;

    let target = if key.contains("#img") { Target::Image } else { Target::Text };

    let mut a = if verdict == "good" {
        NewAnnotation::good(target)
    } else {
        NewAnnotation::bad(target, tags.clone())
    };
    a = a.on_job(job.id);

    if let Some(c) = flag(args, "--comment") {
        a = a.comment(c);
    }
    if let Some(author) = std::env::var("USER").ok().or_else(|| std::env::var("USERNAME").ok()) {
        a = a.by(author);
    }

    let id = store.annotate(session, a).await?;
    println!(
        "Пометка #{id}: {verdict}{}",
        if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) }
    );
    Ok(())
}

async fn cmd_feedback(args: &[String]) -> R {
    let session = arg(args, 1).ok_or("нужен идентификатор сессии")?;
    let store = open_store().await?;
    let s = store.feedback_summary(session).await?;

    if s.total() == 0 {
        println!("Пометок пока нет. Помечайте: forge mark <сессия> <ключ> bad фон");
        return Ok(());
    }

    println!(
        "Просмотрено {} · принято {} · брак {} ({:.0}%)\n",
        s.total(),
        s.good,
        s.bad,
        s.reject_rate() * 100.0
    );

    println!("Метки:");
    for t in &s.tags {
        let share = t.count as f64 / s.total() as f64;
        let bar = "█".repeat((share * 30.0).round() as usize);
        println!("  {:>4} {bar:<30} {}", t.count, t.tag);
    }

    let systemic = s.systemic(0.15);
    if !systemic.is_empty() {
        println!("\nСистемные дефекты — чинить словарём, а не перегенерацией:");
        for t in systemic {
            println!("  • {} ({} раз)", t.tag, t.count);
        }
    }
    Ok(())
}

/// Все записи коллекции как пул для сборки.
fn load_pool(collection: &str) -> Vec<synthforge_compose_center::Candidate> {
    let path = format!("{OUT_DIR}/{collection}.jsonl");
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|r| {
            let key = r.get("natural_key")?.as_str()?.to_string();
            Some(synthforge_compose_center::Candidate::new(key, r))
        })
        .collect()
}

/// Собрать центры из того, что уже сгенерировано.
///
/// Ничего не сочиняет: здание, программа, руководитель и команда берутся из
/// пулов. Модель пишет только название и текст о центре.
async fn cmd_compose(args: &[String]) -> R {
    let count: usize = arg(args, 1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let seed: u64 = flag(args, "--seed").and_then(|s| s.parse().ok()).unwrap_or(2026);

    // Всё, что уже занято собранными раньше центрами, в новую сборку не идёт.
    let mut reserved = std::collections::HashSet::new();
    for c in load_pool("centers") {
        if let Some(n) = c.record.get("center_name").and_then(|v| v.as_str()) {
            reserved.insert(format!("name:{n}"));
        }
        for k in ["place_key", "program_key", "director_key"] {
            if let Some(v) = c.record.get(k).and_then(|v| v.as_str()) {
                reserved.insert(v.to_string());
            }
        }
        for v in c.record.get("staff_keys").and_then(|v| v.as_array()).into_iter().flatten() {
            if let Some(s) = v.as_str() {
                reserved.insert(s.to_string());
            }
        }
    }

    let pools = synthforge_compose_center::Pools {
        places: load_pool("places"),
        programs: load_pool("programs"),
        directors: load_pool("directors"),
        doctors: load_pool("doctors"),
        psychologists: load_pool("psychologists"),
        consultants: load_pool("consultants"),
        reserved,
    };
    println!(
        "Пулы: {}\nУже занято в собранных центрах: {}\n",
        pools.sizes(),
        pools.reserved.len()
    );

    let model = ParamModel::load("dictionaries/object-center.json")?;
    let names: Vec<String> = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string("reference/center-names.json")?,
    )?["names"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let services = synthforge_compose_center::ServiceCatalog::from_json(
        &std::fs::read_to_string("reference/services-ru.json")?,
    )?;

    let g: Arc<dyn Generator> = Arc::new(
        synthforge_compose_center::CenterGenerator::new(model, pools)
            .with_names(names)
            .with_services(services),
    );

    let store = open_store().await?;
    let catalog = Arc::new(Catalog::load("config/model-catalog.json")?);
    let proxy = std::env::var("OUTBOUND_PROXY").ok().filter(|p| !p.trim().is_empty());
    let xai = std::env::var("XAI_API_KEY").map_err(|_| "не задан XAI_API_KEY")?;
    let text = Arc::new(OpenAiCompatText::new(
        ProviderConfig::xai(&xai, TEXT_MODEL).with_proxy(proxy),
        catalog,
    )?);

    let engine = Engine::new(store, Arc::new(JsonlStore::new(OUT_DIR)), text);
    let spec = GenSpec::new(count).seed(seed);
    let session = engine.start_session(&*g, &spec).await?;
    let p = engine.run(g.clone(), &session, None).await?;
    let written = engine.flush(&*g, &session).await?;

    println!("Центров собрано: {written} (брак {})", p.dead);
    println!("Расход ${:.4}", p.spent_usd);
    println!("\nПосмотреть: forge center {session}");
    Ok(())
}

/// Показать центр целиком: текст о нём, здание, программу и людей.
async fn cmd_center(args: &[String]) -> R {
    let session = arg(args, 1).ok_or("нужен идентификатор сессии")?;
    let limit: usize = arg(args, 2).and_then(|s| s.parse().ok()).unwrap_or(1);

    let all: std::collections::HashMap<String, serde_json::Value> =
        ["places", "programs", "directors", "doctors", "psychologists", "consultants"]
            .iter()
            .flat_map(|c| load_pool(c))
            .map(|c| (c.key, c.record))
            .collect();

    let centers = std::fs::read_to_string(format!("{OUT_DIR}/centers.jsonl")).unwrap_or_default();
    let s = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

    for r in centers
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(session))
        .take(limit)
    {
        println!("════════════════════════════════════════");
        println!("«{}»", s(&r, "center_name"));
        println!(
            "{} · {} · {} мест · {}",
            s(&r, "focus"),
            s(&r, "region"),
            r.get("capacity").and_then(|v| v.as_i64()).unwrap_or(0),
            s(&r, "setting")
        );
        println!("\n{}", wrap(&s(&r, "about"), 92));
        println!("\n{}", wrap(&s(&r, "approach_text"), 92));

        if let Some(p) = all.get(&s(&r, "program_key")) {
            println!("\n── Программа: {} ({}, {})", s(p, "title"), s(p, "approach"), s(p, "duration"));
        }
        if let Some(d) = all.get(&s(&r, "director_key")) {
            println!("── Руководитель: {} — {}", s(d, "full_name"), s(d, "background"));
        }
        println!("── Команда:");
        for key in r.get("staff_keys").and_then(|v| v.as_array()).into_iter().flatten() {
            if let Some(m) = key.as_str().and_then(|k| all.get(k)) {
                let what = ["specialty", "education", "path_to_work"]
                    .iter()
                    .map(|k| s(m, k))
                    .find(|v| !v.is_empty())
                    .unwrap_or_default();
                println!("   {} — {}, {} лет", s(m, "full_name"), what, m["age"]);
            }
        }
        println!();
    }
    Ok(())
}

/// Доснять снимки для первых N собранных центров: здание, руководитель, команда.
///
/// Снимки на порядки дороже текста, поэтому делаются не для всех пулов, а
/// только для тех, кто уже попал в центры и будет показан. Результат —
/// карта «ключ сущности → снимки» в `out/photos.json`; записи в пулах не
/// переписываются.
async fn cmd_photos(args: &[String]) -> R {
    use synthforge_ports::{AssetStore, ImageModel};

    let limit: usize = arg(args, 1).and_then(|s| s.parse().ok()).unwrap_or(1);

    let pools: std::collections::HashMap<String, serde_json::Value> =
        ["places", "programs", "directors", "doctors", "psychologists", "consultants"]
            .iter()
            .flat_map(|c| load_pool(c))
            .map(|c| (c.key, c.record))
            .collect();

    let catalog = Arc::new(Catalog::load("config/model-catalog.json")?);
    let key = std::env::var("OPENAI_API_KEY").map_err(|_| "не задан OPENAI_API_KEY")?;
    let proxy = std::env::var("OUTBOUND_PROXY").ok().filter(|p| !p.trim().is_empty());
    let model = OpenAiImages::new(ProviderConfig::openai(&key, IMAGE_MODEL).with_proxy(proxy), catalog)?;
    let assets = FsAssetStore::new(OUT_DIR);

    let map_path = format!("{OUT_DIR}/photos.json");
    let mut map = read_photo_map(&map_path);

    let mut generators: std::collections::HashMap<String, Arc<dyn Generator>> = Default::default();
    let mut spent = 0.0f64;
    let mut made = 0usize;

    for center in load_pool("centers").into_iter().take(limit) {
        let r = &center.record;
        let mut keys: Vec<String> = ["place_key", "director_key"]
            .iter()
            .filter_map(|k| r.get(*k).and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        keys.extend(
            r.get("staff_keys")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string)),
        );

        for k in keys {
            if map.contains_key(&k) {
                continue; // уже снято — второй раз не платим
            }
            let Some(rec) = pools.get(&k) else { continue };
            let kind = rec.get("entity_kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !generators.contains_key(&kind) {
                generators.insert(kind.clone(), load_generator(&kind)?);
            }
            let g = &generators[&kind];

            // Запись содержит все параметры сущности — из неё восстанавливается
            // строка, по которой строятся промпты снимков.
            // Поля, не являющиеся параметрами (снимки, служебные метки со
            // вложенной структурой), пропускаются, а не роняют восстановление.
            let row: synthforge_params::ParamRow = rec
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| {
                    serde_json::from_value::<synthforge_params::Value>(v.clone())
                        .ok()
                        .map(|pv| (k.clone(), pv))
                })
                .collect();

            let mut shots = Vec::new();
            // Снимки сущности идут по порядку, поэтому опора (фасад) к моменту
            // снимка, который на неё ссылается (территория), уже готова.
            let mut rendered: std::collections::HashMap<String, Vec<u8>> = Default::default();
            for mut req in g.image_requests(&row) {
                let role = req.role.clone();
                if let Some(png) = req.reference_role.as_ref().and_then(|b| rendered.get(b)) {
                    req.reference_png = Some(png.clone());
                }
                match model.render(req).await {
                    Ok(img) => {
                        rendered.insert(role.clone(), img.png.clone());
                        let path = assets.put("photos", &k, &role, &img.png).await?;
                        spent += img.usage.cost_usd;
                        made += 1;
                        println!("  {kind:<13} {role:<9} {}", rec.get("full_name").and_then(|v| v.as_str()).unwrap_or(&k));
                        shots.push(serde_json::json!({"role": role, "path": path}));
                    }
                    Err(e) => println!("  ⨯ {k} {role}: {e}"),
                }
            }
            // Пустой результат не записывается: иначе сущность считалась бы
            // снятой, и повторный запуск после пополнения счёта её пропустил бы.
            if !shots.is_empty() {
                map.insert(k, shots);
                std::fs::write(&map_path, serde_json::to_string_pretty(&map)?)?;
            }
        }
    }

    println!("\nСнимков сделано: {made} · ${spent:.2}");
    println!("Дальше: forge site");
    Ok(())
}

/// Карта снимков. Файл мог быть поправлен руками в редакторе, который пишет
/// метку порядка байтов, — её надо снять, иначе разбор молча вернёт пустую
/// карту, и `photos` переснимет всё заново за деньги.
fn read_photo_map(path: &str) -> std::collections::BTreeMap<String, Vec<serde_json::Value>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Default::default();
    };
    match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("⚠ {path} не разобрался ({e}); считаю, что снимков нет");
            Default::default()
        }
    }
}

/// Демо-страницы центров для приёмки глазами.
async fn cmd_site() -> R {
    let mut pools: std::collections::HashMap<String, serde_json::Value> =
        ["places", "programs", "directors", "doctors", "psychologists", "consultants"]
            .iter()
            .flat_map(|c| load_pool(c))
            .map(|c| (c.key, c.record))
            .collect();

    // Снимки, сделанные отдельно командой photos, подмешиваются к записям.
    for (k, shots) in read_photo_map(&format!("{OUT_DIR}/photos.json")) {
        if let Some(rec) = pools.get_mut(&k) {
            rec["images"] = serde_json::Value::Array(shots);
        }
    }

    let centers: Vec<serde_json::Value> = load_pool("centers").into_iter().map(|c| c.record).collect();

    if centers.is_empty() {
        println!("Центров пока нет. Сначала: forge compose 3");
        return Ok(());
    }

    let n = site::build(OUT_DIR, &pools, &centers)?;
    let index = std::fs::canonicalize(format!("{OUT_DIR}/site/index.html"))?;
    println!("Страниц центров: {n}");
    println!("Открыть: {}", index.display().to_string().trim_start_matches(r"\\?\"));
    Ok(())
}

/// Насколько однообразны тексты прогона.
///
/// Для каждой записи ищется самая похожая из остальных. Распределение этих
/// значений показывает, есть ли у модели шаблон, и подсказывает порог для
/// режима уникальности: его ставят выше типичного значения, но ниже хвоста.
async fn cmd_similarity(args: &[String]) -> R {
    let session = arg(args, 1).ok_or("нужен идентификатор сессии")?;
    let store = open_store().await?;
    let s = store.session(session).await?.ok_or("сессия не найдена")?;

    let path = format!("{OUT_DIR}/{}s.jsonl", s.kind);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("не читается {path}: {e}"))?;

    let docs: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(session))
        .filter_map(|r| {
            let name = r.get("full_name")?.as_str()?.to_string();
            let bio = r.get("biography")?.as_str()?;
            let p = r.get("professional_path")?.as_str()?;
            Some((name, format!("{bio} {p}")))
        })
        .collect();

    if docs.len() < 2 {
        println!("Для сравнения нужно хотя бы две записи.");
        return Ok(());
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

    let scores: Vec<f32> = best.iter().map(|b| b.0).collect();
    let mean = scores.iter().sum::<f32>() / scores.len() as f32;
    let median = {
        let mut v = scores.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };

    println!("Записей: {} · совпадение с ближайшим соседом:", docs.len());
    println!(
        "  среднее {:.1}% · медиана {:.1}% · максимум {:.1}%\n",
        mean * 100.0,
        median * 100.0,
        scores[0] * 100.0
    );
    println!("Самые похожие пары:");
    for (score, a, b) in best.iter().take(5) {
        println!("  {:>5.1}%  {a}  ↔  {b}", score * 100.0);
    }
    Ok(())
}

fn wrap(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = 0usize;
    for word in s.split_whitespace() {
        let w = word.chars().count();
        if line + w + 1 > width {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += w;
    }
    out
}
