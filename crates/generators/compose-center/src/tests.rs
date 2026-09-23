//! Сборка центра на синтетических пулах: без сети и без модели.

use std::collections::HashSet;

use serde_json::json;

use super::*;

fn person(key: &str, age: i64, extra: serde_json::Value) -> Candidate {
    // У каждого своя фамилия: правило «без однофамильцев в команде» иначе не
    // дало бы собрать ни одного центра.
    let mut r = json!({"full_name": format!("Фамилия{key} Имя Отчество"), "age": age, "home_region": "Поволжье"});
    for (k, v) in extra.as_object().unwrap() {
        r[k] = v.clone();
    }
    Candidate::new(key, r)
}

/// Пулы, в которых специализированных людей заведомо меньше, чем обычных:
/// так видно, что отбор действительно тянет подходящих, а не берёт подряд.
fn pools(n_centers: usize) -> Pools {
    let mut p = Pools::default();

    for i in 0..n_centers * 2 {
        let segment = ["эконом", "средний", "премиум"][i % 3];
        p.places.push(Candidate::new(
            format!("place:{i}"),
            json!({"segment": segment, "region": "Поволжье", "capacity": 12 + (i as i64 % 3) * 8,
                   "setting": "сосновый лес", "building_type": "частный коттедж",
                   "room_type": "комнаты на 2-3 человека", "amenities": ["комната для групповых занятий"],
                   "distance_km": 40}),
        ));
        let focus = ["двойной диагноз", "подростки и молодёжь", "алкогольная зависимость"][i % 3];
        p.programs.push(Candidate::new(
            format!("program:{i}"),
            json!({"segment": segment, "focus": focus, "title": format!("Программа {i}"),
                   "approach": "миннесотская модель", "duration": "28 дней"}),
        ));
    }

    for i in 0..n_centers * 3 {
        p.directors.push(person(&format!("dir:{i}"), 40 + (i as i64 % 20), json!({
            "background": "врач, открывший свой центр", "motivation": "увидел, как мало хороших мест",
            "founder": true
        })));
    }

    for i in 0..n_centers * 8 {
        let teen = i % 5 == 0;
        let dual = i % 4 == 0;
        p.doctors.push(person(&format!("doc:{i}"), 35 + (i as i64 % 25), json!({
            "specialty": "психиатр-нарколог",
            "practice": if dual { json!(["двойной диагноз", "наркозависимые"]) }
                        else if teen { json!(["подростковая зависимость"]) }
                        else { json!(["алкогольная зависимость"]) },
            "psychiatry_experience": dual,
            "patient_age_focus": if teen { "подростки и молодёжь" } else { "взрослые" }
        })));
    }

    for i in 0..n_centers * 8 {
        p.psychologists.push(person(&format!("psy:{i}"), 30 + (i as i64 % 25), json!({
            "education": "клиническая психология, специалитет",
            "client_focus": if i % 4 == 0 { "подростки и молодёжь" } else { "взрослые" },
            "methods": ["когнитивно-поведенческая терапия"],
            "diagnostics": i % 2 == 0
        })));
    }

    for i in 0..n_centers * 16 {
        p.consultants.push(person(&format!("con:{i}"), 30 + (i as i64 % 25), json!({
            "path_to_work": "через сообщество, остался помогать",
            "substance": if i % 3 == 0 { "алкоголь" } else { "опиаты" },
            "focus": if i % 4 == 0 { json!(["молодые"]) } else { json!(["новички в программе"]) }
        })));
    }

    p
}

#[test]
fn nobody_works_in_two_centers() {
    let centers = compose(&pools(6), 6, 1).unwrap();
    assert_eq!(centers.len(), 6);

    let mut seen = HashSet::new();
    for c in &centers {
        for key in c.staff().chain([&c.director, &c.place, &c.program]) {
            assert!(seen.insert(key.clone()), "«{key}» использован дважды");
        }
    }
}

#[test]
fn program_matches_place_price_segment() {
    let p = pools(6);
    let seg = |pool: &[Candidate], key: &str| {
        pool.iter().find(|c| c.key == key).unwrap().record["segment"].as_str().unwrap().to_string()
    };
    for c in compose(&p, 6, 2).unwrap() {
        assert_eq!(
            seg(&p.places, &c.place),
            seg(&p.programs, &c.program),
            "программа и здание разного ценового уровня"
        );
    }
}

