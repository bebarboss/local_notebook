use super::{decode_image, keep_images, ocr, pptx, Extractor, Page};
use crate::errors::{AppError, AppResult};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

/// Stands in for a picture inside the text returned by `document_xml_to_text`.
pub const PICTURE: char = '\u{FFFC}';

/// `.docx`: unzip and read `word/document.xml` with quick-xml. Pictures are OCR'd
/// and kept at the place they appear in the document.
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
        let (text, pictures) = document_xml_to_text(&xml)?;

        let mut rels_xml = String::new();
        if let Ok(mut f) = zip.by_name("word/_rels/document.xml.rels") {
            let _ = f.read_to_string(&mut rels_xml);
        }
        let targets: HashMap<String, String> =
            pptx::relationships(&rels_xml).into_iter().map(|(id, _, t)| (id, pptx::resolve("word", &t))).collect();

        let wanted = ocr::engine().is_some() || keep_images();
        let mut seen = HashSet::new();
        let mut parts = text.split(PICTURE);
        let mut page = Page::new(None, parts.next().unwrap_or_default().to_string());
        for (rid, rest) in pictures.iter().zip(parts) {
            let img = targets
                .get(rid)
                .filter(|t| wanted && seen.insert((*t).clone()))
                .and_then(|t| {
                    let mut bytes = Vec::new();
                    zip.by_name(t).ok()?.read_to_end(&mut bytes).ok()?;
                    decode_image(&bytes)
                });
            if let Some(img) = &img {
                page.push_image(Some(img), ocr::image_text(img).as_deref());
            }
            page.text.push_str(rest);
        }
        Ok(vec![page])
    }
}

/// Text of `word/document.xml`; each picture becomes `PICTURE` and its relationship id
/// is returned in document order.
pub fn document_xml_to_text(xml: &str) -> AppResult<(String, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let mut pictures = Vec::new();
    let mut in_text = false;
    let mut picture = |e: &quick_xml::events::BytesStart, out: &mut String| {
        // DrawingML <a:blip r:embed="rId5"/> or legacy VML <v:imagedata r:id="rId6"/>
        let key: &[u8] = match e.local_name().as_ref() {
            b"blip" => b"embed",
            b"imagedata" => b"id",
            _ => return,
        };
        let rid = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == key).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()));
        if let Some(rid) = rid {
            out.push(PICTURE);
            pictures.push(rid);
        }
    };
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"t" => in_text = true,
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                _ => picture(&e, &mut out),
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                _ => picture(&e, &mut out),
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
    drop(picture);
    Ok((out, pictures))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_paragraphs() {
        let xml = r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>ข้อ 1 </w:t></w:r><w:r><w:t xml:space="preserve">การลา &amp; สิทธิ์</w:t></w:r></w:p>
            <w:p><w:r><w:t>Line</w:t><w:br/><w:t>two</w:t></w:r></w:p></w:body></w:document>"#;
        let (t, pics) = document_xml_to_text(xml).unwrap();
        assert!(t.contains("ข้อ 1 การลา & สิทธิ์\n"));
        assert!(t.contains("Line\ntwo"));
        assert!(pics.is_empty());
    }

    #[test]
    fn pictures_in_place() {
        let xml = r#"<w:document xmlns:w="w" xmlns:a="a" xmlns:r="r" xmlns:v="v"><w:body>
            <w:p><w:r><w:t>ก่อนรูป</w:t></w:r></w:p>
            <w:p><w:r><w:drawing><a:graphic><a:blip r:embed="rId7"><a:extLst/></a:blip></a:graphic></w:drawing></w:r></w:p>
            <w:p><w:r><w:t>ระหว่าง</w:t></w:r><w:r><w:pict><v:imagedata r:id="rId9"/></w:pict></w:r></w:p>
            <w:p><w:r><w:t>หลังรูป</w:t></w:r></w:p></w:body></w:document>"#;
        let (t, pics) = document_xml_to_text(xml).unwrap();
        assert_eq!(pics, vec!["rId7", "rId9"]);
        let parts: Vec<&str> = t.split(PICTURE).collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].contains("ก่อนรูป") && parts[1].contains("ระหว่าง") && parts[2].contains("หลังรูป"));
    }
}
