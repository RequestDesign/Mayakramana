//! Обратная связь: пометки накапливаются и превращаются в измеримый сигнал.

use synthforge_store::{
    FeedbackSummary, NewAnnotation, NewJob, NewSession, Store, TagCount, Target, Verdict,
};

/// Уникальное имя файла на каждый тест.
///
/// Одного лишь времени мало: тесты идут параллельно, и два вызова легко
/// попадают в одну отметку — тогда они делят базу и мешают друг другу.
/// Счётчик снимает эту неопределённость.
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn store() -> Store {
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("synthforge-fb-{}-{n}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Store::open(path).await.unwrap()
}

async fn session(s: &Store) -> String {
    s.create_session(NewSession {
        kind: "doctor".into(),
        spec: serde_json::json!({}),
        seed: 1,
        budget_usd: None,
    })
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn annotations_accumulate_into_a_signal() {
    let s = store().await;
    let sid = session(&s).await;

    // Приёмка: человек просмотрел двадцать портретов.
    for _ in 0..12 {
        s.annotate(&sid, NewAnnotation::good(Target::Image)).await.unwrap();
    }
    for _ in 0..6 {
        s.annotate(
            &sid,
            NewAnnotation::bad(Target::Image, ["фон"]).comment("везде один и тот же кабинет"),
        )
        .await
        .unwrap();
    }
    for _ in 0..2 {
        s.annotate(&sid, NewAnnotation::bad(Target::Image, ["угрюмый"]))
            .await
            .unwrap();
    }

    let summary = s.feedback_summary(&sid).await.unwrap();
    assert_eq!(summary.good, 12);
    assert_eq!(summary.bad, 8);
    assert!((summary.reject_rate() - 0.4).abs() < 1e-9);

    // Метки отсортированы по частоте: самое частое — первое.
    assert_eq!(summary.tags[0].tag, "фон");
    assert_eq!(summary.tags[0].count, 6);

    // Системным считается то, что повторилось достаточно часто. Разовое
    // «не нравится» — вкусовщина, одно и то же двадцать раз — дефект словаря.
    let systemic: Vec<&str> = summary.systemic(0.25).iter().map(|t| t.tag.as_str()).collect();
    assert_eq!(systemic, vec!["фон"], "«угрюмый» встретился дважды и системным не является");

    // Абсолютный порог: на малой выборке одиночное замечание не должно
    // проходить в системные даже при низкой доле.
    let small = FeedbackSummary {
        good: 5,
        bad: 1,
        tags: vec![TagCount { tag: "вкусовщина".into(), count: 1 }],
    };
    assert!(
        small.systemic(0.1).is_empty(),
        "единичное замечание на шести просмотрах системным не является"
    );
}

#[tokio::test]
async fn annotation_binds_to_a_specific_result() {
    let s = store().await;
    let sid = session(&s).await;

    s.enqueue(
        &sid,
        &[NewJob::new("doctor_text", "doctor:1:0", serde_json::json!({"age": 44}))],
    )
    .await
    .unwrap();

    let job = s.job_by_natural_key("doctor:1:0").await.unwrap().unwrap();

    let id = s
        .annotate(
            &sid,
            NewAnnotation::bad(Target::Image, ["вылизанный", "улыбка"])
                .on_job(job.id)
                .on_asset("out/doctor-1-0-portrait.png")
                .comment("стоковое лицо, идеальная симметрия")
                .by("Андрей"),
        )
        .await
        .unwrap();
    assert!(id > 0);

    let all = s.annotations(&sid).await.unwrap();
    assert_eq!(all.len(), 1);

    let a = &all[0];
    assert_eq!(a.job_id, Some(job.id));
    assert_eq!(a.verdict, Verdict::Bad);
    assert_eq!(a.tag_list(), vec!["вылизанный", "улыбка"]);
    assert_eq!(a.asset.as_deref(), Some("out/doctor-1-0-portrait.png"));
    assert_eq!(a.author.as_deref(), Some("Андрей"));
}

#[tokio::test]
async fn assets_are_keyed_by_entity_not_job() {
    let s = store().await;
    let sid = session(&s).await;

    s.enqueue(
        &sid,
        &[NewJob::new("doctor_image", "doctor:1:0#img0", serde_json::json!({}))],
    )
    .await
    .unwrap();
    let job = s.job_by_natural_key("doctor:1:0#img0").await.unwrap().unwrap();

    s.record_asset(&sid, "doctor:1:0", job.id, "portrait", "out/a.png", 1024, 0.04)
        .await
        .unwrap();

    // Ключ сущности, а не задания: так текст и картинки сходятся при сборке.
    let found = s.assets_for_entity("doctor:1:0").await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].role, "portrait");
    assert!((found[0].cost_usd - 0.04).abs() < 1e-9);

    // Повторная запись того же назначения не плодит дубликаты.
    s.record_asset(&sid, "doctor:1:0", job.id, "portrait", "out/b.png", 2048, 0.04)
        .await
        .unwrap();
    let again = s.assets_for_entity("doctor:1:0").await.unwrap();
    assert_eq!(again.len(), 1, "перегенерация снимка не должна создавать вторую запись");
    assert_eq!(again[0].path, "out/b.png");
}

#[tokio::test]
async fn empty_feedback_does_not_divide_by_zero() {
    let s = store().await;
    let sid = session(&s).await;
    let summary = s.feedback_summary(&sid).await.unwrap();
    assert_eq!(summary.total(), 0);
    assert_eq!(summary.reject_rate(), 0.0);
    assert!(summary.systemic(0.1).is_empty());
}
