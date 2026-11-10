use super::{ocr, Extractor, Page};
use crate::errors::{AppError, AppResult};
use std::path::Path;

pub const NO_TEXT_IN_IMAGE: &str = "ข้าม · ไม่พบข้อความในรูปภาพ";

/// Standalone pictures (screenshots, photos of documents) — text via OCR.
pub struct ImageExtractor;

impl Extractor for ImageExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &["png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff"]
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let Some(engine) = ocr::engine() else {
            return Err(AppError::new("ocr_off", "ข้าม · รูปภาพ (OCR ปิดอยู่หรือไม่มีโมเดล OCR)"));
        };
        let img = image::open(path).map_err(|e| AppError::new("image", format!("เปิดรูปไม่ได้: {e}")))?;
        let text = engine.recognize(&img)?;
        if text.trim().is_empty() {
            return Err(AppError::new("no_text", NO_TEXT_IN_IMAGE));
        }
        let mut page = Page::new(None, text);
        page.set_page_image(&img);
        Ok(vec![page])
    }
}
