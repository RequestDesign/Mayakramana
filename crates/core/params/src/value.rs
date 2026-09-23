use std::fmt;

use serde::{Deserialize, Serialize};

/// Значение параметра.
///
/// Намеренно узкий набор типов: всё, что нужно для описания личности или
/// объекта, укладывается в шесть вариантов. Более богатая система типов здесь
/// только мешала бы — словарь должен оставаться редактируемым руками.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// Для параметров с множественным выбором: «работал с наркозависимыми,
    /// алкоголиками, игроманами» — это один параметр со списком значений.
    List(Vec<String>),
    Null,
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Человекочитаемое представление для подстановки в промпт.
    pub fn render(&self) -> String {
        match self {
            Value::Bool(true) => "да".into(),
            Value::Bool(false) => "нет".into(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format!("{f:.2}"),
            Value::Str(s) => s.clone(),
            Value::List(v) => v.join(", "),
            Value::Null => "—".into(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render())
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Str(v.to_string())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(v)
    }
}
