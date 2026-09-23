use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::param::{Mention, Usage};
use crate::value::Value;

/// Набор значений параметров для одной сущности.
///
/// Это результат работы невидимого слоя: к моменту, когда строка готова, уже
/// решено, каким будет человек. Модели остаётся изложить это текстом.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamRow {
    values: BTreeMap<String, Value>,
}

impl ParamRow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.values.insert(key.into(), value);
    }

    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.values.iter()
    }

    pub fn as_map(&self) -> &BTreeMap<String, Value> {
        &self.values
    }

    /// Подпись для проверки уникальности.
    ///
    /// Строится только по параметрам, помеченным `identity`: два человека с
    /// одинаковым возрастом, но разными биографиями — это разные люди, а вот
    /// совпадение по всему опорному набору означает дубликат.
    pub fn signature(&self, model: &crate::ParamModel) -> String {
        let mut parts = Vec::new();
        for p in model.params.iter().filter(|p| p.identity) {
            let v = self.get(&p.key).map(|v| v.render()).unwrap_or_default();
            parts.push(format!("{}={}", p.key, v));
        }
        parts.join("|")
    }

    /// Блок параметров для подстановки в промпт.
    ///
    /// Фильтрация по назначению принципиальна: все параметры хранятся, но в
    /// промпт уходит только нужное подмножество — биография пишется по одним,
    /// портрет по другим. Иначе на десяти тысячах сущностей набегает лишний
    /// объём входных токенов без всякой пользы.
    pub fn prompt_block(&self, model: &crate::ParamModel, usage: Usage) -> String {
        self.block_filtered(model, usage, &[Mention::Natural, Mention::Required])
    }

    /// Параметры, которые задают фон, но называть их прямо нельзя.
    ///
    /// Подаются моделью отдельным блоком с отдельной инструкцией — иначе она
    /// добросовестно перечислит их в тексте, и выйдет анкета вместо биографии.
    pub fn background_block(&self, model: &crate::ParamModel, usage: Usage) -> String {
        let mut out = String::new();
        for p in &model.params {
            if !p.usage.goes_to(usage) || p.mention != Mention::Background {
                continue;
            }
            let Some(v) = self.get(&p.key) else { continue };
            if v.is_null() {
                continue;
            }
            // Директивой, а не парой «название: значение» — иначе модель
            // принимает фон за факты и пересказывает его в тексте.
            out.push_str(&format!("— {}: {}\n", p.directive_label(), v.render()));
        }
        out
    }

    /// Параметры, которые обязаны прозвучать.
    pub fn required_params(&self, model: &crate::ParamModel) -> Vec<String> {
        model
            .params
            .iter()
            .filter(|p| p.mention == Mention::Required)
            .filter_map(|p| {
                let v = self.get(&p.key)?;
                if v.is_null() {
                    None
                } else {
                    Some(format!("{}: {}", p.title, v.render()))
                }
            })
            .collect()
    }

    fn block_filtered(
        &self,
        model: &crate::ParamModel,
        usage: Usage,
        mentions: &[Mention],
    ) -> String {
        // Собираем по группам, а не по порядку объявления: порядок параметров
        // диктуется связностью (условие корреляции должно стоять раньше цели),
        // и из-за этого одна группа может оказаться разорванной на куски.
        // В промпте она должна выглядеть цельной.
        let mut groups: Vec<&str> = Vec::new();
        let mut lines: std::collections::BTreeMap<&str, Vec<String>> = Default::default();

        // Ограничение на упоминание касается только прозы. Промпту портрета
        // нужны все визуальные параметры: «называть прямо» для изображения
        // смысла не имеет.
        let filter_by_mention = usage != Usage::Visual;

        for p in &model.params {
            if !p.usage.goes_to(usage) {
                continue;
            }
            if filter_by_mention && !mentions.contains(&p.mention) {
                continue;
            }
            let Some(v) = self.get(&p.key) else { continue };
            if v.is_null() {
                continue;
            }
            // Ложный признак в тексте — это отсутствие, а не факт. Переданный
            // модели как «Работа с родственниками: нет», он превращается во
            // фразу «с родственниками он не работает»: просьба не перечислять
            // отрицания соблюдается через раз. Надёжнее не передавать вовсе.
            if filter_by_mention && matches!(v, Value::Bool(false)) {
                continue;
            }
            let g = p.group.as_str();
            if !groups.contains(&g) {
                groups.push(g);
            }
            lines
                .entry(g)
                .or_default()
                .push(format!("{}: {}", p.title, v.render()));
        }

        let mut out = String::new();
        for g in groups {
            if !out.is_empty() {
                out.push('\n');
            }
            if !g.is_empty() {
                out.push_str(&format!("[{g}]\n"));
            }
            for line in lines.get(g).into_iter().flatten() {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

impl FromIterator<(String, Value)> for ParamRow {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        Self { values: iter.into_iter().collect() }
    }
}
