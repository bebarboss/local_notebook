//! What the model sees of a long chat. The model remembers nothing: every question resends
//! the chat. DocAsk sends the last N turns (Settings › การถามตอบ, default 6), trimmed to a
//! budget, plus a short **summary of the turns before them** — so details from the start of a
//! long chat (an employee id, what was already tried) are not lost when those turns drop out.
//!
//! The summary is written in the background after an answer, when turns have dropped out of
//! the window since the last summary, and kept with the chat (chats.summary). A question never
//! waits for it. Chat text is never logged.

use crate::llm::ChatMessage;

/// Characters of history sent with a question (≈ 4–6k tokens of Thai).
pub const HISTORY_BUDGET: usize = 16_000;
/// The newest turn is kept nearly whole; older answers are cut to this.
const NEWEST_ANSWER: usize = 6_000;
const OLDER_ANSWER: usize = 1_500;
const QUESTION: usize = 1_500;
/// Summary length the model is asked for, and the cap applied to what it returns.
const SUMMARY_TARGET: usize = 1_200;
pub const SUMMARY_MAX: usize = 2_000;
/// Old answers given to the summarizer.
const SUMMARIZE_ANSWER: usize = 2_000;

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let t: String = s.chars().take(n).collect();
    format!("{t}… (ตัดให้สั้น)")
}

/// The window, oldest first, fitted into `budget` characters: the newest turn nearly whole,
/// older answers shortened, and the oldest turns dropped when it still does not fit.
pub fn fit(history: &[(String, String)], budget: usize) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut used = 0;
    for (i, (q, a)) in history.iter().rev().enumerate() {
        let a = cut(a, if i == 0 { NEWEST_ANSWER } else { OLDER_ANSWER });
        let q = cut(q, QUESTION);
        let size = q.chars().count() + a.chars().count();
        if i > 0 && used + size > budget {
            break;
        }
        used += size;
        out.push((q, a));
    }
    out.reverse();
    out
}

/// The summary as it goes into the system message.
pub fn system_note(summary: &str) -> String {
    format!(
        "\n\nสรุปบทสนทนาช่วงก่อนหน้าในแชตนี้ (ข้อความเก่ากว่าที่ส่งมาด้านล่าง ใช้เป็นบริบท ไม่ใช่แหล่งอ้างอิง ห้ามอ้าง [n] จากสรุปนี้):\n{}",
        summary.trim()
    )
}

/// Ask the model to fold turns that left the window into the running summary.
pub fn summary_prompt(previous: Option<&str>, dropped: &[(String, String)]) -> Vec<ChatMessage> {
    let mut convo = String::new();
    for (q, a) in dropped {
        convo.push_str(&format!("ผู้ใช้: {}\nผู้ช่วย: {}\n\n", cut(q, QUESTION), cut(a, SUMMARIZE_ANSWER)));
    }
    let prev = previous.map(|p| format!("สรุปเดิม:\n{}\n\n", p.trim())).unwrap_or_default();
    vec![
        ChatMessage::new(
            "system",
            format!(
                "หน้าที่ของคุณ: สรุปบทสนทนาช่วงต้นของแชตถามตอบเอกสาร ให้ผู้ช่วยใช้เป็นความจำเมื่อคุยต่อ\n\
                 - เขียนเป็นข้อสั้นๆ ภาษาเดียวกับบทสนทนา ไม่เกิน {SUMMARY_TARGET} ตัวอักษร\n\
                 - เก็บ: เรื่อง/ปัญหาที่คุย, ระบบ ตาราง ชื่อเฉพาะ, รหัส ตัวเลข วันที่ ค่าที่ผู้ใช้บอก, สิ่งที่ลองแล้วและผล, ข้อสรุป, สิ่งที่ยังค้าง\n\
                 - SQL ให้เก็บแค่ชื่อตาราง/เงื่อนไขสำคัญ ไม่ต้องคัดลอกทั้งคำสั่ง · ไม่ต้องเก็บเลขอ้างอิง [n]\n\
                 - รวมสรุปเดิม (ถ้ามี) กับบทสนทนาใหม่เป็นสรุปเดียว ห้ามแต่งเพิ่ม ตอบเฉพาะสรุป"
            ),
        ),
        ChatMessage::new("user", format!("{prev}บทสนทนาที่ต้องสรุปเพิ่ม:\n{convo}สรุป:")),
    ]
}

/// The model's reply, trimmed and capped.
pub fn clean_summary(reply: &str) -> Option<String> {
    let s = reply.trim().trim_start_matches("สรุป:").trim();
    if s.chars().count() < 10 {
        return None;
    }
    Some(s.chars().take(SUMMARY_MAX).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(i: usize, len: usize) -> (String, String) {
        (format!("q{i}"), format!("a{i}{}", "ก".repeat(len)))
    }

    #[test]
    fn short_chats_go_whole() {
        let h: Vec<_> = (0..3).map(|i| turn(i, 100)).collect();
        assert_eq!(fit(&h, HISTORY_BUDGET), h);
    }

    #[test]
    fn long_answers_are_shortened_and_the_newest_kept() {
        let h: Vec<_> = (0..6).map(|i| turn(i, 5_000)).collect();
        let f = fit(&h, HISTORY_BUDGET);
        let total: usize = f.iter().map(|(q, a)| q.chars().count() + a.chars().count()).sum();
        assert!(total <= HISTORY_BUDGET + NEWEST_ANSWER, "{total}");
        assert_eq!(f.last().unwrap().0, "q5", "the newest turn is always there");
        assert!(f.last().unwrap().1.chars().count() > 5_000, "and nearly whole");
        assert!(f[0].1.ends_with("(ตัดให้สั้น)"), "older answers cut");
        assert_eq!(f.len(), 6);
        // a tiny budget keeps only the newest turn
        assert_eq!(fit(&h, 100).len(), 1);
    }

    #[test]
    fn summary_prompt_carries_the_old_summary_and_dropped_turns() {
        let m = summary_prompt(Some("- รหัสพนักงาน 12345"), &[turn(1, 10)]);
        assert!(m[1].content.contains("สรุปเดิม:\n- รหัสพนักงาน 12345") && m[1].content.contains("ผู้ใช้: q1"));
        assert!(!summary_prompt(None, &[turn(1, 10)])[1].content.contains("สรุปเดิม"));
    }

    #[test]
    fn summaries_are_cleaned() {
        assert_eq!(clean_summary("สรุป: - ใบลาไปขึ้นที่ผู้จัดการเขต").as_deref(), Some("- ใบลาไปขึ้นที่ผู้จัดการเขต"));
        assert_eq!(clean_summary("  "), None);
        assert_eq!(clean_summary(&"ก".repeat(5_000)).unwrap().chars().count(), SUMMARY_MAX);
    }
}
