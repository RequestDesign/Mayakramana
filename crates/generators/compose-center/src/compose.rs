//! Композиция: из пулов готовых сущностей собираются центры.
//!
//! Здесь ничего не сочиняется. Центр для престарелых получает персонал с
//! опытом работы с пожилыми не потому, что модель так придумала, а потому, что
//! отбор идёт по параметрам уже созданных людей.
//!
//! Чистая функция от пулов и сида: одинаковый вход даёт одинаковые центры.

use std::collections::HashSet;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Готовая сущность из пула: ключ и запись в том виде, как она легла в
/// хранилище.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub key: String,
    pub record: Value,
}

impl Candidate {
    pub fn new(key: impl Into<String>, record: Value) -> Self {
        Self { key: key.into(), record }
    }

    fn str(&self, k: &str) -> &str {
        self.record.get(k).and_then(|v| v.as_str()).unwrap_or("")
    }

    fn int(&self, k: &str) -> i64 {
        self.record.get(k).and_then(|v| v.as_i64()).unwrap_or(0)
    }

    fn bool(&self, k: &str) -> bool {
        self.record.get(k).and_then(|v| v.as_bool()).unwrap_or(false)
    }

    fn has(&self, k: &str, value: &str) -> bool {
        match self.record.get(k) {
            Some(Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(value)),
            Some(Value::String(s)) => s == value,
            _ => false,
        }
    }

    pub fn age(&self) -> i64 {
        self.int("age")
    }

