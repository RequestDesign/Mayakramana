use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::param::{ParamDef, Usage};
use crate::rule::{Bias, HardRule, SoftRule};

/// Словарь параметров — невидимый слой в его материальном виде.
///
/// Три части, как и договаривались: сами параметры, допустимые диапазоны
/// (внутри [`crate::Domain`]) и правила связности. Хранится в JSON, потому что
/// его же формат — родной для Nexorium, и словарь можно положить туда записью
/// без конвертации.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamModel {
    /// Что описывает словарь: `role-doctor`, `human-visual` и т.п.
    pub name: String,
    #[serde(default = "one_u32")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Словари, которые подмешиваются перед собственными параметрами.
    ///
    /// Так общие части живут отдельно и переиспользуются: `human-core` и
    /// `human-visual` одни на все роли, а врач, консультант и психолог
    /// добавляют к ним своё. Пути указываются относительно файла словаря.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    pub params: Vec<ParamDef>,
    /// Жёсткие инварианты.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hard: Vec<HardRule>,
    /// Мягкие корреляции.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub soft: Vec<SoftRule>,
}

fn one_u32() -> u32 {
    1
}

impl ParamModel {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: 1,
            description: String::new(),
            extends: Vec::new(),
            params: Vec::new(),
            hard: Vec::new(),
            soft: Vec::new(),
        }
    }

    /// Подмешать базовый словарь **перед** собственными параметрами.
    ///
    /// Порядок важен: мягкие корреляции опираются на уже сэмплированные
    /// значения, поэтому общие параметры (возраст, пол) обязаны идти раньше
    /// ролевых, которые от них зависят.
    ///
    /// Совпадение ключей — не ошибка, а штатное сужение: роль вправе задать
    /// врачу более узкий диапазон возраста, чем общечеловеческий. Определение
    /// заменяется, позиция сохраняется.
    pub fn absorb(&mut self, base: ParamModel) {
        let mut merged: Vec<ParamDef> = Vec::with_capacity(base.params.len() + self.params.len());

        for bp in base.params {
            match self.params.iter().position(|p| p.key == bp.key) {
                Some(i) => merged.push(self.params.remove(i)),
                None => merged.push(bp),
            }
        }
        merged.append(&mut self.params);
        self.params = merged;

        let mut hard = base.hard;
        hard.append(&mut self.hard);
        self.hard = hard;

        let mut soft = base.soft;
        soft.append(&mut self.soft);
        self.soft = soft;
    }

    pub fn get(&self, key: &str) -> Option<&ParamDef> {
        self.params.iter().find(|p| p.key == key)
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.params.iter().position(|p| p.key == key)
    }

    /// Параметры, уходящие в промпт указанного назначения.
    pub fn prompt_params(&self, usage: Usage) -> impl Iterator<Item = &ParamDef> {
        self.params.iter().filter(move |p| p.usage.goes_to(usage))
    }

    // ------------------------------------------------------------ загрузка

    pub fn from_json_str(src: &str) -> Result<Self> {
        let model: Self = serde_json::from_str(src)?;
        if !model.extends.is_empty() {
            return Err(Error::Invalid(vec![format!(
                "словарь «{}» наследует {:?}, но разбирается из строки — \
                 пути наследования разрешаются только при загрузке из файла",
                model.name, model.extends
            )]));
        }
        model.check()?;
        Ok(model)
    }

    pub fn to_json_string(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Загрузить словарь, разрешив наследование, и проверить результат.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let model = Self::load_at(path.as_ref(), 0)?;
        model.check()?;
        Ok(model)
    }

    fn load_at(path: &Path, depth: u32) -> Result<Self> {
        const MAX_DEPTH: u32 = 8;
        if depth > MAX_DEPTH {
            return Err(Error::Invalid(vec![format!(
                "наследование словарей глубже {MAX_DEPTH} уровней на «{}» — \
                 похоже на цикл",
                path.display()
            )]));
        }

        let src = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        let mut model: Self = serde_json::from_str(&src)?;

        let dir = path.parent().unwrap_or(Path::new("."));
        let extends = std::mem::take(&mut model.extends);

        // В обратном порядке: absorb кладёт базу перед собственными
        // параметрами, поэтому первый в списке должен подмешиваться последним.
        for rel in extends.iter().rev() {
            let base = Self::load_at(&dir.join(rel), depth + 1)?;
            model.absorb(base);
        }

        Ok(model)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| Error::Io {
                path: dir.display().to_string(),
                source,
            })?;
        }
        std::fs::write(path, self.to_json_string()?).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })
    }

    // ------------------------------------------------------------ проверка

    /// Проверка словаря целиком. Ошибка в словаре размножится на всю популяцию,
    /// поэтому ловить её надо при загрузке, а не по факту плохого контента.
    pub fn check(&self) -> Result<()> {
        let issues = self.issues();
        if issues.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(issues))
        }
    }

    /// Совместим ли сдвиг с областью значений параметра.
    ///
    /// Несовместимый сдвиг не падает при генерации — он **молча не делает
    /// ничего**. Опечатка в названии варианта или вес, наложенный на числовой
    /// параметр, тихо обесценивают правило, и заметить это по контенту почти
    /// невозможно. Поэтому ловим при загрузке.
    pub fn bias_issues(&self, bias: &Bias) -> Vec<String> {
        use crate::param::Domain;

        let mut out = Vec::new();
        let target = bias.target();
        let Some(def) = self.get(target) else {
            return vec![format!("неизвестный параметр «{target}»")];
        };

        match bias {
            Bias::Weight { value, .. } => match &def.domain {
                Domain::Enum { variants } | Domain::MultiEnum { variants, .. } => {
                    if !variants.iter().any(|v| &v.value == value) {
                        out.push(format!(
                            "«{target}» не имеет варианта «{value}» — сдвиг веса не сработает \
                             (доступны: {})",
                            variants
                                .iter()
                                .map(|v| v.value.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
                Domain::Bool { .. } => {
                    if value != "true" && value != "false" {
                        out.push(format!(
                            "«{target}» булев, сдвиг веса принимает только 'true' или 'false', \
                             а не «{value}»"
                        ));
                    }
                }
                _ => out.push(format!(
                    "«{target}» числовой или производный — сдвиг веса к нему неприменим, \
                     нужен range"
                )),
            },

            Bias::Range { .. } => {
                if !matches!(def.domain, Domain::Int { .. } | Domain::Float { .. }) {
                    out.push(format!(
                        "«{target}» не числовой — сужение диапазона к нему неприменимо, \
                         нужен weight"
                    ));
                }
            }

            Bias::Force { .. } => {}
        }

        out
    }

    pub fn issues(&self) -> Vec<String> {
        let mut out = Vec::new();

        if self.params.is_empty() {
            out.push("словарь не содержит ни одного параметра".into());
        }

        // Дубликаты ключей.
        let mut seen = HashSet::new();
        for p in &self.params {
            if p.key.trim().is_empty() {
                out.push("есть параметр с пустым ключом".into());
            }
            if !seen.insert(p.key.as_str()) {
                out.push(format!("ключ «{}» объявлен дважды", p.key));
            }
            out.extend(p.domain.issues(&p.key));
        }

        // Жёсткие правила: ссылки только на объявленные параметры.
        for r in &self.hard {
            let mut refs = Vec::new();
            r.expr.referenced_params(&mut refs);
            for name in refs {
                if self.get(&name).is_none() {
                    out.push(format!(
                        "правило «{}» ссылается на неизвестный параметр «{name}»",
                        r.name
                    ));
                }
            }
        }

        // Мягкие правила: и ссылки, и порядок объявления.
        for r in &self.soft {
            let mut cond_refs = Vec::new();
            r.when.referenced_params(&mut cond_refs);

            for name in &cond_refs {
                if self.get(name).is_none() {
                    out.push(format!(
                        "корреляция «{}»: условие ссылается на неизвестный параметр «{name}»",
                        r.name
                    ));
                }
            }

            if r.then.is_empty() {
                out.push(format!("корреляция «{}» ничего не меняет", r.name));
            }

            for bias in &r.then {
                let target = bias.target();
                let Some(target_idx) = self.index_of(target) else {
                    out.push(format!(
                        "корреляция «{}» правит неизвестный параметр «{target}»",
                        r.name
                    ));
                    continue;
                };

                out.extend(
                    self.bias_issues(bias)
                        .into_iter()
                        .map(|m| format!("корреляция «{}»: {m}", r.name)),
                );

                // Условие вычисляется по уже сэмплированным значениям, поэтому
                // все его параметры обязаны стоять в словаре раньше цели.
                // Иначе правило будет молча не срабатывать — худший вид поломки.
                for name in &cond_refs {
                    if let Some(cond_idx) = self.index_of(name) {
                        if cond_idx >= target_idx {
                            out.push(format!(
                                "корреляция «{}»: условие опирается на «{name}», \
                                 но он объявлен не раньше цели «{target}» — \
                                 правило никогда не сработает, переставьте параметры",
                                r.name
                            ));
                        }
                    }
                }
            }
        }

        if !self.params.iter().any(|p| p.identity) {
            out.push(
                "ни один параметр не помечен identity — проверка уникальности работать не будет"
                    .into(),
            );
        }

        out
    }
}
