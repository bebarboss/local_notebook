//! Character-based chunker that works for Thai (no spaces between words).
//! Counts and cuts on grapheme clusters, so a Thai vowel or tone mark is never
//! separated from its base consonant. Prefers to cut at paragraph > line > space.

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy)]
pub struct ChunkOptions {
    pub size: usize,
    pub overlap: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self { size: 1000, overlap: 150 }
    }
}

/// Collapse runs of spaces / blank lines and drop control characters.
pub fn normalize(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(text.len());
    let mut blank_lines = 0;
    for line in text.split('\n') {
        let mut l = String::with_capacity(line.len());
        let mut prev_space = false;
        for c in line.chars() {
            if c == '\u{FEFF}' || (c.is_control() && c != '\t') {
                continue;
            }
            let is_space = c == ' ' || c == '\t' || c == '\u{00A0}';
            if is_space {
                if !prev_space {
                    l.push(' ');
                }
            } else {
                l.push(c);
            }
            prev_space = is_space;
        }
        let l = l.trim();
        if l.is_empty() {
            blank_lines += 1;
            continue;
        }
        // paragraph break (any number of blank lines) → exactly one empty line
        if !out.is_empty() {
            out.push_str(if blank_lines > 0 { "\n\n" } else { "\n" });
        }
        blank_lines = 0;
        out.push_str(l);
    }
    out
}

pub fn chunk(text: &str, opts: ChunkOptions) -> Vec<String> {
    let size = opts.size.max(50);
    let overlap = opts.overlap.min(size / 2);
    let g: Vec<&str> = text.graphemes(true).collect();
    let n = g.len();
    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    let mut start = 0usize;
    loop {
        let hard_end = (start + size).min(n);
        let end = if hard_end >= n { n } else { best_break(&g, start, hard_end, size) };
        let piece = g[start..end].concat();
        let piece = piece.trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        if end >= n {
            break;
        }
        // step back by `overlap`, then move forward to a soft boundary so the
        // next chunk does not start mid-word in space-separated text
        let mut next = end.saturating_sub(overlap).max(start + 1);
        let limit = end;
        let mut j = next;
        while j < limit {
            if is_space(g[j]) {
                next = j + 1;
                break;
            }
            j += 1;
        }
        start = next.min(end);
    }
    out
}

fn is_space(g: &str) -> bool {
    g.chars().all(char::is_whitespace)
}

/// Find a cut point in (start + size/2, hard_end] — paragraph, then newline,
/// then sentence end / space. Falls back to the hard limit (still a grapheme boundary).
fn best_break(g: &[&str], start: usize, hard_end: usize, size: usize) -> usize {
    let min = start + size / 2;
    let scan = |pred: &dyn Fn(usize) -> bool| -> Option<usize> { (min..hard_end).rev().find(|&i| pred(i)).map(|i| i + 1) };
    if let Some(i) = scan(&|i| g[i] == "\n" && i > 0 && g[i - 1] == "\n") {
        return i;
    }
    if let Some(i) = scan(&|i| g[i] == "\n") {
        return i;
    }
    if let Some(i) = scan(&|i| matches!(g[i], "." | "!" | "?" | "。" | "ฯ") && i + 1 < g.len() && is_space(g[i + 1])) {
        return i + 1;
    }
    if let Some(i) = scan(&|i| is_space(g[i])) {
        return i;
    }
    hard_end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_thai_combining(c: char) -> bool {
        matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}')
    }

    const THAI: &str = "พนักงานที่ปฏิบัติงานติดต่อกันครบหนึ่งปีมีสิทธิ์ลาพักผ่อนประจำปีได้สิบวันทำการ\
        การลาต้องยื่นผ่านระบบล่วงหน้าอย่างน้อยสามวันทำการและได้รับอนุมัติจากผู้บังคับบัญชาก่อนวันลา\
        เมื่อพนักงานมีอายุงานครบสามปีให้ได้รับสิทธิ์ลาพักผ่อนเพิ่มขึ้นปีละหนึ่งวันทำการรวมแล้วไม่เกินสิบห้าวัน";

    #[test]
    fn thai_never_splits_marks() {
        let text = THAI.repeat(8);
        let chunks = chunk(&text, ChunkOptions { size: 97, overlap: 20 });
        assert!(chunks.len() > 5);
        for c in &chunks {
            let first = c.chars().next().unwrap();
            assert!(!is_thai_combining(first), "chunk starts with combining mark: {c}");
            assert!(c.graphemes(true).count() <= 97);
        }
    }

    #[test]
    fn thai_covers_all_text() {
        let text = THAI.repeat(3);
        let chunks = chunk(&text, ChunkOptions { size: 120, overlap: 0 });
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn english_breaks_at_spaces() {
        let words = ["The", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog."];
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        let chunks = chunk(text.trim(), ChunkOptions { size: 200, overlap: 30 });
        assert!(chunks.len() > 5);
        for c in &chunks {
            assert!(c.chars().count() <= 200);
            for w in c.split_whitespace() {
                assert!(words.contains(&w), "cut mid-word: {w:?} in {c:?}");
            }
        }
    }

    #[test]
    fn overlap_repeats_context() {
        let text = "a b c d e f g h i j k l m n o p q r s t u v w x y z ".repeat(10);
        let chunks = chunk(text.trim(), ChunkOptions { size: 100, overlap: 30 });
        assert!(chunks.len() >= 5);
        for w in chunks.windows(2) {
            let head: String = w[1].chars().take(5).collect();
            let tail: String = { let v: Vec<char> = w[0].chars().collect(); v[v.len().saturating_sub(30)..].iter().collect() };
            assert!(tail.contains(&head), "no overlap between {:?} and {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn mixed_prefers_newlines() {
        let para = "ระเบียบการจัดซื้อ Procurement policy ฉบับปี 2568 มีผลบังคับใช้ทันที";
        let text = format!("{para}\n{para}\n\n{para}\n{para}\n\n{para}");
        let chunks = chunk(&text, ChunkOptions { size: 150, overlap: 0 });
        for c in &chunks {
            assert!(c.starts_with("ระเบียบ"), "chunk should start at a line: {c}");
        }
    }

    #[test]
    fn empty_and_small() {
        assert!(chunk("", ChunkOptions::default()).is_empty());
        assert_eq!(chunk("สั้นๆ", ChunkOptions::default()), vec!["สั้นๆ".to_string()]);
    }

    #[test]
    fn normalize_collapses() {
        assert_eq!(normalize("a  \t b\r\n\r\n\r\n\r\nc\u{FEFF}"), "a b\n\nc");
    }
}