    pub fn name(&self) -> &str {
        self.str("full_name")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Pools {
    pub places: Vec<Candidate>,
    pub programs: Vec<Candidate>,
    pub directors: Vec<Candidate>,
    pub doctors: Vec<Candidate>,
    pub psychologists: Vec<Candidate>,
    pub consultants: Vec<Candidate>,
    /// Ключи, уже занятые центрами, собранными раньше.
    ///
    /// Без этого правило «один человек — один центр» действовало бы только в
    /// пределах одного запуска: второй запуск сборки снова раздал бы тех же
    /// врачей.
    pub reserved: HashSet<String>,
}

impl Pools {
    pub fn sizes(&self) -> String {
        format!(
            "зданий {}, программ {}, руководителей {}, врачей {}, психологов {}, консультантов {}",
            self.places.len(),
            self.programs.len(),
            self.directors.len(),
            self.doctors.len(),
            self.psychologists.len(),
            self.consultants.len()
        )
    }
}

/// Собранный центр — ссылки на готовые сущности и то, почему они вместе.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CenterPlan {
    pub place: String,
    pub program: String,
    pub director: String,
    pub doctors: Vec<String>,
    pub psychologists: Vec<String>,
    pub consultants: Vec<String>,
    pub segment: String,
    pub focus: String,
    /// Насколько команда соответствует специализации, от 0 до 1.
    pub fit: f32,
}

impl CenterPlan {
    pub fn staff(&self) -> impl Iterator<Item = &String> {
        self.doctors
            .iter()
            .chain(&self.psychologists)
            .chain(&self.consultants)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("собрано {built} центров из {wanted}: закончились {what}. Пулы: {pools}")]
    Exhausted {
        built: usize,
        wanted: usize,
        what: String,
        pools: String,
    },
}

/// Сколько людей нужно центру такой вместимости.
///
/// Соотношение персонала и мест — не декор: центр на пятьдесят человек с одним
/// психологом неправдоподобен, а на восемь мест с шестью консультантами — тоже.
pub fn team_size(capacity: i64) -> (usize, usize, usize) {
    let doctors = (1 + capacity / 25).clamp(1, 3) as usize;
    let psychologists = (1 + capacity / 20).clamp(1, 3) as usize;
    let consultants = (2 + capacity / 10).clamp(2, 7) as usize;
    (doctors, psychologists, consultants)
}

/// Насколько человек подходит под специализацию центра, от 0 до 1.
pub fn fit(role: Role, c: &Candidate, focus: &str) -> f32 {
    match (role, focus) {
        (Role::Doctor, "алкогольная зависимость") => c.has("practice", "алкогольная зависимость") as u8 as f32,
        (Role::Doctor, "наркотическая зависимость") => c.has("practice", "наркозависимые") as u8 as f32,
        (Role::Doctor, "двойной диагноз") => {
            (c.has("practice", "двойной диагноз") as u8 as f32) * 0.6 + (c.bool("psychiatry_experience") as u8 as f32) * 0.4
        }
        (Role::Doctor, "подростки и молодёжь") => {
            let practice = c.has("practice", "подростковая зависимость") as u8 as f32;
            let age_focus = (c.str("patient_age_focus") == "подростки и молодёжь") as u8 as f32;
            (practice * 0.5 + age_focus * 0.5).min(1.0)
        }
        (Role::Doctor, "игровая зависимость") => c.has("practice", "игровая зависимость") as u8 as f32,

        (Role::Psychologist, "подростки и молодёжь") => {
            (c.str("client_focus") == "подростки и молодёжь") as u8 as f32
        }
        (Role::Psychologist, "двойной диагноз") => c.bool("diagnostics") as u8 as f32,
        (Role::Psychologist, _) => {
            let m = c.has("methods", "мотивационное интервьюирование")
                || c.has("methods", "когнитивно-поведенческая терапия");
            0.5 + (m as u8 as f32) * 0.5
        }

        (Role::Consultant, "алкогольная зависимость") => (c.str("substance") == "алкоголь") as u8 as f32,
        (Role::Consultant, "наркотическая зависимость") => {
            matches!(c.str("substance"), "опиаты" | "стимуляторы" | "смешанное употребление") as u8 as f32
        }
        (Role::Consultant, "игровая зависимость") => (c.str("substance") == "игровая зависимость") as u8 as f32,
        (Role::Consultant, "подростки и молодёжь") => c.has("focus", "молодые") as u8 as f32,

        // Смешанная специализация — любой подходит одинаково.
        _ => 0.5,
    }
}

/// Специализации, при которых в команде обязан быть нарколог.
pub fn needs_narcologist(focus: &str) -> bool {
    matches!(
        focus,
        "алкогольная зависимость"
            | "наркотическая зависимость"
            | "двойной диагноз"
            | "смешанная зависимость"
    )
}

pub fn is_narcologist(c: &Candidate) -> bool {
    matches!(c.str("specialty"), "психиатр-нарколог" | "нарколог-реаниматолог")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Doctor,
    Psychologist,
    Consultant,
}

/// Собрать центры.
///
/// Каждый человек, здание и программа используются не больше одного раза:
/// один врач не может числиться в трёх центрах сразу — на связанных сайтах
/// это видно, и доверие к ним рушится разом.
pub fn compose(pools: &Pools, count: usize, seed: u64) -> Result<Vec<CenterPlan>, ComposeError> {
    let mut rng = StdRng::seed_from_u64(seed);

    let mut places: Vec<&Candidate> = pools.places.iter().collect();
    places.shuffle(&mut rng);

    let mut used: HashSet<String> = pools.reserved.clone();
    let mut out = Vec::with_capacity(count);

    for place in places {
        if out.len() == count {
            break;
        }
        if used.contains(&place.key) {
            continue;
        }

        let segment = place.str("segment").to_string();
        let region = place.str("region").to_string();
        let capacity = place.int("capacity");

        // Программа того же ценового уровня. Специализация центра — это
        // специализация его программы.
        let Some(program) = pick_one(&pools.programs, &used, &mut rng, |p| {
            if p.str("segment") == segment { 1.0 } else { -1.0 }
        })
        .filter(|p| p.str("segment") == segment) else {
            continue;
        };
        let focus = program.str("focus").to_string();

        let (nd, np, nc) = team_size(capacity);

        let mut taken: HashSet<String> = used.clone();
        taken.insert(place.key.clone());
        taken.insert(program.key.clone());

        let mut surnames: HashSet<String> = HashSet::new();

        // Нарколог получает прибавку к оценке там, где он обязателен: иначе при
        // равном соответствии практики его могли бы не взять вовсе.
        let doctors = pick_team(&pools.doctors, &taken, &mut surnames, nd, &mut rng, |c| {
            let bonus = if needs_narcologist(&focus) && is_narcologist(c) { 0.5 } else { 0.0 };
            fit(Role::Doctor, c, &focus) + bonus
        });
        if doctors.len() < nd {
            return Err(exhausted(out.len(), count, "врачи", pools));
        }
        taken.extend(doctors.iter().map(|c| c.key.clone()));

        let psychologists = pick_team(&pools.psychologists, &taken, &mut surnames, np, &mut rng, |c| {
            fit(Role::Psychologist, c, &focus)
        });
        if psychologists.len() < np {
            return Err(exhausted(out.len(), count, "психологи", pools));
        }
        taken.extend(psychologists.iter().map(|c| c.key.clone()));

        let consultants = pick_team(&pools.consultants, &taken, &mut surnames, nc, &mut rng, |c| {
            fit(Role::Consultant, c, &focus)
        });
        if consultants.len() < nc {
            return Err(exhausted(out.len(), count, "консультанты", pools));
        }

        // --- правила состава -------------------------------------------------
        // Двойной диагноз без психиатрического опыта в команде — это не
        // специализация, а надпись на вывеске.
        if focus == "двойной диагноз" && !doctors.iter().any(|d| d.bool("psychiatry_experience")) {
            continue;
        }

        // Центр, работающий с химической зависимостью, без единого нарколога.
        // На живом прогоне так вышел центр для наркозависимых с тремя
        // психотерапевтами: пул был маленький, подходящие кончились, и отбор
        // взял кого есть.
        if needs_narcologist(&focus) && !doctors.iter().any(|d| is_narcologist(d)) {
            continue;
        }

        let team: Vec<&Candidate> = doctors
            .iter()
            .chain(&psychologists)
            .chain(&consultants)
            .copied()
            .collect();

        // Руководитель подбирается после команды, а не до: он не может быть
        // заметно моложе своих людей — «руководителю тридцать, специалистам
        // по пятьдесят-шестьдесят». Сначала команда, потом тот, кто ей по
        // возрасту и опыту соответствует.
        let mut ages: Vec<i64> = team.iter().map(|c| c.age()).collect();
        ages.sort_unstable();
        let median = ages[ages.len() / 2];

        let mut taken_all = taken.clone();
        taken_all.extend(team.iter().map(|c| c.key.clone()));

        let Some(director) = pick_one(&pools.directors, &taken_all, &mut rng, |d| {
            if d.age() < 35 || d.age() + 8 < median || surnames.contains(&surname_root(d)) {
                return -1.0;
            }
            let dr = d.str("home_region");
            let region_bonus =
                if !dr.is_empty() && (dr.contains(&region) || region.contains(dr)) { 0.3 } else { 0.0 };
            let founder_bonus = if segment == "премиум" && d.bool("founder") { 0.2 } else { 0.0 };
            0.5 + region_bonus + founder_bonus
        })
        .filter(|d| {
            d.age() >= 35 && d.age() + 8 >= median && !surnames.contains(&surname_root(d))
        }) else {
            if pools.directors.iter().all(|d| used.contains(&d.key)) {
                return Err(exhausted(out.len(), count, "руководители", pools));
            }
            // Свободные руководители есть, но ни один не годится этой
            // команде по возрасту — пробуем следующее здание.
            continue;
        };

        let fit_score = {
            let scores: Vec<f32> = doctors
                .iter()
                .map(|c| fit(Role::Doctor, c, &focus))
                .chain(psychologists.iter().map(|c| fit(Role::Psychologist, c, &focus)))
                .chain(consultants.iter().map(|c| fit(Role::Consultant, c, &focus)))
                .collect();
            scores.iter().sum::<f32>() / scores.len() as f32
        };

        used.insert(place.key.clone());
        used.insert(program.key.clone());
        used.insert(director.key.clone());
        for c in &team {
            used.insert(c.key.clone());
        }

        out.push(CenterPlan {
            place: place.key.clone(),
            program: program.key.clone(),
            director: director.key.clone(),
            doctors: doctors.iter().map(|c| c.key.clone()).collect(),
            psychologists: psychologists.iter().map(|c| c.key.clone()).collect(),
            consultants: consultants.iter().map(|c| c.key.clone()).collect(),
            segment,
            focus,
            fit: fit_score,
        });
    }

    if out.len() < count {
        return Err(exhausted(out.len(), count, "подходящие сочетания зданий и программ", pools));
    }
    Ok(out)
}

fn exhausted(built: usize, wanted: usize, what: &str, pools: &Pools) -> ComposeError {
    ComposeError::Exhausted {
        built,
        wanted,
        what: what.to_string(),
        pools: pools.sizes(),
    }
}

/// Лучший по оценке свободный кандидат; при равенстве решает сид.
fn pick_one<'a>(
    pool: &'a [Candidate],
    used: &HashSet<String>,
    rng: &mut StdRng,
    score: impl Fn(&Candidate) -> f32,
) -> Option<&'a Candidate> {
    let mut free: Vec<&Candidate> = pool.iter().filter(|c| !used.contains(&c.key)).collect();
    free.shuffle(rng);
    free.into_iter()
        .max_by(|a, b| score(a).partial_cmp(&score(b)).unwrap_or(std::cmp::Ordering::Equal))
}

