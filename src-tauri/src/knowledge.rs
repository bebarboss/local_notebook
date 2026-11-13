//! "บันทึกเป็นความรู้": turn a chat into short notes (problem / checks / fix), one Markdown
//! file per problem (`*.knowledge.md`) in the service's knowledge folder. The folder is one of
//! the service's folders, so the notes are indexed like any document; answers then rank a
//! matching note first (see `prefer_notes`).

use crate::chats::Message;
use crate::errors::{AppError, AppResult};
use crate::ingest::KNOWLEDGE_EXT;
use crate::llm::ChatMessage;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Notes put in front of the documents (at most).
pub const MAX_NOTES: usize = 2;
/// The chat text sent for drafting (characters, newest kept).
const MAX_TRANSCRIPT: usize = 24_000;
const MAX_NOTE: usize = 100_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub title: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeInfo {
    /// where the next note goes (may not exist yet)
    pub dir: String,
    pub notes: usize,
}

/// Question + answer pairs up to (and including) message `upto`, as plain text.
pub fn transcript(messages: &[Message], upto: Option<i64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        if m.error.is_none() && !m.content.trim().is_empty() {
            let who = if m.role == "user" { "ผู้ใช้" } else { "ผู้ช่วย" };
            let mut p = format!("{who}: {}", m.content.trim());
            if m.role != "user" && !m.sources.is_empty() {
                let refs: Vec<String> = m
                    .sources
                    .iter()
                    .map(|s| match s.page {
                        Some(pg) => format!("[{}] {} หน้า/แถว {pg}", s.n, s.file_name),
                        None => format!("[{}] {}", s.n, s.file_name),
                    })
                    .collect();
                p.push_str(&format!("\n(แหล่งอ้างอิงของคำตอบนี้: {})", refs.join(", ")));
            }
            parts.push(p);
        }
        if Some(m.id) == upto {
            break;
        }
    }
    // keep the first question (what the chat is about) and the newest part
    let full = parts.join("\n\n");
    if full.chars().count() <= MAX_TRANSCRIPT {
        return full;
    }
    let first = parts.first().cloned().unwrap_or_default();
    let first: String = first.chars().take(2_000).collect();
    let rest = MAX_TRANSCRIPT.saturating_sub(first.chars().count() + 20);
    let tail: Vec<char> = full.chars().collect();
    let tail: String = tail[tail.len() - rest..].iter().collect();
    format!("{first}\n\n(…ตัดช่วงกลางออก…)\n\n{tail}")
}

pub fn draft_prompt(transcript: &str) -> Vec<ChatMessage> {
    let system = "คุณช่วยเรียบเรียงบทสนทนาการแก้ปัญหา ให้เป็น \"บันทึกความรู้\" สำหรับใช้ตอบปัญหาเดียวกันในครั้งหน้า\n\
        - แยกเป็นหนึ่งบันทึกต่อหนึ่งปัญหา ถ้าบทสนทนามีหลายปัญหาให้แยกหลายบันทึก\n\
        - แต่ละบันทึกขึ้นต้นด้วยบรรทัด @@@ ตามด้วยชื่อเรื่องสั้นๆ ที่บอกอาการหรือเรื่อง (เช่น @@@ OT ไม่ขึ้นใน Payable Time)\n\
        - ใต้บรรทัดนั้นเขียน Markdown ด้วยหัวข้อเหล่านี้ตามลำดับ:\n\
          ## ปัญหา / อาการ\n## สาเหตุ\n## การตรวจสอบ\n## วิธีแก้ไข\n## เอกสารอ้างอิง\n\
        - ใช้เฉพาะข้อมูลที่อยู่ในบทสนทนา ห้ามเดาหรือเติมความรู้ภายนอก หัวข้อไหนไม่มีข้อมูลให้เขียนว่า \"ไม่ระบุ\"\n\
        - คำสั่ง SQL หรือโค้ด ให้คัดลอกตรงตัวในบล็อก ```sql (หรือภาษานั้น) ห้ามแก้ชื่อตารางหรือฟิลด์\n\
        - การตรวจสอบและวิธีแก้ไขเขียนเป็นขั้นตอนแบบมีเลขข้อ\n\
        - เอกสารอ้างอิง: ชื่อไฟล์และหน้าจากแหล่งอ้างอิงในบทสนทนา (ไม่ต้องใส่เลข [n])\n\
        - เขียนกระชับ ไม่เกินราว 1,500 ตัวอักษรต่อบันทึก (ไม่นับโค้ด) ภาษาเดียวกับบทสนทนา\n\
        - ไม่ต้องมีข้อความอื่นนอกจากบันทึก ไม่ต้องใส่หัวข้อระดับ # (แอปใส่ชื่อเรื่องให้)";
    vec![
        ChatMessage::new("system", system),
        ChatMessage::new("user", format!("บทสนทนา:\n\n{transcript}")),
    ]
}

