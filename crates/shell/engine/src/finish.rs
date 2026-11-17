//! Доводка снимка после модели.
//!
//! Модель изображений отдаёт узнаваемую «генеративную» картинку: одинаковую
//! тёплую тонировку на всех кадрах, стерильно чистый файл, гиперрезкую кожу.
//! Часть этого снимается промптом, остальное надёжнее убрать кодом — это
//! дёшево, детерминированно и работает и на уже сделанных снимках.
//!
//! Что делается:
//! 1. баланс белого по «серому миру» — уходит общая жёлто-коричневая тонировка;
//! 2. чуть ниже насыщенность — как у обычной камеры, а не у рекламы;
//! 3. кадрирование со сдвигом по ключу сущности — серия не выглядит отснятой
//!    по одному шаблону;
//! 4. размер веб-фотографии и лёгкое смягчение — уходит гиперрезкость;
//! 5. шум сенсора и сжатие JPEG — как у файла с телефона или фотоаппарата.
//!
//! Сила каждого шага своя у каждого снимка (по ключу): у настоящих фото на
//! одной странице разная техника — один файл чистый, другой мыльный и
//! пережатый, у третьего остался жёлтый свет ламп. Одинаково «обработанная»
//! серия выдаёт генерацию не хуже одинаковых лиц (живой прогон 16.11).

use image::{imageops, DynamicImage, ImageBuffer, Rgb};
use rand::{Rng, SeedableRng};

/// Длинная сторона готового снимка: размер фото на странице специалиста.
const LONG_SIDE: u32 = 1100;

/// Характер файла: у каждого снимка свой, но постоянный для ключа.
struct Look {
    /// Насколько тянуть к нейтральному балансу (1.0 — полностью). Не до
    /// конца: свет ламп или окна, заданный промптом, должен частично остаться.
    wb: f32,
    saturation: f32,
    /// Гамма: чуть темнее или светлее, как у автоэкспозиции телефона.
    gamma: f32,
    blur: f32,
    noise: f32,
    jpeg: u8,
    /// Доля размера, до которой снимок ужимается и растягивается обратно:
    /// мыльная картинка дешёвого телефона или пересланного файла.
    mush: Option<f32>,
    /// Доля «шумодава»: смешивание с размытой копией. Модель рисует
    /// подчёркнутую фактуру кожи и ткани с локальным контрастом, как HDR —
    /// по этому её узнают. Телефон, наоборот, мелкую фактуру сглаживает.
    smooth: f32,
}

impl Look {
    fn for_seed(seed: u64) -> Self {
        let mut r = rand::rngs::StdRng::seed_from_u64(seed ^ 0x9e37_79b9_7f4a_7c15);
        let mut span = |a: f32, b: f32| a + (b - a) * r.gen::<f32>();
        let wb = span(0.45, 0.85);
        let saturation = span(0.92, 1.02);
        let gamma = span(0.96, 1.04);
        let blur = span(0.3, 0.6);
        let noise = span(1.5, 3.5);
        let jpeg = span(74.0, 90.0) as u8;
        let mush = if span(0.0, 1.0) < 0.15 { Some(span(0.62, 0.78)) } else { None };
        let smooth = span(0.35, 0.6);
        Self { wb, saturation, gamma, blur, noise, jpeg, mush, smooth }
    }

    /// Студийная съёмка фотографом: чистый файл, почти без шума и мыла.
    /// Остаётся только лёгкое сглаживание фактуры (против «HDR-кожи»).
    fn clean(seed: u64) -> Self {
        let mut r = rand::rngs::StdRng::seed_from_u64(seed ^ 0x9e37_79b9_7f4a_7c15);
        let mut span = |a: f32, b: f32| a + (b - a) * r.gen::<f32>();
        Self {
            wb: span(0.8, 0.95),
            saturation: span(0.97, 1.0),
            gamma: span(0.98, 1.02),
            blur: 0.0,
            noise: span(0.4, 0.9),
            jpeg: 92,
            mush: None,
            smooth: span(0.05, 0.12),
        }
    }
}

