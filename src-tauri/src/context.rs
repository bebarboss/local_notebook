//! Shaping the passages a question gets, after the search:
//! - **duplicates out**: the same text found twice (the same file in two folders, a copy under
//!   another name) takes one place, not two;
//! - **neighbours in**: the best hits get the chunk before and after them, so a SQL statement
//!   or a numbered procedure cut across two chunks reaches the model whole.

/// How many of the best hits get their neighbours.
pub const NEIGHBOUR_TOP: usize = 2;
/// Characters a widened passage may grow to.
pub const MAX_WIDENED: usize = 4_000;
/// The longest chunk overlap to look for when joining (the chunker overlaps ~150).
const MAX_OVERLAP: usize = 400;
const MIN_OVERLAP: usize = 20;

/// Whitespace-insensitive key: two chunks with the same words are the same passage.
pub fn text_key(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Indexes to keep from `texts` (already in rank order): the first of each text.
pub fn first_of_each(texts: &[&str]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    texts.iter().enumerate().filter(|(_, t)| seen.insert(text_key(t))).map(|(i, _)| i).collect()
}

/// `a` then `b`, without the text they share (the end of `a` = the start of `b`).
pub fn join_overlap(a: &str, b: &str) -> String {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let max = MAX_OVERLAP.min(ac.len()).min(bc.len());
    for k in (MIN_OVERLAP..=max).rev() {
        if ac[ac.len() - k..] == bc[..k] {
            return format!("{a}{}", bc[k..].iter().collect::<String>());
        }
    }
    format!("{}\n{}", a.trim_end(), b.trim_start())
}

/// The hit's text with its neighbours around it, kept under MAX_WIDENED (the hit itself is
/// never cut; neighbours are dropped first, the later one before the earlier).
pub fn widen(before: Option<&str>, hit: &str, after: Option<&str>) -> String {
    let mut out = hit.to_string();
    if let Some(a) = after {
        let w = join_overlap(&out, a);
        if w.chars().count() <= MAX_WIDENED {
            out = w;
        }
    }
    if let Some(b) = before {
        let w = join_overlap(b, &out);
        if w.chars().count() <= MAX_WIDENED {
            out = w;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_keep_the_best_ranked() {
        let texts = ["ลา 10 วัน", "OT", "ลา  10\nวัน", "ลา 10 วัน ต่อปี"];
        assert_eq!(first_of_each(&texts), vec![0, 1, 3]);
    }

    #[test]
    fn overlapping_chunks_join_once() {
        let a = "SELECT EMPLID, BALANCE FROM PS_GP_ABS_EVENT WHERE EMPLID = '{EMPLID}'";
        let b = "WHERE EMPLID = '{EMPLID}' AND CAL_YEAR = {YEAR};";
        assert_eq!(join_overlap(a, b), "SELECT EMPLID, BALANCE FROM PS_GP_ABS_EVENT WHERE EMPLID = '{EMPLID}' AND CAL_YEAR = {YEAR};");
        // no shared text: a line between
        assert_eq!(join_overlap("ข้อ 1 ยื่นใบลา", "ข้อ 2 หัวหน้าอนุมัติ"), "ข้อ 1 ยื่นใบลา\nข้อ 2 หัวหน้าอนุมัติ");
    }

    #[test]
    fn widened_passages_stay_bounded() {
        let w = widen(Some("ก่อน"), "ท่อนที่เจอ", Some("หลัง"));
        assert_eq!(w, "ก่อน\nท่อนที่เจอ\nหลัง");
        let big = "x".repeat(MAX_WIDENED);
        assert_eq!(widen(Some(&big), "hit", Some("after")), "hit\nafter", "a neighbour that does not fit is left out");
        assert_eq!(widen(None, "hit", None), "hit");
    }
}
