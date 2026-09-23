//! Генератор объектов.
//!
//! Объект — то, что описывается параметрами, но не имеет биографии и имени:
//! здание с территорией, программа лечения. Логика у них общая, поэтому один
//! генератор, а различия — в словаре, наборе текстовых полей и снимках.
//!
//! Отличие от персоналий принципиальное, а не косметическое: у объекта нет
//! справочника имён, нет эха возраста и стажа, зато есть несколько снимков
//! разного назначения — фасад, комната, территория. Смешивать это с логикой
//! людей значило бы обвешивать один генератор условиями «если это не человек».

use serde_json::json;
use synthforge_params::{ParamModel, ParamRow, Sampler, Usage as PromptUsage, Value};
use synthforge_ports::{
    AcceptedText, EntityDescriptor, GenSpec, Generator, ImageRequest, ImageSize, Message,
    RejectReason, TextRequest,
};
use synthforge_textsim as textsim;

/// Текстовое поле объекта, которое пишет модель.
#[derive(Debug, Clone)]
pub struct TextField {
    pub key: &'static str,
    /// Что писать — уходит в описание схемы ответа.
    pub what: &'static str,
    pub min_len: usize,
}

/// Снимок объекта.
#[derive(Debug, Clone)]
pub struct ShotSpec {
    pub role: &'static str,
    /// Что в кадре. Параметры объекта подклеиваются после.
    pub scene: &'static str,
    pub size: ImageSize,
}

/// Обороты, недопустимые в описании объекта: рекламные штампы и обещания.
const FORBIDDEN: &[&str] = &[
    "уникальн",
    "лучший",
    "лучшие условия",
    "гарантируем",
    "гарантия",
    "100%",
    "стопроцентн",
    "эффективность доказана",
    "не имеет аналогов",
    "европейского уровня",
    "премиум-класса",
    "вымышлен",
    "сгенерирован",
];

/// Для снимков объектов: всё, что превращает фото в рекламный буклет.
const SHOT_AVOID: &[&str] = &[
    "рекламная глянцевая съёмка",
    "перенасыщенные цвета и HDR",
    "люди в кадре",
    "текст, подписи, водяные знаки, логотипы",
    "идеально пустой стерильный интерьер из каталога мебели",
];

pub struct ObjectGenerator {
    descriptor: EntityDescriptor,
    model: ParamModel,
    system_prompt: String,
    fields: Vec<TextField>,
    shots: Vec<ShotSpec>,
    max_field_overlap: f32,
}

impl ObjectGenerator {
    pub fn new(
        descriptor: EntityDescriptor,
        model: ParamModel,
        system_prompt: impl Into<String>,
        fields: Vec<TextField>,
        shots: Vec<ShotSpec>,
    ) -> Self {
        Self {
            descriptor,
            model,
            system_prompt: system_prompt.into(),
            fields,
            shots,
            max_field_overlap: 0.35,
        }
    }

    /// Здание с территорией. Словарь — `dictionaries/object-place.json`.
    pub fn place(model: ParamModel) -> Self {
        Self::new(
            EntityDescriptor {
                kind: "place".into(),
                collection: "places".into(),
                title: "Здания".into(),
                images_per_entity: 3,
            },
            model,
            PLACE_SYSTEM,
            vec![
                TextField {
                    key: "description",
                    what: "Описание дома и того, как в нём живут: 4-6 предложений",
                    min_len: 220,
                },
                TextField {
                    key: "territory",
                    what: "Территория и окружение: 3-4 предложения",
                    min_len: 150,
                },
                TextField {
                    key: "rooms",
                    what: "Как устроены комнаты для проживания: 2-3 предложения",
                    min_len: 100,
                },
            ],
            vec![
                ShotSpec {
                    role: "facade",
                    scene: "Фасад здания и вход, снято с дорожки перед домом",
                    size: ImageSize::Landscape,
                },
                ShotSpec {
                    role: "room",
                    scene: "Жилая комната для проживания, снято от двери",
                    size: ImageSize::Landscape,
                },
                ShotSpec {
                    role: "territory",
                    scene: "Территория вокруг дома и ближайшее окружение",
                    size: ImageSize::Landscape,
                },
            ],
        )
    }

