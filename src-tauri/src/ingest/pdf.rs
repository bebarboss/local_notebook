use super::{Extractor, Page};
use crate::errors::{AppError, AppResult};
use std::path::Path;

pub const SCANNED_PDF: &str = "ข้าม · PDF สแกน ไม่มีข้อความ";

/// `.pdf` via `pdf-extract`, one Page per PDF page (keeps page numbers).
/// A PDF with no extractable text is reported as `scanned_pdf` (no OCR).
/// If Thai comes out garbled for your documents, swap this for `pdfium-render`
/// behind the same `Extractor` trait.
pub struct PdfExtractor;

impl Extractor for PdfExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let p = path.to_path_buf();
        // pdf-extract can panic on malformed files — contain it.
        let result = std::panic::catch_unwind(move || pdf_extract::extract_text_by_pages(&p));
        let pages = match result {
            Ok(Ok(pages)) => pages,
            Ok(Err(e)) => return Err(AppError::new("pdf", format!("อ่าน PDF ไม่ได้: {e}"))),
            Err(_) => return Err(AppError::new("pdf", "อ่าน PDF ไม่ได้ (ไฟล์เสียหรือรูปแบบไม่รองรับ)")),
        };
        let out: Vec<Page> = pages
            .into_iter()
            .enumerate()
            .map(|(i, text)| Page { page: Some(i as u32 + 1), text })
            .filter(|p| !p.text.trim().is_empty())
            .collect();
        if out.is_empty() {
            return Err(AppError::new("scanned_pdf", SCANNED_PDF));
        }
        Ok(out)
    }
}