/// Split the model's reply at "@@@ title" lines. A reply without them is one note.
pub fn parse_drafts(reply: &str, fallback_title: &str) -> Vec<Draft> {
    let reply = reply.trim();
    // some models wrap everything in a ```markdown block
    let reply = reply
        .strip_prefix("```markdown")
        .or_else(|| reply.strip_prefix("```md"))
        .and_then(|r| r.trim_end().strip_suffix("```"))
        .unwrap_or(reply);
    let mut out: Vec<Draft> = Vec::new();
    let mut cur: Option<Draft> = None;
    let mut before = String::new();
    for line in reply.lines() {
        if let Some(t) = line.trim().strip_prefix("@@@") {
            if let Some(d) = cur.take() {
                out.push(d);
            }
            cur = Some(Draft { title: clean_title(t), markdown: String::new() });
        } else if let Some(d) = cur.as_mut() {
            d.markdown.push_str(line);
            d.markdown.push('\n');
        } else {
            before.push_str(line);
            before.push('\n');
        }
    }
    if let Some(d) = cur {
        out.push(d);
    }
    if out.is_empty() && !before.trim().is_empty() {
        out.push(Draft { title: clean_title(fallback_title), markdown: before });
    }
    for d in &mut out {
        // a "# title" the model added anyway duplicates ours
        let body = d.markdown.trim();
        let body = match body.lines().next() {
            Some(l) if l.starts_with("# ") => body[l.len()..].trim_start(),
            _ => body,
        };
        d.markdown = body.to_string();
        if d.title.is_empty() {
            d.title = clean_title(fallback_title);
        }
    }
    out.retain(|d| !d.markdown.is_empty());
    out
}

fn clean_title(t: &str) -> String {
    let t = t.trim().trim_matches(|c| c == '#' || c == '*' || c == ':' || c == '"').trim();
    t.chars().filter(|c| !c.is_control()).take(120).collect::<String>().trim().to_string()
}

/// A file-name part from a title: Thai kept, Windows-forbidden characters dropped.
pub fn slug(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_control() || r#"\/:*?"<>|"#.contains(c) {
            continue;
        }
        if c.is_whitespace() {
            if !s.ends_with('-') {
                s.push('-');
            }
        } else {
            s.push(c);
        }
    }
    let s: String = s.chars().take(60).collect();
    let s = s.trim_matches(|c: char| c == '-' || c == '.').to_string();
    if s.is_empty() { "note".into() } else { s }
}

/// The note as written to disk.
pub fn compose(title: &str, status: &str, date: &str, chat_title: &str, body: &str) -> String {
    let title = clean_title(title);
    let mut meta = format!("> บันทึกความรู้ · {date} · สถานะ: {status}");
    let chat_title = chat_title.trim();
    if !chat_title.is_empty() {
        meta.push_str(&format!(" · จากแชต \"{}\"", chat_title.replace('"', "'")));
    }
    format!("# {title}\n\n{meta}\n\n{}\n", body.trim())
}

