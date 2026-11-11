use super::{keep_images, ocr, Extractor, Page};
use crate::errors::{AppError, AppResult};
use once_cell::sync::OnceCell;
use pdfium_render::prelude::{PdfPage, PdfPageObjectCommon, PdfPageTextObject, PdfPageObjectsCommon, PdfRenderConfig, Pdfium};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const SCANNED_PDF: &str = "ข้าม · PDF สแกน ไม่มีข้อความ (OCR ปิดอยู่หรือไม่มีโมเดล OCR)";
pub const IMAGE_MARK: &str = "[ข้อความในภาพ]";

/// Pages with fewer non-space characters than this are treated as scanned → OCR the whole page.
const SCANNED_PAGE_CHARS: usize = 40;
/// Embedded images smaller than this (px) are icons/logos — not worth OCR.
const MIN_IMAGE_W: i32 = 120;
const MIN_IMAGE_H: i32 = 40;

static LIB: OnceCell<PathBuf> = OnceCell::new();
static PDFIUM: OnceCell<Option<Pdfium>> = OnceCell::new();

/// Called once at startup with the path of the bundled pdfium.dll.
pub fn init(dll: PathBuf) {
    let _ = LIB.set(dll);
}

fn pdfium() -> Option<&'static Pdfium> {
    LIB.get()?; // not initialised yet → don't cache "unavailable"
    PDFIUM
        .get_or_init(|| {
            let lib = LIB.get()?;
            match Pdfium::bind_to_library(lib) {
                Ok(b) => Some(Pdfium::new(b)),
                Err(e) => {
                    log::warn!("pdfium unavailable ({}): {e:?}", lib.display());
                    None
                }
            }
        })
        .as_ref()
}

/// `.pdf` — text layer via PDFium, plus OCR for scanned pages and for text inside
/// embedded images (screenshots, diagrams). One Page per PDF page (keeps page numbers).
/// Falls back to `pdf-extract` (text only) when PDFium cannot be loaded.
pub struct PdfExtractor;

impl Extractor for PdfExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let pages = match pdfium() {
            Some(p) => extract_pdfium(p, path)?,
            None => extract_text_only(path)?,
        };
        if pages.is_empty() {
            return Err(AppError::new("scanned_pdf", SCANNED_PDF));
        }
        Ok(pages)
    }
}

fn extract_pdfium(pdfium: &Pdfium, path: &Path) -> AppResult<Vec<Page>> {
    let doc = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| AppError::new("pdf", format!("อ่าน PDF ไม่ได้: {e:?}")))?;
    let engine = ocr::engine();
    let keep = keep_images();
    let mut seen_images: HashSet<[u8; 16]> = HashSet::new();
    let mut out = Vec::new();

    for (i, page) in doc.pages().iter().enumerate() {
        let text = deduped_text(&page).unwrap_or_else(|| page.text().map(|t| t.all()).unwrap_or_default());
        let chars = text.chars().filter(|c| !c.is_whitespace()).count();
        let mut p = Page::new(Some(i as u32 + 1), text);

        if chars < SCANNED_PAGE_CHARS && (engine.is_some() || keep) {
            // scanned page: render it; OCR everything on it, keep the picture of the page
            let cfg = PdfRenderConfig::new().set_target_width(1700).set_maximum_height(2400);
            if let Ok(img) = page.render_with_config(&cfg).and_then(|b| b.as_image()) {
                if let Some(t) = engine.and_then(|o| o.recognize(&img).ok()) {
                    if t.chars().filter(|c| !c.is_whitespace()).count() > chars {
                        p.text = t;
                    }
                }
                p.set_page_image(&img);
            }
        } else if engine.is_some() || keep {
            // text page: OCR / keep the pictures (screenshots etc.), once per distinct image
            for obj in page.objects().iter() {
                let Some(io) = obj.as_image_object() else { continue };
                let (w, h) = (io.width().unwrap_or(0), io.height().unwrap_or(0));
                if w < MIN_IMAGE_W || h < MIN_IMAGE_H {
                    continue;
                }
                let Ok(img) = io.get_raw_image() else { continue };
                let digest: [u8; 16] = Sha256::digest(img.as_bytes())[..16].try_into().expect("16 bytes");
                if !seen_images.insert(digest) {
                    continue; // repeated logo/header image
                }
                let t = engine.and_then(|o| o.recognize(&img).ok());
                p.push_image(Some(&img), t.as_deref());
            }
        }

        if !p.text.trim().is_empty() {
            out.push(p);
        }
    }
    Ok(out)
}

