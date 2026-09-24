//! Приёмка ответа модели — самая ответственная часть генератора.
//!
//! Если пропустить расхождение фактов, несогласованность уйдёт в базу и
//! всплывёт уже на витрине. Перегенерировать её тогда будет дороже, чем не
//! допустить сейчас.

use std::path::PathBuf;

use serde_json::json;
use synthforge_gen_person::PersonGenerator;
use synthforge_params::{ParamModel, ParamRow, Usage, Value};
use synthforge_ports::{GenSpec, Generator, RejectReason};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn generator() -> PersonGenerator {
    let model = ParamModel::load(root().join("dictionaries/role-doctor.json"))
        .unwrap_or_else(|e| panic!("{e}"));
    let names = synthforge_refdata::NameBook::load(root().join("reference/names-ru.json"))
        .unwrap_or_else(|e| panic!("{e}"));
    PersonGenerator::doctor(model).with_names(names)
}

/// Строка с заведомо известными возрастом и стажем.
fn row_with(age: i64, experience: i64) -> ParamRow {
    let g = generator();
    let mut row = g.plan(&GenSpec::new(1).seed(5)).unwrap().remove(0);
    row.set("age", Value::Int(age));
    row.set("experience_years", Value::Int(experience));
    row
}

fn good_answer(age: i64, experience: i64) -> String {
    json!({
        "biography": "Родился и вырос в небольшом городе, медицинское образование получил \
            в областном центре. В наркологию пришёл не сразу: первые годы работал в \
            общепсихиатрическом отделении, где впервые столкнулся с пациентами, у которых \
            зависимость накладывалась на психическое расстройство. Тогда и решил, что \
            хочет заниматься именно этим направлением, и прошёл переподготовку.",
        "professional_path": "Начинал в государственной клинике, где вёл приём и дежурства. \
            Постепенно сосредоточился на работе с длительными программами, а не только на \
            снятии острых состояний. Считает, что без участия семьи результат почти всегда \
            оказывается временным, поэтому много времени уделяет разговорам с родственниками.",
        "quote": "Срыв — это не конец работы, а её часть. Важно, чтобы человек вернулся.",
        "stated_age": age,
        "stated_experience_years": experience
    })
    .to_string()
}

#[test]
fn plan_is_reproducible_and_valid() {
    let g = generator();
    let spec = GenSpec::new(50).seed(2026);

    let a = g.plan(&spec).unwrap();
    let b = g.plan(&spec).unwrap();
    assert_eq!(a, b, "одинаковая постановка обязана давать одинаковый план");
    assert_eq!(a.len(), 50);

    for row in &a {
        for rule in &g.model().hard {
            assert!(rule.holds(row), "нарушено «{}»", rule.name);
        }
    }
}

#[test]
fn text_and_image_prompts_do_not_share_facts() {
    let g = generator();
    let row = g.plan(&GenSpec::new(1)).unwrap().remove(0);

    let text = g.text_request(&row, Some("с опытом работы от семи лет"));
    let user = text
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(user.contains("Стаж"), "{user}");
    assert!(!user.contains("Мешки под глазами"), "внешность в текстовом промпте");
    assert!(user.contains("не источник фактов"), "бриф должен подаваться как тон, а не как факты");
    assert!(text.schema.is_some(), "ответ должен быть структурным");

    let images = g.image_requests(&row);
    assert_eq!(images.len(), 1);
    let img = &images[0];
    assert!(img.prompt.contains("Выражение лица"), "{}", img.prompt);
    assert!(!img.prompt.contains("Публикаций"), "биография в промпте портрета");

    // Запреты должны доезжать до провайдера даже без отдельного поля.
    assert!(!img.avoid.is_empty());
    assert!(img.full_prompt().contains("идеально симметричное лицо"));
}

#[test]
fn well_formed_answer_is_accepted() {
    let g = generator();
    let row = row_with(49, 18);
    let accepted = g
        .accept_text(&row, &good_answer(49, 18))
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(accepted.fields.contains_key("biography"));
    assert!(accepted.fields.contains_key("quote"));
}

