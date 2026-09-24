//! Проверка того, ради чего движок вообще устроен именно так.
//!
//! Всё на заглушках: ни сети, ни ключей, ни внешних сервисов. Это возможно
//! потому, что движок работает с портами, а не с конкретными Nexorium и Grok.
//! Иначе сценарии «оборвался канал на записи» и «воркер умер посреди прогона»
//! пришлось бы ловить в проде.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use synthforge_engine::{Engine, EngineConfig, ImagePipeline};
use synthforge_params::{Domain, ParamDef, ParamModel, ParamRow, PopulationPlan, Sampler, Variant};
use synthforge_ports::{
    AcceptedText, BatchVerdict, ContentStore, EntityDescriptor, GenSpec, Generator, ImageRequest,
    Message, PortError, PortResult, RejectReason, TextModel, TextRequest, TextResponse, Usage,
    WriteOutcome,
};
use synthforge_store::Store;

// ----------------------------------------------------------------- заглушки

struct FakeGen {
    descriptor: EntityDescriptor,
    model: ParamModel,
    images: usize,
}

impl FakeGen {
    fn with_images(n: usize) -> Self {
        let mut g = Self::new();
        g.images = n;
        g.descriptor.images_per_entity = n;
        g
    }

    fn new() -> Self {
        let mut model = ParamModel::new("fake");
        model.params.push(
            ParamDef::new("age", "Возраст", Domain::Int { min: 30, max: 60, weights: vec![] })
                .identity(),
        );
        model.params.push(
            ParamDef::new(
                "role",
                "Роль",
                Domain::Enum {
                    variants: vec![Variant::new("врач", 1.0), Variant::new("психолог", 1.0)],
                },
            )
            .identity(),
        );
        model.params.push(ParamDef::new("serial", "Номер", Domain::Int {
            min: 0,
            max: 100_000,
            weights: vec![],
        }).identity());

        Self {
            descriptor: EntityDescriptor {
                kind: "fake".into(),
                collection: "fakes".into(),
                title: "Заглушки".into(),
                images_per_entity: 0,
            },
            model,
            images: 0,
        }
    }
}

impl Generator for FakeGen {
    fn descriptor(&self) -> &EntityDescriptor {
        &self.descriptor
    }

    fn model(&self) -> &ParamModel {
        &self.model
    }

    fn plan(&self, spec: &GenSpec) -> Result<Vec<ParamRow>, synthforge_params::Error> {
        let mut s = Sampler::new(&self.model, spec.seed);
        let rows = s.sample_population(&PopulationPlan::uniform(spec.count))?;
        Ok(rows.into_iter().map(|r| r.row).collect())
    }

    fn text_request(&self, row: &ParamRow, _brief: Option<&str>) -> TextRequest {
        TextRequest::new(vec![Message::user(format!("{:?}", row.get("serial")))])
    }

    fn image_requests(&self, _row: &ParamRow) -> Vec<ImageRequest> {
        ["portrait", "room"]
            .iter()
            .take(self.images)
            .map(|r| ImageRequest::new(format!("снимок: {r}")).role(*r))
            .collect()
    }

    fn accept_text(&self, _row: &ParamRow, text: &str) -> Result<AcceptedText, RejectReason> {
        let v: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| RejectReason::NotStructured(e.to_string()))?;
        let obj = v
            .as_object()
            .ok_or_else(|| RejectReason::NotStructured("не объект".into()))?;
        if !obj.contains_key("bio") {
            return Err(RejectReason::MissingField("bio".into()));
        }
        Ok(AcceptedText { fields: obj.clone() })
    }

    fn uniqueness_text(&self, accepted: &AcceptedText) -> Option<String> {
        accepted.fields.get("bio").and_then(|v| v.as_str()).map(str::to_string)
    }

    fn assemble(&self, row: &ParamRow, accepted: &AcceptedText) -> serde_json::Value {
        json!({
            "serial": row.get("serial").and_then(|v| v.as_i64()),
            "age": row.get("age").and_then(|v| v.as_i64()),
            "bio": accepted.fields.get("bio"),
        })
    }
}

/// Модель, которая всегда отвечает. Считает вызовы.
struct FakeText {
    calls: AtomicUsize,
    /// После какого вызова отвечать фатальной ошибкой. `None` — никогда.
    fatal_after: Option<usize>,
}

impl FakeText {
    fn ok() -> Self {
        Self { calls: AtomicUsize::new(0), fatal_after: None }
    }

