use crate::error::{Error, Result};

/// Зарезервированные Nexorium query-параметры.
///
/// Всё остальное трактуется как **фильтр по одноимённому полю** (грабля №2),
/// поэтому имена фильтров надо проверять, а не надеяться.
const RESERVED: &[&str] = &["sort", "page", "per_page", "format"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

impl Dir {
    fn as_str(self) -> &'static str {
        match self {
            Dir::Asc => "asc",
            Dir::Desc => "desc",
        }
    }
}

/// Сортировка. Рендерится строго как `field:dir`.
///
/// Отдельного параметра направления не существует: `&order=asc` был бы понят как
/// фильтр «поле `order` равно `asc`» и обнулил бы выдачу.
#[derive(Debug, Clone)]
pub struct Sort {
    field: String,
    dir: Dir,
}

impl Sort {
    pub fn asc(field: impl Into<String>) -> Self {
        Self { field: field.into(), dir: Dir::Asc }
    }

    pub fn desc(field: impl Into<String>) -> Self {
        Self { field: field.into(), dir: Dir::Desc }
    }

    pub fn render(&self) -> String {
        format!("{}:{}", self.field, self.dir.as_str())
    }
}

/// Запрос списка записей.
///
/// Сортировка обязательна конструктивно: без явного `ORDER BY` PostgreSQL не
/// гарантирует порядок при `LIMIT/OFFSET`, и постраничный обход большой коллекции
/// часть строк вернёт дважды, а часть потеряет — при сходящемся `pagination.total`
/// (грабля №3).
///
/// Для чтения коллекции целиком постраничный обход не нужен вовсе — есть
/// [`Nexorium::export`](crate::Nexorium::export), он на порядок быстрее (грабля №4).
#[derive(Debug, Clone)]
pub struct Query {
    sort: Sort,
    filters: Vec<(String, String)>,
    page: u32,
    per_page: u32,
}

impl Query {
    pub fn new(sort: Sort) -> Self {
        Self { sort, filters: Vec::new(), page: 1, per_page: 100 }
    }

    /// Фильтр по полю. Ошибка, если имя совпало с зарезервированным параметром.
    pub fn filter(mut self, field: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let field = field.into();
        if RESERVED.contains(&field.as_str()) {
            return Err(Error::Config(format!(
                "«{field}» — зарезервированный параметр Nexorium, фильтровать по нему нельзя"
            )));
        }
        self.filters.push((field, value.into()));
        Ok(self)
    }

    pub fn page(mut self, page: u32) -> Self {
        self.page = page.max(1);
        self
    }

    pub fn per_page(mut self, per_page: u32) -> Self {
        self.per_page = per_page.clamp(1, 1000);
        self
    }

    pub fn params(&self) -> Vec<(String, String)> {
        let mut out = Vec::with_capacity(self.filters.len() + 3);
        out.push(("sort".into(), self.sort.render()));
        out.push(("page".into(), self.page.to_string()));
        out.push(("per_page".into(), self.per_page.to_string()));
        out.extend(self.filters.iter().cloned());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_renders_with_colon_not_separate_param() {
        assert_eq!(Sort::asc("url").render(), "url:asc");
        assert_eq!(Sort::desc("created_at").render(), "created_at:desc");
    }

    #[test]
    fn reserved_names_rejected_as_filters() {
        assert!(Query::new(Sort::asc("id")).filter("sort", "url").is_err());
        assert!(Query::new(Sort::asc("id")).filter("per_page", "10").is_err());
        assert!(Query::new(Sort::asc("id")).filter("host_id", "example.com").is_ok());
    }

    #[test]
    fn params_always_carry_sort() {
        let q = Query::new(Sort::asc("natural_key")).per_page(250);
        let p = q.params();
        assert!(p.contains(&("sort".to_string(), "natural_key:asc".to_string())));
        assert!(p.contains(&("per_page".to_string(), "250".to_string())));
    }
}