/// Fallback without PDFium: text layer only.
fn extract_text_only(path: &Path) -> AppResult<Vec<Page>> {
    let p = path.to_path_buf();
    // pdf-extract can panic on malformed files — contain it.
    let result = std::panic::catch_unwind(move || pdf_extract::extract_text_by_pages(&p));
    let pages = match result {
        Ok(Ok(pages)) => pages,
        Ok(Err(e)) => return Err(AppError::new("pdf", format!("อ่าน PDF ไม่ได้: {e}"))),
        Err(_) => return Err(AppError::new("pdf", "อ่าน PDF ไม่ได้ (ไฟล์เสียหรือรูปแบบไม่รองรับ)")),
    };
    Ok(pages
        .into_iter()
        .enumerate()
        .map(|(i, text)| Page::new(Some(i as u32 + 1), text))
        .filter(|p| !p.text.trim().is_empty())
        .collect())
}


fn has_thai_mark(s: &str) -> bool {
    // above/below vowels, tone marks, and SARA AM (drawn as NIKHAHIT + SARA AA)
    s.chars().any(|c| matches!(c, '\u{0E31}' | '\u{0E33}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}'))
}

/// Some PDF writers (Chrome/Skia, some reporting tools) emit every Thai cluster that has a
/// vowel/tone mark twice — once for the base glyph, once for the mark — both carrying the
/// whole cluster as text. PDFium then returns doubled, shuffled Thai ("คู่มืคู่ อการแ").
/// Rebuild the page from text objects in drawing order, dropping those duplicates.
/// Returns None when the page has no such duplicates (PDFium's normal text is used then).
fn deduped_text(page: &PdfPage) -> Option<String> {
    struct Run {
        t: String,
        /// left / right edge from the glyph advance boxes (not the ink), so narrow glyphs
        /// like ' or _ in a monospace font do not look like word gaps
        l: f32,
        r: f32,
        /// baseline y — the same for every glyph on a line, also `_` and descenders
        base: f32,
        /// font size
        h: f32,
    }
    let text = page.text().ok()?;
    // Glyph boxes per text object. `PdfPageTextObject::chars` scans every character of the
    // page for each object (quadratic: 15 s for a 14-page manual), so walk the page's
    // characters once and group them by their object instead. Objects are told apart by
    // their ink box + position — identical ones are identical drawings anyway.
    type Key = [u32; 6];
    fn key(o: &PdfPageTextObject<'_>) -> Option<Key> {
        let q = o.bounds().ok()?;
        let m = o.matrix().ok()?;
        Some([q.left().value, q.right().value, q.bottom().value, q.top().value, m.e(), m.f()].map(f32::to_bits))
    }
    // object → (left, right, baseline of its first glyph)
    let mut boxes: HashMap<Key, (f32, f32, f32)> = HashMap::new();
    for c in text.chars().iter() {
        let Ok(obj) = c.text_object() else { continue }; // generated space / line break
        let (Some(k), Ok(b)) = (key(&obj), c.loose_bounds()) else { continue };
        let (l, r) = (b.left().value, b.right().value);
        boxes
            .entry(k)
            .and_modify(|e| {
                e.0 = e.0.min(l);
                e.1 = e.1.max(r);
            })
            .or_insert_with(|| (l, r, c.origin_y().map(|y| y.value).unwrap_or(b.bottom().value)));
    }
    let mut runs = Vec::new();
    for o in page.objects().iter() {
        let Some(t) = o.as_text_object() else { continue };
        // not `t.text()`: that re-analyses the whole page on every call
        let s = text.for_object(t);
        let Ok(q) = o.bounds() else { continue };
        let h = t.scaled_font_size().value.max(1.0);
        let (l, r, base) = key(t)
            .and_then(|k| boxes.get(&k).copied())
            .unwrap_or((q.left().value, q.right().value, q.bottom().value));
        runs.push(Run { t: s, l, r, base, h });
    }
    let mut dups = 0usize;
    let mut out = String::new();
    let mut prev: Option<&Run> = None;
    let mut prev_right = 0.0f32; // right edge of the cluster incl. skipped mark glyphs
    for r in &runs {
        if r.t.is_empty() {
            // glyph without text of its own (e.g. the AA part of SARA AM) — widens the cluster
            prev_right = prev_right.max(r.r);
            continue;
        }
        if let Some(p) = prev {
            if p.t.trim() == r.t.trim() && has_thai_mark(&r.t) && (r.l - p.l).abs() < p.h {
                dups += 1;
                prev_right = prev_right.max(r.r);
                continue;
            }
            let size = p.h.max(r.h);
            let new_line = (r.base - p.base).abs() > size * 0.5;
            if new_line {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
            } else if r.l - prev_right > size * 0.2 && !out.ends_with(' ') && !p.t.ends_with(' ') && !r.t.starts_with(' ') {
                out.push(' ');
            }
        }
        out.push_str(&r.t);
        prev = Some(r);
        prev_right = r.r;
    }
    // require a real pattern, and don't lose text that lives outside top-level objects
    let full = page.text().map(|t| t.all()).unwrap_or_default();
    let count = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
    (dups >= 2 && count(&out) * 10 >= count(&full) * 6).then_some(out)
}