    fn fatal_after(n: usize) -> Self {
        Self { calls: AtomicUsize::new(0), fatal_after: Some(n) }
    }
}

#[async_trait]
impl TextModel for FakeText {
    fn id(&self) -> &str {
        "fake-text"
    }
    fn provider(&self) -> &str {
        "fake"
    }

    async fn complete(&self, _req: TextRequest) -> PortResult<TextResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fatal_after.is_some_and(|limit| n >= limit) {
            return Err(PortError::Fatal("кончились деньги".into()));
        }
        Ok(TextResponse {
            text: json!({"bio": format!("биография номер {n}")}).to_string(),
            model: "fake-text".into(),
            usage: Usage { tokens_in: 100, tokens_out: 50, cost_usd: 0.001 },
        })
    }
}

#[derive(Default)]
struct FakeImage;

#[async_trait]
impl synthforge_ports::ImageModel for FakeImage {
    fn id(&self) -> &str {
        "fake-image"
    }
    fn provider(&self) -> &str {
        "fake"
    }

    async fn render(
        &self,
        req: ImageRequest,
    ) -> PortResult<synthforge_ports::ImageResponse> {
        Ok(synthforge_ports::ImageResponse {
            png: req.role.as_bytes().to_vec(),
            model: "fake-image".into(),
            usage: Usage { tokens_in: 0, tokens_out: 0, cost_usd: 0.04 },
        })
    }
}

#[derive(Clone, Default)]
struct MemAssets {
    files: Arc<Mutex<Vec<(String, String)>>>,
}

impl MemAssets {
    fn count(&self) -> usize {
        self.files.lock().unwrap().len()
    }
}

#[async_trait]
impl synthforge_ports::AssetStore for MemAssets {
    async fn put(
        &self,
        _session: &str,
        entity_key: &str,
        role: &str,
        bytes: &[u8],
    ) -> PortResult<String> {
        let path = format!("mem://{entity_key}/{role}");
        self.files
            .lock()
            .unwrap()
            .push((path.clone(), String::from_utf8_lossy(bytes).to_string()));
        Ok(path)
    }
}

#[derive(Default)]
struct FakeStoreState {
    /// Что реально «легло»: метка пачки → записи.
    written: Vec<(String, serde_json::Value)>,
    /// Сколько ближайших записей вернут неопределённый исход.
    unknown_next: usize,
    /// Записывать ли при неопределённом исходе на самом деле.
    unknown_actually_lands: bool,
    /// Сколько записей из пачки терять при неопределённом исходе.
    lose_from_batch: usize,
}

#[derive(Clone, Default)]
struct FakeContent {
    state: Arc<Mutex<FakeStoreState>>,
}

impl FakeContent {
    fn records(&self) -> Vec<serde_json::Value> {
        self.state.lock().unwrap().written.iter().map(|(_, v)| v.clone()).collect()
    }

    fn serials(&self) -> Vec<i64> {
        let mut s: Vec<i64> = self
            .records()
            .iter()
            .filter_map(|r| r.get("serial").and_then(|v| v.as_i64()))
            .collect();
        s.sort_unstable();
        s
    }
}

#[async_trait]
impl ContentStore for FakeContent {
    async fn ensure_collection(&self, _slug: &str, _title: &str) -> PortResult<()> {
        Ok(())
    }

    async fn write_batch(
        &self,
        _collection: &str,
        batch_id: &str,
        records: &[serde_json::Value],
    ) -> PortResult<WriteOutcome> {
        let mut st = self.state.lock().unwrap();

        if st.unknown_next > 0 {
            st.unknown_next -= 1;
            if st.unknown_actually_lands {
                // Канал оборвался уже после того, как сервер всё записал —
                // самый коварный случай.
                let keep = records.len().saturating_sub(st.lose_from_batch);
                for r in records.iter().take(keep) {
                    st.written.push((batch_id.to_string(), r.clone()));
                }
            }
            return Ok(WriteOutcome::Unknown);
        }

        for r in records {
            st.written.push((batch_id.to_string(), r.clone()));
        }
        Ok(WriteOutcome::Committed)
    }

    async fn verify_batch(
        &self,
        _collection: &str,
        batch_id: &str,
        expected: u64,
    ) -> PortResult<BatchVerdict> {
        let st = self.state.lock().unwrap();
        let found = st.written.iter().filter(|(b, _)| b == batch_id).count() as u64;
        Ok(if found == 0 {
            BatchVerdict::Absent
        } else if found >= expected {
            BatchVerdict::Committed
        } else {
            BatchVerdict::Partial { found }
        })
    }

