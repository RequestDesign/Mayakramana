//! Предпросмотр словаря: что реально порождает невидимый слой.
//!
//! ```text
//! cargo run -p synthforge-params --example preview -- role-doctor 3
//! cargo run -p synthforge-params --example preview -- role-doctor 500 --stats
//! ```
//!
//! Первый режим показывает готовые строки — ровно то, что уйдёт в промпт.
//! Второй считает распределения по всей популяции: так видно перекосы словаря
//! до того, как на генерацию потрачены деньги.

use std::collections::BTreeMap;
use std::path::PathBuf;

use synthforge_params::{Domain, ParamModel, PopulationPlan, Sampler, Usage};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let name = args.first().map(String::as_str).unwrap_or("role-doctor");
    let count: usize = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let stats = args.iter().any(|a| a == "--stats");
    let seed: u64 = args
        .iter()
        .position(|a| a == "--seed")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(2026);

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../dictionaries")
        .join(format!("{name}.json"));

    let model = match ParamModel::load(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Словарь не загрузился:\n{e}");
            std::process::exit(1);
        }
    };

    println!("Словарь: {} (версия {})", model.name, model.version);
    println!(
        "Параметров: {} · жёстких правил: {} · корреляций: {}",
        model.params.len(),
        model.hard.len(),
        model.soft.len()
    );

    let sampled_params = model.params.iter().filter(|p| p.domain.is_sampled()).count();
    println!(
        "Сэмплируется кодом: {sampled_params} · заполняется моделью: {}",
        model.params.len() - sampled_params
    );
    println!("Сид: {seed}\n");

    let mut sampler = Sampler::new(&model, seed);
    let pop = match sampler.sample_population(&PopulationPlan::uniform(count)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Не удалось собрать популяцию:\n{e}");
            std::process::exit(1);
        }
    };

    if stats {
        print_stats(&model, &pop);
    } else {
        for (i, s) in pop.iter().enumerate() {
            println!("══════════ {} из {} ══════════", i + 1, pop.len());
            println!("── в промпт биографии ──");
            print!("{}", s.row.prompt_block(&model, Usage::Text));
            println!("\n── в промпт портрета ──");
            print!("{}", s.row.prompt_block(&model, Usage::Visual));
            println!();
        }
    }
}

fn print_stats(model: &ParamModel, pop: &[synthforge_params::Sampled]) {
    println!("Распределения по {} сущностям:\n", pop.len());

    for p in &model.params {
        if !p.domain.is_sampled() {
            continue;
        }

        match &p.domain {
            Domain::Int { .. } | Domain::Float { .. } => {
                let vals: Vec<f64> = pop
                    .iter()
                    .filter_map(|s| s.row.get(&p.key).and_then(|v| v.as_f64()))
                    .collect();
                if vals.is_empty() {
                    continue;
                }
                let mean = vals.iter().sum::<f64>() / vals.len() as f64;
                let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
                let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                println!("{} — среднее {mean:.1}, от {min:.0} до {max:.0}", p.title);
            }

            _ => {
                let mut counts: BTreeMap<String, usize> = BTreeMap::new();
                for s in pop {
                    match s.row.get(&p.key) {
                        Some(synthforge_params::Value::List(items)) => {
                            for it in items {
                                *counts.entry(it.clone()).or_default() += 1;
                            }
                        }
                        Some(v) => *counts.entry(v.render()).or_default() += 1,
                        None => {}
                    }
                }
                if counts.is_empty() {
                    continue;
                }

                println!("{}:", p.title);
                let mut rows: Vec<_> = counts.into_iter().collect();
                rows.sort_by(|a, b| b.1.cmp(&a.1));
                for (value, n) in rows {
                    let share = n as f64 / pop.len() as f64;
                    let bar = "█".repeat((share * 40.0).round() as usize);
                    println!("  {:>5.1}% {bar:<40} {value}", share * 100.0);
                }
            }
        }
        println!();
    }
}