#[test]
fn dual_diagnosis_center_has_psychiatric_experience() {
    let p = pools(6);
    for c in compose(&p, 6, 3).unwrap().iter().filter(|c| c.focus == "двойной диагноз") {
        let any = c.doctors.iter().any(|k| {
            p.doctors.iter().find(|d| &d.key == k).unwrap().record["psychiatry_experience"]
                .as_bool()
                .unwrap()
        });
        assert!(any, "центр двойного диагноза без психиатрического опыта в команде");
    }
}

#[test]
fn director_is_not_much_younger_than_the_team() {
    let p = pools(6);
    let age = |key: &str| {
        p.directors
            .iter()
            .chain(&p.doctors)
            .chain(&p.psychologists)
            .chain(&p.consultants)
            .find(|c| c.key == key)
            .unwrap()
            .age()
    };

    for c in compose(&p, 6, 4).unwrap() {
        let mut ages: Vec<i64> = c.staff().map(|k| age(k)).collect();
        ages.sort_unstable();
        let median = ages[ages.len() / 2];
        let d = age(&c.director);
        assert!(d >= 35, "руководитель моложе 35");
        assert!(d + 8 >= median, "руководитель {d} при медиане команды {median}");
    }
}

/// Отбор должен тянуть подходящих: в подростковом центре доля людей с опытом
/// работы с подростками заметно выше, чем в пулах в среднем.
#[test]
fn teen_center_pulls_teen_specialists() {
    let p = pools(9);
    let teen: Vec<_> = compose(&p, 9, 5)
        .unwrap()
        .into_iter()
        .filter(|c| c.focus == "подростки и молодёжь")
        .collect();
    assert!(!teen.is_empty());

    for c in &teen {
        let hits = c
            .doctors
            .iter()
            .filter(|k| {
                p.doctors.iter().find(|d| &d.key == *k).unwrap().record["patient_age_focus"]
                    == "подростки и молодёжь"
            })
            .count();
        assert!(hits >= 1, "в подростковом центре нет врача, работающего с подростками");
        assert!(c.fit > 0.5, "соответствие команды {:.2} не выше случайного", c.fit);
    }
}

/// Однофамильцы в одной команде читаются как семейный подряд — дефект с
/// живого прогона, где в центр попали двое Морозовых.
#[test]
fn no_namesakes_within_one_center() {
    let mut p = pools(4);
    // Нарочно делаем половину людей однофамильцами.
    for (i, c) in p
        .doctors
        .iter_mut()
        .chain(p.psychologists.iter_mut())
        .chain(p.consultants.iter_mut())
        .enumerate()
    {
        if i % 2 == 0 {
            let name = if i % 4 == 0 { "Морозов Иван Петрович" } else { "Морозова Анна Петровна" };
            c.record["full_name"] = json!(name);
        }
    }

    for center in compose(&p, 4, 8).unwrap() {
        let all = [&p.directors, &p.doctors, &p.psychologists, &p.consultants];
        let roots: Vec<String> = center
            .staff()
            .chain([&center.director])
            .map(|k| {
                let c = all.iter().flat_map(|v| v.iter()).find(|c| &c.key == k).unwrap();
                compose::surname_root(c)
            })
            .collect();
        let uniq: HashSet<_> = roots.iter().collect();
        assert_eq!(uniq.len(), roots.len(), "однофамильцы в одном центре: {roots:?}");
    }
}

#[test]
fn surname_root_joins_gender_forms() {
    let m = Candidate::new("a", json!({"full_name": "Морозов Иван Петрович"}));
    let f = Candidate::new("b", json!({"full_name": "Морозова Анна Петровна"}));
    let g = Candidate::new("c", json!({"full_name": "Дубровская Анна Петровна"}));
    let h = Candidate::new("d", json!({"full_name": "Дубровский Иван Петрович"}));
    assert_eq!(compose::surname_root(&m), compose::surname_root(&f));
    assert_eq!(compose::surname_root(&g), compose::surname_root(&h));
}

/// Второй запуск сборки не должен раздавать людей, уже работающих в центрах.
#[test]
fn second_run_does_not_reuse_people_from_first() {
    let mut p = pools(6);
    let first = compose(&p, 3, 11).unwrap();

    for c in &first {
        p.reserved.extend(c.staff().cloned());
        p.reserved.insert(c.director.clone());
        p.reserved.insert(c.place.clone());
        p.reserved.insert(c.program.clone());
    }

    let second = compose(&p, 3, 12).unwrap();
    for c in &second {
        for k in c.staff().chain([&c.director, &c.place, &c.program]) {
            assert!(!p.reserved.contains(k), "«{k}» уже занят в центре первого запуска");
        }
    }
}