    async fn delete_batch(&self, _collection: &str, batch_id: &str) -> PortResult<usize> {
        let mut st = self.state.lock().unwrap();
        let before = st.written.len();
        st.written.retain(|(b, _)| b != batch_id);
        Ok(before - st.written.len())
    }

    async fn count(&self, _collection: &str) -> PortResult<u64> {
        Ok(self.state.lock().unwrap().written.len() as u64)
    }
}

// ------------------------------------------------------------------- помощь

/// Уникальное имя файла на каждый тест: одного времени мало, тесты идут
/// параллельно и легко попадают в одну отметку.
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn store() -> Store {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("synthforge-engine-{}-{n}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Store::open(path).await.unwrap()
}

fn cfg() -> EngineConfig {
    EngineConfig { concurrency: 4, claim_size: 8, stale_after: Duration::from_millis(0) }
}

// -------------------------------------------------------------------- тесты

#[tokio::test]
async fn happy_path_writes_everything_exactly_once() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let content = FakeContent::default();
    let engine = Engine::new(store().await, Arc::new(content.clone()), Arc::new(FakeText::ok()))
        .with_config(cfg());

    let spec = GenSpec::new(40).seed(1);
    let session = engine.start_session(&*gen, &spec).await.unwrap();

    let progress = engine.run(gen.clone(), &session, None).await.unwrap();
    assert_eq!(progress.done, 40);

    let written = engine.flush(&*gen, &session).await.unwrap();
    assert_eq!(written, 40);

    let serials = content.serials();
    assert_eq!(serials.len(), 40);

    let mut uniq = serials.clone();
    uniq.dedup();
    assert_eq!(uniq.len(), 40, "записи продублировались");
}

/// Прогон прерван на середине: процесс умер, задания остались захваченными.
/// После перезапуска всё должно доехать, и ровно по одному разу.
#[tokio::test]
async fn interrupted_run_resumes_without_duplicates() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let content = FakeContent::default();
    let st = store().await;

    let spec = GenSpec::new(30).seed(2);

    // Первый запуск: модель отвечает фатальной ошибкой после 12 вызовов.
    let session = {
        let engine = Engine::new(
            st.clone(),
            Arc::new(content.clone()),
            Arc::new(FakeText::fatal_after(12)),
        )
        .with_config(cfg());

        let session = engine.start_session(&*gen, &spec).await.unwrap();
        let _ = engine.run(gen.clone(), &session, None).await;
        engine.flush(&*gen, &session).await.unwrap();
        session
    };

    let after_crash = st.progress(&session).await.unwrap();
    assert!(after_crash.done < 30, "прогон должен был прерваться");
    assert!(after_crash.done > 0, "часть работы должна была успеть пройти");

    // Второй запуск с тем же хранилищем: подхватывает оставшееся.
    let engine = Engine::new(st.clone(), Arc::new(content.clone()), Arc::new(FakeText::ok()))
        .with_config(cfg());

    let progress = engine.run(gen.clone(), &session, None).await.unwrap();
    assert_eq!(progress.done, 30, "после возобновления должны быть готовы все");
    assert_eq!(progress.running, 0, "подвисших захватов остаться не должно");

    engine.flush(&*gen, &session).await.unwrap();

    let mut serials = content.serials();
    let total = serials.len();
    serials.dedup();
    assert_eq!(total, 30, "записей должно быть ровно столько, сколько заданий");
    assert_eq!(serials.len(), 30, "возобновление продублировало записи");
}

/// Канал оборвался на записи, но сервер её выполнил. Слепой повтор создал бы
/// дубликаты — сверка по метке пачки этого не допускает.
#[tokio::test]
async fn unknown_outcome_that_actually_landed_is_not_rewritten() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let content = FakeContent::default();
    {
        let mut st = content.state.lock().unwrap();
        st.unknown_next = 1;
        st.unknown_actually_lands = true;
    }

    let engine = Engine::new(store().await, Arc::new(content.clone()), Arc::new(FakeText::ok()))
        .with_config(cfg());

    let spec = GenSpec::new(20).seed(3);
    let session = engine.start_session(&*gen, &spec).await.unwrap();
    engine.run(gen.clone(), &session, None).await.unwrap();

    // Первая отправка вернёт неопределённый исход.
    engine.flush(&*gen, &session).await.unwrap();
    // Вторая должна сначала свериться и понять, что всё уже легло.
    engine.flush(&*gen, &session).await.unwrap();

    let mut serials = content.serials();
    let total = serials.len();
    serials.dedup();
    assert_eq!(total, 20, "получилось {total} записей вместо 20 — дубликаты");
    assert_eq!(serials.len(), 20);
}

