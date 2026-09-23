use serde::{Deserialize, Serialize};
use synthforge_params::{Bias, Cohort, ParamModel, ParamRow, PopulationPlan};

use crate::image::ImageRequest;
use crate::text::TextRequest;

/// Что производит генератор.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityDescriptor {
    /// Машинное имя: `doctor`, `consultant`, `building`.
    pub kind: String,
    /// Слаг коллекции в хранилище.
    pub collection: String,
    /// Название для интерфейса.
    pub title: String,
    /// Сколько изображений нужно одной сущности.
    pub images_per_entity: usize,
}

/// Режим генерации.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum GenMode {
    /// Случайная: параметры даёт сэмплер, база не читается.
    /// Быстро и дёшево, годится пока база пуста.
    Random,
    /// Уникальная относительно базы: перед записью сущность сверяется с уже
    /// существующими, при слишком близком совпадении — перегенерация.
    UniqueAgainstBase(Uniqueness),
}

impl Default for GenMode {
    fn default() -> Self {
        GenMode::Random
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Uniqueness {
    pub level: UniquenessLevel,
    /// Область сравнения.
    #[serde(default)]
    pub scope: Scope,
    #[serde(default = "default_retries")]
    pub max_retries: u32,
}

fn default_retries() -> u32 {
    3
}

impl Uniqueness {
    /// Лексический порог, если он задан уровнем.
    pub fn lexical_threshold(&self) -> Option<f32> {
        match self.level {
            UniquenessLevel::Lexical { max_ngram_overlap }
            | UniquenessLevel::Both { max_ngram_overlap, .. } => Some(max_ngram_overlap),
            UniquenessLevel::Semantic { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum UniquenessLevel {
    /// Текстовая: не совпадают формулировки. Дёшево, ловит шаблонность языка.
    Lexical { max_ngram_overlap: f32 },
    /// Смысловая: не «тот же человек другими словами». Дороже, требует
    /// эмбеддингов, ловит однотипность по сути.
    Semantic { max_cosine: f32 },
    /// Оба. Рекомендуется для людей и развёрнутых описаний.
    Both { max_ngram_overlap: f32, max_cosine: f32 },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Со всей коллекцией. Дороже всего, но единственный честный вариант при
    /// доливке в уже наполненную базу.
    #[default]
    Collection,
    /// Только внутри текущего прогона. Дёшево, годится для первичного наполнения.
    Session,
}

/// Постановка задачи на генерацию.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenSpec {
    pub count: usize,
    #[serde(default)]
    pub mode: GenMode,
    /// Дополнительный вводный промпт поверх структурной спецификации.
    ///
    /// Например: «консультанты с опытом работы от 7 лет и сложной историей
    /// выздоровления». Мягкая подсказка — уходит в промпт, но **не** заменяет
    /// ограничения: словесные рамки модель нарушает молча.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// Жёсткие сужения поверх словаря. Проверяются кодом, а не просьбой.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Bias>,
    /// Когорты: «20% таких, 10% таких». Раскладываются точно.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cohorts: Vec<Cohort>,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    /// Ставить ли задания на изображения.
    ///
    /// Отдельный флаг, потому что снимки на порядки дороже текста: отладка
    /// словаря идёт без них, а включаются они осознанно.
    #[serde(default)]
    pub with_images: bool,
}

fn default_seed() -> u64 {
    2026
}

impl GenSpec {
    pub fn new(count: usize) -> Self {
        Self {
            count,
            mode: GenMode::Random,
            brief: None,
            constraints: Vec::new(),
            cohorts: Vec::new(),
            seed: default_seed(),
            budget_usd: None,
            with_images: false,
        }
    }

    pub fn with_images(mut self, yes: bool) -> Self {
        self.with_images = yes;
        self
    }

    pub fn brief(mut self, b: impl Into<String>) -> Self {
        self.brief = Some(b.into());
        self
    }

    pub fn seed(mut self, s: u64) -> Self {
        self.seed = s;
        self
    }

    pub fn cohort(mut self, c: Cohort) -> Self {
        self.cohorts.push(c);
        self
    }

    /// План популяции: когорты как заданы, либо одна общая.
    ///
    /// Жёсткие сужения из `constraints` добавляются в каждую когорту — они
    /// действуют на всю постановку, а не на её часть.
    pub fn population_plan(&self) -> PopulationPlan {
        let mut plan = if self.cohorts.is_empty() {
            PopulationPlan::uniform(self.count)
        } else {
            PopulationPlan { count: self.count, cohorts: self.cohorts.clone() }
        };

        if !self.constraints.is_empty() {
            for c in &mut plan.cohorts {
                c.biases.extend(self.constraints.iter().cloned());
            }
        }
        plan
    }
}

/// Результат приёмки текстового ответа.
#[derive(Debug, Clone)]
pub struct AcceptedText {
    /// Поля, которые модель заполнила: биография, цитата и прочее.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum RejectReason {
    #[error("ответ не разобрался: {0}")]
    NotStructured(String),

    #[error("нет обязательного поля «{0}»")]
    MissingField(String),

    #[error("поле «{field}» слишком короткое: {got} знаков при минимуме {min}")]
    TooShort { field: String, got: usize, min: usize },

    /// Главная проверка: модель переписала заданный факт.
    #[error("модель разошлась с параметрами по «{param}»: задано {expected}, в тексте {found}")]
    FactDrift {
        param: String,
        expected: String,
        found: String,
    },

    #[error("в тексте встретилось запрещённое: «{0}»")]
    Forbidden(String),
}

/// Контракт генератора сущности.
///
/// Генератор намеренно **не занимается вводом-выводом**: он не ходит в
/// хранилище, не вызывает модель, не знает про очередь. Он знает предметную
/// область — как разложить постановку, что спросить у модели и как проверить
/// ответ. Всю плумбинг-часть делает движок.
///
/// Отсюда два следствия. Генератор тестируется без сети. И смена хранилища или
/// провайдера не трогает ни один генератор.
pub trait Generator: Send + Sync {
    fn descriptor(&self) -> &EntityDescriptor;

    /// Словарь, по которому работает генератор.
    fn model(&self) -> &ParamModel;

    /// Разложить постановку в набор строк параметров.
    ///
    /// Чистая функция от спецификации и сида: одинаковый вход даёт одинаковый
    /// выход. Модель здесь ещё не участвует — к концу этого шага уже решено,
    /// какими будут все сущности.
    fn plan(&self, spec: &GenSpec) -> Result<Vec<ParamRow>, synthforge_params::Error>;

    /// Естественный ключ сущности: делает постановку в очередь идемпотентной.
    fn natural_key(&self, session_id: &str, index: usize) -> String {
        format!("{}:{}:{}", self.descriptor().kind, session_id, index)
    }

    /// Запрос на текстовую часть.
    fn text_request(&self, row: &ParamRow, brief: Option<&str>) -> TextRequest;

    /// Запросы на изображения. Пусто — сущности картинки не нужны.
    fn image_requests(&self, row: &ParamRow) -> Vec<ImageRequest>;

    /// Приёмка ответа модели.
    ///
    /// Здесь ловится расхождение фактов: модель получила возраст 49 и стаж 18,
    /// а написала «более двадцати лет в профессии». Без этой проверки
    /// несогласованность уйдёт в базу и всплывёт на витрине.
    fn accept_text(&self, row: &ParamRow, text: &str) -> Result<AcceptedText, RejectReason>;

    /// Собрать итоговую запись для хранилища.
    fn assemble(&self, row: &ParamRow, accepted: &AcceptedText) -> serde_json::Value;

    /// Текст, по которому сущность сравнивается с остальными на уникальность.
    ///
    /// Движок не знает, какие поля у сущности содержательные, а какие
    /// служебные, — это знает генератор. `None` — сущность не проверяется.
    fn uniqueness_text(&self, _accepted: &AcceptedText) -> Option<String> {
        None
    }
}
