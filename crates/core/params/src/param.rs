use serde::{Deserialize, Serialize};

use crate::value::Value;

/// Куда параметр попадает при генерации.
///
/// Разделение существует, потому что у сущности два вектора: кто это по жизни
/// и как он выглядит. Текстовой модели незачем знать про мешки под глазами,
/// модели изображений незачем знать, в каком вузе он учился.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Usage {
    /// В промпт биографии и описаний.
    #[default]
    Text,
    /// В промпт портрета.
    Visual,
    /// В оба.
    Both,
    /// Никуда: служебный параметр, нужен только для правил связности и фильтров.
    StoreOnly,
}

impl Usage {
    /// Уходит ли параметр в промпт указанного назначения.
    pub fn goes_to(self, target: Usage) -> bool {
        match (self, target) {
            (Usage::StoreOnly, _) => false,
            (_, Usage::StoreOnly) => false,
            (Usage::Both, _) => true,
            (a, b) => a == b,
        }
    }
}

/// Как параметр подаётся модели.
///
/// Нужно, потому что первый же живой прогон дал не биографию, а пересказ
/// списка: «прошёл шесть повышений квалификации», «за карьеру работал в двух
/// местах», «возрастной фокус — подростки». Часть параметров существует для
/// связности и фильтров, а не для того, чтобы их называли вслух.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mention {
    /// Можно упомянуть, если ложится в текст естественно.
    #[default]
    Natural,
    /// Задаёт фон, но называть прямо нельзя. Национальность влияет на имя и
    /// внешность, а не на фразу «специалист украинского происхождения».
    Background,
    /// Должен прозвучать обязательно.
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub value: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Относительный вес. Ноль означает «возможно, но не сэмплируется само».
    #[serde(default = "one")]
    pub weight: f64,
}

fn one() -> f64 {
    1.0
}

impl Variant {
    pub fn new(value: impl Into<String>, weight: f64) -> Self {
        Self { value: value.into(), label: None, weight }
    }
}

/// Область допустимых значений параметра.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Domain {
    Int {
        min: i64,
        max: i64,
        /// Веса по значениям от `min` до `max` включительно.
        ///
        /// Пусто — равномерно. Но равномерность почти всегда неверна: детей у
        /// людей не поровну от нуля до четырёх, и публикаций тоже. Равномерный
        /// параметр раздувает хвосты распределения и портит популяцию.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        weights: Vec<f64>,
    },
    Float {
        min: f64,
        max: f64,
    },
    Bool {
        #[serde(default = "half")]
        p_true: f64,
    },
    /// Один вариант из списка.
    Enum {
        variants: Vec<Variant>,
    },
    /// Несколько вариантов из списка: «работал с наркозависимыми и алкоголиками».
    MultiEnum {
        variants: Vec<Variant>,
        #[serde(default)]
        min_pick: usize,
        #[serde(default = "one_usize")]
        max_pick: usize,
    },
    /// Значение не сэмплируется — его заполнит модель или другой шаг пайплайна.
    Derived,
}

fn half() -> f64 {
    0.5
}

fn one_usize() -> usize {
    1
}

impl Domain {
    pub fn is_sampled(&self) -> bool {
        !matches!(self, Domain::Derived)
    }

    pub fn variants(&self) -> Option<&[Variant]> {
        match self {
            Domain::Enum { variants } | Domain::MultiEnum { variants, .. } => Some(variants),
            _ => None,
        }
    }

    /// Проверка на осмысленность. Пустой enum или min > max — это не «странно»,
    /// а неработающий словарь, и поймать это надо при загрузке.
    pub fn issues(&self, key: &str) -> Vec<String> {
        let mut v = Vec::new();
        match self {
            Domain::Int { min, max, weights } => {
                if min > max {
                    v.push(format!("«{key}»: min {min} больше max {max}"));
                } else if !weights.is_empty() {
                    let expected = (max - min + 1) as usize;
                    if weights.len() != expected {
                        v.push(format!(
                            "«{key}»: {} весов при {expected} значениях в диапазоне {min}..{max}",
                            weights.len()
                        ));
                    } else if weights.iter().all(|w| *w <= 0.0) {
                        v.push(format!("«{key}»: все веса нулевые"));
                    }
                }
            }
            Domain::Float { min, max } if min > max => {
                v.push(format!("«{key}»: min {min} больше max {max}"))
            }
            Domain::Bool { p_true } if !(0.0..=1.0).contains(p_true) => {
                v.push(format!("«{key}»: вероятность {p_true} вне диапазона 0..1"))
            }
            Domain::Enum { variants } => {
                if variants.is_empty() {
                    v.push(format!("«{key}»: список вариантов пуст"));
                } else if variants.iter().all(|x| x.weight <= 0.0) {
                    v.push(format!("«{key}»: у всех вариантов нулевой вес"));
                }
            }
            Domain::MultiEnum { variants, min_pick, max_pick } => {
                if variants.is_empty() {
                    v.push(format!("«{key}»: список вариантов пуст"));
                }
                if min_pick > max_pick {
                    v.push(format!("«{key}»: min_pick {min_pick} больше max_pick {max_pick}"));
                }
                if *max_pick > variants.len() {
                    v.push(format!(
                        "«{key}»: max_pick {max_pick} больше числа вариантов {}",
                        variants.len()
                    ));
                }
            }
            _ => {}
        }
        v
    }
}

/// Описание одного параметра невидимого слоя.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDef {
    /// Машинное имя, по нему параметр адресуется в выражениях.
    pub key: String,
    /// Человеческое название, уходит в промпт.
    pub title: String,
    /// Группа для читаемости промпта: «образование», «опыт», «внешность».
    #[serde(default)]
    pub group: String,
    pub domain: Domain,
    #[serde(default)]
    pub usage: Usage,
    /// Можно ли называть параметр прямо в тексте.
    #[serde(default)]
    pub mention: Mention,
    /// Участвует ли в подписи уникальности.
    #[serde(default)]
    pub identity: bool,
    /// Как подать параметр модели, когда он фоновый.
    ///
    /// Без этого фон приходит в виде «Склад характера: мягкий, участливый» —
    /// то есть выглядит как факт, и модель добросовестно пишет «проявляет
    /// мягкий, участливый подход». Директива формулируется как указание:
    /// «тон повествования», «этапов карьеры описать».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive: Option<String>,
    /// Пояснение для того, кто правит словарь руками.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ParamDef {
    pub fn new(key: impl Into<String>, title: impl Into<String>, domain: Domain) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            group: String::new(),
            domain,
            usage: Usage::Text,
            mention: Mention::Natural,
            identity: false,
            directive: None,
            note: None,
        }
    }

    /// Как параметр называется в блоке фона.
    pub fn directive_label(&self) -> &str {
        self.directive.as_deref().unwrap_or(&self.title)
    }

    pub fn group(mut self, g: impl Into<String>) -> Self {
        self.group = g.into();
        self
    }

    pub fn usage(mut self, u: Usage) -> Self {
        self.usage = u;
        self
    }

    pub fn identity(mut self) -> Self {
        self.identity = true;
        self
    }

    pub fn default_value(&self) -> Value {
        match &self.domain {
            Domain::Int { min, .. } => Value::Int(*min),
            Domain::Float { min, .. } => Value::Float(*min),
            Domain::Bool { .. } => Value::Bool(false),
            Domain::Enum { variants } => variants
                .first()
                .map(|v| Value::Str(v.value.clone()))
                .unwrap_or(Value::Null),
            Domain::MultiEnum { .. } => Value::List(Vec::new()),
            Domain::Derived => Value::Null,
        }
    }
}
