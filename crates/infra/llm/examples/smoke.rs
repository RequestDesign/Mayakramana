//! Проверка связи с провайдерами: работают ли ключи, проходит ли трафик,
//! сколько стоит и сколько занимает один вызов.
//!
//! ```text
//! $env:XAI_API_KEY='xai-...'; $env:OPENAI_API_KEY='sk-...'
//! cargo run -p synthforge-llm --example smoke
//! cargo run -p synthforge-llm --example smoke -- --image
//! ```
//!
//! С российского IP прямой доступ закрыт: OpenAI отвечает
//! `unsupported_country_region_territory`, x.ai не отвечает вовсе. Тогда нужен
//! исходящий прокси — задаётся переменной `OUTBOUND_PROXY`.

use std::sync::Arc;
use std::time::Instant;

use synthforge_llm::{
    Catalog, Effort, ImageModel, ImageRequest, ImageSize, Message, OpenAiCompatText,
    OpenAiImages, ProviderConfig, TextModel, TextRequest,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let want_image = std::env::args().any(|a| a == "--image");
    let proxy = std::env::var("OUTBOUND_PROXY").ok().filter(|p| !p.trim().is_empty());

    let catalog = match Catalog::load("config/model-catalog.json") {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("Каталог моделей не загрузился: {e}");
            eprintln!("Запускать из корня проекта.");
            std::process::exit(1);
        }
    };

    let unverified = catalog.unverified();
    if !unverified.is_empty() {
        println!("⚠ Цены не сверены с консолью провайдера: {}", unverified.join(", "));
        println!("  Учёт расхода и бюджетный потолок будут приблизительными.\n");
    }

    match &proxy {
        Some(p) => println!("Исходящий прокси: {p}\n"),
        None => println!("Идём напрямую, без прокси\n"),
    }

    // ---------------------------------------------------------------- текст
    let xai_key = std::env::var("XAI_API_KEY").unwrap_or_default();
    if xai_key.is_empty() {
        println!("⨯ XAI_API_KEY не задан — текстовая проверка пропущена");
    } else {
        let cfg = ProviderConfig::xai(&xai_key, "grok-4-fast").with_proxy(proxy.clone());
        match OpenAiCompatText::new(cfg, catalog.clone()) {
            Err(e) => println!("⨯ Клиент к x.ai не создался: {e}"),
            Ok(client) => {
                let req = TextRequest::new(vec![
                    Message::system("Отвечай одним словом, без пояснений."),
                    Message::user("Назови столицу Португалии."),
                ])
                .effort(Effort::Low)
                .max_tokens(16);

                // Внутренний вызов, а не порт: диагностике нужна подробная
                // классификация, а порт её схлопывает.
                let t = Instant::now();
                match client.complete_inner(req).await {
                    Ok(r) => {
                        println!("✓ x.ai / {} — «{}»", r.model, r.text.trim());
                        println!(
                            "  {} мс · токенов {}→{} · ${:.6}",
                            t.elapsed().as_millis(),
                            r.usage.tokens_in,
                            r.usage.tokens_out,
                            r.usage.cost_usd
                        );
                        let per_10k = r.usage.cost_usd * 10_000.0;
                        println!("  в пересчёте на 10 000 таких вызовов: ${per_10k:.2}");
                    }
                    Err(e) => report(&e),
                }
            }
        }
    }

    // ----------------------------------------------------------- изображения
    if !want_image {
        println!("\nИзображение не проверялось (добавьте --image; вызов платный).");
        return;
    }

    let openai_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if openai_key.is_empty() {
        println!("⨯ OPENAI_API_KEY не задан — проверка изображений пропущена");
        return;
    }

    let cfg = ProviderConfig::openai(&openai_key, "gpt-image-1").with_proxy(proxy);
    match OpenAiImages::new(cfg, catalog.clone()) {
        Err(e) => println!("⨯ Клиент к OpenAI не создался: {e}"),
        Ok(client) => {
            let req = ImageRequest::new(
                "Документальный портрет мужчины 49 лет, русского, плотного телосложения. \
                 Лицо с едва заметной асимметрией, волосы аккуратные но без укладки, \
                 лёгкие мешки под глазами. Выражение спокойное, нейтральное, без улыбки. \
                 Рубашка без галстука. Рабочий кабинет, книжный стеллаж позади, \
                 расфокусирован. Любительский портрет, естественный свет из окна. \
                 Не студийная фотосессия, без ретуши.",
            )
            .size(ImageSize::Portrait);

            let t = Instant::now();
            match client.render_inner(req).await {
                Ok(r) => {
                    let path = "smoke-portrait.png";
                    if let Err(e) = std::fs::write(path, &r.png) {
                        println!("⨯ Не записался файл: {e}");
                    }
                    println!(
                        "✓ OpenAI / {} — {} КБ → {path}",
                        r.model,
                        r.png.len() / 1024
                    );
                    println!(
                        "  {} мс · ${:.4}",
                        t.elapsed().as_millis(),
                        r.usage.cost_usd
                    );
                    let per_40k = r.usage.cost_usd * 40_000.0;
                    println!("  в пересчёте на 40 000 изображений: ${per_40k:.0}");
                }
                Err(e) => report(&e),
            }
        }
    }
}

fn report(e: &synthforge_llm::Error) {
    println!("⨯ {e}");
    if e.is_fatal() {
        println!("  Это не лечится повтором: нужен прокси, другой ключ или пополнение счёта.");
    } else if e.is_retryable() {
        println!("  Сбой преходящий, повтор имеет смысл.");
    }
}