/// Модель сама призналась, что считала стаж другим.
#[test]
fn echoed_numbers_must_match_parameters() {
    let g = generator();
    let row = row_with(49, 18);

    let err = g.accept_text(&row, &good_answer(49, 25)).unwrap_err();
    match err {
        RejectReason::FactDrift { param, expected, found } => {
            assert_eq!(param, "experience_years");
            assert_eq!(expected, "18");
            assert_eq!(found, "25");
        }
        other => panic!("ожидалось расхождение фактов, получено {other}"),
    }
}

/// Главный случай: эхо совпало, а в прозе всё равно «более двадцати лет».
#[test]
fn prose_claiming_more_years_than_experience_is_rejected() {
    let g = generator();
    let row = row_with(49, 18);

    let answer = json!({
        "biography": "Родился в небольшом городе, окончил медицинский университет и сразу \
            пришёл в практическую медицину. Более двадцати лет в профессии, из них \
            значительная часть отдана работе с зависимыми пациентами и их семьями. \
            Считает важным не бросать человека сразу после выписки из стационара.",
        "professional_path": "Начинал в государственной клинике, вёл приём и дежурства, \
            позже сосредоточился на длительном сопровождении. Много работает с \
            родственниками, потому что без них результат обычно оказывается временным.",
        "quote": "Человека нельзя вылечить за месяц, можно только начать.",
        "stated_age": 49,
        "stated_experience_years": 18
    })
    .to_string();

    match g.accept_text(&row, &answer).unwrap_err() {
        RejectReason::FactDrift { found, .. } => {
            assert!(found.contains("20"), "должно поймать «двадцати лет»: {found}");
        }
        other => panic!("расхождение в прозе не поймано: {other}"),
    }
}

/// Отрезки внутри общего стажа — это нормально, а не расхождение.
#[test]
fn sub_periods_within_experience_are_fine() {
    let g = generator();
    let row = row_with(49, 18);

    let answer = json!({
        "biography": "Окончил медицинский университет в областном центре. Первые пять лет \
            работал в общепсихиатрическом отделении, затем перешёл в наркологию. Именно \
            там сформировался его интерес к пациентам с двойным диагнозом, с которыми он \
            работает до сих пор и которых считает самой трудной категорией.",
        "professional_path": "Три года вёл дневной стационар, потом занялся длительными \
            программами. Уверен, что без участия семьи результат оказывается временным, \
            поэтому отдельно работает с родственниками пациентов.",
        "quote": "Моя задача — довести человека до момента, когда он сам захочет продолжать.",
        "stated_age": 49,
        "stated_experience_years": 18
    })
    .to_string();

    assert!(
        g.accept_text(&row, &answer).is_ok(),
        "пять и три года укладываются в стаж 18 — это не расхождение"
    );
}

/// Регрессия на ложное срабатывание, пойманное на живом прогоне.
///
/// При возрасте 57 и стаже 9 фраза «в 48 лет пришёл в наркологию» законна:
/// это возраст в момент смены профессии, а не завышенный стаж. Первая версия
/// правила отбраковывала такие ответы, то есть жгла деньги на перегенерацию
/// корректного текста.
#[test]
fn age_at_a_past_career_moment_is_not_drift() {
    let g = generator();
    let row = row_with(57, 9);

    let answer = json!({
        "biography": "Долгое время работал неврологом в государственной клинике, и только \
            в 48 лет всерьёз занялся наркологией — к тому моменту накопилось слишком много \
            пациентов, у которых неврологические жалобы оказывались следствием зависимости. \
            Прошёл переподготовку и с тех пор ведёт приём по новому профилю.",
        "professional_path": "Основная часть его пациентов — люди старшего возраста, у \
            которых зависимость наложилась на возрастные изменения. Считает, что лекарства \
            здесь только половина дела, и много времени тратит на разговоры с семьёй.",
        "quote": "В пожилом возрасте отказ от привычки даётся тяжелее, но и держится крепче.",
        "stated_age": 57,
        "stated_experience_years": 9
    })
    .to_string();

    assert!(
        g.accept_text(&row, &answer).is_ok(),
        "возраст в момент события не должен считаться расхождением по стажу"
    );
}