/// Пачка легла частично. Откат по метке и повтор целиком дают ровно один набор.
#[tokio::test]
async fn partially_landed_batch_is_rolled_back_and_retried() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let content = FakeContent::default();
    {
        let mut st = content.state.lock().unwrap();
        st.unknown_next = 1;
        st.unknown_actually_lands = true;
        st.lose_from_batch = 7;
    }

    let engine = Engine::new(store().await, Arc::new(content.clone()), Arc::new(FakeText::ok()))
        .with_config(cfg());

    let spec = GenSpec::new(20).seed(4);
    let session = engine.start_session(&*gen, &spec).await.unwrap();
    engine.run(gen.clone(), &session, None).await.unwrap();

    engine.flush(&*gen, &session).await.unwrap();
    engine.flush(&*gen, &session).await.unwrap();

    let mut serials = content.serials();
    let total = serials.len();
    serials.dedup();
    assert_eq!(total, 20, "после отката и повтора должно быть ровно 20, а не {total}");
    assert_eq!(serials.len(), 20, "остались дубликаты");
}

/// Повторный запуск постановки не создаёт второй комплект заданий.
#[tokio::test]
async fn enqueueing_is_idempotent_per_session() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let engine = Engine::new(
        store().await,
        Arc::new(FakeContent::default()),
        Arc::new(FakeText::ok()),
    )
    .with_config(cfg());

    let spec = GenSpec::new(15).seed(5);
    let a = engine.start_session(&*gen, &spec).await.unwrap();
    assert_eq!(engine.progress(&a).await.unwrap().total(), 15);

    // Новая сессия — новые естественные ключи, это другой прогон.
    let b = engine.start_session(&*gen, &spec).await.unwrap();
    assert_ne!(a, b);
    assert_eq!(engine.progress(&b).await.unwrap().total(), 15);
}

/// Текст и изображения делаются разными заданиями, разными линиями и в разное
/// время. В хранилище должна уйти одна запись на сущность, со всеми снимками.
#[tokio::test]
async fn images_and_text_converge_into_one_record() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::with_images(2));
    let content = FakeContent::default();
    let assets = Arc::new(MemAssets::default());
    let st = store().await;

    let engine = Engine::new(st.clone(), Arc::new(content.clone()), Arc::new(FakeText::ok()))
        .with_config(cfg());

    let spec = GenSpec::new(10).seed(7).with_images(true);
    let session = engine.start_session(&*gen, &spec).await.unwrap();

    // В очереди и текст, и снимки: 10 текстов плюс по два снимка на каждого.
    assert_eq!(engine.progress(&session).await.unwrap().total(), 30);

    // Текстовая линия не трогает задания на изображения.
    engine.run(gen.clone(), &session, None).await.unwrap();
    let after_text = engine.progress(&session).await.unwrap();
    assert_eq!(after_text.done, 10, "текстовая линия захватила лишнее");
    assert_eq!(after_text.pending, 20, "задания на снимки должны остаться в очереди");

    // Линия изображений — своя, со своим параллелизмом.
    let images = ImagePipeline::new(st.clone(), Arc::new(FakeImage::default()), assets.clone())
        .concurrency(3);
    let produced = images
        .run(gen.clone(), &session, &synthforge_engine::image_kind("fake"))
        .await
        .unwrap();
    assert_eq!(produced, 20);

    let written = engine.flush(&*gen, &session).await.unwrap();
    assert_eq!(written, 10, "запись на сущность, а не на задание");

    // Каждая запись несёт оба своих снимка.
    for record in content.records() {
        let images = record["images"].as_array().expect("снимки не приложены");
        assert_eq!(images.len(), 2, "к записи приложено {} снимков", images.len());
        let roles: Vec<&str> = images.iter().filter_map(|i| i["role"].as_str()).collect();
        assert!(roles.contains(&"portrait"), "{roles:?}");
        assert!(roles.contains(&"room"), "{roles:?}");
    }

    assert_eq!(assets.count(), 20);
}

/// Модель, которая на всё отвечает одним и тем же текстом.
struct SameText;

