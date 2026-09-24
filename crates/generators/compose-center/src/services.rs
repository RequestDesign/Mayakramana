//! Услуги и цены центра.
//!
//! Какие услуги есть у центра, решает код по его составу, а не модель. Вывод из
//! запоя появляется только там, где есть нарколог; группа для родственников —
//! только при семейной программе; консультация психиатра — только если в
//! команде есть психиатрический опыт. Поэтому страница услуг не может
//! противоречить странице команды.
//!
//! Цены — ориентиры по ценовому уровню с поправкой на регион. С рынком они не
//! сверены, и это сказано в самом справочнике.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::compose::{is_narcologist, Candidate, CenterPlan, Pools};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    pub key: String,
    pub title: String,
    /// Условие, при котором услуга есть у центра.
    pub needs: String,
    pub unit: String,
    /// Диапазон цены по ценовому уровню, рублей.
    pub price: BTreeMap<String, [i64; 2]>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceCatalog {
    #[serde(default)]
    pub region_factor: BTreeMap<String, f64>,
    #[serde(default)]
    pub services: Vec<ServiceDef>,
}

impl ServiceCatalog {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text.trim_start_matches('\u{feff}'))
    }
}

/// Услуга конкретного центра.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub key: String,
    pub title: String,
    pub unit: String,
    /// Цена «от», рублей. Ноль — бесплатно или входит в стоимость.
    pub price_from: i64,
}

impl Offer {
    pub fn render(&self) -> String {
        if self.price_from == 0 {
            format!("{} — входит в стоимость", self.title)
        } else {
            format!("{} — от {} ₽ за {}", self.title, group_thousands(self.price_from), self.unit)
        }
    }
}

fn group_thousands(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

fn find<'a>(pool: &'a [Candidate], key: &str) -> Option<&'a Candidate> {
    pool.iter().find(|c| c.key == key)
}

fn rec_bool(c: &Candidate, k: &str) -> bool {
    c.record.get(k).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn rec_int(c: &Candidate, k: &str) -> i64 {
    c.record.get(k).and_then(|v| v.as_i64()).unwrap_or(0)
}

/// Выполнено ли условие услуги для этого центра.
fn satisfied(needs: &str, plan: &CenterPlan, pools: &Pools) -> bool {
    let doctors: Vec<&Candidate> = plan.doctors.iter().filter_map(|k| find(&pools.doctors, k)).collect();
    let program = find(&pools.programs, &plan.program);

    match needs {
        "program" => true,
        "narcologist" => doctors.iter().any(|d| is_narcologist(d)),
        "psychiatrist" => doctors.iter().any(|d| {
            rec_bool(d, "psychiatry_experience")
                || matches!(d.record.get("specialty").and_then(|v| v.as_str()), Some("психиатр"))
        }),
        "detox" => {
            program.is_some_and(|p| rec_bool(p, "detox_included"))
                && doctors.iter().any(|d| rec_bool(d, "detox_experience"))
        }
        "psychologist" => !plan.psychologists.is_empty(),
        "consultant" => !plan.consultants.is_empty(),
        "family_program" => program.is_some_and(|p| rec_bool(p, "family_program")),
        "aftercare" => program.is_some_and(|p| rec_int(p, "aftercare_months") > 0),
        "not_economy" => plan.segment != "эконом",
        _ => false,
    }
}

/// Детерминированное значение из диапазона: один центр — одна цена при любом
/// повторном запуске.
fn pick_in(range: [i64; 2], salt: &str, seed: u64) -> i64 {
    let [lo, hi] = range;
    if hi <= lo {
        return lo;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (salt, seed).hash(&mut h);
    let t = (h.finish() % 1000) as f64 / 1000.0;
    lo + ((hi - lo) as f64 * t) as i64
}

/// Округление цены так, как округляют прайсы: до сотен в мелких суммах, до
/// тысяч в крупных.
fn round_price(p: f64) -> i64 {
    let step = if p >= 50_000.0 { 5_000.0 } else if p >= 10_000.0 { 1_000.0 } else { 100.0 };
    ((p / step).round() * step) as i64
}

/// Услуги центра с ценами.
pub fn offers(plan: &CenterPlan, pools: &Pools, catalog: &ServiceCatalog, seed: u64) -> Vec<Offer> {
    let region = find(&pools.places, &plan.place)
        .and_then(|p| p.record.get("region").and_then(|v| v.as_str()))
        .unwrap_or("");
    let factor = catalog.region_factor.get(region).copied().unwrap_or(1.0);

    catalog
        .services
        .iter()
        .filter(|s| satisfied(&s.needs, plan, pools))
        .filter_map(|s| {
            let range = s.price.get(&plan.segment).copied()?;
            let base = pick_in(range, &format!("{}:{}", plan.place, s.key), seed);
            Some(Offer {
                key: s.key.clone(),
                title: s.title.clone(),
                unit: s.unit.clone(),
                price_from: round_price(base as f64 * factor),
            })
        })
        .collect()
}

/// Работает ли центр круглосуточно: да, если есть стационарная детоксикация
/// или выезд на дом — без дежурного врача такие услуги не оказывают.
pub fn round_the_clock(offers: &[Offer]) -> bool {
    offers.iter().any(|o| o.key == "detox_inpatient" || o.key == "detox_home")
}

/// Цена полного курса по длительности программы.
pub fn course_price(offers: &[Offer], duration: &str) -> Option<i64> {
    let month = offers.iter().find(|o| o.key == "rehab")?.price_from;
    let months = match duration {
        "28 дней" => 1.0,
        "45 дней" => 1.5,
        "2 месяца" => 2.0,
        "3 месяца" => 3.0,
        "6 месяцев" => 6.0,
        _ => return None,
    };
    Some(round_price(month as f64 * months))
}
