use std::collections::HashSet;

use rand::distributions::{Distribution, WeightedIndex};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::ParamModel;
use crate::param::{Domain, Variant};
use crate::row::ParamRow;
use crate::rule::Bias;
use crate::value::Value;

/// Когорта популяции.
///
/// Существует ровно для того, чтобы требование «20% с опытом от 7 лет»
/// выполнялось точно. Модель такие пропорции не удерживает — она сваливается в
/// среднее. Код раскладывает N строк по долям детерминированно.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cohort {
    pub name: String,
    /// Доля от 0 до 1.
    pub share: f64,
    /// Чем эта когорта отличается: сужение диапазонов, сдвиг весов.
    #[serde(default)]
    pub biases: Vec<Bias>,
}

impl Cohort {
    pub fn new(name: impl Into<String>, share: f64) -> Self {
        Self { name: name.into(), share, biases: Vec::new() }
    }

    pub fn with(mut self, bias: Bias) -> Self {
        self.biases.push(bias);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PopulationPlan {
    pub count: usize,
    pub cohorts: Vec<Cohort>,
}

impl PopulationPlan {
    /// Однородная популяция без когорт.
    pub fn uniform(count: usize) -> Self {
        Self { count, cohorts: vec![Cohort::new("основная", 1.0)] }
    }

    /// Разложить общее количество по когортам точно.
    ///
    /// Метод наибольших остатков: сумма всегда равна `count`, без потерь на
    /// округлении. Остаток от долей, не дотягивающих до 1, уходит крупнейшим.
    pub fn allocate(&self) -> Vec<usize> {
        if self.cohorts.is_empty() {
            return Vec::new();
        }
        let total_share: f64 = self.cohorts.iter().map(|c| c.share.max(0.0)).sum();
        if total_share <= 0.0 {
            // Все доли нулевые — делим поровну, чтобы не потерять популяцию.
            let base = self.count / self.cohorts.len();
            let mut v = vec![base; self.cohorts.len()];
            for i in 0..(self.count - base * self.cohorts.len()) {
                v[i] += 1;
            }
            return v;
        }

        let exact: Vec<f64> = self
            .cohorts
            .iter()
            .map(|c| c.share.max(0.0) / total_share * self.count as f64)
            .collect();

        let mut counts: Vec<usize> = exact.iter().map(|x| x.floor() as usize).collect();
        let assigned: usize = counts.iter().sum();
        let mut remainder = self.count.saturating_sub(assigned);

        let mut order: Vec<usize> = (0..self.cohorts.len()).collect();
        order.sort_by(|&a, &b| {
            let fa = exact[a] - exact[a].floor();
            let fb = exact[b] - exact[b].floor();
            fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut k = 0;
        while remainder > 0 {
            counts[order[k % order.len()]] += 1;
            remainder -= 1;
            k += 1;
        }
        counts
    }
}

/// Строка с пометкой, из какой когорты она вышла.
#[derive(Debug, Clone)]
pub struct Sampled {
    pub cohort: String,
    pub row: ParamRow,
}

pub struct Sampler<'a> {
    model: &'a ParamModel,
    rng: StdRng,
    /// Сколько раз пересобирать строку при нарушении жёсткого правила.
    pub max_row_attempts: u32,
    /// Сколько раз пересобирать строку при совпадении подписи.
    pub max_unique_attempts: u32,
}

impl<'a> Sampler<'a> {
    /// Сид определяет всю популяцию: одинаковый сид — одинаковый результат.
    pub fn new(model: &'a ParamModel, seed: u64) -> Self {
        Self {
            model,
            rng: StdRng::seed_from_u64(seed),
            max_row_attempts: 200,
            max_unique_attempts: 60,
        }
    }

    /// Одна строка с учётом дополнительных сдвигов (например, когортных).
    pub fn sample_row(&mut self, extra: &[Bias]) -> Result<ParamRow> {
        let mut last_violated = String::from("(нет)");

        for _ in 0..self.max_row_attempts {
            let row = self.build_row(extra);
            match self.model.hard.iter().find(|r| !r.holds(&row)) {
                None => return Ok(row),
                Some(violated) => last_violated = violated.explain().to_string(),
            }
        }

        Err(Error::Unsatisfiable {
            attempts: self.max_row_attempts,
            rule: last_violated,
        })
    }

    /// Популяция целиком: точные доли по когортам плюс проверка уникальности
    /// комбинаций параметров.
    ///
    /// Уникальность проверяется здесь, **до** обращения к модели: переставить
    /// параметры стоит ноль, перегенерировать текст — деньги.
    pub fn sample_population(&mut self, plan: &PopulationPlan) -> Result<Vec<Sampled>> {
        let counts = plan.allocate();
        let mut out = Vec::with_capacity(plan.count);
        let mut seen: HashSet<String> = HashSet::new();

        for (cohort, want) in plan.cohorts.iter().zip(counts) {
            let mut produced = 0usize;
            while produced < want {
                let mut accepted = None;

                for _ in 0..self.max_unique_attempts {
                    let row = self.sample_row(&cohort.biases)?;
                    let sig = row.signature(self.model);
                    if sig.is_empty() || seen.insert(sig) {
                        accepted = Some(row);
                        break;
                    }
                }

                let Some(row) = accepted else {
                    return Err(Error::ExhaustedUniqueness {
                        attempts: self.max_unique_attempts,
                        wanted: plan.count,
                    });
                };

                out.push(Sampled { cohort: cohort.name.clone(), row });
                produced += 1;
            }
        }

        Ok(out)
    }

    // --------------------------------------------------------------- нутрянка

    fn build_row(&mut self, extra: &[Bias]) -> ParamRow {
        // Копируем ссылку на модель, чтобы не держать заём self во время сэмплирования.
        let model = self.model;
        let mut row = ParamRow::new();

        for p in &model.params {
            if !p.domain.is_sampled() {
                row.set(p.key.clone(), Value::Null);
                continue;
            }

            let biases = collect_biases(model, &row, &p.key, extra);

            if let Some(Bias::Force { value, .. }) =
                biases.iter().find(|b| matches!(b, Bias::Force { .. }))
            {
                row.set(p.key.clone(), value.clone());
                continue;
            }

            let value = self.sample_value(&p.domain, &biases);
            row.set(p.key.clone(), value);
        }

        row
    }

    fn sample_value(&mut self, domain: &Domain, biases: &[&Bias]) -> Value {
        match domain {
            Domain::Int { min, max, weights } => {
                let (lo_f, hi_f) = apply_range(*min as f64, *max as f64, biases);
                let lo = lo_f.round() as i64;
                let hi = hi_f.round() as i64;

                if lo >= hi {
                    return Value::Int(lo);
                }

                if weights.len() == (*max - *min + 1) as usize {
                    // Берём только веса, попавшие в суженный сдвигами диапазон,
                    // иначе сужение молча перестало бы действовать.
                    let from = (lo - *min).max(0) as usize;
                    let to = (hi - *min).min(*max - *min) as usize;
                    let slice = &weights[from..=to];
                    if let Some(i) = self.pick_index(slice) {
                        return Value::Int(lo + i as i64);
                    }
                }

                Value::Int(self.rng.gen_range(lo..=hi))
            }

            Domain::Float { min, max } => {
                let (lo, hi) = apply_range(*min, *max, biases);
                Value::Float(if lo >= hi { lo } else { self.rng.gen_range(lo..hi) })
            }

            Domain::Bool { p_true } => {
                // Сдвиг веса применим и к булеву параметру: множитель работает
                // как отношение шансов. Без этого правило вида «психотерапевт
                // реже занимается детоксикацией» молча не срабатывало бы.
                let mut p = p_true.clamp(0.0, 1.0);
                for b in biases {
                    if let Bias::Weight { value, factor, .. } = b {
                        let f = factor.max(0.0);
                        p = match value.as_str() {
                            "true" => scale_odds(p, f),
                            "false" => 1.0 - scale_odds(1.0 - p, f),
                            _ => p,
                        };
                    }
                }
                Value::Bool(self.rng.gen_bool(p.clamp(0.0, 1.0)))
            }

            Domain::Enum { variants } => {
                let weights = weighted(variants, biases);
                match self.pick(variants, &weights) {
                    Some(v) => Value::Str(v),
                    None => Value::Null,
                }
            }

            Domain::MultiEnum { variants, min_pick, max_pick } => {
                let lo = *min_pick;
                let hi = (*max_pick).min(variants.len()).max(lo);
                let k = if lo >= hi { lo } else { self.rng.gen_range(lo..=hi) };

                let mut weights = weighted(variants, biases);
                let mut picked = Vec::with_capacity(k);
                for _ in 0..k.min(variants.len()) {
                    match self.pick_index(&weights) {
                        Some(i) => {
                            picked.push(variants[i].value.clone());
                            weights[i] = 0.0; // без повторов
                        }
                        None => break,
                    }
                }
                Value::List(picked)
            }

            Domain::Derived => Value::Null,
        }
    }

    fn pick(&mut self, variants: &[Variant], weights: &[f64]) -> Option<String> {
        self.pick_index(weights).map(|i| variants[i].value.clone())
    }

    fn pick_index(&mut self, weights: &[f64]) -> Option<usize> {
        if weights.is_empty() {
            return None;
        }
        if weights.iter().all(|w| *w <= 0.0) {
            return None;
        }
        match WeightedIndex::new(weights.iter().map(|w| w.max(0.0))) {
            Ok(dist) => Some(dist.sample(&mut self.rng)),
            Err(_) => Some(self.rng.gen_range(0..weights.len())),
        }
    }
}

/// Сдвиги, применимые к параметру: из мягких правил, чьё условие выполнено на
/// уже собранной части строки, плюс внешние (когортные).
fn collect_biases<'b>(
    model: &'b ParamModel,
    row: &ParamRow,
    key: &str,
    extra: &'b [Bias],
) -> Vec<&'b Bias> {
    let mut out: Vec<&Bias> = Vec::new();

    for rule in &model.soft {
        if !rule.when.holds(row) {
            continue;
        }
        for bias in &rule.then {
            if bias.target() == key {
                out.push(bias);
            }
        }
    }

    // Внешние идут последними — когорта важнее общей корреляции.
    for bias in extra {
        if bias.target() == key {
            out.push(bias);
        }
    }

    out
}

fn apply_range(mut lo: f64, mut hi: f64, biases: &[&Bias]) -> (f64, f64) {
    for b in biases {
        if let Bias::Range { min, max, .. } = b {
            if let Some(m) = min {
                lo = lo.max(*m);
            }
            if let Some(m) = max {
                hi = hi.min(*m);
            }
        }
    }
    if lo > hi {
        // Сужение схлопнуло диапазон — берём границу, а не паникуем.
        hi = lo;
    }
    (lo, hi)
}

/// Домножить шансы события с вероятностью `p` на `factor`.
///
/// Именно шансы, а не саму вероятность: умножение вероятности вылезло бы за
/// единицу, а отношение шансов остаётся корректным при любом множителе.
fn scale_odds(p: f64, factor: f64) -> f64 {
    if p <= 0.0 || factor <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return 1.0;
    }
    let odds = p / (1.0 - p) * factor;
    odds / (1.0 + odds)
}

fn weighted(variants: &[Variant], biases: &[&Bias]) -> Vec<f64> {
    let mut w: Vec<f64> = variants.iter().map(|v| v.weight.max(0.0)).collect();
    for b in biases {
        if let Bias::Weight { value, factor, .. } = b {
            if let Some(i) = variants.iter().position(|v| &v.value == value) {
                w[i] *= factor.max(0.0);
            }
        }
    }
    w
}
