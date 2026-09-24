//! Движок: превращает постановку в возобновляемый прогон.
//!
//! Здесь генератор встречается с инфраструктурой. Генератор остаётся чистым —
//! он не знает ни про очередь, ни про хранилище, ни про провайдера; всё это
//! знает движок.
//!
//! # Почему единица работы — строка в таблице
//!
//! Прогон на тысячи сущностей идёт сутками, и за это время что-то обязательно
//! отвалится: перезагрузится сервер, оборвётся канал, кончатся токены. Длинный
//! агентный цикл в такой ситуации невозможно возобновить — состояние живёт в
//! контексте модели и теряется. Поэтому сессия это таблица мелких адресуемых
//! заданий, а восстановление сводится к возврату протухших захватов в очередь.

use std::sync::Arc;
use std::time::Duration;

use synthforge_params::ParamRow;
use synthforge_ports::{
    AcceptedText, BatchVerdict, ContentStore, GenSpec, Generator, PortError, TextModel,
    WriteOutcome,
};
use synthforge_textsim::LexicalIndex;
use synthforge_store::{
    BatchStatus, JobStatus, NewJob, NewSession, Progress, SessionStatus, Store, Usage,
};

mod error;
mod images;
mod jsonl;

pub use error::{Error, Result};
pub use images::{FsAssetStore, ImageJobPayload, ImagePipeline};
pub use jsonl::JsonlStore;

/// Как называется вид задания на текст для сущности.
pub fn text_kind(entity_kind: &str) -> String {
    format!("{entity_kind}_text")
}

/// Как называется вид задания на изображение.
pub fn image_kind(entity_kind: &str) -> String {
    format!("{entity_kind}_image")
}

/// Сколько записей уходит в одной пачке. Предел bulk-операции Nexorium.
const BATCH_SIZE: i64 = 250;

/// Через сколько лок считается протухшим.
///
/// Должно заметно превышать время одного задания, иначе живые воркеры начнут
/// отбирать работу друг у друга.
const STALE_AFTER: Duration = Duration::from_secs(900);

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Сколько заданий обрабатывать одновременно.
    ///
    /// Реальные лимиты провайдера заранее неизвестны, поэтому параметр, а не
    /// константа.
    pub concurrency: usize,
    /// Сколько заданий захватывать за один раз.
    pub claim_size: i64,
    pub stale_after: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { concurrency: 4, claim_size: 16, stale_after: STALE_AFTER }
    }
}

pub struct Engine {
    store: Store,
    content: Arc<dyn ContentStore>,
    text: Arc<dyn TextModel>,
    cfg: EngineConfig,
}

impl Engine {
    pub fn new(store: Store, content: Arc<dyn ContentStore>, text: Arc<dyn TextModel>) -> Self {
        Self { store, content, text, cfg: EngineConfig::default() }
    }

