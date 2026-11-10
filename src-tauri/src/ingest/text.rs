use super::{Extractor, Page};
use crate::errors::AppResult;
use std::path::Path;

/// `.txt` and `.md`. Handles UTF-8 (with/without BOM) and UTF-16 with BOM.
pub struct TextExtractor;

impl Extractor for TextExtractor {
    fn extensions(&self) -> &'static [&'static str] {
        &["txt", "md", "markdown"]
    }

    fn extract(&self, path: &Path) -> AppResult<Vec<Page>> {
        let bytes = std::fs::read(path)?;
        Ok(vec![Page { page: None, text: decode(&bytes) }])
    }
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
}
