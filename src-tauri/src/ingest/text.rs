use super::{Extractor, Page};
use crate::errors::AppResult;
use std::path::Path;

/// `.txt`, `.md` and `.sql`. Handles UTF-8 (with/without BOM) and UTF-16 with BOM.
pub struct TextExtractor;

impl Extractor for TextExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &["txt", "md", "markdown", "sql"]
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let bytes = std::fs::read(path)?;
        let text = decode(&bytes);
        let is_sql = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sql"));
        Ok(vec![Page::new(None, if is_sql { separate_statements(&text) } else { text })])
    }
}

/// Put a blank line after every statement (`;` at line end, or a `GO` / `/` line) so the
/// chunker cuts between statements; comments written above a statement stay with it.
pub fn separate_statements(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 64);
    for line in sql.replace("\r\n", "\n").split('\n') {
        let t = line.trim_end();
        out.push_str(t);
        out.push('\n');
        let end = t.ends_with(';') || t.eq_ignore_ascii_case("go") || t == "/";
        if end {
            out.push('\n');
        }
    }
    out
}

pub fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.len() >= 2 && (bytes[0..2] == [0xFF, 0xFE] || bytes[0..2] == [0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bom_and_utf16() {
        assert_eq!(decode(&[0xEF, 0xBB, 0xBF, b'h', b'i']), "hi");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("ไทย".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode(&utf16), "ไทย");
    }

    #[test]
    fn sql_statements_keep_their_comments() {
        use crate::ingest::chunker::{chunk, normalize, ChunkOptions};
        let mut sql = String::new();
        for i in 0..30 {
            sql.push_str(&format!("-- {i}: ดูข้อมูลการลาของพนักงาน\nSELECT * FROM SYSADM.PS_GP_ABS_{i}\n WHERE EMPLID = '{{EMPLID}}';\n"));
        }
        let chunks = chunk(&normalize(&separate_statements(&sql)), ChunkOptions { size: 300, overlap: 50 });
        assert!(chunks.len() > 5);
        for c in &chunks {
            assert!(c.starts_with("--"), "chunk should start at a comment: {c}");
            assert!(c.ends_with(';'), "statement cut in the middle: {c}");
        }
    }
}