/// Фамилия без родового окончания: «Морозова» и «Морозов» — одна фамилия.
pub fn surname_root(c: &Candidate) -> String {
    let s = c.name().split_whitespace().next().unwrap_or("").to_lowercase();
    s.strip_suffix("ая")
        .map(|b| format!("{b}ий"))
        .or_else(|| s.strip_suffix('а').map(str::to_string))
        .unwrap_or(s)
}

/// `n` лучших свободных кандидатов без совпадающих фамилий.
///
/// Перемешивание перед сортировкой нужно, чтобы при равных оценках один и тот
/// же человек не попадал всегда в первый центр.
///
/// Однофамильцы в одной команде отсекаются: на живом прогоне в один центр
/// попали психолог и консультант Морозовы. Фамилия частая, но в одном
/// коллективе это читается как семейный подряд.
fn pick_team<'a>(
    pool: &'a [Candidate],
    used: &HashSet<String>,
    surnames: &mut HashSet<String>,
    n: usize,
    rng: &mut StdRng,
    score: impl Fn(&Candidate) -> f32,
) -> Vec<&'a Candidate> {
    let mut free: Vec<&Candidate> = pool.iter().filter(|c| !used.contains(&c.key)).collect();
    free.shuffle(rng);
    free.sort_by(|a, b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = Vec::with_capacity(n);
    for c in free {
        if out.len() == n {
            break;
        }
        let root = surname_root(c);
        if !root.is_empty() && !surnames.insert(root) {
            continue;
        }
        out.push(c);
    }
    out
}