/// Анкетные обороты — признак пересказа параметров вместо рассказа о человеке.
#[test]
fn questionnaire_phrasing_is_rejected() {
    let g = generator();
    let row = row_with(49, 18);

    let answer = json!({
        "biography": "Окончил медицинский университет и пришёл в наркологию сразу после \
            ординатуры. За карьеру сменил два места работы, начинал в государственной \
            клинике. Постепенно сосредоточился на пациентах с сочетанными расстройствами, \
            которых считает самой трудной категорией в своей практике.",
        "professional_path": "Возрастной фокус — подростки и молодёжь. Сочетает \
            медикаментозную поддержку с длительным сопровождением, считая, что одно без \
            другого даёт лишь временный результат и быстро приводит к возвращению.",
        "quote": "Работа не заканчивается выпиской, она с неё начинается.",
        "stated_age": 49,
        "stated_experience_years": 18
    })
    .to_string();

    assert!(
        matches!(g.accept_text(&row, &answer).unwrap_err(), RejectReason::Forbidden(_)),
        "анкетные обороты должны отбраковываться"
    );
}

/// Второй абзац, пересказывающий первый, — дефект, пойманный на живом прогоне.
#[test]
fn paragraph_restating_the_other_is_rejected() {
    let g = generator();
    let row = row_with(49, 18);

    let answer = json!({
        "biography": "Окончил медицинский университет и пришёл в наркологию. Ведёт приём, \
            занимается детоксикацией и консультирует родственников пациентов, опираясь на \
            медикаментозную стабилизацию как основу лечения и длительное сопровождение.",
        "professional_path": "Ведёт приём, занимается детоксикацией и консультирует \
            родственников пациентов, опираясь на медикаментозную стабилизацию как основу \
            лечения и длительное сопровождение после выписки.",
        "quote": "Работа не заканчивается выпиской, она с неё начинается.",
        "stated_age": 49,
        "stated_experience_years": 18
    })
    .to_string();

    match g.accept_text(&row, &answer).unwrap_err() {
        RejectReason::Forbidden(msg) => assert!(msg.contains("пересказывают"), "{msg}"),
        other => panic!("пересказ абзаца не пойман: {other}"),
    }
}

/// Английское слово посреди русского текста — дефект с живого прогона.
#[test]
fn latin_words_are_rejected() {
    let g = generator();
    let row = row_with(49, 18);
    let mut v: serde_json::Value = serde_json::from_str(&good_answer(49, 18)).unwrap();
    v["professional_path"] = json!(
        "Выстраивает работу так, чтобы в центре соблюдался чёткий распорядок. Такой подход \
         позволяет поддерживать стабильность и predictability в ежедневной жизни центра, \
         а пациентам — понимать, что их ждёт завтра."
    );

    match g.accept_text(&row, &v.to_string()).unwrap_err() {
        RejectReason::Forbidden(msg) => assert!(msg.contains("predictability"), "{msg}"),
        other => panic!("латиница не поймана: {other}"),
    }
}

/// Разные роли с одним сидом не должны получать одних и тех же «родственников».
#[test]
fn different_roles_with_same_seed_get_different_names() {
    let root = root();
    let names = || synthforge_refdata::NameBook::load(root.join("reference/names-ru.json")).unwrap();

    let psy = PersonGenerator::psychologist(
        ParamModel::load(root.join("dictionaries/role-psychologist.json")).unwrap(),
    )
    .with_names(names());
    let dir = PersonGenerator::director(
        ParamModel::load(root.join("dictionaries/role-director.json")).unwrap(),
    )
    .with_names(names());

    let spec = GenSpec::new(20).seed(31);
    let surname = |r: &ParamRow| {
        r.get("full_name")
            .and_then(|v| v.as_str())
            .and_then(|s| s.split_whitespace().next())
            .map(|s| s.trim_end_matches('а').to_string())
            .unwrap_or_default()
    };

    let a: Vec<String> = psy.plan(&spec).unwrap().iter().map(surname).collect();
    let b: Vec<String> = dir.plan(&spec).unwrap().iter().map(surname).collect();

    let same = a.iter().zip(&b).filter(|(x, y)| x == y).count();
    assert!(same <= 2, "у {same} из 20 пар совпала фамилия — роли делят генератор имён");
}

