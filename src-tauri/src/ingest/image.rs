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
        let text = ocr::image_file_text(path)?;
        if text.trim().is_empty() {
            return Err(AppError::new("no_text", NO_TEXT_IN_IMAGE));
        }
        Ok(vec![Page { page: None, text }])
    }
}