    /// Программа лечения. Словарь — `dictionaries/object-program.json`.
    ///
    /// Программы генерируются отдельным пулом и потом привязываются к центрам,
    /// так же как персонал.
    pub fn program(model: ParamModel) -> Self {
        Self::new(
            EntityDescriptor {
                kind: "program".into(),
                collection: "programs".into(),
                title: "Программы".into(),
                images_per_entity: 0,
            },
            model,
            PROGRAM_SYSTEM,
            vec![
                TextField {
                    key: "title",
                    what: "Короткое название программы, 2-5 слов, без кавычек и рекламы",
                    min_len: 8,
                },
                TextField {
                    key: "description",
                    what: "Как устроена программа и для кого она: 4-6 предложений",
                    min_len: 220,
                },
                TextField {
                    key: "stages",
                    what: "Этапы программы по порядку, связным текстом: 3-5 предложений",
                    min_len: 160,
                },
            ],
            Vec::new(),
        )
    }

    fn schema(&self) -> serde_json::Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for f in &self.fields {
            props.insert(
                f.key.to_string(),
                json!({
                    "type": "string",
                    "description": format!("{}. Не короче {} знаков.", f.what, f.min_len + 40)
                }),
            );
            required.push(f.key);
        }
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": required,
            "properties": props,
        })
    }
}

impl Generator for ObjectGenerator {
    fn descriptor(&self) -> &EntityDescriptor {
        &self.descriptor
    }

    fn model(&self) -> &ParamModel {
        &self.model
    }

    fn plan(&self, spec: &GenSpec) -> Result<Vec<ParamRow>, synthforge_params::Error> {
        let mut s = Sampler::new(&self.model, spec.seed);
        Ok(s.sample_population(&spec.population_plan())?
            .into_iter()
            .map(|x| x.row)
            .collect())
    }

    fn text_request(&self, row: &ParamRow, brief: Option<&str>) -> TextRequest {
        let known = row.prompt_block(&self.model, PromptUsage::Text);
        let background = row.background_block(&self.model, PromptUsage::Text);

        let mut user = format!("ЧТО ИЗВЕСТНО (можно называть прямо):\n\n{known}");
        if !background.trim().is_empty() {
            user.push_str(&format!(
                "\nКАК ПИСАТЬ — это указания, а не факты. В текст не переносить:\n{background}"
            ));
        }
        if let Some(b) = brief.filter(|b| !b.trim().is_empty()) {
            user.push_str(&format!("\n\nОбщее пожелание к подаче (не источник фактов): {b}"));
        }

        TextRequest::new(vec![Message::system(&self.system_prompt), Message::user(user)])
            .schema(self.schema())
            .temperature(0.9)
            .max_tokens(1400)
    }

    fn image_requests(&self, row: &ParamRow) -> Vec<ImageRequest> {
        let visual = row.prompt_block(&self.model, PromptUsage::Visual);
        self.shots
            .iter()
            .map(|s| {
                ImageRequest::new(format!(
                    "{}. Документальная фотография реального места, естественный свет, \
                     обычная камера, без постановки.\n\n{visual}",
                    s.scene
                ))
                .role(s.role)
                .size(s.size)
                .avoid(SHOT_AVOID.iter().copied())
            })
            .collect()
    }