#[test]
fn advertising_cliches_are_rejected() {
    let g = generator();
    let row = row_with(49, 18);

    let answer = json!({
        "biography": "Ведущий специалист в области наркологии, за плечами которого \
            множество непростых случаев. Окончил медицинский университет и с тех пор \
            занимается лечением зависимостей, уделяя внимание каждому обратившемуся. \
            Пациенты отмечают внимательность и готовность разбираться в деталях.",
        "professional_path": "Работал в государственной клинике, затем перешёл в частную \
            практику. Сочетает медикаментозную поддержку с длительным сопровождением, \
            считая, что одно без другого работает плохо и даёт лишь временный результат.",
        "quote": "Каждый случай уникален, и подходить к нему нужно индивидуально.",
        "stated_age": 49,
        "stated_experience_years": 18
    })
    .to_string();

    match g.accept_text(&row, &answer).unwrap_err() {
        RejectReason::Forbidden(w) => assert_eq!(w, "ведущий специалист"),
        other => panic!("штамп не пойман: {other}"),
    }
}

#[test]
fn short_and_malformed_answers_are_rejected() {
    let g = generator();
    let row = row_with(49, 18);

    let short = json!({
        "biography": "Врач.",
        "professional_path": "Работает.",
        "quote": "Лечу.",
        "stated_age": 49,
        "stated_experience_years": 18
    })
    .to_string();
    assert!(matches!(
        g.accept_text(&row, &short).unwrap_err(),
        RejectReason::TooShort { .. }
    ));

    assert!(matches!(
        g.accept_text(&row, "Извините, не могу выполнить").unwrap_err(),
        RejectReason::NotStructured(_)
    ));

    let missing = json!({"biography": "x".repeat(200)}).to_string();
    assert!(matches!(
        g.accept_text(&row, &missing).unwrap_err(),
        RejectReason::MissingField(_)
    ));
}

/// Модель любит оборачивать JSON в тройные кавычки — это не повод отбраковывать.
#[test]
fn markdown_fenced_json_is_accepted() {
    let g = generator();
    let row = row_with(49, 18);
    let fenced = format!("```json\n{}\n```", good_answer(49, 18));
    assert!(g.accept_text(&row, &fenced).is_ok());
}

#[test]
fn assembled_record_carries_params_and_texts_but_not_echo() {
    let g = generator();
    let row = row_with(49, 18);
    let accepted = g.accept_text(&row, &good_answer(49, 18)).unwrap();
    let record = g.assemble(&row, &accepted);

    let obj = record.as_object().unwrap();

    assert_eq!(obj["age"], 49);
    assert_eq!(obj["experience_years"], 18);
    assert!(obj.contains_key("specialty"), "параметры должны попасть в запись");
    assert!(obj.contains_key("biography"));
    assert_eq!(obj["entity_kind"], "doctor");

    assert!(
        !obj.contains_key("stated_age"),
        "служебное эхо не должно уходить в хранилище"
    );
    assert!(
        !obj.contains_key("biography_null_check") && !obj.values().any(|v| v.is_null()),
        "пустые значения в записи не нужны"
    );
}

/// Имя приходит из справочника, а не от модели: согласовано с полом и годом
/// рождения, и воспроизводимо по сиду.
#[test]
fn names_come_from_the_reference_book() {
    let g = generator();
    let spec = GenSpec::new(120).seed(4242);
    let rows = g.plan(&spec).unwrap();

    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        let name = row
            .get("full_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        assert!(!name.is_empty(), "ФИО не проставлено");
        assert_eq!(name.split_whitespace().count(), 3, "ожидались фамилия, имя, отчество: {name}");

        let female = row.get("gender").and_then(|v| v.as_str()) == Some("женский");
        let patronymic = name.split_whitespace().nth(2).unwrap();
        if female {
            assert!(patronymic.ends_with("на"), "женское отчество в мужской форме: {name}");
        } else {
            assert!(patronymic.ends_with("ич"), "мужское отчество в женской форме: {name}");
        }
        seen.insert(name);
    }

    assert!(seen.len() > 110, "на 120 человек всего {} разных ФИО", seen.len());

    // Воспроизводимость: тот же сид — те же люди с теми же именами.
    assert_eq!(rows, g.plan(&spec).unwrap());
}

#[test]
fn full_name_reaches_the_prompt_as_required() {
    let g = generator();
    let row = g.plan(&GenSpec::new(1).seed(9)).unwrap().remove(0);
    let expected = row.get("full_name").unwrap().render();

    let user = g
        .text_request(&row, None)
        .messages
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(user.contains("ОБЯЗАТЕЛЬНО УПОМЯНУТЬ"), "{user}");
    assert!(user.contains(&expected), "ФИО не доехало до промпта: {user}");
}

