//! Центр — составная сущность, собираемая последней.
//!
//! Ничего не сочиняет: здание, программа, руководитель и команда берутся из
//! уже созданных пулов. Модели остаётся только написать о центре — название,
//! «о нас» и как здесь работают, — опираясь на то, что уже есть.
//!
//! Поэтому центр и получается целостным. Отзыв, страница команды, описание
//! дома и программа сходятся друг с другом не потому, что модель постаралась
//! быть последовательной, а потому, что все они ссылаются на одни и те же
//! записи.

use std::collections::HashMap;

use serde_json::Value as Json;
use synthforge_gen_object::{ObjectGenerator, TextField};
use synthforge_params::{ParamModel, ParamRow, Value};
use synthforge_ports::{
    AcceptedText, EntityDescriptor, GenSpec, Generator, ImageRequest, RejectReason, TextRequest,
};

mod compose;

pub use compose::{
    compose, fit, surname_root, team_size, Candidate, CenterPlan, ComposeError, Pools, Role,
};

pub struct CenterGenerator {
    text: ObjectGenerator,
    pools: Pools,
    index: HashMap<String, Json>,
    /// Справочник названий. Название выбирает код, а не модель: на живом
    /// прогоне модель писала названия из пяти слов с регионом и методом, и
    /// каждое уходило в брак — полной перегенерацией за деньги.
    names: Vec<String>,
}

impl CenterGenerator {
    pub fn new(model: ParamModel, pools: Pools) -> Self {
        let text = ObjectGenerator::new(
            EntityDescriptor {
                kind: "center".into(),
                collection: "centers".into(),
                title: "Центры".into(),
                // Снимки у центра свои не делаются: это снимки его здания.
                images_per_entity: 0,
            },
            model,
            CENTER_SYSTEM,
            vec![
                TextField {
                    key: "about",
                    what: "О центре: кто его создал, для кого он, как устроена жизнь, 5-7 предложений",
                    min_len: 260,
                },
                TextField {
                    key: "approach_text",
                    what: "Как здесь работают с людьми: программа и команда, 3-5 предложений",
                    min_len: 170,
                },
            ],
            Vec::new(),
        );

        let index = pools
            .places
            .iter()
            .chain(&pools.programs)
            .chain(&pools.directors)
            .chain(&pools.doctors)
            .chain(&pools.psychologists)
            .chain(&pools.consultants)
            .map(|c| (c.key.clone(), c.record.clone()))
            .collect();

        Self { text, pools, index, names: Vec::new() }
    }

    pub fn with_names(mut self, names: Vec<String>) -> Self {
        self.names = names;
        self
    }

    /// Раздать названия: каждое один раз на всю базу, с учётом уже занятых.
    fn assign_names(&self, n: usize, seed: u64) -> Result<Vec<String>, String> {
        use rand::seq::SliceRandom;
        use rand::SeedableRng;

        let mut free: Vec<&String> = self
            .names
            .iter()
            .filter(|name| !self.pools.reserved.contains(&format!("name:{name}")))
            .collect();

        if free.len() < n {
            return Err(format!(
                "свободных названий {} при нужных {n}: справочник названий исчерпан",
                free.len()
            ));
        }

        let mut rng = rand::rngs::StdRng::seed_from_u64(seed ^ 0x5eed_c0de);
        free.shuffle(&mut rng);
        Ok(free.into_iter().take(n).cloned().collect())
    }

    fn rec(&self, key: &str) -> &Json {
        static EMPTY: Json = Json::Null;
        self.index.get(key).unwrap_or(&EMPTY)
    }

    /// Строка параметров центра из собранного плана.
    fn row_for(&self, plan: &CenterPlan) -> ParamRow {
        let place = self.rec(&plan.place);
        let program = self.rec(&plan.program);
        let director = self.rec(&plan.director);

        let s = |r: &Json, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let i = |r: &Json, k: &str| r.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let list = |r: &Json, k: &str| -> Vec<String> {
            r.get(k)
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };

        let mut row = ParamRow::new();
        row.set("place_key", Value::Str(plan.place.clone()));
        row.set("program_key", Value::Str(plan.program.clone()));
        row.set("director_key", Value::Str(plan.director.clone()));
        row.set("staff_keys", Value::List(plan.staff().cloned().collect()));

        row.set("segment", Value::Str(plan.segment.clone()));
        row.set("focus", Value::Str(plan.focus.clone()));
        row.set("region", Value::Str(s(place, "region")));
        row.set("setting", Value::Str(s(place, "setting")));
        row.set("distance_km", Value::Int(i(place, "distance_km")));
        row.set("building_type", Value::Str(s(place, "building_type")));
        row.set("capacity", Value::Int(i(place, "capacity")));
        row.set("room_type", Value::Str(s(place, "room_type")));
        row.set("amenities", Value::List(list(place, "amenities")));

        row.set("program_title", Value::Str(s(program, "title")));
        row.set("program_approach", Value::Str(s(program, "approach")));
        row.set("program_duration", Value::Str(s(program, "duration")));

        row.set("director_name", Value::Str(s(director, "full_name")));
        row.set("director_background", Value::Str(s(director, "background")));
        row.set("director_motivation", Value::Str(s(director, "motivation")));

        // Команда подаётся модели одной строкой: имя и то, чем человек
        // занимается. Этого достаточно, чтобы рассказ о центре ссылался на
        // реальных людей, а не на «опытных специалистов».
        let person = |key: &String, what: &str| {
            let r = self.rec(key);
            format!("{} ({})", s(r, "full_name"), s(r, what))
        };
        let team = [
            ("врачи", plan.doctors.iter().map(|k| person(k, "specialty")).collect::<Vec<_>>()),
            ("психологи", plan.psychologists.iter().map(|k| person(k, "education")).collect()),
            ("консультанты", plan.consultants.iter().map(|k| person(k, "path_to_work")).collect()),
        ]
        .into_iter()
        .map(|(role, people)| format!("{role}: {}", people.join("; ")))
        .collect::<Vec<_>>()
        .join(". ");
        row.set("team", Value::Str(team));

        row
    }
}