    pub fn with_config(mut self, cfg: EngineConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    // ------------------------------------------------------------- постановка

    /// Спланировать прогон и поставить задания в очередь.
    ///
    /// Идемпотентна: повторный вызов с той же сессией не создаст второй
    /// комплект заданий, потому что естественные ключи совпадут.
    pub async fn start_session(
        &self,
        generator: &dyn Generator,
        spec: &GenSpec,
    ) -> Result<String> {
        let session = self
            .store
            .create_session(NewSession {
                kind: generator.descriptor().kind.clone(),
                spec: serde_json::to_value(spec)?,
                seed: spec.seed as i64,
                budget_usd: spec.budget_usd,
            })
            .await?;

        self.store
            .set_session_status(&session.id, SessionStatus::Planning)
            .await?;

        // Планирование — чистая функция от постановки. Модель ещё не вызвана,
        // но уже решено, какими будут все сущности.
        let rows = generator.plan(spec)?;

        let kind = &generator.descriptor().kind;
        let mut jobs: Vec<NewJob> = Vec::with_capacity(rows.len() * 2);

        for (i, row) in rows.iter().enumerate() {
            let entity_key = generator.natural_key(&session.id, i);

            jobs.push(NewJob::new(text_kind(kind), &entity_key, row.to_json()));

            // Задания на изображения ставятся сразу, а не после текста: промпт
            // портрета строится из визуальных параметров и от текста не
            // зависит. Значит обе линии могут идти параллельно.
            //
            // Но только если снимки вообще запрошены: отладка словаря идёт без
            // них, и висящие в очереди задания искажали бы и прогресс, и
            // оценку готовности прогона.
            if !spec.with_images {
                continue;
            }
            // Опорные снимки ставятся раньше зависимых: очередь берёт задания
            // по порядку, и иначе территория, стоящая до фасада, захватывалась
            // бы снова и снова, не пуская фасад в работу.
            let requests = generator.image_requests(row);
            let mut order: Vec<usize> = (0..requests.len()).collect();
            order.sort_by_key(|&n| requests[n].reference_role.is_some());
            for n in order {
                let payload = ImageJobPayload {
                    entity_key: entity_key.clone(),
                    index: n,
                    row: row.clone(),
                };
                jobs.push(NewJob::new(
                    image_kind(kind),
                    images::image_job_key(&entity_key, n),
                    serde_json::to_value(&payload)?,
                ));
            }
        }

        let added = self.store.enqueue(&session.id, &jobs).await?;
        tracing::info!(session = %session.id, added, "задания поставлены в очередь");

        self.store
            .set_session_status(&session.id, SessionStatus::Running)
            .await?;

        Ok(session.id)
    }

    // ------------------------------------------------------------------ пауза

    /// Поставить сессию на паузу. Идущий прогон остановится после текущей
    /// пачки, новые запуски ничего не возьмут до `resume`.
    pub async fn pause(&self, session_id: &str) -> Result<()> {
        self.store.set_session_status(session_id, SessionStatus::Paused).await?;
        Ok(())
    }

    /// Снять с паузы. Работу продолжает следующий вызов `run`.
    pub async fn resume(&self, session_id: &str) -> Result<()> {
        self.store.set_session_status(session_id, SessionStatus::Running).await?;
        Ok(())
    }

    // ------------------------------------------------------------------ прогон

    /// Обработать все задания сессии.
    ///
    /// Возвращается, когда работы не осталось: всё либо готово, либо признано
    /// безнадёжным. Вызов можно повторять — он подхватит то, что осталось.
    pub async fn run(
        &self,
        generator: Arc<dyn Generator>,
        session_id: &str,
        brief: Option<&str>,
    ) -> Result<Progress> {
        // Сначала возвращаем в очередь то, что осталось от прошлого запуска.
        // Воркер, умерший вместе с процессом, свои задания не разблокирует.
        let revived = self.store.reap(self.cfg.stale_after).await?;
        if revived > 0 {
            tracing::info!(revived, "подняты задания после прошлого прогона");
        }

        // Повторный запуск после сбоя — это продолжение работы.
        if let Some(s) = self.store.session(session_id).await? {
            if s.status == SessionStatus::Failed {
                self.store.set_session_status(session_id, SessionStatus::Running).await?;
            }
        }

        let worker_id = format!("w-{}", std::process::id());
        let uniq = self.uniqueness_guard(&*generator, session_id).await?;

        loop {
            let session = self
                .store
                .session(session_id)
                .await?
                .ok_or_else(|| Error::SessionNotFound(session_id.to_string()))?;

            // Пауза, поставленная человеком или агентом из другого процесса,
            // вступает в силу между пачками: начатые задания доделываются.
            if session.status == SessionStatus::Paused {
                tracing::info!(session = %session_id, "сессия на паузе, прогон остановлен");
                return Ok(self.store.progress(session_id).await?);
            }

            if session.over_budget() {
                tracing::warn!(
                    session = %session_id,
                    spent = session.spent_usd,
                    budget = ?session.budget_usd,
                    "бюджет исчерпан, прогон остановлен"
                );
                self.store
                    .set_session_status(session_id, SessionStatus::Paused)
                    .await?;
                break;
            }

            let claimed = self
                .store
                .claim_of_kind(
                    session_id,
                    &worker_id,
                    &text_kind(&generator.descriptor().kind),
                    self.cfg.claim_size,
                )
                .await?;

            if claimed.is_empty() {
                break;
            }

            // Задания независимы, поэтому идут параллельно. Степень
            // параллелизма — настройка: лимиты провайдера заранее неизвестны.
            for chunk in claimed.chunks(self.cfg.concurrency.max(1)) {
                let mut tasks = Vec::with_capacity(chunk.len());

                for job in chunk {
                    let generator = generator.clone();
                    let text = self.text.clone();
                    let store = self.store.clone();
                    let job = job.clone();
                    let brief = brief.map(str::to_string);
                    let uniq = uniq.clone();

                    tasks.push(tokio::spawn(async move {
                        process_one(&*generator, &*text, &store, job, brief.as_deref(), uniq.as_ref())
                            .await
                    }));
                }

                for t in tasks {
                    match t.await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) if e.is_fatal() => {
                            tracing::error!(error = %e, "фатальная ошибка, прогон остановлен");
                            // Не пауза: пауза — решение человека и держит сессию,
                            // пока её не снимут. После сбоя повторный запуск
                            // должен просто подхватить оставшееся.
                            self.store
                                .set_session_status(session_id, SessionStatus::Failed)
                                .await?;
                            return Ok(self.store.progress(session_id).await?);
                        }
                        Ok(Err(e)) => tracing::warn!(error = %e, "задание не выполнено"),
                        Err(e) => tracing::error!(error = %e, "воркер упал"),
                    }
                }
            }
        }

