use super::{Extractor, Page};
use crate::errors::{AppError, AppResult};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::Read;
use std::path::Path;

/// `.docx`: unzip and read `word/document.xml` with quick-xml.
pub struct DocxExtractor;

impl Extractor for DocxExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &["docx"]
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let file = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| AppError::new("docx", format!("เปิดไฟล์ DOCX ไม่ได้: {e}")))?;
        let mut xml = String::new();
        zip.by_name("word/document.xml")
            .map_err(|_| AppError::new("docx", "ไม่พบ word/document.xml ในไฟล์ DOCX"))?
            .read_to_string(&mut xml)?;
        let mut text = document_xml_to_text(&xml)?;

        // pictures inside the document (screenshots, scanned inserts) → OCR
        if super::ocr::engine().is_some() {
            let mut media: Vec<String> = zip.file_names().filter(|n| n.starts_with("word/media/")).map(str::to_string).collect();
            media.sort();
            for name in media {
                let mut bytes = Vec::new();
                if zip.by_name(&name).ok().and_then(|mut f| f.read_to_end(&mut bytes).ok()).is_none() {
                    continue;
                }
                if let Some(t) = super::ocr::bytes_text(&bytes) {
                    text.push_str(&format!("\n{}\n{t}\n", super::pdf::IMAGE_MARK));
                }
            }
        }
        Ok(vec![Page { page: None, text }])
    }
}

pub fn document_xml_to_text(xml: &str) -> AppResult<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"t" => in_text = true,
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_text => {
                let s = t.unescape().map_err(|e| AppError::new("docx", format!("XML ผิดรูปแบบ: {e}")))?;
                out.push_str(&s);
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => out.push('\n'),
                b"tc" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(AppError::new("docx", format!("XML ผิดรูปแบบ: {e}"))),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_paragraphs() {
        let xml = r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>ข้อ 1 </w:t></w:r><w:r><w:t xml:space="preserve">การลา &amp; สิทธิ์</w:t></w:r></w:p>
            <w:p><w:r><w:t>Line</w:t><w:br/><w:t>two</w:t></w:r></w:p></w:body></w:document>"#;
        let t = document_xml_to_text(xml).unwrap();
        assert!(t.contains("ข้อ 1 การลา & สิทธิ์\n"));
        assert!(t.contains("Line\ntwo"));
    }
}
