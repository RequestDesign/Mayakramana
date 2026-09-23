//! Генератор персоналий.
//!
//! Здесь живёт машинерия, общая для всех людей: как разложить постановку, как
//! спросить у модели биографию и портрет, как принять ответ. Роли — врач,
//! психолог, консультант, руководитель — отличаются словарём и промптом, а не
//! логикой, поэтому у каждой будет свой тонкий крейд поверх этого.
//!
//! Генератор **не делает ввода-вывода**: не ходит в хранилище, не вызывает
//! модель, не знает про очередь. Отсюда два следствия — он тестируется без
//! сети, и смена провайдера или хранилища его не трогает.

use rand::rngs::StdRng;
use rand::SeedableRng;
use serde_json::json;
use synthforge_refdata::{NameBook, Sex};
use synthforge_textsim as textsim;
// `Usage` есть и в params (назначение параметра), и в ports (расход токенов).
// Алиас, чтобы читающий не гадал, какой из них имеется в виду.
use synthforge_params::{ParamModel, ParamRow, Sampler, Usage as PromptUsage, Value};
use synthforge_ports::{
    AcceptedText, EntityDescriptor, GenSpec, Generator, ImageRequest, ImageSize, Message,
    RejectReason, TextRequest,
};

mod claims;
mod prompt;

pub use claims::year_claims;
pub use prompt::{biography_schema, BIOGRAPHY_SYSTEM, PORTRAIT_AVOID};

/// Обороты, которых в карточке специалиста быть не должно.
///
/// Рекламный штамп — главный признак сгенерированного текста, заметный даже
/// беглому читателю. Ловится кодом, потому что просьба «не пиши рекламно»
/// соблюдается моделью через раз.
const FORBIDDEN: &[&str] = &[
    "ведущий специалист",
    "ведущий эксперт",
    "золотые руки",
    "уникальная методика",
    "уникальный подход",
    "не имеет аналогов",
    "лучший в своей области",
    "врач от бога",
    "как искусственный интеллект",
    "как ии",
    "вымышлен",
    "сгенерирован",
    // Анкетные обороты: признак того, что модель пересказала список параметров
    // вместо рассказа о человеке. Ловятся кодом, потому что просьба «не пиши
    // анкетой» соблюдается через раз.
    "мест работы",
    "места работы",
    "возрастной фокус",
    "повышений квалификации",
    "повышения квалификации",
    "курсов повышения",
    "научной деятельностью не",
];

/// Минимальная длина содержательных полей, знаков.
///
/// Свойство роли, а не константа. У консультанта фактов для рассказа меньше,
/// чем у врача, и биография естественно короче. Врачебный порог на
/// консультанте давал стабильный брак на 171 знаке при минимуме 180 — пять
/// перегенераций подряд, и тройная стоимость сущности.
#[derive(Debug, Clone, Copy)]
pub struct Lengths {
    pub biography: usize,
    pub path: usize,
    pub quote: usize,
}

impl Lengths {
    pub const DOCTOR: Lengths = Lengths { biography: 180, path: 150, quote: 25 };
    pub const CONSULTANT: Lengths = Lengths { biography: 140, path: 120, quote: 25 };
}

/// Год, относительно которого считается год рождения.
const REFERENCE_YEAR: i32 = 2026;

pub struct PersonGenerator {
    descriptor: EntityDescriptor,
    model: ParamModel,
    system_prompt: String,
    /// Справочник имён. Без него имя пришлось бы выдумывать модели, а она на
    /// десяти тысячах человек выдаёт полторы сотни разных имён, все из числа
    /// самых частотных, и без всякой связи с годом рождения.
    names: Option<NameBook>,
    lengths: Lengths,
    /// Параметры, означающие срок в годах. Числа в тексте сверяются с ними:
    /// всё, что не объясняется ни одной длительностью, — расхождение фактов.
    duration_params: Vec<&'static str>,
    /// Какая доля оборотов одного абзаца может встречаться в другом.
    max_paragraph_overlap: f32,
}

