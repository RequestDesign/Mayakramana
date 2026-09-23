//! Проверка настоящих словарей из каталога `dictionaries/`.
//!
//! Юнит-тесты проверяют механизм на игрушечных данных. Здесь проверяется то,
//! что реально пойдёт в генерацию: словарь врача со всем наследованием.

use std::path::PathBuf;

use synthforge_params::{Cohort, ParamModel, PopulationPlan, Sampler, Usage};

fn dict(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../dictionaries")
        .join(name)
}

fn doctor() -> ParamModel {
    ParamModel::load(dict("role-doctor.json")).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn doctor_dictionary_is_valid() {
    let m = doctor();
    assert!(m.issues().is_empty(), "{:#?}", m.issues());
}

#[test]
fn inheritance_puts_base_params_first() {
    let m = doctor();

    // Общечеловеческие идут раньше внешности, внешность — раньше ролевых.
    // От этого зависит, сработают ли корреляции: условие читает уже
    // сэмплированные значения.
    let age = m.index_of("age").expect("возраст из human-core");
    let hair = m.index_of("hair_color").expect("волосы из human-visual");
    let spec = m.index_of("specialty").expect("специальность из role-doctor");

    assert!(age < hair, "human-core должен подмешиваться раньше human-visual");
    assert!(hair < spec, "общие словари должны идти раньше ролевого");
}

#[test]
fn population_satisfies_every_hard_rule() {
    let m = doctor();
    let mut s = Sampler::new(&m, 2026);
    let pop = s
        .sample_population(&PopulationPlan::uniform(500))
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(pop.len(), 500);

    for sampled in &pop {
        for rule in &m.hard {
            assert!(
                rule.holds(&sampled.row),
                "нарушено «{}»: {}\nстрока: {:#?}",
                rule.name,
                rule.explain(),
                sampled.row
            );
        }
    }
}

#[test]
fn identity_signatures_are_unique_across_population() {
    let m = doctor();
    let mut s = Sampler::new(&m, 77);
    let pop = s.sample_population(&PopulationPlan::uniform(500)).unwrap();

    let mut sigs: Vec<String> = pop.iter().map(|x| x.row.signature(&m)).collect();
    sigs.sort();
    let before = sigs.len();
    sigs.dedup();
    assert_eq!(before, sigs.len(), "в популяции нашлись совпадающие комбинации");
}

/// Пример из постановки: многодетность повышает шанс подростковой практики.
#[test]
fn many_children_shift_practice_to_teenagers() {
    let m = doctor();

    let with_kids = share_of_teen_focus(&m, Some(3.0), 900);
    let baseline = share_of_teen_focus(&m, None, 900);

    assert!(
        with_kids > baseline * 2.0,
        "корреляция не работает: многодетные {with_kids:.3}, база {baseline:.3}"
    );
}

fn share_of_teen_focus(m: &ParamModel, min_children: Option<f64>, n: usize) -> f64 {
    let mut cohort = Cohort::new("проба", 1.0);
    if let Some(min) = min_children {
        cohort = cohort.with(synthforge_params::Bias::Range {
            param: "children_count".into(),
            min: Some(min),
            max: None,
        });
    }
    let plan = PopulationPlan { count: n, cohorts: vec![cohort] };
    let pop = Sampler::new(m, 31).sample_population(&plan).unwrap();

    let hits = pop
        .iter()
        .filter(|x| {
            x.row.get("patient_age_focus").and_then(|v| v.as_str()) == Some("подростки и молодёжь")
        })
        .count();
    hits as f64 / pop.len() as f64
}

#[test]
fn women_never_get_beards() {
    let m = doctor();
    let mut s = Sampler::new(&m, 9);
    let pop = s.sample_population(&PopulationPlan::uniform(600)).unwrap();

    for x in &pop {
        if x.row.get("gender").and_then(|v| v.as_str()) == Some("женский") {
            let fh = x.row.get("facial_hair").and_then(|v| v.as_str()).unwrap_or("нет");
            assert_eq!(fh, "нет", "жёсткая подстановка не сработала: {fh}");
        }
    }
}

/// Вылизанность — главная претензия к сгенерированным портретам. Проверяем,
/// что словарь действительно раздаёт разнообразие, а не сваливается в один вид.
#[test]
fn visual_params_are_actually_varied() {
    let m = doctor();
    let mut s = Sampler::new(&m, 4242);
    let pop = s.sample_population(&PopulationPlan::uniform(400)).unwrap();

    for key in ["expression", "photo_setting", "photo_quality", "face_asymmetry"] {
        let mut seen: Vec<&str> = pop
            .iter()
            .filter_map(|x| x.row.get(key).and_then(|v| v.as_str()))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        assert!(
            seen.len() >= 3,
            "«{key}» почти не варьируется: всего {} значений на 400 портретов",
            seen.len()
        );

        // И ни один вариант не должен съедать больше двух третей выборки.
        for v in &seen {
            let share = pop
                .iter()
                .filter(|x| x.row.get(key).and_then(|x| x.as_str()) == Some(v))
                .count() as f64
                / pop.len() as f64;
            assert!(share < 0.67, "«{key}» = «{v}» занимает {share:.2} выборки");
        }
    }

    // Студийная съёмка должна остаться редкой — именно она даёт вылизанность.
    let studio = pop
        .iter()
        .filter(|x| {
            x.row.get("photo_quality").and_then(|v| v.as_str()) == Some("профессиональная съёмка")
        })
        .count() as f64
        / pop.len() as f64;
    assert!(studio < 0.2, "слишком много студийных портретов: {studio:.2}");
}

/// Ложный признак — это отсутствие, а не факт. Переданный в промпт, он
/// превращается во фразу «с родственниками не работает».
#[test]
fn false_flags_never_reach_the_text_prompt() {
    let m = doctor();
    let mut row = Sampler::new(&m, 1).sample_row(&[]).unwrap();
    row.set("family_counseling", synthforge_params::Value::Bool(false));
    row.set("detox_experience", synthforge_params::Value::Bool(true));

    let text = row.prompt_block(&m, Usage::Text);
    assert!(
        !text.contains("Консультирование родственников"),
        "ложный признак ушёл в промпт:\n{text}"
    );
    assert!(text.contains("Опыт детоксикации"), "истинный признак потерялся:\n{text}");
}

/// Прийти в профессию через сообщество можно только имея личный опыт.
#[test]
fn consultant_path_is_consistent_with_background() {
    let m = ParamModel::load(dict("role-consultant.json")).unwrap_or_else(|e| panic!("{e}"));
    assert!(m.issues().is_empty(), "{:#?}", m.issues());

    let pop = Sampler::new(&m, 51)
        .sample_population(&PopulationPlan::uniform(800))
        .unwrap();

    for s in &pop {
        let bg = s.row.get("recovery_background").and_then(|v| v.as_str()).unwrap();
        let path = s.row.get("path_to_work").and_then(|v| v.as_str()).unwrap();
        if bg == "без личного опыта, пришёл через образование" {
            assert!(
                path != "через сообщество, остался помогать"
                    && path != "вернулся в центр, где проходил программу",
                "без личного опыта, но путь «{path}»"
            );
        }

        // Стаж работы не может превышать срок трезвости у выздоравливающих.
        if bg == "выздоравливающий, прошёл программу" {
            let clean = s.row.get("clean_years").and_then(|v| v.as_i64()).unwrap();
            let exp = s.row.get("experience_years").and_then(|v| v.as_i64()).unwrap();
            assert!(exp <= clean, "стаж {exp} при трезвости {clean}");
        }
    }
}

/// Каждый словарь ролей загружается, проверяется и даёт популяцию, в которой
/// выполнены все жёсткие правила. Новая роль не может попасть в работу
/// сломанной — этот тест упадёт первым.
#[test]
fn every_role_dictionary_is_valid_and_satisfiable() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../dictionaries");
    let mut roles = 0;

    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // human-* — части, подмешиваемые в роли; сами по себе не проверяются.
        if !(name.starts_with("role-") || name.starts_with("object-")) {
            continue;
        }
        roles += 1;

        let m = ParamModel::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(m.issues().is_empty(), "{name}: {:#?}", m.issues());

        // Составные сущности ничего не сэмплируют — всё берётся из готового.
        // Популяцию для них собирает не сэмплер, а сборка из пулов.
        if !m.params.iter().any(|p| p.domain.is_sampled()) {
            continue;
        }

        let pop = Sampler::new(&m, 777)
            .sample_population(&PopulationPlan::uniform(400))
            .unwrap_or_else(|e| panic!("{name}: популяция не собралась: {e}"));

        for s in &pop {
            for rule in &m.hard {
                assert!(rule.holds(&s.row), "{name}: нарушено «{}»", rule.name);
            }
        }
    }

    assert!(roles >= 6, "ожидалось не меньше шести словарей, найдено {roles}");
}

#[test]
fn prompt_blocks_do_not_leak_across_purposes() {
    let m = doctor();
    let row = Sampler::new(&m, 1).sample_row(&[]).unwrap();

    let text = row.prompt_block(&m, Usage::Text);
    let visual = row.prompt_block(&m, Usage::Visual);

    assert!(text.contains("Стаж"), "{text}");
    assert!(!text.contains("Мешки под глазами"), "внешность в текстовом промпте:\n{text}");

    assert!(visual.contains("Выражение лица"), "{visual}");
    assert!(!visual.contains("Публикаций"), "биография в промпте портрета:\n{visual}");

    // Возраст и пол нужны обоим — они помечены both.
    assert!(text.contains("Возраст") && visual.contains("Возраст"));
}