/// "2026-11-12" only (the date comes from the window, in the user's time zone).
fn valid_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Write a new note into `dir` (never outside it, never over another file). Returns the path.
pub fn write_note(dir: &Path, title: &str, status: &str, date: &str, chat_title: &str, body: &str) -> AppResult<PathBuf> {
    let title = clean_title(title);
    if title.is_empty() {
        return Err(AppError::new("invalid", "กรุณาใส่ชื่อเรื่อง"));
    }
    if body.trim().is_empty() {
        return Err(AppError::new("invalid", "บันทึกว่างอยู่"));
    }
    if body.chars().count() > MAX_NOTE {
        return Err(AppError::new("invalid", "บันทึกยาวเกินไป — แยกเป็นหลายเรื่อง"));
    }
    if !valid_date(date) {
        return Err(AppError::new("invalid", "วันที่ไม่ถูกต้อง"));
    }
    let status = if status == "unverified" { "ยังไม่ยืนยัน" } else { "แก้ได้แล้ว" };
    std::fs::create_dir_all(dir).map_err(|e| AppError::new("io", format!("สร้างโฟลเดอร์ความรู้ไม่ได้: {e}")))?;
    let base = format!("{date}_{}", slug(&title));
    let mut path = dir.join(format!("{base}{KNOWLEDGE_EXT}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{n}{KNOWLEDGE_EXT}"));
        n += 1;
    }
    debug_assert!(path.parent() == Some(dir));
    let text = compose(&title, status, date, chat_title, body);
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(path)
}

pub fn count_notes(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| crate::ingest::is_knowledge(&e.file_name().to_string_lossy())).count())
        .unwrap_or(0)
}

/// The title of a note from its file name ("2026-11-12_OT-ไม่ขึ้น.knowledge.md" → "OT ไม่ขึ้น").
pub fn title_of(file_name: &str) -> String {
    let stem = &file_name[..file_name.len().saturating_sub(KNOWLEDGE_EXT.len())];
    let stem = match stem.split_once('_') {
        Some((d, rest)) if valid_date(d) => rest,
        _ => stem,
    };
    stem.replace('-', " ").trim().to_string()
}

