//! Текстовая похожесть.
//!
//! Первый уровень уникальности — лексический: не совпадают ли формулировки.
//! Дёшево, без эмбеддингов и без сети, и ловит главную беду массовой
//! генерации — однообразие языка. Модель, написавшая тысячу биографий,
//! начинает повторять одни и те же обороты, и на витрине это видно сразу.
//!
//! Смысловой уровень («тот же человек другими словами») требует эмбеддингов и
//! живёт в другом месте. Здесь — только слова.
//!
//! # Как считается
//!
//! Текст режется на шинглы — перекрывающиеся тройки слов. Похожесть двух
//! текстов — доля общих шинглов (коэффициент Жаккара). Совпадение отдельных
//! слов ничего не значит, совпадение троек подряд — уже заимствованный оборот.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// Длина шингла в словах.
///
/// Двойки ловят слишком много случайных совпадений («в центре», «с
/// пациентами»). Четвёрки пропускают перефразированные штампы. Тройки —
/// рабочая середина для русского текста.
pub const SHINGLE: usize = 3;

/// Слова, которые не несут смысла и только раздувают совпадения.
const STOP: &[&str] = &[
    "и", "в", "во", "на", "с", "со", "к", "ко", "по", "о", "об", "от", "до", "из", "у", "за",
    "а", "но", "что", "как", "это", "он", "она", "его", "её", "ее", "их", "он", "же", "ли",
    "не", "ни", "то", "так", "для", "при", "под", "над", "без",
];

/// Нормализованные слова текста.
pub fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('ё', "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter(|w| !STOP.contains(w))
        .map(str::to_string)
        .collect()
}

/// Шинглы текста в виде хешей.
pub fn shingles(text: &str) -> HashSet<u64> {
    let w = words(text);
    if w.len() < SHINGLE {
        // Короткий текст — один шингл из всего, что есть. Иначе цитата из
        // двух слов оказалась бы «ни на что не похожей» и прошла бы любую
        // проверку.
        return if w.is_empty() {
            HashSet::new()
        } else {
            HashSet::from([hash(&w)])
        };
    }
    w.windows(SHINGLE).map(hash).collect()
}