    fn accept_text(&self, _row: &ParamRow, text: &str) -> Result<AcceptedText, RejectReason> {
        let t = text.trim();
        let cleaned = t
            .strip_prefix("```json")
            .or_else(|| t.strip_prefix("```"))
            .map(|s| s.trim_start())
            .and_then(|s| s.strip_suffix("```"))
            .map(|s| s.trim())
            .unwrap_or(t);
        let v: serde_json::Value = serde_json::from_str(cleaned)
            .map_err(|e| RejectReason::NotStructured(e.to_string()))?;
        let obj = v
            .as_object()
            .cloned()
            .ok_or_else(|| RejectReason::NotStructured("ответ не объект".into()))?;

        for f in &self.fields {
            let s = obj
                .get(f.key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| RejectReason::MissingField(f.key.into()))?;
            let got = s.chars().count();
            if got < f.min_len {
                return Err(RejectReason::TooShort { field: f.key.into(), got, min: f.min_len });
            }
        }

        let prose: String = self
            .fields
            .iter()
            .filter_map(|f| obj.get(f.key).and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        let lower = prose.to_lowercase();

        if let Some(bad) = FORBIDDEN.iter().find(|f| lower.contains(**f)) {
            return Err(RejectReason::Forbidden((*bad).to_string()));
        }

        if let Some(w) = prose
            .split(|c: char| !c.is_alphanumeric())
            .find(|w| w.chars().count() >= 3 && w.chars().all(|c| c.is_ascii_alphabetic()))
        {
            return Err(RejectReason::Forbidden(format!("латиница в тексте: «{w}»")));
        }

        // Длинные поля не должны пересказывать друг друга.
        let long: Vec<&str> = self
            .fields
            .iter()
            .filter(|f| f.min_len >= 100)
            .filter_map(|f| obj.get(f.key).and_then(|v| v.as_str()))
            .collect();
        for i in 0..long.len() {
            for j in (i + 1)..long.len() {
                let (a, b) = (textsim::shingles(long[i]), textsim::shingles(long[j]));
                let overlap = textsim::containment(&a, &b).max(textsim::containment(&b, &a));
                if overlap > self.max_field_overlap {
                    return Err(RejectReason::Forbidden(format!(
                        "поля пересказывают друг друга: {:.0}% общих оборотов",
                        overlap * 100.0
                    )));
                }
            }
        }

        Ok(AcceptedText { fields: obj })
    }

    fn assemble(&self, row: &ParamRow, accepted: &AcceptedText) -> serde_json::Value {
        let mut record = serde_json::Map::new();
        for (k, v) in row.iter() {
            let j = match v {
                Value::Null => continue,
                Value::Bool(b) => json!(b),
                Value::Int(i) => json!(i),
                Value::Float(f) => json!(f),
                Value::Str(s) => json!(s),
                Value::List(l) => json!(l),
            };
            record.insert(k.clone(), j);
        }
        for f in &self.fields {
            if let Some(v) = accepted.fields.get(f.key) {
                record.insert(f.key.to_string(), v.clone());
            }
        }
        record.insert("entity_kind".into(), json!(self.descriptor.kind));
        serde_json::Value::Object(record)
    }

    fn uniqueness_text(&self, accepted: &AcceptedText) -> Option<String> {
        let text: Vec<&str> = self
            .fields
            .iter()
            .filter(|f| f.min_len >= 100)
            .filter_map(|f| accepted.fields.get(f.key).and_then(|v| v.as_str()))
            .collect();
        (!text.is_empty()).then(|| text.join(" "))
    }
}

/// Инструкция для здания.
pub const PLACE_SYSTEM: &str = "\
Ты пишешь описание дома, в котором находится реабилитационный центр, для
страницы центра.

Все факты о доме уже заданы. Твоя работа — описать его связным текстом так,
чтобы человек, выбирающий место для близкого, понял, как там живут. Не
перечисляй параметры.

Требования:
1. Не добавляй удобств, помещений и расстояний, которых нет в данных.
2. Пиши как очевидец, сдержанно: что видно, чем пахнет, что слышно. Никакой
   рекламы, никаких «лучших условий» и «европейского уровня».
3. Уровень цены должен чувствоваться по описанию, но слово «эконом» или
   «премиум» не произносится.
4. Не упоминай, что текст сгенерирован, и не обращайся к читателю.

Верни строго JSON по заданной схеме, без пояснений вокруг.";

/// Инструкция для программы.
pub const PROGRAM_SYSTEM: &str = "\
Ты описываешь программу реабилитации для страницы центра.

Все параметры программы уже заданы. Твоя работа — объяснить, как она устроена и
чего ждать человеку, который в неё приходит. Не перечисляй параметры.

Требования:
1. Не обещай результата. Никаких процентов выздоровления, гарантий и
   «доказанной эффективности».
2. Не добавляй этапов, сроков и методик, которых нет в данных.
3. Пиши понятным языком для родственника, который впервые с этим столкнулся.
4. Не упоминай, что текст сгенерирован, и не обращайся к читателю.

Верни строго JSON по заданной схеме, без пояснений вокруг.";

#[cfg(test)]
mod tests {
    use super::*;