impl Generator for CenterGenerator {
    fn descriptor(&self) -> &EntityDescriptor {
        self.text.descriptor()
    }

    fn model(&self) -> &ParamModel {
        self.text.model()
    }

    fn plan(&self, spec: &GenSpec) -> Result<Vec<ParamRow>, synthforge_params::Error> {
        let plans = compose(&self.pools, spec.count, spec.seed)
            .map_err(|e| synthforge_params::Error::Invalid(vec![e.to_string()]))?;

        let names = if self.names.is_empty() {
            Vec::new()
        } else {
            self.assign_names(plans.len(), spec.seed)
                .map_err(|e| synthforge_params::Error::Invalid(vec![e]))?
        };

        Ok(plans
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut row = self.row_for(p);
                if let Some(name) = names.get(i) {
                    row.set("center_name", Value::Str(name.clone()));
                }
                row
            })
            .collect())
    }

    fn text_request(&self, row: &ParamRow, brief: Option<&str>) -> TextRequest {
        self.text.text_request(row, brief)
    }

    fn image_requests(&self, _row: &ParamRow) -> Vec<ImageRequest> {
        Vec::new()
    }

    fn accept_text(&self, row: &ParamRow, text: &str) -> Result<AcceptedText, RejectReason> {
        let mut accepted = self.text.accept_text(row, text)?;

        if let Some(full) = row.get("director_name").and_then(|v| v.as_str()) {
            repair_name(&mut accepted.fields, full);
        }

        // Центр обязан говорить о своих людях, а не об «опытных
        // специалистах»: руководитель должен быть назван по имени.
        if let Some(name) = row.get("director_name").and_then(|v| v.as_str()) {
            let surname = name.split_whitespace().next().unwrap_or(name);
            let about = accepted.fields.get("about").and_then(|v| v.as_str()).unwrap_or("");
            let approach =
                accepted.fields.get("approach_text").and_then(|v| v.as_str()).unwrap_or("");
            if !surname.is_empty() && !about.contains(surname) && !approach.contains(surname) {
                return Err(RejectReason::MissingField(format!(
                    "руководитель «{name}» в тексте о центре не назван"
                )));
            }
        }

        Ok(accepted)
    }

    fn assemble(&self, row: &ParamRow, accepted: &AcceptedText) -> serde_json::Value {
        self.text.assemble(row, accepted)
    }

    fn uniqueness_text(&self, accepted: &AcceptedText) -> Option<String> {
        self.text.uniqueness_text(accepted)
    }
}

pub const CENTER_SYSTEM: &str = "\
Ты пишешь главную страницу реабилитационного центра.

Всё о центре уже известно: здание, программа, руководитель и люди, которые там
работают. Твоя работа — рассказать о центре связно, опираясь только на это.

Требования:
1. Руководителя называй по имени. Если упоминаешь сотрудников — только тех, кто
   перечислен в команде, с их настоящими именами.
2. Не добавляй людей, удобств, методик и цифр, которых нет в данных.
3. Не обещай результата: никаких процентов выздоровления и гарантий.
4. Никакой рекламы: ни «лучших», ни «уникальных», ни «европейского уровня».
   Уровень цены должен чувствоваться, но слова «эконом» и «премиум» не звучат.
5. Название центра уже задано. Используй его как есть, не меняй и не
   придумывай своё.
6. Полное имя руководителя назови один раз; дальше — по имени и отчеству.
7. Не упоминай, что текст сгенерирован, и не обращайся к читателю.

Верни строго JSON по заданной схеме, без пояснений вокруг.";

/// Проверка названия центра.
///
/// Первый живой прогон дал «Реабилитационный уральский центр
/// двенадцатишагов»: длинно, с регионом, с методом и со склеенным словом.
/// Так центры не называются. Проверка ловит признаки такого названия, а не
/// пытается оценить вкус.
pub fn check_center_name(name: &str) -> Result<(), String> {
    let words: Vec<&str> = name.split_whitespace().collect();
    if words.is_empty() {
        return Err("пустое название".into());
    }
    if words.len() > 3 {
        return Err(format!("название из {} слов: «{name}»", words.len()));
    }
    let lower = name.to_lowercase();
    for bad in ["центр", "реабилит", "клиник", "шагов", "миннесот", "урал", "сибир", "поволж",
                "подмосков", "элитн", "лучш", "премиум"] {
        if lower.contains(bad) {
            return Err(format!("в названии «{name}» есть «{bad}»"));
        }
    }
    if name.chars().any(|c| c.is_ascii_digit()) {
        return Err(format!("цифры в названии «{name}»"));
    }
    Ok(())
}

/// Оставить полное имя только при первом упоминании, дальше — имя и отчество.
fn repair_name(fields: &mut serde_json::Map<String, Json>, full: &str) {
    let polite: String = full.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
    if polite.is_empty() {
        return;
    }
    let mut seen = false;
    for key in ["about", "approach_text"] {
        let Some(Json::String(text)) = fields.get_mut(key) else { continue };
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(pos) = rest.find(full) {
            out.push_str(&rest[..pos]);
            out.push_str(if seen { &polite } else { full });
            seen = true;
            rest = &rest[pos + full.len()..];
        }
        out.push_str(rest);
        *text = out;
    }
}

#[cfg(test)]
mod tests;
