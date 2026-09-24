//! Невидимый слой: словарь параметров, допустимые диапазоны, правила связности.
//!
//! Уникальность сущностей берётся из **комбинаторики параметров**, а не из
//! набора заготовок. Сотня-другая характеристик с заданными диапазонами даёт
//! пространство, в котором двух одинаковых личностей просто не получается.
//!
//! Слой состоит из трёх частей, и третья не менее важна первых двух:
//!
//! 1. [`ParamDef`] — какими характеристиками описывается сущность;
//! 2. [`Domain`] — какие значения допустимы у каждой;
//! 3. [`HardRule`] и [`SoftRule`] — **правила связности**.
//!
//! Без третьей части комбинаторика даёт не разнообразие, а мусор: двести
//! независимо сэмплированных параметров породят врача 25 лет с 30-летним
//! стажем. Жёсткие правила запрещают невозможное, мягкие делают правдоподобное
//! более вероятным.
//!
//! # Что здесь делает код, а что модель
//!
//! Весь этот крейт работает **до** первого обращения к языковой модели. К
//! моменту, когда [`ParamRow`] готова, уже решено, каким будет человек: возраст,
//! образование, стаж, практика. Модели остаётся изложить это текстом — она не
//! выбирает факты, она их излагает.
//!
//! # Пример
//!
//! ```no_run
//! use synthforge_params::{ParamModel, PopulationPlan, Sampler, Usage};
//!
//! let model = ParamModel::load("dictionaries/role-doctor.json")?;
//! let mut sampler = Sampler::new(&model, 42);
//!
//! // Точные доли по когортам — не «попросим модель сделать 20%».
//! let plan = PopulationPlan::uniform(300);
//! for sampled in sampler.sample_population(&plan)? {
//!     let prompt = sampled.row.prompt_block(&model, Usage::Text);
//!     // → в очередь заданий, оттуда в модель
//! }
//! # Ok::<(), synthforge_params::Error>(())
//! ```

mod error;
mod expr;
mod model;
mod param;
mod parse;
mod row;
mod rule;
mod sampler;
mod value;

pub use error::{Error, Result};
pub use expr::{ArithOp, CmpOp, Expr};
pub use model::ParamModel;
pub use param::{Domain, Mention, ParamDef, Usage, Variant};
pub use parse::parse_expr;
pub use row::ParamRow;
pub use rule::{Bias, HardRule, SoftRule};
pub use sampler::{Cohort, PopulationPlan, Sampled, Sampler};
pub use value::Value;

#[cfg(test)]
mod tests {
    use super::*;

