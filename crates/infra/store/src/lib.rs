//! Оперативное состояние движка: сессии, очередь заданий, пачки отправки.
//!
//! Живёт в локальном SQLite и наружу не выходит. Причина — в ARCHITECTURE.md,
//! решение 1: очередь требует тысяч мелких обновлений статуса, а Nexorium правит
//! записи со скоростью порядка ста в минуту.
//!
//! Единица работы — строка в таблице `jobs`. Отсюда возобновляемость: упал
//! сервер, отвалился VPN, кончились токены — задания остались в `running`,
//! [`Store::reap`] вернул их в `pending`, воркеры разобрали заново. Отдельного
//! механизма чекпоинтов не нужно.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::SqlitePool;
use uuid::Uuid;

mod error;
mod feedback;
mod models;

pub use error::{Error, Result};
pub use feedback::{
    Annotation, Asset, FeedbackSummary, NewAnnotation, TagCount, Target, Verdict,
};
pub use models::*;

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // WAL: писатель не блокирует читателей, и база переживает падение процесса.
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            // Без этого конкурентные захваты падают с SQLITE_BUSY вместо ожидания.
            .busy_timeout(Duration::from_secs(30))
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;

        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    /// Только для тестов: одно соединение, база в памяти.
    pub async fn open_memory() -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    async fn migrate(pool: &SqlitePool) -> Result<()> {
        sqlx::migrate!("./migrations").run(pool).await?;
        Ok(())
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // ---------------------------------------------------------------- сессии

    pub async fn create_session(&self, new: NewSession) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        let now = now_millis();
        let spec = serde_json::to_string(&new.spec)?;

        sqlx::query(
            "insert into sessions (id, kind, spec, status, seed, budget_usd, spent_usd, created_at, updated_at)
             values (?1, ?2, ?3, 'pending', ?4, ?5, 0, ?6, ?6)",
        )
        .bind(&id)
        .bind(&new.kind)
        .bind(&spec)
        .bind(new.seed)
        .bind(new.budget_usd)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.session(&id).await?.ok_or(Error::SessionNotFound(id))
    }

    pub async fn session(&self, id: &str) -> Result<Option<Session>> {
        Ok(sqlx::query_as::<_, Session>("select * from sessions where id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn sessions(&self) -> Result<Vec<Session>> {
        Ok(
            sqlx::query_as::<_, Session>("select * from sessions order by created_at desc")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn set_session_status(&self, id: &str, status: SessionStatus) -> Result<()> {
        sqlx::query("update sessions set status = ?1, updated_at = ?2 where id = ?3")
            .bind(status)
            .bind(now_millis())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // --------------------------------------------------------------- очередь

    /// Постановка заданий в очередь. Идемпотентна по `natural_key`: повторный
    /// запуск планировщика не создаст второй комплект.
    ///
    /// Возвращает число реально добавленных заданий.
    pub async fn enqueue(&self, session_id: &str, jobs: &[NewJob]) -> Result<u64> {
        if jobs.is_empty() {
            return Ok(0);
        }
        let now = now_millis();
        let mut tx = self.pool.begin().await?;
        let mut inserted = 0u64;

        for job in jobs {
            let payload = serde_json::to_string(&job.payload)?;
            let res = sqlx::query(
                "insert or ignore into jobs
                   (session_id, kind, natural_key, payload, status, max_attempts, created_at, updated_at)
                 values (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?6)",
            )
            .bind(session_id)
            .bind(&job.kind)
            .bind(&job.natural_key)
            .bind(&payload)
            .bind(job.max_attempts)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            inserted += res.rows_affected();
        }

        tx.commit().await?;
        Ok(inserted)
    }

    /// Атомарный захват пачки заданий.
    ///
    /// Одиночный `UPDATE` в SQLite выполняется в неявной транзакции, а писателей
    /// движок сериализует, поэтому два воркера не могут захватить одно задание —
    /// это аналог `SELECT … FOR UPDATE SKIP LOCKED` в Postgres.
    pub async fn claim(&self, session_id: &str, worker: &str, limit: i64) -> Result<Vec<Job>> {
        self.claim_inner(session_id, worker, None, limit).await
    }

    /// Захват заданий одного вида.
    ///
    /// Нужен, потому что текст и изображения — разные линии с разным
    /// параллелизмом. Без фильтра текстовый воркер захватывал бы задания на
    /// картинки и держал их заблокированными, ничего с ними не делая.
    pub async fn claim_of_kind(
        &self,
        session_id: &str,
        worker: &str,
        kind: &str,
        limit: i64,
    ) -> Result<Vec<Job>> {
        self.claim_inner(session_id, worker, Some(kind), limit).await
    }

    async fn claim_inner(
        &self,
        session_id: &str,
        worker: &str,
        kind: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Job>> {
        let now = now_millis();
        Ok(sqlx::query_as::<_, Job>(
            "update jobs
                set status     = 'running',
                    locked_by  = ?1,
                    locked_at  = ?2,
                    attempts   = attempts + 1,
                    updated_at = ?2
              where id in (
                    select id from jobs
                     where session_id = ?3 and status = 'pending'
                       and (?5 is null or kind = ?5)
                     order by id
                     limit ?4
              )
              returning *",
        )
        .bind(worker)
        .bind(now)
        .bind(session_id)
        .bind(limit)
        .bind(kind)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Задание выполнено. `result` — запись, готовая к отправке в Nexorium.
    pub async fn complete(&self, job_id: i64, result: &serde_json::Value, usage: Usage) -> Result<()> {
        let now = now_millis();
        let payload = serde_json::to_string(result)?;
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "update jobs
                set status = 'done', result = ?1, locked_by = null, locked_at = null,
                    tokens_in = tokens_in + ?2, tokens_out = tokens_out + ?3,
                    cost_usd = cost_usd + ?4, last_error = null, updated_at = ?5
              where id = ?6",
        )
        .bind(&payload)
        .bind(usage.tokens_in)
        .bind(usage.tokens_out)
        .bind(usage.cost_usd)
        .bind(now)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "update sessions
                set spent_usd = spent_usd + ?1, updated_at = ?2
              where id = (select session_id from jobs where id = ?3)",
        )
        .bind(usage.cost_usd)
        .bind(now)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Задание не удалось. Возвращается в очередь, пока не исчерпаны попытки;
    /// после этого признаётся безнадёжным (`dead`) и больше не берётся.
    pub async fn fail(&self, job_id: i64, err: &str, usage: Usage) -> Result<JobStatus> {
        let now = now_millis();

        // Расход учитывается и на неудаче: модель отработала, деньги потрачены.
        // Иначе сводка сессии разойдётся с суммой по заданиям, а бюджетный
        // потолок будет пропускать брак бесплатно.
        if usage.cost_usd != 0.0 {
            sqlx::query(
                "update sessions
                    set spent_usd = spent_usd + ?1, updated_at = ?2
                  where id = (select session_id from jobs where id = ?3)",
            )
            .bind(usage.cost_usd)
            .bind(now)
            .bind(job_id)
            .execute(&self.pool)
            .await?;
        }

        let row = sqlx::query_as::<_, (JobStatus,)>(
            "update jobs
                set status = case when attempts >= max_attempts then 'dead' else 'pending' end,
                    last_error = ?1, locked_by = null, locked_at = null,
                    tokens_in = tokens_in + ?2, tokens_out = tokens_out + ?3,
                    cost_usd = cost_usd + ?4, updated_at = ?5
              where id = ?6
              returning status",
        )
        .bind(err)
        .bind(usage.tokens_in)
        .bind(usage.tokens_out)
        .bind(usage.cost_usd)
        .bind(now)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(Error::JobNotFound(job_id))?;

        Ok(row.0)
    }

    /// Возврат протухших захватов в очередь.
    ///
    /// Это и есть весь механизм восстановления после падения: воркер, который
    /// умер вместе с процессом, не разблокирует свои задания сам.
    pub async fn reap(&self, stale_after: Duration) -> Result<u64> {
        let now = now_millis();
        let cutoff = now - stale_after.as_millis() as i64;

        let res = sqlx::query(
            "update jobs
                set status = case when attempts >= max_attempts then 'dead' else 'pending' end,
                    locked_by = null, locked_at = null,
                    last_error = 'лок протух: воркер не завершил задание',
                    updated_at = ?1
              where status = 'running' and locked_at is not null and locked_at < ?2",
        )
        .bind(now)
        .bind(cutoff)
        .execute(&self.pool)
        .await?;

        let n = res.rows_affected();
        if n > 0 {
            tracing::warn!(count = n, "возвращены в очередь задания с протухшим локом");
        }
        Ok(n)
    }

    pub async fn progress(&self, session_id: &str) -> Result<Progress> {
        let rows = sqlx::query_as::<_, (JobStatus, i64, f64, i64, i64)>(
            "select status, count(*), coalesce(sum(cost_usd), 0),
                    coalesce(sum(tokens_in), 0), coalesce(sum(tokens_out), 0)
               from jobs where session_id = ?1 group by status",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        let mut p = Progress::default();
        for (status, n, cost, tin, tout) in rows {
            match status {
                JobStatus::Pending => p.pending = n,
                JobStatus::Running => p.running = n,
                JobStatus::Done => p.done = n,
                JobStatus::Failed => p.failed = n,
                JobStatus::Dead => p.dead = n,
            }
            p.spent_usd += cost;
            p.tokens_in += tin;
            p.tokens_out += tout;
        }
        Ok(p)
    }

    // ----------------------------------------------------------------- пачки

    /// Собрать пачку из готовых, ещё не отправленных заданий.
    ///
    /// `limit` не должен превышать предел bulk-операции Nexorium (250).
    /// Собрать пачку из готовых заданий указанного вида.
    ///
    /// Вид важен: в сессии соседствуют текстовые задания и задания на
    /// изображения, а в хранилище уходит одна запись на сущность — собранная
    /// из текстового задания и приложенных к нему файлов.
    pub async fn open_batch_of_kind(
        &self,
        session_id: &str,
        collection: &str,
        kind: &str,
        limit: i64,
    ) -> Result<Option<(Batch, Vec<Job>)>> {
        self.open_batch_inner(session_id, collection, Some(kind), limit)
            .await
    }

    pub async fn open_batch(
        &self,
        session_id: &str,
        collection: &str,
        limit: i64,
    ) -> Result<Option<(Batch, Vec<Job>)>> {
        self.open_batch_inner(session_id, collection, None, limit).await
    }

    async fn open_batch_inner(
        &self,
        session_id: &str,
        collection: &str,
        kind: Option<&str>,
        limit: i64,
    ) -> Result<Option<(Batch, Vec<Job>)>> {
        let now = now_millis();
        let batch_id = Uuid::new_v4().to_string();
        let mut tx = self.pool.begin().await?;

        let jobs = sqlx::query_as::<_, Job>(
            "update jobs set batch_id = ?1, updated_at = ?2
              where id in (
                    select id from jobs
                     where session_id = ?3 and status = 'done' and batch_id is null
                       and (?5 is null or kind = ?5)
                     order by id limit ?4
              )
              returning *",
        )
        .bind(&batch_id)
        .bind(now)
        .bind(session_id)
        .bind(limit)
        .bind(kind)
        .fetch_all(&mut *tx)
        .await?;

        if jobs.is_empty() {
            tx.rollback().await?;
            return Ok(None);
        }

        sqlx::query(
            "insert into batches (id, session_id, collection, expected, status, created_at, updated_at)
             values (?1, ?2, ?3, ?4, 'pending', ?5, ?5)",
        )
        .bind(&batch_id)
        .bind(session_id)
        .bind(collection)
        .bind(jobs.len() as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let batch = sqlx::query_as::<_, Batch>("select * from batches where id = ?1")
            .bind(&batch_id)
            .fetch_one(&self.pool)
            .await?;

        Ok(Some((batch, jobs)))
    }

    pub async fn set_batch_status(
        &self,
        batch_id: &str,
        status: BatchStatus,
        err: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "update batches
                set status = ?1, last_error = ?2,
                    attempts = attempts + case when ?1 = 'inflight' then 1 else 0 end,
                    updated_at = ?3
              where id = ?4",
        )
        .bind(status)
        .bind(err)
        .bind(now_millis())
        .bind(batch_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Пачки с неопределённым исходом — их надо сверить с Nexorium по `batch_id`.
    pub async fn unresolved_batches(&self) -> Result<Vec<Batch>> {
        Ok(sqlx::query_as::<_, Batch>(
            "select * from batches where status in ('unknown', 'inflight') order by created_at",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Открепить задания от пачки, чтобы они попали в новую. Нужно, когда пачка
    /// легла частично и её удалили из Nexorium целиком.
    pub async fn detach_batch(&self, batch_id: &str) -> Result<u64> {
        let res = sqlx::query("update jobs set batch_id = null, updated_at = ?1 where batch_id = ?2")
            .bind(now_millis())
            .bind(batch_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    async fn store() -> Store {
        let path = std::env::temp_dir().join(format!("synthforge-test-{}.db", Uuid::new_v4()));
        Store::open(path).await.unwrap()
    }

    async fn seeded(n: usize) -> (Store, Session) {
        let s = store().await;
        let session = s
            .create_session(NewSession {
                kind: "houses".into(),
                spec: serde_json::json!({"count": n}),
                seed: 42,
                budget_usd: None,
            })
            .await
            .unwrap();

        let jobs: Vec<NewJob> = (0..n)
            .map(|i| NewJob::new("house_text", format!("house:{i}"), serde_json::json!({"i": i})))
            .collect();
        s.enqueue(&session.id, &jobs).await.unwrap();
        (s, session)
    }

    #[tokio::test]
    async fn enqueue_is_idempotent_by_natural_key() {
        let (s, session) = seeded(10).await;

        // Планировщик перезапустился и поставил тот же комплект заново.
        let jobs: Vec<NewJob> = (0..10)
            .map(|i| NewJob::new("house_text", format!("house:{i}"), serde_json::json!({"i": i})))
            .collect();
        let added = s.enqueue(&session.id, &jobs).await.unwrap();

        assert_eq!(added, 0, "повторная постановка не должна создавать дубликаты");
        assert_eq!(s.progress(&session.id).await.unwrap().total(), 10);
    }

    #[tokio::test]
    async fn concurrent_claims_never_overlap() {
        let (s, session) = seeded(100).await;

        let mut handles = Vec::new();
        for w in 0..4 {
            let s = s.clone();
            let sid = session.id.clone();
            handles.push(tokio::spawn(async move {
                let mut got = Vec::new();
                loop {
                    let batch = s.claim(&sid, &format!("worker-{w}"), 7).await.unwrap();
                    if batch.is_empty() {
                        break;
                    }
                    got.extend(batch.into_iter().map(|j| j.id));
                }
                got
            }));
        }

        let mut all = Vec::new();
        for h in handles {
            all.extend(h.await.unwrap());
        }

        let unique: HashSet<_> = all.iter().copied().collect();
        assert_eq!(all.len(), 100, "должны разобрать все задания");
        assert_eq!(unique.len(), 100, "ни одно задание не должно достаться двум воркерам");
    }

    /// Главный тест возобновляемости: воркер захватил задания и умер, не завершив их.
    #[tokio::test]
    async fn reaper_returns_jobs_abandoned_by_dead_worker() {
        let (s, session) = seeded(20).await;

        let claimed = s.claim(&session.id, "worker-который-умрёт", 20).await.unwrap();
        assert_eq!(claimed.len(), 20);

        let p = s.progress(&session.id).await.unwrap();
        assert_eq!(p.running, 20);
        assert_eq!(p.pending, 0);

        // Процесс упал: сервер перезагрузился / отвалился VPN / кончились токены.
        // Никто не разблокировал задания — это делает reaper.
        let revived = s.reap(Duration::from_millis(0)).await.unwrap();
        assert_eq!(revived, 20);

        let p = s.progress(&session.id).await.unwrap();
        assert_eq!(p.pending, 20, "задания должны вернуться в очередь");
        assert_eq!(p.running, 0);

        // И их снова можно взять в работу.
        assert_eq!(s.claim(&session.id, "worker-2", 20).await.unwrap().len(), 20);
    }

    #[tokio::test]
    async fn failing_job_retries_then_dies() {
        let s = store().await;
        let session = s
            .create_session(NewSession {
                kind: "houses".into(),
                spec: serde_json::json!({}),
                seed: 1,
                budget_usd: None,
            })
            .await
            .unwrap();

        let mut job = NewJob::new("house_text", "house:0", serde_json::json!({}));
        job.max_attempts = 3;
        s.enqueue(&session.id, &[job]).await.unwrap();

        let mut statuses = Vec::new();
        for _ in 0..3 {
            let claimed = s.claim(&session.id, "w", 1).await.unwrap();
            assert_eq!(claimed.len(), 1);
            statuses.push(s.fail(claimed[0].id, "провайдер вернул 500", Usage::default()).await.unwrap());
        }

        assert_eq!(statuses, vec![JobStatus::Pending, JobStatus::Pending, JobStatus::Dead]);
        assert!(
            s.claim(&session.id, "w", 1).await.unwrap().is_empty(),
            "безнадёжное задание больше не должно выдаваться"
        );
    }

    #[tokio::test]
    async fn batch_is_opened_from_completed_jobs_only() {
        let (s, session) = seeded(10).await;

        let claimed = s.claim(&session.id, "w", 4).await.unwrap();
        for j in &claimed {
            s.complete(
                j.id,
                &serde_json::json!({"name": "Дом"}),
                Usage { tokens_in: 10, tokens_out: 20, cost_usd: 0.001 },
            )
            .await
            .unwrap();
        }

        let (batch, jobs) = s.open_batch(&session.id, "houses", 250).await.unwrap().unwrap();
        assert_eq!(batch.expected, 4);
        assert_eq!(jobs.len(), 4, "в пачку попадают только выполненные задания");

        // Повторное открытие пачки не должно захватить те же задания.
        assert!(s.open_batch(&session.id, "houses", 250).await.unwrap().is_none());

        // Частично легла — открепляем и собираем заново.
        assert_eq!(s.detach_batch(&batch.id).await.unwrap(), 4);
        assert!(s.open_batch(&session.id, "houses", 250).await.unwrap().is_some());

        let p = s.progress(&session.id).await.unwrap();
        assert_eq!(p.done, 4);
        assert!((p.spent_usd - 0.004).abs() < 1e-9);
    }
}