#[cfg(test)]
mod bench {
    use super::*;
    /// `cargo test --release --lib bench_pdf -- --ignored --nocapture` with DOCASK_TEST_RES, DOCASK_TEST_FILE
    #[test]
    #[ignore]
    fn bench_pdf() {
        use std::time::Instant;
        let res = PathBuf::from(std::env::var("DOCASK_TEST_RES").unwrap());
        init(res.join("pdfium").join("pdfium.dll"));
        let file = PathBuf::from(std::env::var("DOCASK_TEST_FILE").unwrap());
        let p = pdfium().unwrap();
        let t = Instant::now();
        let doc = p.load_pdf_from_file(&file, None).unwrap();
        println!("load {:?}", t.elapsed());
        use std::time::Duration;
        let mut tm = [Duration::ZERO; 6];
        for page in doc.pages().iter() {
            let t = Instant::now();
            let text = page.text().unwrap();
            tm[0] += t.elapsed();
            let t = Instant::now();
            let all = text.chars();
            let chars: Vec<_> = all.iter().collect();
            tm[1] += t.elapsed();
            let t = Instant::now();
            for c in &chars { let _ = c.loose_bounds(); let _ = c.origin_y(); }
            tm[2] += t.elapsed();
            let t = Instant::now();
            for c in &chars { let _ = c.text_object(); }
            tm[3] += t.elapsed();
            let t = Instant::now();
            for o in page.objects().iter() { if let Some(x) = o.as_text_object() { let _ = text.for_object(x); } }
            tm[4] += t.elapsed();
            let t = Instant::now();
            for o in page.objects().iter() { if let Some(x) = o.as_text_object() { let _ = x.scaled_font_size(); let _ = o.bounds(); let _ = x.matrix(); } }
            tm[5] += t.elapsed();
        }
        println!("text {:?} chars {:?} bounds {:?} text_object {:?} obj.text {:?} obj.geom {:?}", tm[0], tm[1], tm[2], tm[3], tm[4], tm[5]);
    }
}