        let progress = self.store.progress(session_id).await?;
        if progress.is_settled() {
            self.store
                .set_session_status(session_id, SessionStatus::Done)
                .await?;
        }
        Ok(progress)
    }

    /// Подготовить проверку уникальности, если постановка её требует.
    ///
    /// Индекс сразу наполняется тем, что уже принято: после перезапуска новые
    /// тексты должны сверяться и с принятыми до падения, иначе уникальность
    /// держалась бы только в пределах одного запуска процесса.
    async fn uniqueness_guard(
        &self,
        generator: &dyn Generator,
        session_id: &str,
    ) -> Result<Option<UniquenessGuard>> {
        let session = self
            .store
            .session(session_id)
            .await?
            .ok_or_else(|| Error::SessionNotFound(session_id.to_string()))?;

        let spec: GenSpec = match serde_json::from_str(&session.spec) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };

        let synthforge_ports::GenMode::UniqueAgainstBase(u) = &spec.mode else {
            return Ok(None);
        };
        let Some(threshold) = u.lexical_threshold() else {
            return Ok(None);
        };

        let scope_session = match u.scope {
            synthforge_ports::Scope::Session => Some(session_id),
            synthforge_ports::Scope::Collection => None,
        };

        let mut index = LexicalIndex::new();
        let prior = self
            .store
            .done_results_of_kind(&text_kind(&generator.descriptor().kind), scope_session)
            .await?;

        for (key, raw) in prior {
            let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(&raw) else {
                continue;
            };
            if let Some(text) = generator.uniqueness_text(&AcceptedText { fields }) {
                index.insert(key, &text);
            }
        }

        tracing::info!(
            threshold,
            preloaded = index.len(),
            "проверка лексической уникальности включена"
        );

        Ok(Some(UniquenessGuard {
            index: Arc::new(tokio::sync::Mutex::new(index)),
            threshold,
        }))
    }

    // ------------------------------------------------------------------- пачки

    /// Отправить готовые результаты в хранилище пачками.
    ///
    /// Возвращает число записанных сущностей.
    pub async fn flush(&self, generator: &dyn Generator, session_id: &str) -> Result<usize> {
        let collection = &generator.descriptor().collection;
        self.content
            .ensure_collection(collection, &generator.descriptor().title)
            .await?;

        // До новых пачек разбираем старые с неизвестным исходом: иначе можно
        // записать дубликаты поверх того, что уже легло.
        self.resolve_unknown_batches(generator).await?;

        let mut written = 0usize;

        let kind = text_kind(&generator.descriptor().kind);

        while let Some((batch, jobs)) = self
            .store
            .open_batch_of_kind(session_id, collection, &kind, BATCH_SIZE)
            .await?
        {
            // Здесь текст и изображения сущности сходятся: их делали разные
            // задания, связывает их ключ сущности.
            let mut records: Vec<serde_json::Value> = Vec::with_capacity(jobs.len());

            for job in &jobs {
                let Some(raw) = job.result.as_deref() else { continue };
                let Ok(mut record) = serde_json::from_str::<serde_json::Value>(raw) else {
                    continue;
                };

                // Метки происхождения: из какого прогона и какой это сущность.
                // Без них записи разных прогонов в одной коллекции неотличимы,
                // и ни просмотр, ни откат прогона целиком невозможны.
                if let Some(obj) = record.as_object_mut() {
                    obj.insert("session_id".into(), serde_json::Value::String(session_id.into()));
                    obj.insert(
                        "natural_key".into(),
                        serde_json::Value::String(job.natural_key.clone()),
                    );
                }

                let assets = self.store.assets_for_entity(&job.natural_key).await?;
                if !assets.is_empty() {
                    if let Some(obj) = record.as_object_mut() {
                        let images: Vec<serde_json::Value> = assets
                            .iter()
                            .map(|a| serde_json::json!({"role": a.role, "path": a.path}))
                            .collect();
                        obj.insert("images".into(), serde_json::Value::Array(images));
                    }
                }

                records.push(record);
            }

            self.store
                .set_batch_status(&batch.id, BatchStatus::Inflight, None)
                .await?;

            match self.content.write_batch(collection, &batch.id, &records).await {
                Ok(WriteOutcome::Committed) => {
                    self.store
                        .set_batch_status(&batch.id, BatchStatus::Committed, None)
                        .await?;
                    written += records.len();
                }
                Ok(WriteOutcome::Unknown) => {
                    // Не повторяем вслепую: сверка по метке пачки разрешит,
                    // легло или нет.
                    self.store
                        .set_batch_status(&batch.id, BatchStatus::Unknown, None)
                        .await?;
                }
                Err(e) => {
                    self.store
                        .set_batch_status(&batch.id, BatchStatus::Failed, Some(&e.to_string()))
                        .await?;
                    self.store.detach_batch(&batch.id).await?;
                    if e.is_fatal() {
                        return Err(e.into());
                    }
                }
            }
        }

        Ok(written)
    }

    /// Разрешить пачки с неопределённым исходом.
    ///
    /// Обрыв соединения на записи не означает, что сервер её не выполнил.
    /// Один запрос по метке пачки отвечает, что произошло на самом деле.
    pub async fn resolve_unknown_batches(&self, generator: &dyn Generator) -> Result<()> {
        let collection = &generator.descriptor().collection;

        for batch in self.store.unresolved_batches().await? {
            let verdict = self
                .content
                .verify_batch(collection, &batch.id, batch.expected as u64)
                .await?;

            match verdict {
                BatchVerdict::Committed => {
                    tracing::info!(batch = %batch.id, "пачка легла целиком");
                    self.store
                        .set_batch_status(&batch.id, BatchStatus::Committed, None)
                        .await?;
                }
                BatchVerdict::Absent => {
                    tracing::info!(batch = %batch.id, "пачка не легла, задания вернутся в отправку");
                    self.store.detach_batch(&batch.id).await?;
                    self.store
                        .set_batch_status(&batch.id, BatchStatus::Failed, Some("не легла"))
                        .await?;
                }
                BatchVerdict::Partial { found } => {
                    // Удаление идемпотентно, поэтому частичный случай
                    // разрешается откатом и повтором целиком.
                    tracing::warn!(batch = %batch.id, found, "пачка легла частично, откатываю");
                    self.content.delete_batch(collection, &batch.id).await?;
                    self.store.detach_batch(&batch.id).await?;
                    self.store
                        .set_batch_status(&batch.id, BatchStatus::Failed, Some("легла частично"))
                        .await?;
                }
            }
        }

        Ok(())
    }

    pub async fn progress(&self, session_id: &str) -> Result<Progress> {
        Ok(self.store.progress(session_id).await?)
    }
}

