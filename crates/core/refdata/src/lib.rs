//! Справочники реального мира.
//!
//! Часть невидимого слоя, которая не выдумывается, а импортируется. Имена,
//! регионы, учреждения, методики — всё, где выдумка дала бы выпускников
//! несуществующих вузов и полторы сотни разных имён на десять тысяч человек.
//!
//! Отделено от словаря параметров намеренно: словарь описывает, **чем**
//! характеризуется сущность, справочник — **какие значения бывают на самом
//! деле**.

mod error;
mod names;

pub use error::{Error, Result};
pub use names::{FirstName, FullName, NameBook, Patronymic, Sex, Surname};

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::collections::{BTreeMap, HashSet};
    use std::path::PathBuf;

    fn book() -> NameBook {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../reference/names-ru.json");
        NameBook::load(&path).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn reference_file_is_valid() {
        let b = book();
        assert!(b.male_first.len() >= 20, "мужских имён мало: {}", b.male_first.len());
        assert!(b.female_first.len() >= 20, "женских имён мало: {}", b.female_first.len());
        assert!(b.surnames.len() >= 30, "фамилий мало: {}", b.surnames.len());
    }

    /// Справочника должно хватать на объём, ради которого он заводился.
    #[test]
    fn capacity_covers_ten_thousand() {
        let b = book();
        for sex in [Sex::Male, Sex::Female] {
            // 10 000 сущностей из N сочетаний: совпадения начинаются примерно
            // на корне из N. При 80 тысячах это первые совпадения в районе
            // трёхсот человек — как в жизни, где полные тёзки встречаются.
            // Уникальность сущности обеспечивается не именем, а подписью по
            // опорным параметрам.
            assert!(
                b.capacity(sex) > 50_000,
                "сочетаний всего {} — справочник слишком тесный",
                b.capacity(sex)
            );
        }
    }

    #[test]
    fn surnames_are_gendered_correctly() {
        let b = book();
        let mut rng = StdRng::seed_from_u64(1);

        for _ in 0..200 {
            let f = b.pick(&mut rng, Sex::Female, 1975).unwrap();
            assert!(
                f.last.ends_with('а') || f.last.ends_with("ая"),
                "женская фамилия в мужской форме: {}",
                f.last
            );
            assert!(
                f.patronymic.ends_with("на"),
                "женское отчество в мужской форме: {}",
                f.patronymic
            );
        }

        for _ in 0..200 {
            let m = b.pick(&mut rng, Sex::Male, 1975).unwrap();
            assert!(
                m.patronymic.ends_with("ич"),
                "мужское отчество в женской форме: {}",
                m.patronymic
            );
        }
    }

    /// Мода на имена меняется: набор у рождённых в 1955 и в 1995 обязан
    /// отличаться, иначе справочник не делает того, ради чего заведён.
    #[test]
    fn name_fashion_shifts_between_generations() {
        let b = book();
        let older = name_shares(&b, 1955, 3000);
        let younger = name_shares(&b, 1995, 3000);

        // Имена, вошедшие в моду позже, должны быть заметно чаще у молодых.
        for name in ["Артём", "Никита", "Максим"] {
            let old = older.get(name).copied().unwrap_or(0.0);
            let new = younger.get(name).copied().unwrap_or(0.0);
            assert!(
                new > old * 3.0,
                "«{name}»: у рождённых в 1955 доля {old:.3}, в 1995 — {new:.3}; \
                 веса по десятилетиям не работают"
            );
        }

        // И наоборот — вышедшие из моды должны стать редкими.
        for name in ["Владимир", "Юрий", "Анатолий"] {
            let old = older.get(name).copied().unwrap_or(0.0);
            let new = younger.get(name).copied().unwrap_or(0.0);
            assert!(
                old > new * 3.0,
                "«{name}»: у рождённых в 1955 доля {old:.3}, в 1995 — {new:.3}"
            );
        }

        // Суммарное расхождение распределений должно быть существенным.
        let keys: HashSet<&String> = older.keys().chain(younger.keys()).collect();
        let distance: f64 = keys
            .iter()
            .map(|k| {
                (older.get(*k).copied().unwrap_or(0.0) - younger.get(*k).copied().unwrap_or(0.0))
                    .abs()
            })
            .sum::<f64>()
            / 2.0;

        assert!(
            distance > 0.35,
            "распределения имён поколений почти совпадают (расхождение {distance:.2})"
        );
    }

    fn name_shares(b: &NameBook, year: i32, n: usize) -> BTreeMap<String, f64> {
        let mut rng = StdRng::seed_from_u64(year as u64);
        let mut counts: BTreeMap<String, f64> = BTreeMap::new();
        for _ in 0..n {
            if let Some(f) = b.pick(&mut rng, Sex::Male, year) {
                *counts.entry(f.first).or_default() += 1.0;
            }
        }
        counts.values_mut().for_each(|v| *v /= n as f64);
        counts
    }

    #[test]
    fn full_names_are_diverse_in_a_large_run() {
        let b = book();
        let mut rng = StdRng::seed_from_u64(42);

        let names: HashSet<String> = (0..2000)
            .filter_map(|_| b.pick(&mut rng, Sex::Male, 1975))
            .map(|f| f.formal())
            .collect();

        // Совпадения при 2000 выборок из 85 тысяч сочетаний ожидаемы и
        // естественны; важно, что их немного.
        assert!(
            names.len() > 1940,
            "на 2000 выборок всего {} разных имён — слишком много совпадений",
            names.len()
        );
    }

    #[test]
    fn missing_patronymic_is_caught_at_load() {
        let broken = NameBook {
            male_first: vec![FirstName {
                name: "Пётр".into(),
                by_decade: Default::default(),
                weight: 1.0,
            }],
            female_first: vec![FirstName {
                name: "Анна".into(),
                by_decade: Default::default(),
                weight: 1.0,
            }],
            patronymics: Default::default(),
            surnames: vec![Surname {
                male: "Иванов".into(),
                female: "Иванова".into(),
                weight: 1.0,
            }],
        };

        let err = broken.check().unwrap_err().to_string();
        assert!(err.contains("Пётр"), "{err}");
    }
}