/// New order of the hits: up to `MAX_NOTES` strong notes first, then the rest as they were.
/// `note[i]` = hit i is a knowledge note; `strong[i]` = it matches well enough to lead.
pub fn prefer_notes(note: &[bool], strong: &[bool]) -> Vec<usize> {
    let lead: Vec<usize> = (0..note.len()).filter(|&i| note[i] && strong[i]).take(MAX_NOTES).collect();
    lead.iter().copied().chain((0..note.len()).filter(|i| !lead.contains(i))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::SourceRef;

    fn msg(id: i64, role: &str, content: &str) -> Message {
        Message {
            id, chat_id: "c".into(), role: role.into(), content: content.into(), model: None, reasoning_effort: None,
            sources: vec![], elapsed_ms: None, error: None, created_at: 0,
            search_query: None,
        }
    }

    #[test]
    fn transcript_stops_at_the_chosen_answer() {
        let mut a = msg(2, "assistant", "ตรวจตาราง PS_TL_PAYABLE_TIME [1]");
        a.sources = vec![SourceRef {
            n: 1, service_id: "s".into(), service_name: "S".into(), doc_path: "x".into(), file_name: "SQL.pdf".into(),
            page: Some(12), ord: 0, score: 0.5, rerank_score: None, keyword: false, knowledge: false, text: String::new(),
        }];
        let mut failed = msg(3, "assistant", "");
        failed.error = Some("timeout".into());
        let t = transcript(&[msg(1, "user", "OT ไม่ขึ้น"), a, failed, msg(4, "user", "อีกเรื่อง")], Some(2));
        assert!(t.contains("ผู้ใช้: OT ไม่ขึ้น"));
        assert!(t.contains("SQL.pdf หน้า/แถว 12"));
        assert!(!t.contains("อีกเรื่อง"));
    }

    #[test]
    fn long_transcripts_keep_the_first_question() {
        let long = "ก".repeat(30_000);
        let t = transcript(&[msg(1, "user", "คำถามแรก"), msg(2, "assistant", &long), msg(3, "user", "คำถามสุดท้าย")], None);
        assert!(t.chars().count() <= MAX_TRANSCRIPT + 50);
        assert!(t.starts_with("ผู้ใช้: คำถามแรก"));
        assert!(t.ends_with("คำถามสุดท้าย"));
    }

    #[test]
    fn drafts_split_per_problem() {
        let reply = "@@@ OT ไม่ขึ้น\n## ปัญหา / อาการ\nOT หาย\n\n@@@ **ลาไม่อนุมัติ**\n# ลาไม่อนุมัติ\n## ปัญหา / อาการ\nค้าง\n";
        let d = parse_drafts(reply, "แชต");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].title, "OT ไม่ขึ้น");
        assert!(d[0].markdown.starts_with("## ปัญหา"));
        assert_eq!(d[1].title, "ลาไม่อนุมัติ");
        assert!(d[1].markdown.starts_with("## ปัญหา"), "model's own # title dropped: {}", d[1].markdown);
        // no markers → one note named after the chat
        let d = parse_drafts("```markdown\n## ปัญหา / อาการ\nช้า\n```", "ระบบช้า");
        assert_eq!(d, vec![Draft { title: "ระบบช้า".into(), markdown: "## ปัญหา / อาการ\nช้า".into() }]);
        assert!(parse_drafts("  ", "x").is_empty());
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(slug("OT ไม่ขึ้นใน Payable Time"), "OT-ไม่ขึ้นใน-Payable-Time");
        assert_eq!(slug("..\\..\\C:/Windows"), "CWindows");
        assert_eq!(slug("a/b: c?"), "ab-c");
        assert_eq!(slug("///"), "note");
        assert!(slug(&"ก".repeat(200)).chars().count() <= 60);
    }

    #[test]
    fn notes_stay_in_their_folder() {
        let dir = std::env::temp_dir().join(format!("docask-kn-{}", uuid::Uuid::new_v4().simple()));
        let a = write_note(&dir, "..\\..\\evil", "fixed", "2026-11-12", "แชต \"x\"", "## ปัญหา\nก").unwrap();
        let b = write_note(&dir, "..\\..\\evil", "unverified", "2026-11-12", "", "## ปัญหา\nข").unwrap();
        assert_eq!(a.parent(), Some(dir.as_path()));
        assert_eq!(b.parent(), Some(dir.as_path()));
        assert_ne!(a, b, "second note with the same title gets its own file");
        assert!(crate::ingest::is_knowledge(&b.to_string_lossy()));
        let text = std::fs::read_to_string(&a).unwrap();
        assert!(text.starts_with("# ..\\..\\evil\n\n> บันทึกความรู้ · 2026-11-12 · สถานะ: แก้ได้แล้ว · จากแชต \"แชต 'x'\""), "{text}");
        assert!(std::fs::read_to_string(&b).unwrap().contains("สถานะ: ยังไม่ยืนยัน"));
        assert_eq!(count_notes(&dir), 2);
        assert!(write_note(&dir, "t", "fixed", "12/11/2026", "", "x").is_err());
        assert!(write_note(&dir, "  ", "fixed", "2026-11-12", "", "x").is_err());
        assert!(write_note(&dir, "t", "fixed", "2026-11-12", "", " ").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn titles_from_file_names() {
        assert_eq!(title_of("2026-11-12_OT-ไม่ขึ้น.knowledge.md"), "OT ไม่ขึ้น");
        assert_eq!(title_of("วิธีลา.knowledge.md"), "วิธีลา");
    }

    #[test]
    fn strong_notes_lead() {
        // doc, weak note, strong note, doc, strong note, strong note
        let note = [false, true, true, false, true, true];
        let strong = [true, false, true, true, true, true];
        assert_eq!(prefer_notes(&note, &strong), vec![2, 4, 0, 1, 3, 5]);
        assert_eq!(prefer_notes(&[false, false], &[true, true]), vec![0, 1]);
    }
}
