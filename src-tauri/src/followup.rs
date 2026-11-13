//! Follow-up questions ("แล้วตรวจยังไง") carry their meaning in the chat, not in their words.
//! Before searching, the model rewrites such a question into a standalone search query using
//! the last turns; the passages the previous answer cited join the candidates too.
//! Only the search changes — the answer is still written from the user's own question.

use crate::chats::SourceRef;
use crate::llm::ChatMessage;

/// Turns given to the rewriter, and how much of each answer.
const TURNS: usize = 3;
const ANSWER_CHARS: usize = 600;
/// Previous passages carried into this search.
pub const MAX_CARRIED: usize = 3;

/// Words that point back at the conversation.
const MARKERS: &[&str] = &[
    "แล้ว", "อันนี้", "อันนั้น", "ตัวนี้", "ตัวนั้น", "ข้อนี้", "ข้อนั้น", "เรื่องนี้", "เรื่องนั้น", "เมื่อกี้", "ข้างบน", "ที่ว่า",
    "ต่อ", "อีก", "ด้วย", "ล่ะ", "มัน", "เขา", "นั้น", "นี้", "ดังกล่าว",
    "this", "that", "these", "those", "it", "them", "above", "also", "else", "same",
];

/// A question that only makes sense with the chat before it.
pub fn is_follow_up(question: &str, has_history: bool) -> bool {
    if !has_history {
        return false;
    }
    let q = question.trim().to_lowercase();
    q.chars().count() < 60 || MARKERS.iter().any(|m| q.contains(m))
}

fn cut(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if t.len() < s.len() { format!("{t}…") } else { t }
}

/// Messages asking the model for one standalone search query.
pub fn rewrite_prompt(history: &[(String, String)], question: &str) -> Vec<ChatMessage> {
    let mut convo = String::new();
    for (q, a) in history.iter().skip(history.len().saturating_sub(TURNS)) {
        convo.push_str(&format!("ผู้ใช้: {}\nผู้ช่วย: {}\n\n", cut(q, 400), cut(a, ANSWER_CHARS)));
    }
    vec![
        ChatMessage::new(
            "system",
            "หน้าที่ของคุณ: เขียนคำถามล่าสุดของผู้ใช้ใหม่ให้เป็นคำค้นเอกสารที่เข้าใจได้ในตัวเอง โดยใช้บริบทจากบทสนทนา\n\
             - แทนคำอย่าง \"อันนี้\" \"ตัวนั้น\" \"แล้ว…\" ด้วยสิ่งที่หมายถึงจริง (ชื่อระบบ ตำแหน่ง ตาราง อาการ)\n\
             - คงชื่อเฉพาะ รหัส ชื่อตาราง/คอลัมน์ตามที่ปรากฏ\n\
             - ถ้าคำถามล่าสุดเปลี่ยนเรื่อง หรือเข้าใจได้ในตัวอยู่แล้ว ให้ตอบคำถามเดิม\n\
             - ห้ามตอบคำถาม ตอบเฉพาะคำค้นบรรทัดเดียว ภาษาเดียวกับคำถาม ไม่เกิน 200 ตัวอักษร",
        ),
        ChatMessage::new("user", format!("บทสนทนา:\n{convo}คำถามล่าสุด: {question}\n\nคำค้น:")),
    ]
}

/// The first useful line of the model's reply, or None when it is unusable.
pub fn clean_rewrite(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut s = line;
    for p in ["คำค้น:", "คำค้น :", "Query:", "query:", "Search query:"] {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest.trim();
        }
    }
    let s = s.trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '“' || c == '”').trim();
    let n = s.chars().count();
    (3..=300).contains(&n).then(|| s.to_string())
}

/// Without the model: the previous question in front of this one.
pub fn fallback(history: &[(String, String)], question: &str) -> String {
    match history.last() {
        Some((q, _)) => format!("{} {}", cut(q, 200), question.trim()),
        None => question.trim().to_string(),
    }
}

