//! Демо-страницы для приёмки глазами.
//!
//! Центр как составную сущность по строкам в базе оценить невозможно: не видно,
//! сходятся ли описание дома, программа и люди. Одна страница на центр с
//! единым шаблоном показывает всё сразу.
//!
//! Это внутренний инструмент приёмки, а не витрина. Страницы помечены
//! соответственно и в поисковые системы не предназначены.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

fn s(r: &Value, k: &str) -> String {
    r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn fmt_rub(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

fn esc(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Путь к снимку относительно каталога страниц.
///
/// Снимки лежат в `out/<сессия>/…`, страницы — в `out/site/`, поэтому путь
/// берётся от `out` и поднимается на уровень вверх.
fn rel_image(path: &str) -> Option<String> {
    let norm = path.replace('\\', "/");
    let idx = norm.find("/out/").map(|i| i + 5).or_else(|| norm.strip_prefix("out/").map(|_| 4))?;
    Some(format!("../{}", &norm[idx..]))
}

fn image_of(r: &Value, role: &str) -> Option<String> {
    r.get("images")?
        .as_array()?
        .iter()
        .find(|i| i.get("role").and_then(|v| v.as_str()) == Some(role))
        .and_then(|i| i.get("path").and_then(|v| v.as_str()))
        .and_then(rel_image)
}

const CSS: &str = r#"
*{box-sizing:border-box}body{margin:0;font:16px/1.55 -apple-system,Segoe UI,Roboto,Arial,sans-serif;color:#1f2a37;background:#f4f6fb}
.banner{background:#fff4d6;color:#7a5a00;padding:8px 16px;font-size:13px;text-align:center;border-bottom:1px solid #f0dca0}
.wrap{max-width:1040px;margin:0 auto;padding:24px 20px 60px}
h1{font-size:34px;margin:8px 0 4px}h2{font-size:22px;margin:36px 0 12px}
.meta{color:#5b6b82;margin-bottom:20px}.tag{display:inline-block;background:#e7eefc;color:#2a4ea3;border-radius:14px;padding:3px 10px;font-size:13px;margin:0 6px 6px 0}
.card{background:#fff;border-radius:14px;padding:22px;box-shadow:0 2px 10px rgba(30,50,90,.06);margin-bottom:16px}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(230px,1fr));gap:14px}
.person{background:#fff;border-radius:14px;padding:14px;box-shadow:0 2px 10px rgba(30,50,90,.06)}
.person img{width:100%;aspect-ratio:3/4;object-fit:cover;border-radius:10px;background:#dde3ee}
.ph{width:100%;aspect-ratio:3/4;border-radius:10px;background:linear-gradient(135deg,#dde3ee,#c9d3e3);display:flex;align-items:center;justify-content:center;color:#8492a8;font-size:13px}
.person b{display:block;margin-top:10px}.person span{color:#5b6b82;font-size:14px}
.photos{display:grid;grid-template-columns:repeat(3,1fr);gap:10px}.photos img{width:100%;aspect-ratio:3/2;object-fit:cover;border-radius:10px}
table{width:100%;border-collapse:collapse;background:#fff;border-radius:14px;overflow:hidden;box-shadow:0 2px 10px rgba(30,50,90,.06)}
th,td{text-align:left;padding:12px 14px;border-bottom:1px solid #edf0f5;font-size:15px}th{background:#f7f9fc;color:#5b6b82;font-weight:600}
a{color:#2a4ea3;text-decoration:none}a:hover{text-decoration:underline}.quote{font-style:italic;color:#3b4a60;margin-top:8px}
dl{display:grid;grid-template-columns:220px 1fr;gap:6px 16px;margin:0}dt{color:#5b6b82}dd{margin:0}
"#;

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>{}</title><style>{CSS}</style></head><body>\
         <div class=\"banner\">Внутренний просмотр сгенерированных данных для приёмки. Не публикуется.</div>\
         <div class=\"wrap\">{body}</div></body></html>",
        esc(title)
    )
}

fn person_card(r: &Value, subtitle_keys: &[&str]) -> String {
    let img = image_of(r, "portrait")
        .map(|p| format!("<img src=\"{}\" alt=\"\">", esc(&p)))
        .unwrap_or_else(|| "<div class=\"ph\">портрет не генерировался</div>".into());
    let sub = subtitle_keys
        .iter()
        .map(|k| s(r, k))
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    let age = r.get("age").and_then(|v| v.as_i64()).unwrap_or(0);
    format!(
        "<div class=\"person\">{img}<b>{}</b><span>{} · {} лет</span></div>",
        esc(&s(r, "full_name")),
        esc(&sub),
        age
    )
}

/// Собрать страницы. Возвращает число страниц центров.
pub fn build(out_dir: &str, pools: &HashMap<String, Value>, centers: &[Value]) -> std::io::Result<usize> {
    let site = Path::new(out_dir).join("site");
    std::fs::create_dir_all(&site)?;

    let mut rows = String::new();

    for (i, c) in centers.iter().enumerate() {
        let file = format!("center-{}.html", i + 1);
        let place = pools.get(&s(c, "place_key")).cloned().unwrap_or(Value::Null);
        let program = pools.get(&s(c, "program_key")).cloned().unwrap_or(Value::Null);
        let director = pools.get(&s(c, "director_key")).cloned().unwrap_or(Value::Null);

        let _ = write!(
            rows,
            "<tr><td><a href=\"{file}\">{}</a></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&s(c, "center_name")),
            esc(&s(c, "focus")),
            esc(&s(c, "region")),
            c.get("capacity").and_then(|v| v.as_i64()).unwrap_or(0),
            esc(&s(c, "setting")),
            esc(&s(&director, "full_name")),
        );

        let mut body = String::new();
        let _ = write!(
            body,
            "<p><a href=\"index.html\">← все центры</a></p><h1>{}</h1>\
             <div class=\"meta\"><span class=\"tag\">{}</span><span class=\"tag\">{}</span>\
             <span class=\"tag\">{} мест</span><span class=\"tag\">{}</span>\
             <span class=\"tag\">{} км от города</span></div>",
            esc(&s(c, "center_name")),
            esc(&s(c, "focus")),
            esc(&s(c, "region")),
            c.get("capacity").and_then(|v| v.as_i64()).unwrap_or(0),
            esc(&s(c, "setting")),
            c.get("distance_km").and_then(|v| v.as_i64()).unwrap_or(0),
        );

        let founded = c.get("founded_year").and_then(|v| v.as_i64()).unwrap_or(0);
        let mut extra_tags = String::new();
        if founded > 0 {
            let _ = write!(extra_tags, "<span class=\"tag\">с {founded} года</span>");
        }
        if c.get("round_the_clock").and_then(|v| v.as_bool()).unwrap_or(false) {
            extra_tags.push_str("<span class=\"tag\">круглосуточно</span>");
        }
        if !extra_tags.is_empty() {
            let _ = write!(body, "<div class=\"meta\" style=\"margin-top:-12px\">{extra_tags}</div>");
        }

        let _ = write!(
            body,
            "<div class=\"card\"><p>{}</p><p>{}</p></div>",
            esc(&s(c, "about")),
            esc(&s(c, "approach_text"))
        );

        // Услуги и цены — из данных центра, а не из текста модели.
        let offers: Vec<Value> = c
            .get("services_json")
            .and_then(|v| v.as_str())
            .and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or_default();
        if !offers.is_empty() {
            let _ = write!(body, "<h2>Услуги и цены</h2><table><tr><th>Услуга</th><th>Стоимость</th></tr>");
            for o in &offers {
                let price = o.get("price_from").and_then(|v| v.as_i64()).unwrap_or(0);
                let unit = s(o, "unit");
                let cell = if price == 0 {
                    "входит в стоимость".to_string()
                } else {
                    format!("от {} ₽ за {}", fmt_rub(price), esc(&unit))
                };
                let _ = write!(body, "<tr><td>{}</td><td>{cell}</td></tr>", esc(&s(o, "title")));
            }
            body.push_str("</table>");
            if let Some(total) = c.get("course_price").and_then(|v| v.as_i64()) {
                let _ = write!(
                    body,
                    "<p class=\"meta\">Полный курс по программе — от {} ₽. \
                     Цены — ориентиры генератора, с рынком не сверены.</p>",
                    fmt_rub(total)
                );
            }
        }

        let history = s(c, "history");
        if !history.is_empty() {
            let _ = write!(body, "<h2>История</h2><div class=\"card\"><p>{}</p></div>", esc(&history));
        }

        // Здание: снимки, если делались, и описание.
        let photos: Vec<String> = ["facade", "room", "territory"]
            .iter()
            .filter_map(|role| image_of(&place, role))
            .map(|p| format!("<img src=\"{}\" alt=\"\">", esc(&p)))
            .collect();
        let _ = write!(body, "<h2>Дом и территория</h2>");
        if !photos.is_empty() {
            let _ = write!(body, "<div class=\"photos\">{}</div><br>", photos.join(""));
        }
        let _ = write!(
            body,
            "<div class=\"card\"><p>{}</p><p>{}</p><p>{}</p></div>",
            esc(&s(&place, "description")),
            esc(&s(&place, "territory")),
            esc(&s(&place, "rooms"))
        );

        // Программа.
        let _ = write!(
            body,
            "<h2>Программа: {}</h2><div class=\"card\"><dl>\
             <dt>Основа</dt><dd>{}</dd><dt>Длительность</dt><dd>{}</dd>\
             <dt>Сопровождение после выписки</dt><dd>{} мес.</dd></dl>\
             <p>{}</p><p>{}</p></div>",
            esc(&s(&program, "title")),
            esc(&s(&program, "approach")),
            esc(&s(&program, "duration")),
            program.get("aftercare_months").and_then(|v| v.as_i64()).unwrap_or(0),
            esc(&s(&program, "description")),
            esc(&s(&program, "stages")),
        );

        // Руководитель.
        let portrait = image_of(&director, "portrait")
            .map(|p| {
                format!(
                    "<img src=\"{}\" alt=\"\" style=\"float:left;width:180px;aspect-ratio:3/4;\
                     object-fit:cover;border-radius:10px;margin:0 20px 10px 0\">",
                    esc(&p)
                )
            })
            .unwrap_or_default();
        let _ = write!(
            body,
            "<h2>Руководитель</h2><div class=\"card\" style=\"overflow:hidden\">{portrait}<b>{}</b><p>{}</p><p>{}</p>\
             <p class=\"quote\">{}</p></div>",
            esc(&s(&director, "full_name")),
            esc(&s(&director, "biography")),
            esc(&s(&director, "professional_path")),
            esc(&s(&director, "quote")),
        );

        // Команда.
        let _ = write!(body, "<h2>Команда</h2><div class=\"grid\">");
        for key in c.get("staff_keys").and_then(|v| v.as_array()).into_iter().flatten() {
            if let Some(m) = key.as_str().and_then(|k| pools.get(k)) {
                body.push_str(&person_card(m, &["specialty", "education", "path_to_work"]));
            }
        }
        body.push_str("</div>");

        std::fs::write(site.join(&file), page(&s(c, "center_name"), &body))?;
    }

    let index = format!(
        "<h1>Центры</h1><div class=\"meta\">Собрано из сгенерированных зданий, программ и людей. \
         Всего: {}</div><table><tr><th>Название</th><th>Специализация</th><th>Регион</th>\
         <th>Мест</th><th>Окружение</th><th>Руководитель</th></tr>{rows}</table>",
        centers.len()
    );
    std::fs::write(site.join("index.html"), page("Центры", &index))?;

    Ok(centers.len())
}