/// Повтор полного ФИО — верный признак машинного текста, но он чинится
/// механически, поэтому не отбраковывается, а исправляется.
///
/// Первая версия отбраковывала такие ответы. На живом прогоне модель повторила
/// имя пять раз подряд для одного и того же человека — пять полных вызовов
/// впустую ради замены, которую код делает бесплатно.
#[test]
fn repeated_full_name_is_repaired_not_rejected() {
    let g = generator();
    let mut row = row_with(49, 18);
    row.set("full_name", synthforge_params::Value::Str("Иванов Пётр Сергеевич".into()));

    let answer = json!({
        "biography": "Иванов Пётр Сергеевич окончил медицинский университет и пришёл в \
            наркологию сразу после ординатуры. Первые годы работал в общепсихиатрическом \
            отделении, где впервые столкнулся с пациентами, у которых зависимость \
            накладывалась на психическое расстройство.",
        "professional_path": "Иванов Пётр Сергеевич опирается на сочетание фармакологии и \
            психотерапии. Много времени уделяет разговорам с родственниками, считая, что \
            без их участия результат оказывается временным и быстро сходит на нет.",
        "quote": "Срыв — это часть пути, а не его конец.",
        "stated_age": 49,
        "stated_experience_years": 18
    })
    .to_string();

    let accepted = g
        .accept_text(&row, &answer)
        .unwrap_or_else(|e| panic!("повтор имени должен чиниться, а не отбраковываться: {e}"));

    let bio = accepted.fields["biography"].as_str().unwrap();
    let path = accepted.fields["professional_path"].as_str().unwrap();

    assert!(bio.starts_with("Иванов Пётр Сергеевич"), "первое упоминание остаётся полным");
    assert!(
        path.starts_with("Пётр Сергеевич опирается"),
        "повтор должен стать именем-отчеством: {path}"
    );
    assert!(!path.contains("Иванов"), "фамилия в повторе осталась: {path}");
}

/// Искажённая фамилия исправляется, падежные формы — нет.
///
/// Дефект с живого прогона расширенной модели: «Сергейева Наталья Антоновна».
#[test]
fn misspelled_surname_is_repaired_but_declension_kept() {
    let g = generator();
    let mut row = row_with(49, 18);
    row.set("full_name", Value::Str("Сергеева Наталья Антоновна".into()));

    let mut v: serde_json::Value = serde_json::from_str(&good_answer(49, 18)).unwrap();
    v["biography"] = json!(
        "Сергеева Наталья Антоновна окончила медицинский университет и пришла в наркологию \
         после ординатуры. Коллеги ценят Сергееву за спокойствие, а пациенты доверяют \
         Сергеевой самые трудные разговоры о срывах и возвращении к обычной жизни."
    );
    v["professional_path"] = json!(
        "Сергейева Наталья Антоновна начинала в районной больнице, затем работала в \
         диспансере. Сейчас ведёт приём и много времени уделяет родственникам пациентов, \
         считая, что без их участия результат оказывается временным."
    );

    let a = g.accept_text(&row, &v.to_string()).unwrap_or_else(|e| panic!("{e}"));
    let bio = a.fields["biography"].as_str().unwrap();
    let path = a.fields["professional_path"].as_str().unwrap();

    assert!(!path.contains("Сергейева"), "опечатка не исправлена: {path}");
    assert!(bio.contains("Сергееву") && bio.contains("Сергеевой"), "падежи испорчены: {bio}");
}

/// Отчество, похожее на фамилию, исправитель не трогает.
#[test]
fn patronymic_similar_to_surname_is_left_alone() {
    let g = generator();
    let mut row = row_with(49, 18);
    row.set("full_name", Value::Str("Сергеева Наталья Сергеевна".into()));

    let mut v: serde_json::Value = serde_json::from_str(&good_answer(49, 18)).unwrap();
    v["professional_path"] = json!(
        "Наталья Сергеевна начинала в районной больнице, затем работала в диспансере. \
         Сейчас ведёт приём и много времени уделяет родственникам пациентов, считая, \
         что без их участия результат оказывается временным."
    );

    let a = g.accept_text(&row, &v.to_string()).unwrap_or_else(|e| panic!("{e}"));
    let path = a.fields["professional_path"].as_str().unwrap();
    assert!(path.starts_with("Наталья Сергеевна"), "отчество испорчено: {path}");
}