#[async_trait]
impl TextModel for SameText {
    fn id(&self) -> &str {
        "same"
    }
    fn provider(&self) -> &str {
        "fake"
    }
    async fn complete(&self, _req: TextRequest) -> PortResult<TextResponse> {
        Ok(TextResponse {
            text: json!({"bio": "Окончил медицинский университет и сразу пришёл в наркологию, \
                где работает с пациентами и их семьями, опираясь на медикаментозную \
                стабилизацию и длительное сопровождение после выписки."})
            .to_string(),
            model: "same".into(),
            usage: Usage { tokens_in: 10, tokens_out: 10, cost_usd: 0.0001 },
        })
    }
}

/// Режим «уникально относительно базы»: одинаковые тексты не проходят.
///
/// Модель, выдающая один и тот же текст на разных людей, — ровно то, чего
/// надо бояться при массовой генерации. Первый принимается, остальные
/// отбраковываются, а не расползаются по витрине десятками копий.
#[tokio::test]
async fn unique_mode_rejects_repeated_texts() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let engine = Engine::new(
        store().await,
        Arc::new(FakeContent::default()),
        Arc::new(SameText),
    )
    .with_config(cfg());

    let mut spec = GenSpec::new(6).seed(9);
    spec.mode = synthforge_ports::GenMode::UniqueAgainstBase(synthforge_ports::Uniqueness {
        level: synthforge_ports::UniquenessLevel::Lexical { max_ngram_overlap: 0.5 },
        scope: synthforge_ports::Scope::Session,
        max_retries: 3,
    });

    let session = engine.start_session(&*gen, &spec).await.unwrap();
    let p = engine.run(gen.clone(), &session, None).await.unwrap();

    assert_eq!(p.done, 1, "из шести одинаковых текстов принят должен быть ровно один");
    assert_eq!(p.dead, 5, "остальные должны быть отбракованы как повторы");
}

/// Без режима уникальности те же тексты проходят — проверка включается
/// постановкой, а не зашита намертво.
#[tokio::test]
async fn random_mode_does_not_check_uniqueness() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let engine = Engine::new(
        store().await,
        Arc::new(FakeContent::default()),
        Arc::new(SameText),
    )
    .with_config(cfg());

    let session = engine.start_session(&*gen, &GenSpec::new(6).seed(10)).await.unwrap();
    let p = engine.run(gen.clone(), &session, None).await.unwrap();
    assert_eq!(p.done, 6);
}

/// Без явного запроса снимков задания на них не ставятся вовсе.
///
/// Иначе они висели бы в очереди, искажая и прогресс, и оценку готовности:
/// прогон выглядел бы наполовину сделанным, будучи законченным.
#[tokio::test]
async fn images_are_not_queued_unless_asked() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::with_images(2));
    let engine = Engine::new(
        store().await,
        Arc::new(FakeContent::default()),
        Arc::new(FakeText::ok()),
    )
    .with_config(cfg());

    let session = engine
        .start_session(&*gen, &GenSpec::new(10).seed(8))
        .await
        .unwrap();

    assert_eq!(engine.progress(&session).await.unwrap().total(), 10);

    engine.run(gen.clone(), &session, None).await.unwrap();
    let p = engine.progress(&session).await.unwrap();
    assert!(p.is_settled(), "прогон без снимков должен считаться законченным");
    assert_eq!(p.percent(), 100.0);
}

/// Бюджет исчерпан — прогон останавливается, не дожигая оставшиеся задания.
#[tokio::test]
async fn budget_cap_stops_the_run() {
    let gen: Arc<dyn Generator> = Arc::new(FakeGen::new());
    let engine = Engine::new(
        store().await,
        Arc::new(FakeContent::default()),
        Arc::new(FakeText::ok()),
    )
    .with_config(EngineConfig { concurrency: 2, claim_size: 4, stale_after: Duration::ZERO });

    // Каждое задание стоит 0.001, потолок 0.01 — должно хватить примерно на
    // десяток, а не на все сорок.
    let mut spec = GenSpec::new(40).seed(6);
    spec.budget_usd = Some(0.01);

    let session = engine.start_session(&*gen, &spec).await.unwrap();
    let progress = engine.run(gen.clone(), &session, None).await.unwrap();

    assert!(progress.done < 40, "бюджет не остановил прогон: готово {}", progress.done);
    assert!(progress.done >= 8, "остановился слишком рано: готово {}", progress.done);
    assert!(progress.pending > 0, "незавершённые задания должны остаться в очереди");
}

// ------------------------------------------------- снимки с опорой на другой

/// Дом: фасад и территория, территория опирается на фасад.
struct HouseGen(FakeGen);

