//! Выделение числовых утверждений из текста.
//!
//! Нужно для главной проверки приёмки: модель получила стаж 18 лет, а написала
//! «более двадцати лет в профессии». Без ловли таких расхождений несогласованность
//! уходит в базу и всплывает уже на витрине, где её увидят все.
//!
//! Числительные ловятся и цифрами, и словами: модели охотно пишут «двадцать лет»
//! вместо «20 лет», и проверка только по цифрам пропустила бы ровно тот случай,
//! ради которого затевалась.

/// Русские числительные, встречающиеся в описаниях стажа.
const WORDS: &[(&str, i64)] = &[
    ("один", 1), ("одного", 1), ("год", 1),
    ("два", 2), ("двух", 2),
    ("три", 3), ("трёх", 3), ("трех", 3),
    ("четыре", 4), ("четырёх", 4), ("четырех", 4),
    ("пять", 5), ("пяти", 5),
    ("шесть", 6), ("шести", 6),
    ("семь", 7), ("семи", 7),
    ("восемь", 8), ("восьми", 8),
    ("девять", 9), ("девяти", 9),
    ("десять", 10), ("десяти", 10),
    ("одиннадцать", 11), ("одиннадцати", 11),
    ("двенадцать", 12), ("двенадцати", 12),
    ("тринадцать", 13), ("тринадцати", 13),
    ("четырнадцать", 14), ("четырнадцати", 14),
    ("пятнадцать", 15), ("пятнадцати", 15),
    ("шестнадцать", 16), ("шестнадцати", 16),
    ("семнадцать", 17), ("семнадцати", 17),
    ("восемнадцать", 18), ("восемнадцати", 18),
    ("девятнадцать", 19), ("девятнадцати", 19),
    ("двадцать", 20), ("двадцати", 20),
    ("двадцатью", 20),
    ("тридцать", 30), ("тридцати", 30),
    ("сорок", 40), ("сорока", 40),
    ("пятьдесят", 50), ("пятидесяти", 50),
];

/// Слова, после которых число означает срок в годах.
const YEAR_MARKERS: &[&str] = &["лет", "года", "годами", "год", "летний", "летним"];

/// Слова, после которых «N лет» означает возраст в момент события, а не срок:
/// «в 24 года окончил», «с 19 лет работал».
const AT_AGE: &[&str] = &["в", "во", "с", "со"];

/// Утверждение о годах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub value: i64,
    /// «в 24 года» — возраст в какой-то момент; иначе срок или нынешний возраст.
    pub at_age: bool,
}

/// Все утверждения вида «N лет», найденные в тексте.
pub fn year_claims(text: &str) -> Vec<i64> {
    let mut v: Vec<i64> = claims(text).into_iter().map(|c| c.value).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Календарные годы в тексте: «выпуск 2005 года», «в 1990-х».
///
/// Сроком они не являются и проверяются отдельно: событие не может случиться
/// до рождения человека или в будущем.
pub fn calendar_years(text: &str) -> Vec<i64> {
    let mut v: Vec<i64> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() == 4)
        .filter_map(|t| t.parse::<i64>().ok())
        .filter(|y| (1900..=2100).contains(y))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Утверждения о годах с пометкой, возраст это или срок.
pub fn claims(text: &str) -> Vec<Claim> {
    let lower = text.to_lowercase();
    let tokens: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();

    let mut out = Vec::new();
    // Единицы составного числительного («двадцать пять») уже учтены в десятках,
    // отдельным утверждением их считать нельзя.
    let mut consumed = vec![false; tokens.len()];

    for (i, tok) in tokens.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        // Маркер года должен стоять следом или через одно слово:
        // «18 лет», «двадцати лет», «20 с лишним лет».
        let marked = tokens
            .iter()
            .skip(i + 1)
            .take(3)
            .any(|next| YEAR_MARKERS.contains(&next.as_str()));

        if !marked {
            continue;
        }

        let at_age = i > 0 && AT_AGE.contains(&tokens[i - 1].as_str());

        if let Ok(n) = tok.parse::<i64>() {
            // Календарный год — не срок: «2005 года» проверяется отдельно.
            if !(1900..=2100).contains(&n) {
                out.push(Claim { value: n, at_age });
            }
            continue;
        }

        // Составные вида «двадцать пять»: берём сумму, если следом единицы.
        if let Some(tens) = word_value(tok) {
            let next_units = tokens
                .get(i + 1)
                .and_then(|t| word_value(t))
                .filter(|u| tens >= 20 && *u < 10);
            if next_units.is_some() {
                consumed[i + 1] = true;
            }
            out.push(Claim { value: tens + next_units.unwrap_or(0), at_age });
        }
    }

    out.dedup();
    out
}

fn word_value(token: &str) -> Option<i64> {
    WORDS
        .iter()
        .find(|(w, _)| *w == token)
        .map(|(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_found() {
        assert_eq!(year_claims("стаж 18 лет в наркологии"), vec![18]);
        assert_eq!(year_claims("работает 7 лет, до этого 3 года в клинике"), vec![3, 7]);
    }

    /// Ровно тот случай, ради которого всё это: модель написала словом.
    #[test]
    fn words_are_found_too() {
        assert_eq!(year_claims("более двадцати лет в профессии"), vec![20]);
        assert_eq!(year_claims("пятнадцать лет практики"), vec![15]);
        assert_eq!(year_claims("свыше тридцати лет"), vec![30]);
    }

    #[test]
    fn compound_numerals() {
        assert_eq!(year_claims("двадцать пять лет стажа"), vec![25]);
    }

    #[test]
    fn unrelated_numbers_are_ignored() {
        assert!(year_claims("принял 400 пациентов").is_empty());
        assert!(year_claims("кабинет номер 12").is_empty());
    }

    #[test]
    fn age_at_moment_is_told_apart_from_duration() {
        let c = claims("в 24 года окончил университет, 18 лет в наркологии");
        assert!(c.contains(&Claim { value: 24, at_age: true }), "{c:?}");
        assert!(c.contains(&Claim { value: 18, at_age: false }), "{c:?}");
        assert!(claims("с девятнадцати лет работал")[0].at_age);
    }

    #[test]
    fn calendar_years_are_not_durations() {
        assert!(year_claims("выпуск 2005 года").is_empty());
        assert_eq!(calendar_years("выпуск 2005 года, в 1990-х учился"), vec![1990, 2005]);
        assert!(calendar_years("принял 1200 пациентов").is_empty());
    }

    #[test]
    fn age_phrasing_is_caught() {
        assert_eq!(year_claims("ему 49 лет"), vec![49]);
        assert_eq!(year_claims("49-летний врач"), vec![49]);
    }
}
