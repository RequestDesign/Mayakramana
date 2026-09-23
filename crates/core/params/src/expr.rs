use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};
use crate::row::ParamRow;
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl ArithOp {
    pub fn as_str(self) -> &'static str {
        match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
            ArithOp::Mul => "*",
            ArithOp::Div => "/",
        }
    }
}

/// Выражение над параметрами.
///
/// В JSON хранится строкой («experience_years <= age - 22»), а не деревом:
/// словарь должен оставаться редактируемым руками без пересборки проекта.
/// Разбор и обратная сборка — в [`crate::parse`].
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Param(String),
    Lit(Value),
    Arith { op: ArithOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Cmp { op: CmpOp, lhs: Box<Expr>, rhs: Box<Expr> },
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    /// `has(практика, 'наркозависимые')` — параметр со списком содержит значение.
    Has { param: String, value: String },
    /// `in(регион, 'Москва', 'СПб')` — значение параметра входит в набор.
    In { param: String, values: Vec<String> },
}

impl Expr {
    pub fn eval(&self, row: &ParamRow) -> Result<Value> {
        match self {
            Expr::Lit(v) => Ok(v.clone()),

            Expr::Param(name) => Ok(row.get(name).cloned().unwrap_or(Value::Null)),

            Expr::Arith { op, lhs, rhs } => {
                let a = lhs.eval(row)?.as_f64().ok_or_else(|| {
                    Error::Eval(format!("«{lhs}» не число, арифметика невозможна"))
                })?;
                let b = rhs.eval(row)?.as_f64().ok_or_else(|| {
                    Error::Eval(format!("«{rhs}» не число, арифметика невозможна"))
                })?;
                let r = match op {
                    ArithOp::Add => a + b,
                    ArithOp::Sub => a - b,
                    ArithOp::Mul => a * b,
                    ArithOp::Div => {
                        if b == 0.0 {
                            return Err(Error::Eval("деление на ноль".into()));
                        }
                        a / b
                    }
                };
                // Целые остаются целыми — иначе возраст станет «42.00».
                if r.fract() == 0.0 && r.abs() < 9e15 {
                    Ok(Value::Int(r as i64))
                } else {
                    Ok(Value::Float(r))
                }
            }

            Expr::Cmp { op, lhs, rhs } => {
                let a = lhs.eval(row)?;
                let b = rhs.eval(row)?;
                Ok(Value::Bool(compare(*op, &a, &b)))
            }

            Expr::And(parts) => {
                for p in parts {
                    if !truthy(&p.eval(row)?) {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }

            Expr::Or(parts) => {
                for p in parts {
                    if truthy(&p.eval(row)?) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }

            Expr::Not(inner) => Ok(Value::Bool(!truthy(&inner.eval(row)?))),

            Expr::Has { param, value } => {
                let hit = match row.get(param) {
                    Some(Value::List(items)) => items.iter().any(|s| s == value),
                    Some(Value::Str(s)) => s == value,
                    _ => false,
                };
                Ok(Value::Bool(hit))
            }

            Expr::In { param, values } => {
                let hit = match row.get(param) {
                    Some(Value::Str(s)) => values.contains(s),
                    Some(Value::List(items)) => items.iter().any(|s| values.contains(s)),
                    Some(other) => values.contains(&other.render()),
                    None => false,
                };
                Ok(Value::Bool(hit))
            }
        }
    }

    /// Истинно ли выражение на этой строке. Ошибка вычисления трактуется как
    /// «не выполнено»: незаполненный параметр не должен ронять весь прогон.
    pub fn holds(&self, row: &ParamRow) -> bool {
        match self.eval(row) {
            Ok(v) => truthy(&v),
            Err(e) => {
                tracing::debug!(expr = %self, error = %e, "выражение не вычислилось");
                false
            }
        }
    }

    /// Имена параметров, от которых зависит выражение.
    /// Нужно для проверки порядка объявления: мягкое правило не может опираться
    /// на параметр, который сэмплируется позже цели.
    pub fn referenced_params(&self, out: &mut Vec<String>) {
        match self {
            Expr::Param(n) => out.push(n.clone()),
            Expr::Lit(_) => {}
            Expr::Arith { lhs, rhs, .. } | Expr::Cmp { lhs, rhs, .. } => {
                lhs.referenced_params(out);
                rhs.referenced_params(out);
            }
            Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| e.referenced_params(out)),
            Expr::Not(e) => e.referenced_params(out),
            Expr::Has { param, .. } | Expr::In { param, .. } => out.push(param.clone()),
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Null => false,
    }
}

fn compare(op: CmpOp, a: &Value, b: &Value) -> bool {
    // Пустое значение (незаполненный или неизвестный параметр) не упорядочивается.
    // Без этой ветки `nonexistent > 5` сравнивало бы отрендеренный прочерк со
    // строкой «5» и молча давало истину — правило срабатывало бы вхолостую.
    if a.is_null() || b.is_null() {
        return match op {
            CmpOp::Eq => a.is_null() && b.is_null(),
            CmpOp::Ne => a.is_null() != b.is_null(),
            _ => false,
        };
    }

    // Числа сравниваем численно, всё остальное — по отрендеренному виду.
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
        return match op {
            CmpOp::Eq => x == y,
            CmpOp::Ne => x != y,
            CmpOp::Lt => x < y,
            CmpOp::Le => x <= y,
            CmpOp::Gt => x > y,
            CmpOp::Ge => x >= y,
        };
    }
    let x = a.render();
    let y = b.render();
    match op {
        CmpOp::Eq => x == y,
        CmpOp::Ne => x != y,
        CmpOp::Lt => x < y,
        CmpOp::Le => x <= y,
        CmpOp::Gt => x > y,
        CmpOp::Ge => x >= y,
    }
}

// ----------------------------------------------------------- обратная сборка

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Param(n) => write!(f, "{n}"),
            Expr::Lit(Value::Str(s)) => write!(f, "'{}'", s.replace('\'', "\\'")),
            Expr::Lit(v) => match v {
                Value::Bool(b) => write!(f, "{b}"),
                Value::Int(i) => write!(f, "{i}"),
                Value::Float(x) => write!(f, "{x}"),
                Value::Null => write!(f, "null"),
                Value::List(l) => write!(f, "'{}'", l.join(",")),
                Value::Str(_) => unreachable!(),
            },
            Expr::Arith { op, lhs, rhs } => write!(f, "({lhs} {} {rhs})", op.as_str()),
            Expr::Cmp { op, lhs, rhs } => write!(f, "({lhs} {} {rhs})", op.as_str()),
            Expr::And(parts) => {
                let s: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
                write!(f, "({})", s.join(" and "))
            }
            Expr::Or(parts) => {
                let s: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
                write!(f, "({})", s.join(" or "))
            }
            Expr::Not(e) => write!(f, "not {e}"),
            Expr::Has { param, value } => write!(f, "has({param}, '{value}')"),
            Expr::In { param, values } => {
                let s: Vec<String> = values.iter().map(|v| format!("'{v}'")).collect();
                write!(f, "in({param}, {})", s.join(", "))
            }
        }
    }
}

impl Serialize for Expr {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Expr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let src = String::deserialize(d)?;
        crate::parse::parse_expr(&src).map_err(serde::de::Error::custom)
    }
}