fn hash(parts: &[String]) -> u64 {
    let mut h = DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

/// Коэффициент Жаккара: доля общих шинглов в объединении.
pub fn jaccard(a: &HashSet<u64>, b: &HashSet<u64>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    inter as f32 / union as f32
}

/// Доля шинглов `a`, встречающихся в `b`.
///
/// Нужна для несимметричного случая: короткий абзац, целиком пересказывающий
/// кусок длинного, по Жаккару выглядит непохожим — объединение большое. По
/// вложенности видно, что он повтор.
pub fn containment(a: &HashSet<u64>, b: &HashSet<u64>) -> f32 {
    if a.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f32 / a.len() as f32
}

/// Похожесть двух текстов.
pub fn similarity(a: &str, b: &str) -> f32 {
    jaccard(&shingles(a), &shingles(b))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub id: String,
    pub score: f32,
}

/// Индекс для поиска похожих текстов.
///
/// Сравнение «каждый с каждым» на десяти тысячах текстов — пятьдесят миллионов
/// пар. Обратный индекс по шинглам сравнивает новый текст только с теми, у
/// кого есть хоть один общий шингл, — на практике с единицами.
#[derive(Debug, Default)]
pub struct LexicalIndex {
    docs: Vec<(String, HashSet<u64>)>,
    postings: HashMap<u64, Vec<usize>>,
}

impl LexicalIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn insert(&mut self, id: impl Into<String>, text: &str) {
        let set = shingles(text);
        let idx = self.docs.len();
        for s in &set {
            self.postings.entry(*s).or_default().push(idx);
        }
        self.docs.push((id.into(), set));
    }

    /// Самые похожие тексты, по убыванию похожести.
    pub fn nearest(&self, text: &str, k: usize) -> Vec<Match> {
        let q = shingles(text);
        if q.is_empty() {
            return Vec::new();
        }

        let mut shared: HashMap<usize, usize> = HashMap::new();
        for s in &q {
            if let Some(list) = self.postings.get(s) {
                for &i in list {
                    *shared.entry(i).or_default() += 1;
                }
            }
        }

        let mut out: Vec<Match> = shared
            .into_iter()
            .map(|(i, inter)| {
                let (id, set) = &self.docs[i];
                let union = q.len() + set.len() - inter;
                Match { id: id.clone(), score: inter as f32 / union as f32 }
            })
            .collect();

        out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(k);
        out
    }

    /// Ближайший текст, если он похож больше порога.
    pub fn too_close(&self, text: &str, max: f32) -> Option<Match> {
        self.nearest(text, 1).into_iter().find(|m| m.score > max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "Окончил медицинский университет в Поволжье и пришёл в наркологию сразу \
        после ординатуры. Первые годы работал в общепсихиатрическом отделении.";

    const B: &str = "Родился в Сибири, учился в Томске. Долго работал неврологом и только к \
        сорока годам занялся зависимостями, когда увидел, сколько жалоб растёт из них.";

    #[test]
    fn identical_texts_are_fully_similar() {
        assert!((similarity(A, A) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn different_texts_are_far_apart() {
        assert!(similarity(A, B) < 0.05, "{}", similarity(A, B));
    }

    /// Главный случай: шаблонный оборот, переписанный с мелкими правками.
    #[test]
    fn near_duplicate_is_caught() {
        let a2 = "Окончил медицинский университет в Поволжье и пришёл в наркологию сразу \
            после ординатуры. Первые годы трудился в общепсихиатрическом отделении.";
        // В коротком тексте одна замена слова выбивает три шингла из
        // двенадцати, поэтому абсолютное значение умеренное. Важно
        // соотношение: почти дубль на порядок ближе чужого текста.
        let s = similarity(A, a2);
        assert!(s >= 0.5, "почти дубль не пойман: {s}");
        assert!(s > similarity(A, B) * 10.0, "почти дубль неотличим от чужого текста");
    }

    #[test]
    fn case_punctuation_and_yo_do_not_matter() {
        let a = "Ведёт группы, занимается наставничеством!";
        let b = "ведет группы — занимается наставничеством";
        assert!((similarity(a, b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn containment_catches_short_restatement() {
        let long = "Ведёт группы и занимается наставничеством, уделяя внимание родственникам \
            подопечных. Главным считает работу с отрицанием, которое мешает движению вперёд.";
        let short = "Ведёт группы и занимается наставничеством, уделяя внимание родственникам.";
        let (l, s) = (shingles(long), shingles(short));
        assert!(containment(&s, &l) > 0.8, "{}", containment(&s, &l));
        assert!(jaccard(&s, &l) < containment(&s, &l), "Жаккар занижает вложенный повтор");
    }

    #[test]
    fn index_finds_the_near_duplicate_among_many() {
        let mut idx = LexicalIndex::new();
        idx.insert("b", B);
        for i in 0..200 {
            idx.insert(format!("шум{i}"), &format!("Совершенно другой текст номер {i} про погоду и дорогу домой"));
        }
        idx.insert("a", A);

        let a2 = A.replace("работал", "трудился");
        let best = idx.too_close(&a2, 0.5).expect("должен найти почти дубль");
        assert_eq!(best.id, "a");
    }

    #[test]
    fn index_stays_quiet_on_original_text() {
        let mut idx = LexicalIndex::new();
        idx.insert("a", A);
        assert!(idx.too_close(B, 0.3).is_none());
    }

    #[test]
    fn very_short_text_still_compares() {
        let s = shingles("Лечу");
        assert_eq!(s.len(), 1, "короткий текст не должен становиться пустым множеством");
    }
}