/// У консультанта срок трезвости — законная длительность. Первая версия
/// проверки знала только стаж и отбраковывала верные тексты.
#[test]
fn consultant_clean_years_are_a_legitimate_duration() {
    let root = root();
    let model = ParamModel::load(root.join("dictionaries/role-consultant.json")).unwrap();
    let g = PersonGenerator::consultant(model);

    let mut row = g.plan(&GenSpec::new(1).seed(3)).unwrap().remove(0);
    row.set("age", Value::Int(68));
    row.set("experience_years", Value::Int(10));
    row.set("clean_years", Value::Int(13));

    let answer = json!({
        "biography": "Прошёл программу и тринадцать лет живёт трезво. Сначала вернулся в центр \
            волонтёром, помогал на кухне и в хозяйственных делах, потом стал вести группы \
            для новичков. Считает, что без честности с самим собой ничего не держится.",
        "professional_path": "Последние годы работает с теми, кто только пришёл в программу и \
            ещё не верит, что трезвость возможна. Много времени проводит в группах, \
            рассказывая о собственном пути без прикрас и без нравоучений.",
        "quote": "Трезвость начинается с того дня, когда перестаёшь врать себе.",
        "stated_age": 68,
        "stated_experience_years": 10
    })
    .to_string();

    assert!(
        g.accept_text(&row, &answer).is_ok(),
        "«тринадцать лет трезвости» при сроке 13 — не расхождение"
    );
}

#[test]
fn natural_keys_are_stable_and_unique() {
    let g = generator();
    let a = g.natural_key("session-1", 7);
    assert_eq!(a, g.natural_key("session-1", 7), "ключ обязан быть устойчивым");
    assert_ne!(a, g.natural_key("session-1", 8));
    assert_ne!(a, g.natural_key("session-2", 7));
}

#[test]
fn cohort_shares_survive_through_the_generator() {
    let g = generator();
    let spec = GenSpec::new(200)
        .seed(11)
        .cohort(
            synthforge_params::Cohort::new("опытные", 0.2).with(synthforge_params::Bias::Range {
                param: "experience_years".into(),
                min: Some(20.0),
                max: None,
            }),
        )
        .cohort(synthforge_params::Cohort::new("остальные", 0.8));

    let rows = g.plan(&spec).unwrap();
    assert_eq!(rows.len(), 200);

    let experienced = rows
        .iter()
        .filter(|r| r.get("experience_years").and_then(|v| v.as_i64()).unwrap_or(0) >= 20)
        .count();
    assert!(
        experienced >= 40,
        "20% когорта должна дать минимум 40 человек со стажем от 20 лет, получено {experienced}"
    );
}

#[test]
fn prompt_block_is_not_empty_for_either_purpose() {
    let g = generator();
    let row = g.plan(&GenSpec::new(1)).unwrap().remove(0);
    assert!(!row.prompt_block(g.model(), Usage::Text).is_empty());
    assert!(!row.prompt_block(g.model(), Usage::Visual).is_empty());
}

/// Врач v2 с известными возрастом, стажем и возрастом выпуска.
fn doctor_v2_row(age: i64, experience: i64, graduation_age: i64) -> (PersonGenerator, ParamRow) {
    let model = ParamModel::load(root().join("dictionaries/role-doctor-v2.json")).unwrap();
    let g = PersonGenerator::doctor(model);
    let mut row = g.plan(&GenSpec::new(1).seed(8)).unwrap().remove(0);
    row.set("age", Value::Int(age));
    row.set("experience_years", Value::Int(experience));
    row.set("graduation_age", Value::Int(graduation_age));
    (g, row)
}

fn doctor_answer(bio: &str, age: i64, experience: i64) -> String {
    json!({
        "biography": bio,
        "professional_path": "Начинал в государственной клинике, где вёл приём и дежурства. \
            Постепенно сосредоточился на работе с длительными программами, а не только на \
            снятии острых состояний. Считает, что без участия семьи результат почти всегда \
            оказывается временным, поэтому много времени уделяет разговорам с родственниками.",
        "quote": "Срыв — это не конец работы, а её часть. Важно, чтобы человек вернулся.",
        "stated_age": age,
        "stated_experience_years": experience
    })
    .to_string()
}