impl Generator for HouseGen {
    fn descriptor(&self) -> &EntityDescriptor {
        self.0.descriptor()
    }
    fn model(&self) -> &ParamModel {
        self.0.model()
    }
    fn plan(&self, spec: &GenSpec) -> Result<Vec<ParamRow>, synthforge_params::Error> {
        self.0.plan(spec)
    }
    fn text_request(&self, row: &ParamRow, brief: Option<&str>) -> TextRequest {
        self.0.text_request(row, brief)
    }
    // Территория стоит первой: опора не обязана идти раньше по порядку.
    fn image_requests(&self, _row: &ParamRow) -> Vec<ImageRequest> {
        vec![
            ImageRequest::new("территория того же дома").role("territory").based_on("facade"),
            ImageRequest::new("фасад").role("facade"),
        ]
    }
    fn accept_text(&self, row: &ParamRow, text: &str) -> Result<AcceptedText, RejectReason> {
        self.0.accept_text(row, text)
    }
    fn assemble(&self, row: &ParamRow, accepted: &AcceptedText) -> serde_json::Value {
        self.0.assemble(row, accepted)
    }
}

/// Запоминает, с какой опорой пришёл каждый снимок. Фасад может отказывать.
#[derive(Default)]
struct RecordingImage {
    seen: Mutex<Vec<(String, Option<Vec<u8>>)>>,
    facade_refused: bool,
}

#[async_trait]
impl synthforge_ports::ImageModel for RecordingImage {
    fn id(&self) -> &str {
        "recording-image"
    }
    fn provider(&self) -> &str {
        "fake"
    }
    async fn render(&self, req: ImageRequest) -> PortResult<synthforge_ports::ImageResponse> {
        if self.facade_refused && req.role == "facade" {
            return Err(PortError::Rejected("фасад не рисуется".into()));
        }
        self.seen.lock().unwrap().push((req.role.clone(), req.reference_png.clone()));
        Ok(synthforge_ports::ImageResponse {
            png: format!("png:{}", req.role).into_bytes(),
            model: "recording-image".into(),
            usage: Usage { tokens_in: 0, tokens_out: 0, cost_usd: 0.04 },
        })
    }
}

async fn house_run(model: Arc<RecordingImage>, concurrency: usize) -> Vec<(String, Option<Vec<u8>>)> {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("synthforge-house-{}-{n}", std::process::id()));
    let gen: Arc<dyn Generator> = Arc::new(HouseGen(FakeGen::with_images(2)));
    let st = store().await;
    let engine = Engine::new(st.clone(), Arc::new(FakeContent::default()), Arc::new(FakeText::ok()))
        .with_config(cfg());
    let session = engine
        .start_session(&*gen, &GenSpec::new(3).seed(21).with_images(true))
        .await
        .unwrap();

    ImagePipeline::new(st, model.clone(), Arc::new(synthforge_engine::FsAssetStore::new(&dir)))
        .concurrency(concurrency)
        .run(gen, &session, &synthforge_engine::image_kind("fake"))
        .await
        .unwrap();

    let _ = std::fs::remove_dir_all(&dir);
    let seen = model.seen.lock().unwrap().clone();
    seen
}

/// Территория снимается с опорой на готовый фасад того же дома — даже когда
/// в очереди она стоит раньше фасада и берётся с ним в одной пачке.
#[tokio::test]
async fn territory_gets_its_facade_as_reference() {
    for concurrency in [1, 4] {
        let seen = house_run(Arc::new(RecordingImage::default()), concurrency).await;
        let territories: Vec<_> = seen.iter().filter(|(r, _)| r == "territory").collect();
        assert_eq!(territories.len(), 3, "параллелизм {concurrency}: {seen:?}");
        for (_, reference) in territories {
            assert_eq!(reference.as_deref(), Some(&b"png:facade"[..]), "параллелизм {concurrency}");
        }
        assert!(seen.iter().filter(|(r, _)| r == "facade").all(|(_, p)| p.is_none()));
    }
}

/// Фасад не получился совсем — территория не зависает в очереди навсегда,
/// а снимается без опоры.
#[tokio::test]
async fn territory_without_facade_is_still_rendered() {
    let model = Arc::new(RecordingImage { facade_refused: true, ..Default::default() });
    let seen = house_run(model, 2).await;
    let territories: Vec<_> = seen.iter().filter(|(r, _)| r == "territory").collect();
    assert_eq!(territories.len(), 3, "{seen:?}");
    assert!(territories.iter().all(|(_, p)| p.is_none()));
}