/// `[n]` numbers used in an answer.
fn cited(answer: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let b = answer.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'[' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() && j - i <= 3 {
                j += 1;
            }
            if j > i + 1 && j < b.len() && b[j] == b']' {
                if let Ok(n) = answer[i + 1..j].parse::<usize>() {
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
                i = j;
            }
        }
        i += 1;
    }
    out
}

/// The passages the previous answer actually cited, in citation order (at most MAX_CARRIED).
pub fn carried(prev_answer: &str, prev_sources: &[SourceRef]) -> Vec<SourceRef> {
    cited(prev_answer)
        .into_iter()
        .filter_map(|n| prev_sources.iter().find(|s| s.n == n).cloned())
        .take(MAX_CARRIED)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(n: usize) -> SourceRef {
        SourceRef {
            n, service_id: "hr".into(), service_name: "HR".into(), doc_path: format!("d{n}.md"), file_name: format!("d{n}.md"),
            page: None, ord: n as i64, score: 0.5, rerank_score: None, keyword: false, knowledge: false, text: format!("t{n}"),
        }
    }

    #[test]
    fn only_follow_ups_are_rewritten() {
        assert!(!is_follow_up("แล้วตรวจยังไง", false), "first question of a chat");
        assert!(is_follow_up("แล้วตรวจยังไง", true));
        assert!(is_follow_up("คอลัมน์ไหนบอกสถานะ", true), "short");
        let long = "ผู้จัดการร้านส่งใบลาในระบบ PeopleSoft แต่รายการไปขึ้นรออนุมัติที่ผู้จัดการเขตแทนที่จะเป็น FC ต้องดูตารางไหน";
        assert!(!is_follow_up(long, true), "long and standalone");
        assert!(is_follow_up(&format!("{long} แบบเดียวกับข้างบน"), true), "long but points back");
    }

    #[test]
    fn rewrites_are_cleaned() {
        assert_eq!(clean_rewrite("คำค้น: \"วิธีตรวจใบลาที่ไปขึ้นอนุมัติผิดคน\"\nเหตุผล: ...").as_deref(), Some("วิธีตรวจใบลาที่ไปขึ้นอนุมัติผิดคน"));
        assert_eq!(clean_rewrite("\n\n  OT ไม่ขึ้นใน Payable Time  ").as_deref(), Some("OT ไม่ขึ้นใน Payable Time"));
        assert_eq!(clean_rewrite(""), None);
        assert_eq!(clean_rewrite("ok"), None);
    }

    #[test]
    fn fallback_puts_the_last_question_first() {
        let h = vec![("q1".to_string(), "a1".to_string()), ("ใบลาไปขึ้นที่ผู้จัดการเขต".to_string(), "a2".to_string())];
        assert_eq!(fallback(&h, " แล้วตรวจยังไง "), "ใบลาไปขึ้นที่ผู้จัดการเขต แล้วตรวจยังไง");
        assert_eq!(fallback(&[], "x"), "x");
    }

    #[test]
    fn prompt_keeps_the_last_turns_short() {
        let h: Vec<(String, String)> = (0..5).map(|i| (format!("q{i}"), "ก".repeat(2000))).collect();
        let m = rewrite_prompt(&h, "แล้วล่ะ");
        assert!(!m[1].content.contains("q1") && m[1].content.contains("q2") && m[1].content.contains("q4"));
        assert!(m[1].content.chars().count() < 3 * 1100);
        assert!(m[1].content.contains("คำถามล่าสุด: แล้วล่ะ"));
    }

    #[test]
    fn carries_only_cited_passages() {
        let s: Vec<SourceRef> = (1..=6).map(src).collect();
        let c = carried("ตาม [2] และ [5][2] ดู [99] กับ [x]", &s);
        assert_eq!(c.iter().map(|x| x.n).collect::<Vec<_>>(), vec![2, 5]);
        assert_eq!(carried("[1][2][3][4]", &s).len(), MAX_CARRIED);
        assert!(carried("ไม่มีอ้างอิง", &s).is_empty());
    }
}
