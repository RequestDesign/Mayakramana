//! Имена из справочника с учётом года рождения.
//!
//! Это тот самый случай, ради которого справочники вообще существуют. Попросить
//! модель придумать десять тысяч имён — значит получить полторы сотни разных, и
//! все из числа самых частотных. Кроме того, мода на имена меняется: набор,
//! обычный у рождённых в 1965 году, и набор у рождённых в 1995 — разные.
//! Модель этого не соблюдает никогда, таблица — соблюдает.
//!
//! Отчество берётся не из воздуха: сначала сэмплируется имя отца, причём из
//! распределения на поколение раньше. Поэтому у врача 1970 года рождения отец
//! зовётся так, как звали мужчин, рождённых около 1942-го.

use std::collections::BTreeMap;
use std::path::Path;

use rand::distributions::{Distribution, WeightedIndex};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Насколько отец старше ребёнка, лет.
const FATHER_AGE_GAP: i32 = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sex {
    Male,
    Female,
}

impl Sex {
    /// Разбор значения параметра `gender` из словаря.
    pub fn from_param(value: &str) -> Option<Self> {
        match value {
            "мужской" => Some(Sex::Male),
            "женский" => Some(Sex::Female),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirstName {
    pub name: String,
    /// Вес по десятилетию рождения: ключ — «1960», «1970» и так далее.
    #[serde(default)]
    pub by_decade: BTreeMap<String, f64>,
    /// Запасной вес, если десятилетие не описано.
    #[serde(default = "one")]
    pub weight: f64,
}

fn one() -> f64 {
    1.0
}

impl FirstName {
    fn weight_for(&self, birth_year: i32) -> f64 {
        let decade = format!("{}", (birth_year / 10) * 10);
        self.by_decade.get(&decade).copied().unwrap_or(self.weight)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Surname {
    /// Мужская форма.
    pub male: String,
    /// Женская форма.
    pub female: String,
    #[serde(default = "one")]
    pub weight: f64,
}

impl Surname {
    fn form(&self, sex: Sex) -> &str {
        match sex {
            Sex::Male => &self.male,
            Sex::Female => &self.female,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patronymic {
    pub male: String,
    pub female: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NameBook {
    #[serde(default)]
    pub male_first: Vec<FirstName>,
    #[serde(default)]
    pub female_first: Vec<FirstName>,
    /// Имя отца → формы отчества.
    #[serde(default)]
    pub patronymics: BTreeMap<String, Patronymic>,
    #[serde(default)]
    pub surnames: Vec<Surname>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullName {
    pub first: String,
    pub patronymic: String,
    pub last: String,
}

impl FullName {
    /// «Иванов Сергей Петрович» — как пишут в карточке специалиста.
    pub fn formal(&self) -> String {
        format!("{} {} {}", self.last, self.first, self.patronymic)
    }

    /// «Сергей Петрович» — как обращаются.
    pub fn polite(&self) -> String {
        format!("{} {}", self.first, self.patronymic)
    }
}

impl NameBook {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        let book: Self = serde_json::from_str(&src)?;
        book.check()?;
        Ok(book)
    }

    pub fn check(&self) -> Result<()> {
        let mut issues = Vec::new();

        if self.male_first.is_empty() || self.female_first.is_empty() {
            issues.push("списки имён пусты хотя бы для одного пола".to_string());
        }
        if self.surnames.is_empty() {
            issues.push("список фамилий пуст".to_string());
        }

        // Отчество должно найтись для каждого мужского имени, иначе часть
        // популяции останется без отчества, и это вскроется только в тексте.
        let missing: Vec<&str> = self
            .male_first
            .iter()
            .filter(|n| !self.patronymics.contains_key(&n.name))
            .map(|n| n.name.as_str())
            .collect();
        if !missing.is_empty() {
            issues.push(format!(
                "нет отчеств для мужских имён: {}",
                missing.join(", ")
            ));
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(issues))
        }
    }

    pub fn pick(&self, rng: &mut impl Rng, sex: Sex, birth_year: i32) -> Option<FullName> {
        let pool = match sex {
            Sex::Male => &self.male_first,
            Sex::Female => &self.female_first,
        };
        let first = weighted_pick(rng, pool, |n| n.weight_for(birth_year))?.name.clone();

        // Имя отца — из распределения на поколение раньше.
        let father = weighted_pick(rng, &self.male_first, |n| {
            n.weight_for(birth_year - FATHER_AGE_GAP)
        })?;
        let patronymic = self.patronymics.get(&father.name).map(|p| match sex {
            Sex::Male => p.male.clone(),
            Sex::Female => p.female.clone(),
        })?;

        let last = weighted_pick(rng, &self.surnames, |s| s.weight)?
            .form(sex)
            .to_string();

        Some(FullName { first, patronymic, last })
    }

    /// Сколько различных сочетаний способен дать справочник.
    ///
    /// Нужно, чтобы заранее понимать, хватит ли его на нужный объём: если
    /// сочетаний меньше, чем сущностей, совпадения неизбежны.
    pub fn capacity(&self, sex: Sex) -> usize {
        let first = match sex {
            Sex::Male => self.male_first.len(),
            Sex::Female => self.female_first.len(),
        };
        first * self.patronymics.len() * self.surnames.len()
    }
}

fn weighted_pick<'a, T>(
    rng: &mut impl Rng,
    items: &'a [T],
    weight: impl Fn(&T) -> f64,
) -> Option<&'a T> {
    if items.is_empty() {
        return None;
    }
    let weights: Vec<f64> = items.iter().map(|i| weight(i).max(0.0)).collect();
    if weights.iter().all(|w| *w <= 0.0) {
        return items.get(rng.gen_range(0..items.len()));
    }
    WeightedIndex::new(&weights)
        .ok()
        .map(|d| &items[d.sample(rng)])
}