/// PHOTO_STYLE=phone — прежняя «телефонная» доводка (шум, мыло, сжатие).
/// По умолчанию — чистая, как у снимков фотографа.
fn phone_style() -> bool {
    std::env::var("PHOTO_STYLE").as_deref() == Ok("phone")
}

/// Довести снимок. `key` задаёт кадрирование и шум: у одной сущности они
/// одинаковы при повторной обработке, у разных — разные.
pub fn finish_photo(bytes: &[u8], key: &str) -> Result<Vec<u8>, image::ImageError> {
    let mut img = image::load_from_memory(bytes)?.to_rgb8();
    let seed = fnv(key);
    let look = if phone_style() { Look::for_seed(seed) } else { Look::clean(seed) };

    neutralize(&mut img, &look);
    let img = crop(&img, seed);
    let img = resize(&img);
    let img = match look.mush {
        Some(k) => mush(&img, k),
        None => img,
    };
    let img = phone_smooth(&img, look.smooth);
    let mut img = if look.blur > 0.0 { imageops::blur(&img, look.blur) } else { img };
    add_noise(&mut img, seed, look.noise);

    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, look.jpeg)
        .encode_image(&DynamicImage::ImageRgb8(img))?;
    Ok(out.into_inner())
}

/// Серый мир: средний цвет кадра должен быть нейтральным. Тянем каналы к нему
/// не до конца — у настоящих снимков свет тоже бывает тёплым или холодным.
fn neutralize(img: &mut ImageBuffer<Rgb<u8>, Vec<u8>>, look: &Look) {
    let n = (img.width() as f64 * img.height() as f64).max(1.0);
    let mut sum = [0f64; 3];
    for p in img.pixels() {
        for c in 0..3 {
            sum[c] += p.0[c] as f64;
        }
    }
    let mean = sum.map(|s| (s / n) as f32);
    let grey = (mean[0] + mean[1] + mean[2]) / 3.0;
    let gain = mean.map(|m| 1.0 + look.wb * (grey / m.max(1.0) - 1.0));

    for p in img.pixels_mut() {
        let mut c = [0f32; 3];
        for i in 0..3 {
            c[i] = p.0[i] as f32 * gain[i];
        }
        let l = (c[0] + c[1] + c[2]) / 3.0;
        for i in 0..3 {
            let v = (l + (c[i] - l) * look.saturation).clamp(0.0, 255.0) / 255.0;
            p.0[i] = (v.powf(look.gamma) * 255.0) as u8;
        }
    }
}

/// Кадр 4:5 со сдвигом: от 90 до 97% ширины, смещение по горизонтали и чуть
/// по вертикали. Лёгкое — композицию задаёт промпт, здесь только разнобой.
fn crop(img: &ImageBuffer<Rgb<u8>, Vec<u8>>, seed: u64) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let (w, h) = img.dimensions();
    let zoom = 0.90 + (seed % 8) as f32 / 100.0;
    let cw = ((w as f32 * zoom) as u32).max(1);
    let ch = ((cw as f32 * 5.0 / 4.0) as u32).min(h);
    let dx = ((seed >> 8) % 101) as f32 / 100.0;
    let dy = ((seed >> 16) % 101) as f32 / 100.0 * 0.4;
    let x = ((w - cw) as f32 * dx) as u32;
    let y = ((h - ch) as f32 * dy) as u32;
    imageops::crop_imm(img, x, y, cw, ch).to_image()
}

fn resize(img: &ImageBuffer<Rgb<u8>, Vec<u8>>) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    if long <= LONG_SIDE {
        return img.clone();
    }
    let k = LONG_SIDE as f32 / long as f32;
    imageops::resize(img, (w as f32 * k) as u32, (h as f32 * k) as u32, imageops::FilterType::Lanczos3)
}

/// Сгладить мелкую фактуру, сохранив крупные формы: смесь с размытой копией.
fn phone_smooth(img: &ImageBuffer<Rgb<u8>, Vec<u8>>, k: f32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let soft = imageops::blur(img, 1.6);
    let mut out = img.clone();
    for (o, s) in out.pixels_mut().zip(soft.pixels()) {
        for c in 0..3 {
            o.0[c] = (o.0[c] as f32 * (1.0 - k) + s.0[c] as f32 * k) as u8;
        }
    }
    out
}