impl PersonGenerator {
    pub fn new(descriptor: EntityDescriptor, model: ParamModel) -> Self {
        Self {
            descriptor,
            model,
            system_prompt: BIOGRAPHY_SYSTEM.to_string(),
            names: None,
            lengths: Lengths::DOCTOR,
            duration_params: vec!["experience_years"],
            max_paragraph_overlap: 0.35,
        }
    }

    pub fn with_paragraph_overlap(mut self, max: f32) -> Self {
        self.max_paragraph_overlap = max;
        self
    }

    pub fn with_durations(mut self, params: &[&'static str]) -> Self {
        self.duration_params = params.to_vec();
        self
    }

    pub fn with_lengths(mut self, l: Lengths) -> Self {
        self.lengths = l;
        self
    }

    pub fn with_names(mut self, names: NameBook) -> Self {
        self.names = Some(names);
        self
    }

    /// Проставить ФИО из справочника.
    ///
    /// Сид берётся от постановки и номера строки, поэтому план остаётся
    /// воспроизводимым: тот же сид — те же люди с теми же именами.
    fn fill_name(&self, row: &mut ParamRow, seed: u64, index: usize) {
        let Some(book) = &self.names else { return };

        let Some(sex) = row
            .get("gender")
            .and_then(|v| v.as_str())
            .and_then(Sex::from_param)
        else {
            return;
        };
        let Some(age) = row.get("age").and_then(|v| v.as_i64()) else {
            return;
        };

        // Вид сущности входит в сид. Без этого психолог и руководитель,
        // сгенерированные с одним сидом, получали одинаковые фамилию и
        // отчество — на живом прогоне вышли «Захаров Андрей Викторович» и
        // «Захарова Наталья Викторовна». В одном центре это читалось бы как
        // семейный подряд.
        let kind_salt = self
            .descriptor
            .kind
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
        let mut rng = StdRng::seed_from_u64(
            seed ^ kind_salt ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        if let Some(name) = book.pick(&mut rng, sex, REFERENCE_YEAR - age as i32) {
            row.set("full_name", Value::Str(name.formal()));
        }
    }

    /// Врач-психиатр, нарколог. Словарь — `dictionaries/role-doctor.json`.
    pub fn doctor(model: ParamModel) -> Self {
        Self::new(
            EntityDescriptor {
                kind: "doctor".into(),
                collection: "doctors".into(),
                title: "Врачи".into(),
                images_per_entity: 1,
            },
            model,
        )
    }

    /// Консультант реабилитационного центра.
    ///
    /// Отдельная роль, а не подвид врача: другой путь в профессию, другое
    /// прошлое, другой язык. Общая с врачом машинерия переиспользуется,
    /// различия живут в словаре и в инструкции.
    pub fn consultant(model: ParamModel) -> Self {
        Self::new(
            EntityDescriptor {
                kind: "consultant".into(),
                collection: "consultants".into(),
                title: "Консультанты".into(),
                images_per_entity: 1,
            },
            model,
        )
        .with_system_prompt(prompt::CONSULTANT_SYSTEM)
        .with_lengths(Lengths::CONSULTANT)
        // Срок трезвости — законная длительность наравне со стажем.
        .with_durations(&["experience_years", "clean_years"])
    }

    /// Психолог: профильное образование обязательно, медикаментов не назначает.
    pub fn psychologist(model: ParamModel) -> Self {
        Self::new(
            EntityDescriptor {
                kind: "psychologist".into(),
                collection: "psychologists".into(),
                title: "Психологи".into(),
                images_per_entity: 1,
            },
            model,
        )
        .with_durations(&["experience_years", "addiction_years"])
    }

    /// Руководитель центра. Главное в нём — путь и мотивация, от них
    /// зависит, каким получится весь центр.
    pub fn director(model: ParamModel) -> Self {
        Self::new(
            EntityDescriptor {
                kind: "director".into(),
                collection: "directors".into(),
                title: "Руководители".into(),
                images_per_entity: 1,
            },
            model,
        )
        .with_system_prompt(prompt::DIRECTOR_SYSTEM)
        .with_durations(&["field_years", "leading_years", "clean_years"])
    }

    pub fn with_system_prompt(mut self, p: impl Into<String>) -> Self {
        self.system_prompt = p.into();
        self
    }

    fn int_param(&self, row: &ParamRow, key: &str) -> Option<i64> {
        row.get(key).and_then(|v| v.as_i64())
    }
}

impl Generator for PersonGenerator {
    fn descriptor(&self) -> &EntityDescriptor {
        &self.descriptor
    }

    fn model(&self) -> &ParamModel {
        &self.model
    }

    fn plan(&self, spec: &GenSpec) -> Result<Vec<ParamRow>, synthforge_params::Error> {
        let plan = spec.population_plan();
        let mut sampler = Sampler::new(&self.model, spec.seed);
        let mut rows: Vec<ParamRow> = sampler
            .sample_population(&plan)?
            .into_iter()
            .map(|s| s.row)
            .collect();

        for (i, row) in rows.iter_mut().enumerate() {
            self.fill_name(row, spec.seed, i);
        }

        Ok(rows)
    }

    fn text_request(&self, row: &ParamRow, brief: Option<&str>) -> TextRequest {
        let params = row.prompt_block(&self.model, PromptUsage::Text);
        let background = row.background_block(&self.model, PromptUsage::Text);
        let required = row.required_params(&self.model);

        let mut user = format!("ЧТО ИЗВЕСТНО (можно называть прямо):\n\n{params}");

        if !background.trim().is_empty() {
            user.push_str(&format!(
                "\nКАК ПИСАТЬ — это указания, а не факты о человеке.\n\
                 Ничего из перечисленного в текст переносить нельзя:\n{background}"
            ));
        }
        if !required.is_empty() {
            user.push_str(&format!(
                "\nОБЯЗАТЕЛЬНО УПОМЯНУТЬ:\n{}\n",
                required
                    .iter()
                    .map(|r| format!("• {r}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }

        if let Some(b) = brief.filter(|b| !b.trim().is_empty()) {
            // Бриф идёт как уточнение тона, а не как источник фактов: факты уже
            // заданы параметрами, и разрешать модели их дополнять нельзя.
            user.push_str(&format!(
                "\n\nОбщее пожелание к подаче (не источник фактов): {b}"
            ));
        }

        TextRequest::new(vec![
            Message::system(&self.system_prompt),
            Message::user(user),
        ])
        .schema(prompt::biography_schema_for(
            self.lengths.biography,
            self.lengths.path,
        ))
        .temperature(0.9)
        .max_tokens(1200)
    }

    fn image_requests(&self, row: &ParamRow) -> Vec<ImageRequest> {
        let visual = row.prompt_block(&self.model, PromptUsage::Visual);
        vec![ImageRequest::new(prompt::portrait_prompt(&visual))
            .role("portrait")
            .size(ImageSize::Portrait)
            .avoid(PORTRAIT_AVOID.iter().copied())]
    }

    fn accept_text(&self, row: &ParamRow, text: &str) -> Result<AcceptedText, RejectReason> {
        let parsed: serde_json::Value = {
            let t = text.trim();
            let cleaned = t
                .strip_prefix("```json")
                .or_else(|| t.strip_prefix("```"))
                .map(|s| s.trim_start())
                .and_then(|s| s.strip_suffix("```"))
                .map(|s| s.trim())
                .unwrap_or(t);
            serde_json::from_str(cleaned)
                .map_err(|e| RejectReason::NotStructured(format!("{e}: {}", head(text, 200))))?
        };

        let mut obj = parsed
            .as_object()
            .cloned()
            .ok_or_else(|| RejectReason::NotStructured("ответ не объект".into()))?;

        // --- механические исправления ----------------------------------------
        // Отбраковывается только то, что нельзя починить кодом. Повтор полного
        // имени чинится заменой на имя-отчество — это дешевле и надёжнее, чем
        // перегенерация: на живом прогоне модель повторяла имя пять раз подряд
        // для одного и того же человека, и каждая попытка стоила полного вызова.
        if let Some(name) = row.get("full_name").and_then(|v| v.as_str()) {
            repair_repeated_name(&mut obj, name);
        }

        // --- обязательные поля и их объём -----------------------------------
        for (field, min) in [
            ("biography", self.lengths.biography),
            ("professional_path", self.lengths.path),
            ("quote", self.lengths.quote),
        ] {
            let s = obj
                .get(field)
                .and_then(|v| v.as_str())
                .ok_or_else(|| RejectReason::MissingField(field.into()))?;
            let got = s.chars().count();
            if got < min {
                return Err(RejectReason::TooShort { field: field.into(), got, min });
            }
        }

        // --- абзацы не должны пересказывать друг друга ----------------------
        // Живой прогон дал консультанта, у которого второй абзац повторял
        // первый: «ведёт группы, наставничество, родственники» — дважды.
        // Меряем вложенностью, а не Жаккаром: короткий абзац, целиком
        // пересказывающий кусок длинного, по Жаккару выглядит непохожим.
        if let (Some(bio), Some(path)) = (
            obj.get("biography").and_then(|v| v.as_str()),
            obj.get("professional_path").and_then(|v| v.as_str()),
        ) {
            let (a, b) = (textsim::shingles(bio), textsim::shingles(path));
            let overlap = textsim::containment(&b, &a).max(textsim::containment(&a, &b));
            if overlap > self.max_paragraph_overlap {
                return Err(RejectReason::Forbidden(format!(
                    "абзацы пересказывают друг друга: {:.0}% общих оборотов",
                    overlap * 100.0
                )));
            }
        }

        // --- рекламные штампы ------------------------------------------------
        let prose = [
            obj.get("biography").and_then(|v| v.as_str()).unwrap_or(""),
            obj.get("professional_path").and_then(|v| v.as_str()).unwrap_or(""),
            obj.get("quote").and_then(|v| v.as_str()).unwrap_or(""),
        ]
        .join(" ");
        let prose_lower = prose.to_lowercase();

        if let Some(bad) = FORBIDDEN.iter().find(|f| prose_lower.contains(**f)) {
            return Err(RejectReason::Forbidden((*bad).to_string()));
        }

        // --- латиница в русском тексте ---------------------------------------
        // На живом прогоне вышло «стабильность и predictability». Кодом такое
        // не исправить — нужен перевод, — поэтому отбраковка.
        if let Some(word) = latin_word(&prose) {
            return Err(RejectReason::Forbidden(format!("латиница в тексте: «{word}»")));
        }

        // --- сверка эха модели с параметрами ---------------------------------
        // Модель обязана повторить, какими числами она пользовалась. Это точная
        // проверка, не зависящая от разбора прозы.
        let age = self.int_param(row, "age");
        let exp = self.int_param(row, "experience_years");

        for (field, expected) in [("stated_age", age), ("stated_experience_years", exp)] {
            let (Some(expected), Some(stated)) =
                (expected, obj.get(field).and_then(|v| v.as_i64()))
            else {
                continue;
            };
            if stated != expected {
                return Err(RejectReason::FactDrift {
                    param: field.trim_start_matches("stated_").to_string(),
                    expected: expected.to_string(),
                    found: stated.to_string(),
                });
            }
        }

        // --- сверка чисел в самой прозе --------------------------------------
        // Эхо могло совпасть, а в тексте всё равно оказаться «более двадцати
        // лет» при стаже 18. Любой срок в годах должен быть объясним.
        //
        // Законных длительностей у роли может быть несколько: у консультанта
        // кроме стажа есть срок трезвости. Первая версия знала только стаж и
        // отбраковывала верные тексты про «тринадцать лет трезвости».
        if let Some(age) = age {
            let durations: Vec<i64> = self
                .duration_params
                .iter()
                .filter_map(|k| self.int_param(row, k))
                .collect();
            let longest = durations.iter().copied().max().unwrap_or(0);

            for claim in year_claims(&prose) {
                // Срок объясним, если это отрезок внутри одной из длительностей
                // («первые пять лет»), либо возраст в какой-то момент этой
                // длительности («в 48 лет пришёл в наркологию» при возрасте 57
                // и стаже 9).
                let as_duration = claim > 0 && durations.iter().any(|d| claim <= *d);
                let as_age_at_moment = claim >= age - longest && claim <= age;

                if !(as_duration || as_age_at_moment) {
                    return Err(RejectReason::FactDrift {
                        param: if claim > age { "age".into() } else { "experience_years".into() },
                        expected: format!(
                            "возраст {age}, длительности {}",
                            self.duration_params
                                .iter()
                                .zip(&durations)
                                .map(|(k, v)| format!("{k}={v}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        found: format!("{claim} лет"),
                    });
                }
            }
        }

        Ok(AcceptedText { fields: obj })
    }

    fn uniqueness_text(&self, accepted: &AcceptedText) -> Option<String> {
        // Цитата короткая и почти всегда уникальна — сравниваем то, где
        // однообразие реально видно: биографию и путь в профессии.
        let bio = accepted.fields.get("biography")?.as_str()?;
        let path = accepted.fields.get("professional_path")?.as_str()?;
        Some(format!("{bio} {path}"))
    }

    fn assemble(&self, row: &ParamRow, accepted: &AcceptedText) -> serde_json::Value {
        let mut record = serde_json::Map::new();

        // Параметры как есть: по ним потом фильтруют витрину и подбирают
        // персонал под специализацию центра.
        for (key, value) in row.iter() {
            if matches!(value, Value::Null) {
                continue;
            }
            record.insert(key.clone(), value_to_json(value));
        }

        // Тексты от модели. Служебное эхо в запись не идёт — оно нужно было
        // только для приёмки.
        for field in ["biography", "professional_path", "quote"] {
            if let Some(v) = accepted.fields.get(field) {
                record.insert(field.to_string(), v.clone());
            }
        }

        record.insert("entity_kind".into(), json!(self.descriptor.kind));
        serde_json::Value::Object(record)
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Bool(b) => json!(b),
        Value::Int(i) => json!(i),
        Value::Float(f) => json!(f),
        Value::Str(s) => json!(s),
        Value::List(l) => json!(l),
        Value::Null => serde_json::Value::Null,
    }
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Первое латинское слово длиннее двух букв.
///
/// Короткие сочетания пропускаются: в русском тексте законно встречаются
/// обозначения вроде «ВИЧ» латиницей в цитатах или единичные буквы. Всё, что
/// длиннее, — это слово на другом языке.
fn latin_word(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .find(|w| w.chars().count() >= 3 && w.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_string)
}

/// Оставить полное ФИО только при первом упоминании, дальше — имя и отчество.
///
/// «Иванов Пётр Сергеевич окончил… Иванов Пётр Сергеевич опирается…» →
/// «Иванов Пётр Сергеевич окончил… Пётр Сергеевич опирается…».
fn repair_repeated_name(obj: &mut serde_json::Map<String, serde_json::Value>, full: &str) {
    let polite: String = full.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
    if polite.is_empty() {
        return;
    }

    let mut seen = false;
    for field in ["biography", "professional_path", "quote"] {
        let Some(serde_json::Value::String(text)) = obj.get_mut(field) else { continue };

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
