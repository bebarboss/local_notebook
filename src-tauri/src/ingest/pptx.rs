use super::{ocr, Extractor, Page};
use crate::errors::{AppError, AppResult};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashSet;
use std::io::{Read, Seek};
use std::path::Path;

/// `.pptx`: one Page per slide (page number = slide number) with the slide text,
/// speaker notes, and OCR of the pictures placed on that slide.
pub struct PptxExtractor;

impl Extractor for PptxExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &["pptx"]
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let file = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| AppError::new("pptx", format!("เปิดไฟล์ PowerPoint ไม่ได้: {e}")))?;
        extract_zip(&mut zip)
    }
}

pub fn extract_zip<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> AppResult<Vec<Page>> {
    let mut slides: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| {
            let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse::<u32>().ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    if slides.is_empty() {
        return Err(AppError::new("pptx", "ไม่พบสไลด์ในไฟล์ PowerPoint"));
    }
    slides.sort_by_key(|(n, _)| *n);

    let engine = ocr::engine();
    let mut seen_media: HashSet<String> = HashSet::new();
    let mut pages = Vec::new();
    for (num, name) in slides {
        let xml = read_entry(zip, &name)?;
        let mut text = slide_text(&xml)?;

        // relationships: pictures and speaker notes of this slide
        let rels_name = format!("ppt/slides/_rels/slide{num}.xml.rels");
        let rels = read_entry(zip, &rels_name).unwrap_or_default();
        for (kind, target) in relationships(&rels) {
            let full = resolve("ppt/slides", &target);
            if kind.ends_with("/notesSlide") {
                if let Ok(nx) = read_entry(zip, &full) {
                    let notes = slide_text(&nx)?;
                    // notes slides repeat the slide number placeholder; keep real notes only
                    let notes: String = notes.lines().filter(|l| l.trim().parse::<u32>().is_err()).collect::<Vec<_>>().join("\n");
                    if !notes.trim().is_empty() {
                        text.push_str(&format!("\nหมายเหตุผู้บรรยาย:\n{notes}\n"));
                    }
                }
            } else if kind.ends_with("/image") && engine.is_some() && seen_media.insert(full.clone()) {
                if let Ok(bytes) = read_bytes(zip, &full) {
                    if let Some(t) = ocr::bytes_text(&bytes) {
                        text.push_str(&format!("\n{}\n{t}\n", super::pdf::IMAGE_MARK));
                    }
                }
            }
        }
        if !text.trim().is_empty() {
            pages.push(Page { page: Some(num), text });
        }
    }
    Ok(pages)
}

fn read_entry<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> AppResult<String> {
    let mut s = String::new();
    zip.by_name(name)
        .map_err(|_| AppError::new("pptx", format!("ไม่พบ {name}")))?
        .read_to_string(&mut s)?;
    Ok(s)
}

fn read_bytes<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> AppResult<Vec<u8>> {
    let mut v = Vec::new();
    zip.by_name(name)
        .map_err(|_| AppError::new("pptx", format!("ไม่พบ {name}")))?
        .read_to_end(&mut v)?;
    Ok(v)
}

/// Text of `<a:t>` runs; a new line per `<a:p>` paragraph (shapes, tables, notes).
pub fn slide_text(xml: &str) -> AppResult<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let mut in_t = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" => in_t = true,
            Ok(Event::Empty(e)) if e.local_name().as_ref() == b"br" => out.push('\n'),
            Ok(Event::Text(t)) if in_t => {
                out.push_str(&t.unescape().map_err(|e| AppError::new("pptx", format!("XML ผิดรูปแบบ: {e}")))?);
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"t" => in_t = false,
                b"p" => {
                    if !out.ends_with('\n') && !out.is_empty() {
                        out.push('\n');
                    }
                }
                b"tc" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(AppError::new("pptx", format!("XML ผิดรูปแบบ: {e}"))),
            _ => {}
        }
    }
    Ok(out)
}

/// (Type, Target) pairs from a .rels file.
fn relationships(xml: &str) -> Vec<(String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == b"Relationship" => {
                let mut kind = String::new();
                let mut target = String::new();
                let mut external = false;
                for a in e.attributes().flatten() {
                    let v = a.unescape_value().map(|c| c.into_owned()).unwrap_or_default();
                    match a.key.local_name().as_ref() {
                        b"Type" => kind = v,
                        b"Target" => target = v,
                        b"TargetMode" => external = v == "External",
                        _ => {}
                    }
                }
                if !external && !target.is_empty() {
                    out.push((kind, target));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Resolve a relative zip path like `../media/image1.png` against `ppt/slides`.
fn resolve(base: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pptx(slides: &[(&str, &str)]) -> zip::ZipArchive<std::io::Cursor<Vec<u8>>> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opt = zip::write::SimpleFileOptions::default();
        for (name, body) in slides {
            w.start_file(*name, opt).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        zip::ZipArchive::new(w.finish().unwrap()).unwrap()
    }

    const SLIDE: &str = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
        <p:sp><p:txBody><a:p><a:r><a:t>ขั้นตอนการลาออก</a:t></a:r></a:p>
        <a:p><a:r><a:t>Step 1: </a:t></a:r><a:r><a:t>ยื่นใบลา &amp; แจ้งหัวหน้า</a:t></a:r></a:p></p:txBody></p:sp>
        </p:spTree></p:cSld></p:sld>"#;

    #[test]
    fn reads_slides_in_order_with_notes() {
        let rels = r#"<Relationships><Relationship Id="r1" Type="http://x/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/>
            <Relationship Id="r2" Type="http://x/relationships/hyperlink" Target="https://example.com" TargetMode="External"/></Relationships>"#;
        let notes = r#"<p:notes xmlns:p="p" xmlns:a="a"><a:p><a:r><a:t>อธิบายเพิ่มเรื่อง HR</a:t></a:r></a:p><a:p><a:r><a:t>1</a:t></a:r></a:p></p:notes>"#;
        let slide10 = r#"<p:sld xmlns:a="a"><a:p><a:r><a:t>สไลด์ที่สิบ</a:t></a:r></a:p></p:sld>"#;
        let mut z = pptx(&[
            ("ppt/slides/slide10.xml", slide10),
            ("ppt/slides/slide1.xml", SLIDE),
            ("ppt/slides/_rels/slide1.xml.rels", rels),
            ("ppt/notesSlides/notesSlide1.xml", notes),
        ]);
        let pages = extract_zip(&mut z).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].page, Some(1));
        assert_eq!(pages[1].page, Some(10));
        assert!(pages[0].text.contains("ขั้นตอนการลาออก\nStep 1: ยื่นใบลา & แจ้งหัวหน้า"));
        assert!(pages[0].text.contains("หมายเหตุผู้บรรยาย:\nอธิบายเพิ่มเรื่อง HR"));
        assert!(!pages[0].text.contains("\n1\n"));
        assert!(pages[1].text.contains("สไลด์ที่สิบ"));
    }

    #[test]
    fn resolves_relative_targets() {
        assert_eq!(resolve("ppt/slides", "../media/image1.png"), "ppt/media/image1.png");
        assert_eq!(resolve("ppt/slides", "/ppt/media/a.jpg"), "ppt/media/a.jpg");
    }
}