/// Проверка уникальности, общая для всех воркеров прогона.
#[derive(Clone)]
struct UniquenessGuard {
    index: Arc<tokio::sync::Mutex<LexicalIndex>>,
    threshold: f32,
}

/// Обработка одного задания: промпт, вызов модели, приёмка, запись результата.
async fn process_one(
    generator: &dyn Generator,
    text: &dyn TextModel,
    store: &Store,
    job: synthforge_store::Job,
    brief: Option<&str>,
    uniq: Option<&UniquenessGuard>,
) -> Result<()> {
    let row: ParamRow = serde_json::from_str(&job.payload)?;

    let response = match text.complete(generator.text_request(&row, brief)).await {
        Ok(r) => r,
        Err(e) => {
            store.fail(job.id, &e.to_string(), Usage::default()).await?;
            return Err(Error::Port(e));
        }
    };

    let usage = Usage {
        tokens_in: response.usage.tokens_in as i64,
        tokens_out: response.usage.tokens_out as i64,
        cost_usd: response.usage.cost_usd,
    };

    match generator.accept_text(&row, &response.text) {
        Ok(accepted) => {
            // Проверка и вставка в индекс под одним замком: иначе два воркера
            // могли бы одновременно принять два почти одинаковых текста —
            // каждый проверил бы индекс до того, как туда попал другой.
            if let (Some(guard), Some(t)) = (uniq, generator.uniqueness_text(&accepted)) {
                let mut index = guard.index.lock().await;
                if let Some(hit) = index.too_close(&t, guard.threshold) {
                    drop(index);
                    let reason = format!(
                        "слишком похоже на {} (совпадение формулировок {:.0}% при пороге {:.0}%)",
                        hit.id,
                        hit.score * 100.0,
                        guard.threshold * 100.0
                    );
                    store.fail(job.id, &reason, usage).await?;
                    return Ok(());
                }
                index.insert(job.natural_key.clone(), &t);
            }

            let record = generator.assemble(&row, &accepted);
            store.complete(job.id, &record, usage).await?;
            Ok(())
        }
        Err(reason) => {
            // Расход всё равно записываем: модель отработала, деньги потрачены,
            // и в сводке прогона это должно быть видно.
            let status = store.fail(job.id, &reason.to_string(), usage).await?;
            if status == JobStatus::Dead {
                tracing::warn!(job = job.id, reason = %reason, "задание признано безнадёжным");
            }
            Ok(())
        }
    }
}

impl From<PortError> for Error {
    fn from(e: PortError) -> Self {
        Error::Port(e)
    }
}
