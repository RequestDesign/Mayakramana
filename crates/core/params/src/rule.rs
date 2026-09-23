use serde::{Deserialize, Serialize};

use crate::expr::Expr;
use crate::value::Value;

/// Жёсткий инвариант. Нарушение — это брак, а не «неудачный текст»:
/// строка пересобирается заново.
///
/// Существует потому, что без него комбинаторика параметров даёт бессвязных
/// людей: врача 25 лет с 30-летним стажем, консультанта с чистотой больше
/// собственного возраста.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardRule {
    pub name: String,
    /// Выражение, которое должно быть истинным. Пишется текстом:
    /// `"experience_years <= age - 22"`.
    pub expr: Expr,
    #[serde(default)]
    pub message: String,
}

impl HardRule {
    pub fn holds(&self, row: &crate::ParamRow) -> bool {
        self.expr.holds(row)
    }

    pub fn explain(&self) -> &str {
        if self.message.is_empty() {
            &self.name
        } else {
            &self.message
        }
    }
}

/// Сдвиг распределения для зависимого параметра.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Bias {
    /// Домножить вес варианта. Не запрещает, а делает более или менее вероятным.
    Weight {
        param: String,
        value: String,
        factor: f64,
    },
    /// Сузить числовой диапазон.
    Range {
        param: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
    /// Задать значение жёстко. Используется когортами и точечными правилами.
    Force { param: String, value: Value },
}

impl Bias {
    pub fn target(&self) -> &str {
        match self {
            Bias::Weight { param, .. } | Bias::Range { param, .. } | Bias::Force { param, .. } => {
                param
            }
        }
    }
}

/// Мягкая корреляция между параметрами.
///
/// Это вторая половина связности: жёсткие правила запрещают невозможное, мягкие
/// делают правдоподобное более вероятным. Пример из постановки: много детей —
/// выше шанс детской специализации.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoftRule {
    pub name: String,
    /// Условие на уже сэмплированных параметрах.
    pub when: Expr,
    pub then: Vec<Bias>,
}