/// Ужать и растянуть обратно: детали теряются так, как у дешёвой камеры.
fn mush(img: &ImageBuffer<Rgb<u8>, Vec<u8>>, k: f32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    let (w, h) = img.dimensions();
    let small = imageops::resize(
        img,
        ((w as f32 * k) as u32).max(1),
        ((h as f32 * k) as u32).max(1),
        imageops::FilterType::Triangle,
    );
    imageops::resize(&small, w, h, imageops::FilterType::Triangle)
}

/// Яркостный шум: на стерильно чистом файле его отсутствие заметнее, чем
/// кажется.
fn add_noise(img: &mut ImageBuffer<Rgb<u8>, Vec<u8>>, seed: u64, sigma: f32) {
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    for p in img.pixels_mut() {
        // Сумма трёх равномерных ≈ нормальное распределение.
        let n: f32 = (rng.gen::<f32>() + rng.gen::<f32>() + rng.gen::<f32>() - 1.5) * sigma * 2.0;
        for c in p.0.iter_mut() {
            *c = (*c as f32 + n).clamp(0.0, 255.0) as u8;
        }
    }
}

/// Устойчивый хеш ключа (FNV-1a): от версии компилятора не зависит.
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сильная жёлтая тонировка после доводки заметно ослабевает (не до нуля:
    /// часть света сцены остаётся намеренно).
    #[test]
    fn warm_cast_is_neutralized() {
        let img = ImageBuffer::from_fn(400, 600, |x, y| {
            let v = ((x + y) % 120) as u8 + 60;
            Rgb([v.saturating_add(40), v.saturating_add(25), v.saturating_sub(10)])
        });
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img).write_to(&mut png, image::ImageFormat::Png).unwrap();

        let out = finish_photo(png.get_ref(), "doctor:1").unwrap();
        assert!(out.starts_with(&[0xFF, 0xD8, 0xFF]), "должен получиться JPEG");

        let back = image::load_from_memory(&out).unwrap().to_rgb8();
        let n = (back.width() * back.height()) as f64;
        let mean: Vec<f64> = (0..3).map(|c| back.pixels().map(|p| p.0[c] as f64).sum::<f64>() / n).collect();
        let spread = mean.iter().cloned().fold(f64::MIN, f64::max) - mean.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 30.0, "тонировка осталась: средние каналы {mean:?}");
        let (w, h) = back.dimensions();
        assert!((h as f32 / w as f32 - 1.25).abs() < 0.02, "кадр 4:5, получилось {w}×{h}");
    }

    /// Один ключ — один результат; разные ключи — разный кадр.
    #[test]
    fn crop_is_stable_per_entity_and_differs_between_entities() {
        let img = ImageBuffer::from_fn(1024, 1536, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let a1 = crop(&img, fnv("doctor:1"));
        let a2 = crop(&img, fnv("doctor:1"));
        let b = crop(&img, fnv("doctor:2"));
        assert_eq!(a1.as_raw(), a2.as_raw());
        assert_ne!(a1.as_raw(), b.as_raw());
    }

    /// У разных снимков разный характер файла, у одного — всегда один.
    #[test]
    fn look_differs_between_entities() {
        let a = Look::for_seed(fnv("doctor:1#portrait"));
        let a2 = Look::for_seed(fnv("doctor:1#portrait"));
        assert_eq!(a.jpeg, a2.jpeg);
        let jpegs: std::collections::HashSet<u8> =
            (0..20).map(|i| Look::for_seed(fnv(&format!("doctor:{i}#portrait"))).jpeg).collect();
        assert!(jpegs.len() > 5, "сжатие одинаковое у всех: {jpegs:?}");
        let mushy = (0..100).filter(|i| Look::for_seed(fnv(&format!("p:{i}"))).mush.is_some()).count();
        assert!((5..=28).contains(&mushy), "мыльных снимков {mushy} из 100");
    }
}