    fn place() -> ObjectGenerator {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../dictionaries/object-place.json");
        ObjectGenerator::place(ParamModel::load(path).unwrap_or_else(|e| panic!("{e}")))
    }

    fn good() -> String {
        json!({
            "description": "Двухэтажный кирпичный дом стоит в глубине участка, за ним начинается \
                сосновый лес. На первом этаже столовая и большая комната для групп, окна которой \
                выходят на поляну. Утро начинается с общей зарядки, после завтрака идут занятия. \
                Вечером часто собираются на веранде, где пахнет смолой и дымом из бани.",
            "territory": "Участок огорожен, по периметру растут старые сосны. От дома к озеру \
                ведёт тропинка, по ней ходят на прогулки после обеда. Ближайшая деревня в \
                нескольких километрах, машин почти не слышно.",
            "rooms": "Живут по двое-трое в комнате, у каждого своя кровать, тумбочка и полка \
                в общем шкафу. Окна большие, днём в комнатах светло."
        })
        .to_string()
    }

    #[test]
    fn well_formed_description_is_accepted() {
        let g = place();
        let row = g.plan(&GenSpec::new(1)).unwrap().remove(0);
        g.accept_text(&row, &good()).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn advertising_is_rejected() {
        let g = place();
        let row = g.plan(&GenSpec::new(1)).unwrap().remove(0);
        let mut v: serde_json::Value = serde_json::from_str(&good()).unwrap();
        // Текст нарочно длиннее порога: иначе отбраковка сработала бы по
        // длине, и тест проверял бы не то.
        v["description"] = json!(
            "Уникальный центр европейского уровня в сосновом лесу. Лучшие условия для \
             восстановления, гарантируем результат каждому, кто к нам обратится. \
             Двухэтажный дом, просторная столовая, комната для групп, веранда и баня на \
             участке, всё продумано до мелочей и сделано с заботой о каждом."
        );
        assert!(matches!(
            g.accept_text(&row, &v.to_string()).unwrap_err(),
            RejectReason::Forbidden(_)
        ));
    }

    #[test]
    fn place_has_three_shots_of_different_roles() {
        let g = place();
        let row = g.plan(&GenSpec::new(1)).unwrap().remove(0);
        let shots = g.image_requests(&row);
        let roles: Vec<&str> = shots.iter().map(|s| s.role.as_str()).collect();
        assert_eq!(roles, vec!["facade", "room", "territory"]);
        assert!(shots.iter().all(|s| s.full_prompt().contains("люди в кадре")));
    }

/// Текст и снимок одного дома должны описывать одно и то же. На живом прогоне
    /// «частный коттедж в лесу» получил фото казённого трёхэтажного здания:
    /// тип здания и окружение уходили только в текст.
    #[test]
    fn shots_know_what_the_building_is() {
        let g = place();
        let row = g.plan(&GenSpec::new(1).seed(4)).unwrap().remove(0);
        let facade = &g.image_requests(&row)[0];

        for key in ["building_type", "setting", "floors"] {
            let v = row.get(key).unwrap().render();
            assert!(facade.prompt.contains(&v), "«{key}» = «{v}» не попал в промпт фасада");
        }
    }

    #[test]
    fn schema_names_every_field_with_target_length() {
        let g = place();
        let s = g.schema();
        for f in ["description", "territory", "rooms"] {
            let d = s["properties"][f]["description"].as_str().unwrap();
            assert!(d.contains("Не короче"), "{d}");
        }
    }
}
