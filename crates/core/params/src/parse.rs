//! Разбор выражений из текста.
//!
//! Словари параметров должны править люди, а не программисты, поэтому правила
//! в JSON выглядят как `"experience_years <= age - 22"`, а не как дерево из
//! вложенных объектов.
//!
//! Грамматика (по убыванию приоритета связывания):
//! ```text
//! or        := and ('or' and)*
//! and       := not ('and' not)*
//! not       := 'not' not | cmp
//! cmp       := sum (('=='|'!='|'<='|'>='|'<'|'>') sum)?
//! sum       := product (('+'|'-') product)*
//! product   := atom (('*'|'/') atom)*
//! atom      := число | строка | 'true' | 'false' | 'null'
//!            | 'has' '(' ident ',' строка ')'
//!            | 'in' '(' ident (',' строка)+ ')'
//!            | ident | '(' or ')'
//! ```

use crate::error::{Error, Result};
use crate::expr::{ArithOp, CmpOp, Expr};
use crate::value::Value;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(String),
    LParen,
    RParen,
    Comma,
}

fn lex(src: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if c == '(' {
            out.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(Tok::RParen);
            i += 1;
            continue;
        }
        if c == ',' {
            out.push(Tok::Comma);
            i += 1;
            continue;
        }

        if c == '\'' || c == '"' {
            let quote = c;
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                s.push(chars[i]);
                i += 1;
            }
            if i >= chars.len() {
                return Err(Error::Parse(format!("незакрытая кавычка в «{src}»")));
            }
            i += 1; // закрывающая
            out.push(Tok::Str(s));
            continue;
        }

        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let n: f64 = text
                .parse()
                .map_err(|_| Error::Parse(format!("плохое число «{text}»")))?;
            out.push(Tok::Num(n));
            continue;
        }

        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(chars[start..i].iter().collect()));
            continue;
        }

        // Двухсимвольные операторы идут первыми, иначе «<=» разберётся как «<».
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        if matches!(two.as_str(), "==" | "!=" | "<=" | ">=") {
            out.push(Tok::Op(two));
            i += 2;
            continue;
        }
        if "+-*/<>=".contains(c) {
            out.push(Tok::Op(c.to_string()));
            i += 1;
            continue;
        }

        return Err(Error::Parse(format!("неожиданный символ «{c}» в «{src}»")));
    }

    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    src: String,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat_ident(&mut self, kw: &str) -> bool {
        if let Some(Tok::Ident(s)) = self.peek() {
            if s.eq_ignore_ascii_case(kw) {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn expect(&mut self, t: Tok) -> Result<()> {
        if self.peek() == Some(&t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Error::Parse(format!(
                "ожидалось {:?}, получено {:?} в «{}»",
                t,
                self.peek(),
                self.src
            )))
        }
    }

    fn or(&mut self) -> Result<Expr> {
        let mut parts = vec![self.and()?];
        while self.eat_ident("or") {
            parts.push(self.and()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Expr::Or(parts) })
    }

    fn and(&mut self) -> Result<Expr> {
        let mut parts = vec![self.not()?];
        while self.eat_ident("and") {
            parts.push(self.not()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Expr::And(parts) })
    }

    fn not(&mut self) -> Result<Expr> {
        if self.eat_ident("not") {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr> {
        let lhs = self.sum()?;
        let op = match self.peek() {
            Some(Tok::Op(o)) => match o.as_str() {
                "==" => CmpOp::Eq,
                "!=" => CmpOp::Ne,
                "<=" => CmpOp::Le,
                ">=" => CmpOp::Ge,
                "<" => CmpOp::Lt,
                ">" => CmpOp::Gt,
                "=" => CmpOp::Eq, // частая описка, принимаем
                _ => return Ok(lhs),
            },
            _ => return Ok(lhs),
        };
        self.pos += 1;
        let rhs = self.sum()?;
        Ok(Expr::Cmp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
    }

    fn sum(&mut self) -> Result<Expr> {
        let mut lhs = self.product()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) if o == "+" => ArithOp::Add,
                Some(Tok::Op(o)) if o == "-" => ArithOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.product()?;
            lhs = Expr::Arith { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn product(&mut self) -> Result<Expr> {
        let mut lhs = self.atom()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) if o == "*" => ArithOp::Mul,
                Some(Tok::Op(o)) if o == "/" => ArithOp::Div,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.atom()?;
            lhs = Expr::Arith { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn atom(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Lit(if n.fract() == 0.0 {
                Value::Int(n as i64)
            } else {
                Value::Float(n)
            })),
            Some(Tok::Str(s)) => Ok(Expr::Lit(Value::Str(s))),
            Some(Tok::LParen) => {
                let e = self.or()?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::Op(o)) if o == "-" => {
                // Унарный минус: -5 → (0 - 5)
                let inner = self.atom()?;
                Ok(Expr::Arith {
                    op: ArithOp::Sub,
                    lhs: Box::new(Expr::Lit(Value::Int(0))),
                    rhs: Box::new(inner),
                })
            }
            Some(Tok::Ident(name)) => match name.as_str() {
                "true" => Ok(Expr::Lit(Value::Bool(true))),
                "false" => Ok(Expr::Lit(Value::Bool(false))),
                "null" => Ok(Expr::Lit(Value::Null)),
                "has" => {
                    self.expect(Tok::LParen)?;
                    let param = self.ident_arg()?;
                    self.expect(Tok::Comma)?;
                    let value = self.str_arg()?;
                    self.expect(Tok::RParen)?;
                    Ok(Expr::Has { param, value })
                }
                "in" => {
                    self.expect(Tok::LParen)?;
                    let param = self.ident_arg()?;
                    let mut values = Vec::new();
                    while self.peek() == Some(&Tok::Comma) {
                        self.pos += 1;
                        values.push(self.str_arg()?);
                    }
                    self.expect(Tok::RParen)?;
                    if values.is_empty() {
                        return Err(Error::Parse(format!(
                            "in({param}, …) без значений в «{}»",
                            self.src
                        )));
                    }
                    Ok(Expr::In { param, values })
                }
                _ => Ok(Expr::Param(name)),
            },
            other => Err(Error::Parse(format!(
                "неожиданный токен {other:?} в «{}»",
                self.src
            ))),
        }
    }

    fn ident_arg(&mut self) -> Result<String> {
        match self.next() {
            Some(Tok::Ident(s)) => Ok(s),
            Some(Tok::Str(s)) => Ok(s),
            other => Err(Error::Parse(format!(
                "ожидалось имя параметра, получено {other:?} в «{}»",
                self.src
            ))),
        }
    }

    fn str_arg(&mut self) -> Result<String> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(s),
            Some(Tok::Ident(s)) => Ok(s),
            Some(Tok::Num(n)) => Ok(n.to_string()),
            other => Err(Error::Parse(format!(
                "ожидалась строка, получено {other:?} в «{}»",
                self.src
            ))),
        }
    }
}

pub fn parse_expr(src: &str) -> Result<Expr> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return Err(Error::Parse("пустое выражение".into()));
    }
    let mut p = Parser { toks, pos: 0, src: src.to_string() };
    let e = p.or()?;
    if p.pos != p.toks.len() {
        return Err(Error::Parse(format!(
            "лишние символы после выражения в «{src}» (позиция {})",
            p.pos
        )));
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row::ParamRow;

    fn row() -> ParamRow {
        let mut r = ParamRow::new();
        r.set("age", Value::Int(45));
        r.set("experience_years", Value::Int(18));
        r.set("children", Value::Int(3));
        r.set("region", Value::Str("Москва".into()));
        r.set(
            "practice",
            Value::List(vec!["наркозависимые".into(), "алкоголики".into()]),
        );
        r
    }

    #[test]
    fn arithmetic_and_comparison() {
        let e = parse_expr("experience_years <= age - 22").unwrap();
        assert!(e.holds(&row()));

        let e = parse_expr("experience_years <= age - 40").unwrap();
        assert!(!e.holds(&row()));
    }

    #[test]
    fn boolean_logic() {
        let e = parse_expr("age > 40 and children >= 2").unwrap();
        assert!(e.holds(&row()));

        let e = parse_expr("age > 60 or children >= 2").unwrap();
        assert!(e.holds(&row()));

        let e = parse_expr("not (age > 60)").unwrap();
        assert!(e.holds(&row()));
    }

    #[test]
    fn list_and_set_membership() {
        let e = parse_expr("has(practice, 'наркозависимые')").unwrap();
        assert!(e.holds(&row()));

        let e = parse_expr("has(practice, 'игроманы')").unwrap();
        assert!(!e.holds(&row()));

        let e = parse_expr("in(region, 'Москва', 'Санкт-Петербург')").unwrap();
        assert!(e.holds(&row()));
    }

    #[test]
    fn roundtrip_through_string() {
        for src in [
            "experience_years <= age - 22",
            "age > 40 and children >= 2",
            "has(practice, 'алкоголики')",
            "in(region, 'Москва', 'Казань')",
        ] {
            let a = parse_expr(src).unwrap();
            let printed = a.to_string();
            let b = parse_expr(&printed).expect("обратная сборка должна разбираться");
            assert_eq!(a, b, "потеря смысла при пересборке «{src}» → «{printed}»");
        }
    }

    #[test]
    fn missing_param_is_falsy_not_fatal() {
        let e = parse_expr("nonexistent > 5").unwrap();
        assert!(!e.holds(&row()), "неизвестный параметр не должен ронять прогон");
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert!(parse_expr("age > ").is_err());
        assert!(parse_expr("age 5").is_err());
        assert!(parse_expr("'незакрытая").is_err());
    }
}