/// «В 24 года окончил» — возраст до начала стажа, но в пределах описанной
/// жизни. Раньше такой верный текст отбраковывался как расхождение.
#[test]
fn age_at_graduation_is_not_a_drift() {
    let (g, row) = doctor_v2_row(50, 18, 24);
    let bio = "Вырос в небольшом городе и в 24 года окончил медицинский университет. \
        Ординатуру проходил по психиатрии, там впервые увидел, как зависимость ломает \
        жизнь не только пациенту, но и всей его семье. С тех пор работает в наркологии \
        и не жалеет о выборе, хотя путь оказался длиннее, чем он думал.";
    let r = g.accept_text(&row, &doctor_answer(bio, 50, 18));
    assert!(r.is_ok(), "{r:?}");
}

/// Но преувеличенный стаж по-прежнему ловится: без предлога «в» число —
/// срок, и 25 при стаже 18 ничем не объясняется.
#[test]
fn inflated_experience_is_still_caught() {
    let (g, row) = doctor_v2_row(50, 18, 24);
    let bio = "Вырос в небольшом городе, окончил медицинский университет. За 25 лет \
        работы в наркологии видел самые разные истории и научился не делать поспешных \
        выводов. Ординатуру проходил по психиатрии и до сих пор считает её лучшей школой \
        для врача, который хочет работать с зависимостями.";
    assert!(matches!(
        g.accept_text(&row, &doctor_answer(bio, 50, 18)),
        Err(RejectReason::FactDrift { .. })
    ));
}

/// Календарный год проверяется по годам жизни: выпуск до рождения — ошибка,
/// год выпуска в пределах жизни — нет.
#[test]
fn calendar_years_must_fall_within_life() {
    let (g, row) = doctor_v2_row(40, 12, 24);
    let bio_ok = "Окончил медицинский университет в областном центре, выпуск 2010 года. \
        Ординатуру проходил по психиатрии, там впервые увидел, как зависимость ломает \
        жизнь не только пациенту, но и всей его семье. С тех пор работает в наркологии \
        и не жалеет о выборе, хотя путь оказался длиннее, чем он думал.";
    let r = g.accept_text(&row, &doctor_answer(bio_ok, 40, 12));
    assert!(r.is_ok(), "{r:?}");

    let bio_bad = bio_ok.replace("2010", "1975");
    assert!(matches!(
        g.accept_text(&row, &doctor_answer(&bio_bad, 40, 12)),
        Err(RejectReason::FactDrift { .. })
    ));
}

/// Руководитель v2: годы в прежней профессии — законная длительность, хотя в
/// стаж в сфере они не входят.
#[test]
fn director_v2_years_before_field_are_legitimate() {
    let model = ParamModel::load(root().join("dictionaries/role-director-v2.json")).unwrap();
    let g = PersonGenerator::director(model);
    let mut row = g.plan(&GenSpec::new(1).seed(4)).unwrap().remove(0);
    row.set("age", Value::Int(52));
    row.set("field_years", Value::Int(9));
    row.set("leading_years", Value::Int(6));
    row.set("before_field_years", Value::Int(14));
    row.set("start_work_age", Value::Int(22));

    let answer = json!({
        "biography": "Четырнадцать лет проработал в торговле и дошёл до руководителя \
            регионального отделения. В 22 года начинал простым продавцом и привык, что \
            любое дело держится на людях и порядке. В реабилитацию пришёл, когда увидел, \
            как мало в регионе мест, где помогают по-честному, без громких обещаний.",
        "professional_path": "Шесть лет руководит центром. Первым делом выстроил понятные \
            правила для семей: что происходит с человеком на каждом этапе и чего ждать \
            после выписки. Сам в группы не ходит, но знает каждого консультанта и держит \
            команду вместе, когда становится трудно.",
        "quote": "Семья должна понимать, за что платит и чего ждать. Без этого нет доверия.",
        "stated_age": 52
    })
    .to_string();

    let r = g.accept_text(&row, &answer);
    assert!(r.is_ok(), "{r:?}");
}