    /// Словарь, воспроизводящий разобранный пример: врач с образованием,
    /// стажем и практикой, со связностью между параметрами.
    fn doctor_model() -> ParamModel {
        let json = r#"{
          "name": "role-doctor-test",
          "params": [
            {"key":"age","title":"Возраст","group":"базовые",
             "domain":{"kind":"int","min":28,"max":68},"identity":true},
            {"key":"gender","title":"Пол","group":"базовые",
             "domain":{"kind":"enum","variants":[
               {"value":"мужской","weight":6},{"value":"женский","weight":4}]},
             "identity":true},
            {"key":"children","title":"Детей","group":"семья",
             "domain":{"kind":"int","min":0,"max":4}},
            {"key":"experience_years","title":"Стаж","group":"опыт",
             "domain":{"kind":"int","min":1,"max":45},"identity":true},
            {"key":"specialization","title":"Специализация","group":"опыт",
             "domain":{"kind":"enum","variants":[
               {"value":"взрослая","weight":10},
               {"value":"подростковая","weight":2}]},
             "identity":true},
            {"key":"practice","title":"Практика","group":"опыт",
             "domain":{"kind":"multi_enum","min_pick":1,"max_pick":3,"variants":[
               {"value":"наркозависимые","weight":5},
               {"value":"алкоголики","weight":5},
               {"value":"игроманы","weight":2}]}},
            {"key":"build","title":"Телосложение","group":"внешность",
             "usage":"visual",
             "domain":{"kind":"enum","variants":[
               {"value":"худощавое","weight":3},{"value":"плотное","weight":3}]}},
            {"key":"bio","title":"Биография","domain":{"kind":"derived"}}
          ],
          "hard": [
            {"name":"стаж_не_больше_трудоспособного",
             "expr":"experience_years <= age - 22",
             "message":"стаж превышает возможный при данном возрасте"}
          ],
          "soft": [
            {"name":"многодетность_к_подростковой",
             "when":"children >= 3",
             "then":[{"kind":"weight","param":"specialization",
                      "value":"подростковая","factor":8.0}]}
          ]
        }"#;
        ParamModel::from_json_str(json).expect("словарь должен быть корректен")
    }

    #[test]
    fn valid_dictionary_passes_check() {
        let m = doctor_model();
        assert!(m.issues().is_empty(), "{:?}", m.issues());
    }

    #[test]
    fn hard_rules_are_always_satisfied() {
        let m = doctor_model();
        let mut s = Sampler::new(&m, 7);
        let pop = s.sample_population(&PopulationPlan::uniform(300)).unwrap();

        assert_eq!(pop.len(), 300);
        for sampled in &pop {
            let age = sampled.row.get("age").unwrap().as_i64().unwrap();
            let exp = sampled.row.get("experience_years").unwrap().as_i64().unwrap();
            assert!(
                exp <= age - 22,
                "нарушен инвариант: возраст {age}, стаж {exp}"
            );
        }
    }

    #[test]
    fn same_seed_gives_same_population() {
        let m = doctor_model();
        let a = Sampler::new(&m, 123)
            .sample_population(&PopulationPlan::uniform(50))
            .unwrap();
        let b = Sampler::new(&m, 123)
            .sample_population(&PopulationPlan::uniform(50))
            .unwrap();

        let rows_a: Vec<_> = a.iter().map(|s| s.row.clone()).collect();
        let rows_b: Vec<_> = b.iter().map(|s| s.row.clone()).collect();
        assert_eq!(rows_a, rows_b, "сид обязан давать воспроизводимость");
    }

    #[test]
    fn cohort_shares_are_exact() {
        let m = doctor_model();
        let plan = PopulationPlan {
            count: 300,
            cohorts: vec![
                Cohort::new("опытные", 0.20).with(Bias::Range {
                    param: "experience_years".into(),
                    min: Some(7.0),
                    max: None,
                }),
                Cohort::new("остальные", 0.80),
            ],
        };

        let mut s = Sampler::new(&m, 5);
        let pop = s.sample_population(&plan).unwrap();

        let experienced = pop.iter().filter(|x| x.cohort == "опытные").count();
        assert_eq!(experienced, 60, "20% от 300 должно быть ровно 60");
        assert_eq!(pop.len(), 300);

        // И сужение диапазона действительно подействовало.
        for x in pop.iter().filter(|x| x.cohort == "опытные") {
            let exp = x.row.get("experience_years").unwrap().as_i64().unwrap();
            assert!(exp >= 7, "когортное сужение не применилось: стаж {exp}");
        }
    }

    #[test]
    fn allocation_never_loses_or_invents_rows() {
        for count in [1, 7, 33, 100, 999, 1000] {
            let plan = PopulationPlan {
                count,
                cohorts: vec![
                    Cohort::new("a", 0.33),
                    Cohort::new("b", 0.33),
                    Cohort::new("c", 0.34),
                ],
            };
            let alloc = plan.allocate();
            assert_eq!(alloc.iter().sum::<usize>(), count, "потеря при count={count}");
        }
    }

    #[test]
    fn soft_correlation_shifts_distribution() {
        let m = doctor_model();

        // Форсируем многодетность — доля подростковой специализации должна
        // заметно вырасти против базовых 2 к 10.
        let plan = PopulationPlan {
            count: 400,
            cohorts: vec![Cohort::new("многодетные", 1.0).with(Bias::Range {
                param: "children".into(),
                min: Some(3.0),
                max: None,
            })],
        };
        let mut s = Sampler::new(&m, 11);
        let pop = s.sample_population(&plan).unwrap();
        let teen = pop
            .iter()
            .filter(|x| x.row.get("specialization").unwrap().as_str() == Some("подростковая"))
            .count();

        let share = teen as f64 / pop.len() as f64;
        assert!(
            share > 0.35,
            "мягкая корреляция не сработала: подростковая доля {share:.2}"
        );
    }

    #[test]
    fn prompt_block_splits_text_and_visual() {
        let m = doctor_model();
        let mut s = Sampler::new(&m, 3);
        let row = s.sample_row(&[]).unwrap();

        let text = row.prompt_block(&m, Usage::Text);
        let visual = row.prompt_block(&m, Usage::Visual);

        assert!(text.contains("Возраст"), "{text}");
        assert!(
            !text.contains("Телосложение"),
            "внешность не должна уходить в текстовый промпт:\n{text}"
        );
        assert!(visual.contains("Телосложение"), "{visual}");
        assert!(
            !visual.contains("Стаж"),
            "биографические поля не должны уходить в промпт портрета:\n{visual}"
        );
    }

    #[test]
    fn broken_dictionaries_are_rejected() {
        // Дубликат ключа.
        let dup = r#"{"name":"x","params":[
            {"key":"a","title":"A","domain":{"kind":"int","min":1,"max":2},"identity":true},
            {"key":"a","title":"A2","domain":{"kind":"int","min":1,"max":2}}]}"#;
        assert!(ParamModel::from_json_str(dup).is_err());

        // Правило ссылается на несуществующий параметр.
        let bad_ref = r#"{"name":"x","params":[
            {"key":"a","title":"A","domain":{"kind":"int","min":1,"max":2},"identity":true}],
            "hard":[{"name":"r","expr":"b > 1"}]}"#;
        assert!(ParamModel::from_json_str(bad_ref).is_err());

        // Пустой enum.
        let empty = r#"{"name":"x","params":[
            {"key":"a","title":"A","domain":{"kind":"enum","variants":[]},"identity":true}]}"#;
        assert!(ParamModel::from_json_str(empty).is_err());
    }

    /// Порядок объявления: если условие корреляции опирается на параметр,
    /// объявленный позже цели, правило молча не сработает. Это худший вид
    /// поломки — контент будет тихо хуже, и никто не заметит.
    #[test]
    fn soft_rule_with_wrong_declaration_order_is_rejected() {
        let wrong = r#"{"name":"x","params":[
            {"key":"target","title":"T","identity":true,
             "domain":{"kind":"enum","variants":[{"value":"v","weight":1}]}},
            {"key":"cond","title":"C","domain":{"kind":"int","min":0,"max":9}}],
            "soft":[{"name":"поздно","when":"cond > 5",
                     "then":[{"kind":"weight","param":"target","value":"v","factor":2.0}]}]}"#;

        let err = ParamModel::from_json_str(wrong).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("никогда не сработает"),
            "должно ловиться при загрузке, а не молча: {text}"
        );
    }

    #[test]
    fn unsatisfiable_rules_fail_loudly() {
        let impossible = r#"{"name":"x","params":[
            {"key":"a","title":"A","domain":{"kind":"int","min":1,"max":5},"identity":true}],
            "hard":[{"name":"невозможное","expr":"a > 100",
                     "message":"a должно быть больше 100"}]}"#;
        let m = ParamModel::from_json_str(impossible).unwrap();
        let mut s = Sampler::new(&m, 1);
        s.max_row_attempts = 20;

        match s.sample_row(&[]) {
            Err(Error::Unsatisfiable { rule, .. }) => {
                assert!(rule.contains("больше 100"), "{rule}");
            }
            other => panic!("ожидалась внятная ошибка, получено {other:?}"),
        }
    }

    /// Вычисляемый параметр — сумма этапов, а не случайное число рядом с ними.
    #[test]
    fn computed_param_is_derived_from_earlier_ones() {
        let json = r#"{"name":"x","params":[
            {"key":"a","title":"A","identity":true,"domain":{"kind":"int","min":1,"max":10}},
            {"key":"b_on","title":"B есть","domain":{"kind":"bool","p_true":0.5}},
            {"key":"b","title":"B","domain":{"kind":"int","min":1,"max":10}},
            {"key":"total","title":"Итого","domain":{"kind":"computed","expr":"a + b * b_on"}},
            {"key":"count","title":"Сколько","domain":{"kind":"computed","expr":"1 + b_on"}}
        ]}"#;
        let m = ParamModel::from_json_str(json).unwrap();
        // Подпись уникальности здесь — один параметр на десять значений,
        // поэтому больше восьми разных строк просить нельзя.
        let pop = Sampler::new(&m, 3).sample_population(&PopulationPlan::uniform(8)).unwrap();

        for s in pop {
            let r = &s.row;
            let a = r.get("a").unwrap().as_i64().unwrap();
            let b = r.get("b").unwrap().as_i64().unwrap();
            let on = r.get("b_on").unwrap().as_bool().unwrap();
            let expected = a + if on { b } else { 0 };
            assert_eq!(r.get("total").unwrap().as_i64(), Some(expected));
            assert_eq!(r.get("count").unwrap().as_i64(), Some(1 + on as i64));
        }
    }

    #[test]
    fn computed_param_must_come_after_its_inputs() {
        let json = r#"{"name":"x","params":[
            {"key":"total","title":"Итого","identity":true,"domain":{"kind":"computed","expr":"a + 1"}},
            {"key":"a","title":"A","domain":{"kind":"int","min":1,"max":10}}
        ]}"#;
        let err = ParamModel::from_json_str(json).unwrap_err().to_string();
        assert!(err.contains("объявленного позже"), "{err}");
    }

    #[test]
    fn computed_param_cannot_be_biased() {
        let json = r#"{"name":"x","params":[
            {"key":"a","title":"A","identity":true,"domain":{"kind":"int","min":1,"max":10}},
            {"key":"t","title":"T","domain":{"kind":"computed","expr":"a * 2"}}],
            "soft":[{"name":"r","when":"a > 5","then":[{"kind":"range","param":"t","min":3.0}]}]}"#;
        let err = ParamModel::from_json_str(json).unwrap_err().to_string();
        assert!(err.contains("вычисляемый"), "{err}");
    }

    #[test]
    fn model_survives_json_roundtrip() {
        let m = doctor_model();
        let json = m.to_json_string().unwrap();
        let back = ParamModel::from_json_str(&json).unwrap();

        assert_eq!(back.params.len(), m.params.len());
        assert_eq!(back.hard.len(), m.hard.len());
        // Выражения хранятся строками и должны пережить пересборку.
        assert_eq!(back.hard[0].expr.to_string(), m.hard[0].expr.to_string());
    }
}