/// Центр химической зависимости без нарколога — дефект с живого прогона.
#[test]
fn chemical_dependency_center_has_a_narcologist() {
    let mut p = pools(6);
    // Половина врачей — психотерапевты, чтобы было из чего ошибиться.
    for (i, d) in p.doctors.iter_mut().enumerate() {
        if i % 2 == 1 {
            d.record["specialty"] = json!("психотерапевт");
        }
    }

    for c in compose(&p, 6, 13).unwrap() {
        if compose::needs_narcologist(&c.focus) {
            let ok = c.doctors.iter().any(|k| {
                compose::is_narcologist(p.doctors.iter().find(|d| &d.key == k).unwrap())
            });
            assert!(ok, "центр «{}» без нарколога", c.focus);
        }
    }
}

/// Справочник названий сам проходит проверку, которую не прошли названия
/// от модели.
#[test]
fn reference_center_names_pass_the_name_check() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../reference/center-names.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let names: Vec<&str> = v["names"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect();

    assert!(names.len() >= 50, "названий мало: {}", names.len());
    let uniq: HashSet<_> = names.iter().collect();
    assert_eq!(uniq.len(), names.len(), "в справочнике названий есть повторы");
    for n in &names {
        check_center_name(n).unwrap_or_else(|e| panic!("{e}"));
    }
}

#[test]
fn every_center_gets_a_distinct_name_from_the_reference() {
    let p = pools(4);
    let model_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../dictionaries/object-center.json");
    let names = vec!["Берег".to_string(), "Маяк".into(), "Исток".into(), "Опора".into(), "Рассвет".into()];
    let mut pools_reserved = p.clone();
    pools_reserved.reserved.insert("name:Маяк".into());

    let g = CenterGenerator::new(ParamModel::load(&model_path).unwrap(), pools_reserved)
        .with_names(names);
    let rows = g.plan(&GenSpec::new(4).seed(3)).unwrap();

    let got: Vec<String> = rows.iter().map(|r| r.get("center_name").unwrap().render()).collect();
    let uniq: HashSet<_> = got.iter().collect();
    assert_eq!(uniq.len(), 4, "названия повторились: {got:?}");
    assert!(!got.contains(&"Маяк".to_string()), "занятое название выдано повторно");
}

#[test]
fn center_names_are_checked() {
    assert!(check_center_name("Твой шанс").is_ok());
    assert!(check_center_name("Берег").is_ok());
    assert!(check_center_name("Точка опоры").is_ok());

    // Ровно то, что выдал первый живой прогон.
    assert!(check_center_name("Реабилитационный уральский центр двенадцатишагов").is_err());
    assert!(check_center_name("Центр Возрождение").is_err());
    assert!(check_center_name("Элитный берег").is_err());
    assert!(check_center_name("Рассвет 24").is_err());
}

#[test]
fn team_scales_with_capacity() {
    let small = team_size(10);
    let large = team_size(50);
    assert!(large.0 >= small.0 && large.1 >= small.1 && large.2 > small.2);
    assert_eq!(small, (1, 1, 3));
}

#[test]
fn composition_is_reproducible() {
    let p = pools(5);
    let a = compose(&p, 5, 42).unwrap();
    let b = compose(&p, 5, 42).unwrap();
    let keys = |v: &[CenterPlan]| v.iter().map(|c| c.director.clone()).collect::<Vec<_>>();
    assert_eq!(keys(&a), keys(&b));
}

/// Когда людей не хватает, ошибка называет, кого именно.
#[test]
fn exhaustion_names_the_scarce_pool() {
    let mut p = pools(4);
    p.consultants.truncate(3);

    let err = compose(&p, 4, 6).unwrap_err().to_string();
    assert!(err.contains("консультанты"), "{err}");
}

#[test]
fn center_row_references_real_people() {
    let p = pools(3);
    let model_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../dictionaries/object-center.json");
    let g = CenterGenerator::new(ParamModel::load(model_path).unwrap_or_else(|e| panic!("{e}")), p);

    let rows = g.plan(&GenSpec::new(3).seed(7)).unwrap();
    assert_eq!(rows.len(), 3);

    let r = &rows[0];
    let director = r.get("director_name").unwrap().render();
    assert!(director.starts_with("Фамилияdir:"), "{director}");

    let prompt = g
        .text_request(r, None)
        .messages
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains(&director), "руководитель не доехал до промпта");
    assert!(prompt.contains("врачи:"), "команда не доехала до промпта");
    assert!(!prompt.contains("place:"), "служебные ключи попали в промпт");
}
