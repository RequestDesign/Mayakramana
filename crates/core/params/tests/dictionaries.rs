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

/// Черновик врача v2: стаж — сумма этапов карьеры, год выпуска — от возраста,
/// дыр в карьере больше пяти лет нет.
#[test]
fn doctor_v2_career_adds_up() {
    let m = ParamModel::load(dict("role-doctor-v2.json")).unwrap_or_else(|e| panic!("{e}"));
    let pop = Sampler::new(&m, 2027).sample_population(&PopulationPlan::uniform(400)).unwrap();

    let i = |r: &synthforge_params::ParamRow, k: &str| r.get(k).and_then(|v| v.as_i64()).unwrap_or(-1);

    for s in &pop {
        let r = &s.row;
        let stages = i(r, "career_1_years") + i(r, "career_2_years") + i(r, "career_3_years")
            + i(r, "current_years");
        assert_eq!(i(r, "experience_years"), stages, "стаж не равен сумме этапов");

        assert_eq!(
            i(r, "graduation_year"),
            2026 - i(r, "age") + i(r, "graduation_age"),
            "год выпуска не сходится с возрастом"
        );

        let since_grad = i(r, "age") - i(r, "graduation_age") - 2;
        let gap = since_grad - i(r, "experience_years");
        assert!((0..=5).contains(&gap), "дыра в карьере {gap} лет");

        let places = 2
            + (r.get("career_2_type").unwrap().as_str() != Some("нет")) as i64
            + (r.get("career_3_type").unwrap().as_str() != Some("нет")) as i64;
        assert_eq!(i(r, "workplaces_count"), places);
    }
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

fn population(file: &str, seed: u64) -> Vec<synthforge_params::ParamRow> {
    let m = ParamModel::load(dict(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
    Sampler::new(&m, seed)
        .sample_population(&PopulationPlan::uniform(400))
        .unwrap_or_else(|e| panic!("{file}: {e}"))
        .into_iter()
        .map(|s| s.row)
        .collect()
}

fn int(r: &synthforge_params::ParamRow, k: &str) -> i64 {
    r.get(k).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("нет «{k}»"))
}

fn text<'a>(r: &'a synthforge_params::ParamRow, k: &str) -> &'a str {
    r.get(k).and_then(|v| v.as_str()).unwrap_or_else(|| panic!("нет «{k}»"))
}

/// Консультант v2: стаж — сумма мест работы; у выздоравливающего работа и
/// волонтёрство помещаются в срок трезвости, а употребление — в жизнь до неё.
#[test]
fn consultant_v2_story_adds_up() {
    let recovering = "выздоравливающий, прошёл программу";
    let mut seen_recovering = 0;
    for r in population("role-consultant-v2.json", 31) {
        assert_eq!(int(&r, "experience_years"), int(&r, "prior_years") + int(&r, "current_years"));
        assert_eq!(int(&r, "sobriety_age"), int(&r, "age") - int(&r, "clean_years"));

        if text(&r, "recovery_background") == recovering {
            seen_recovering += 1;
            assert!(int(&r, "experience_years") + int(&r, "volunteer_years") <= int(&r, "clean_years"));
            assert!(int(&r, "sobriety_age") - int(&r, "use_years") >= 14);
            assert_ne!(text(&r, "substance"), "нет");
        } else {
            assert_eq!(int(&r, "clean_years"), 0);
            assert_eq!(text(&r, "substance"), "нет");
            assert_eq!(int(&r, "use_years"), 0);
        }
    }
    assert!(seen_recovering > 150, "выздоравливающих всего {seen_recovering} из 400");
}

/// Психолог v2: стаж — сумма этапов, стаж в зависимостях — его часть,
/// прежняя профессия есть ровно у прошедших переподготовку.
#[test]
fn psychologist_v2_career_adds_up() {
    let retrained = "переподготовка на психолога после другой профессии";
    for r in population("role-psychologist-v2.json", 32) {
        let exp = int(&r, "experience_years");
        assert_eq!(exp, int(&r, "first_years") + int(&r, "second_years") + int(&r, "current_years"));
        assert!(int(&r, "addiction_years") >= int(&r, "current_years"));
        assert!(int(&r, "addiction_years") <= exp);
        assert_eq!(int(&r, "graduation_year"), 2026 - int(&r, "age") + int(&r, "graduation_age"));

        let since = int(&r, "age") - int(&r, "graduation_age");
        assert!((0..=6).contains(&(since - exp)), "дыра {} лет", since - exp);
        assert_eq!(text(&r, "education") == retrained, text(&r, "prior_profession") != "нет");
    }
}

/// Руководитель v2: жизнь складывается без остатка — начало работы, прежняя
/// профессия, перерыв и годы в сфере дают ровно возраст; у выздоравливающего
/// трезвость покрывает все годы в сфере.
#[test]
fn director_v2_life_adds_up() {
    let recovering = "выздоравливающий, основавший центр после своего пути";
    let mut founders = 0;
    for r in population("role-director-v2.json", 33) {
        assert_eq!(
            int(&r, "start_work_age") + int(&r, "before_field_years") + int(&r, "career_gap_years")
                + int(&r, "field_years"),
            int(&r, "age")
        );
        assert!(int(&r, "leading_years") <= int(&r, "field_years"));
        assert!(int(&r, "age") >= 30);

        if text(&r, "background") == recovering {
            assert!(int(&r, "clean_years") >= int(&r, "field_years"));
            assert!(int(&r, "clean_years") <= int(&r, "age") - 18);
        } else {
            assert_eq!(int(&r, "clean_years"), 0);
        }
        if text(&r, "background") == "врач, открывший свой центр" {
            assert_eq!(text(&r, "education"), "медицинское");
        }
        let founder = r.get("founder").and_then(|v| v.as_bool()).unwrap();
        founders += founder as usize;
        assert_eq!(founder, int(&r, "centers_opened") >= 1);
    }
    assert!((120..=320).contains(&founders), "основателей {founders} из 400");
}
