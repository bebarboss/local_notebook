//! "บันทึกเป็นความรู้": turn a chat into short notes (problem / checks / fix), one Markdown
//! file per problem (`*.knowledge.md`) in the service's knowledge folder. The folder is one of
//! the service's folders, so the notes are indexed like any document; answers then rank a
//! matching note first (see `prefer_notes`).

use crate::chats::Message;
use crate::errors::{AppError, AppResult};
use crate::ingest::KNOWLEDGE_EXT;
use crate::llm::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
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

/// What to do with each hit when a knowledge note is sent whole instead of as one matched chunk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteAction {
    /// the first hit of a note: replace its text with the whole note (the saved SQL/fix may sit
    /// further down the note than the line the question matched)
    Expand,
    /// a later chunk of a note already expanded: drop it, the whole note already covers it
    Drop,
    /// not a knowledge note: leave it unchanged
    Leave,
}

/// Per hit: `paths[i]` is the note's path when hit i is a knowledge note, else `None`.
/// The first hit of each note expands to the whole note; later chunks of the same note drop.
/// Paths compare case-insensitively (the same note reached by two casings counts once).
pub fn note_actions(paths: &[Option<&str>]) -> Vec<NoteAction> {
    let mut seen: Vec<String> = Vec::new();
    paths
        .iter()
        .map(|p| match p {
            None => NoteAction::Leave,
            Some(path) => {
                let key = path.to_lowercase();
                if seen.contains(&key) {
                    NoteAction::Drop
                } else {
                    seen.push(key);
                    NoteAction::Expand
                }
            }
        })
        .collect()
}

// ------------------------------------------------------------------ duplicate notes
//
// After a note is saved, its first chunk (title + problem + cause, as the indexer cuts it) is
// compared with the first chunk of every other note of the service, passage against passage.
// A close one is offered for merging: the model writes one note that keeps every fix with how
// well it worked, the user checks it, and the two old notes are hidden (moved aside), never
// deleted. Nothing of a note (title, path or text) is logged: file names are titles.

/// Folder (next to the note) that hidden notes are moved into; `.hidden` is never indexed.
pub const HIDDEN_DIR: &str = "ซ่อนแล้ว";
const HIDDEN_EXT: &str = ".hidden";
/// Most similar notes reported for one new note.
pub const MAX_SIMILAR: usize = 3;

/// A note like the one just saved.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SimilarNote {
    pub path: String,
    pub title: String,
    /// cosine of the two notes' first chunks, 0–1
    pub similarity: f32,
    /// the note's document on the server (shared services)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<i64>,
}

/// The "มีบันทึกเรื่องนี้อยู่แล้ว" event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFound {
    pub service_id: String,
    pub path: String,
    pub title: String,
    pub similar: Vec<SimilarNote>,
}

/// The notes closest to `v` (best first), over `threshold`, the note itself left out.
pub fn pick_similar(heads: &[(String, Vec<f32>)], v: &[f32], exclude: &str, threshold: f32, limit: usize) -> Vec<(String, f32)> {
    let mut out: Vec<(String, f32)> = heads
        .iter()
        .filter(|(p, _)| !p.eq_ignore_ascii_case(exclude))
        .map(|(p, h)| (p.clone(), crate::search::dot(v, h)))
        .filter(|(_, s)| *s >= threshold)
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
    out.truncate(limit);
    out
}

/// Title of a note from its path (see `title_of`).
pub fn title_of_path(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    title_of(name)
}

/// The note without its "# title" and "> บันทึกความรู้ · …" lines (what the model merges).
pub fn body_of(text: &str) -> String {
    let mut lines = text.lines().peekable();
    if lines.peek().is_some_and(|l| l.starts_with("# ")) {
        lines.next();
    }
    let rest = lines.collect::<Vec<_>>().join("\n");
    let rest = rest.trim_start();
    if rest.starts_with("> บันทึกความรู้") {
        return rest.split_once('\n').map(|(_, after)| after.trim().to_string()).unwrap_or_default();
    }
    rest.trim().to_string()
}

/// "แก้ได้แล้ว" / "ยังไม่ยืนยัน" from the note's meta line (None: not written by DocAsk).
pub fn status_of(text: &str) -> Option<&'static str> {
    let meta = text.lines().take(4).find(|l| l.starts_with("> บันทึกความรู้"))?;
    if meta.contains("สถานะ: ยังไม่ยืนยัน") {
        Some("ยังไม่ยืนยัน")
    } else if meta.contains("สถานะ: แก้ได้แล้ว") {
        Some("แก้ได้แล้ว")
    } else {
        None
    }
}

pub fn merge_prompt(a: &str, b: &str) -> Vec<ChatMessage> {
    let note = |label: &str, text: &str| {
        let status = status_of(text).unwrap_or("ไม่ระบุ");
        let title = text.lines().next().unwrap_or("").trim_start_matches("# ").trim().to_string();
        format!("บันทึก {label} (สถานะทั้งบันทึก: {status})\nชื่อเรื่อง: {title}\n\n{}", body_of(text))
    };
    let system = "คุณรวม \"บันทึกความรู้\" สองอันที่เป็นปัญหาเดียวกันให้เป็นบันทึกเดียว\n\
        - ขึ้นต้นด้วยบรรทัด @@@ ตามด้วยชื่อเรื่องสั้นๆ ที่ครอบคลุมทั้งสองบันทึก\n\
        - ใต้บรรทัดนั้นเขียน Markdown ด้วยหัวข้อเหล่านี้ตามลำดับ:\n\
          ## ปัญหา / อาการ\n## สาเหตุ\n## การตรวจสอบ\n## วิธีแก้ไข\n## เอกสารอ้างอิง\n\
        - วิธีแก้ไข: เก็บทุกวิธีจากทั้งสองบันทึก วิธีที่ซ้ำกันรวมเป็นข้อเดียว เขียนเป็นข้อมีเลข และต่อท้ายแต่ละข้อด้วยสถานะหนึ่งอย่าง: \
          **(ได้ผล)** **(ยังไม่ยืนยัน)** หรือ **(ไม่ได้ผล)**\n\
        - สถานะของแต่ละวิธี: ถ้าบันทึกบอกชัดว่าวิธีนั้นได้ผลหรือไม่ได้ผล ให้ใช้ตามนั้น ไม่อย่างนั้นใช้สถานะทั้งบันทึก \
          (แก้ได้แล้ว = ได้ผล, ยังไม่ยืนยัน หรือไม่ระบุ = ยังไม่ยืนยัน) ห้ามเดาว่าได้ผล\n\
        - สาเหตุ การตรวจสอบ และเอกสารอ้างอิง: รวมจากทั้งสองบันทึก ตัดที่ซ้ำ ถ้าขัดกันให้เขียนไว้ทั้งสองแบบ\n\
        - ใช้เฉพาะข้อมูลที่อยู่ในสองบันทึก ห้ามเติมความรู้ภายนอก คำสั่ง SQL หรือโค้ดคัดลอกตรงตัว ห้ามแก้ชื่อตารางหรือฟิลด์\n\
        - ไม่ต้องมีข้อความอื่นนอกจากบันทึก ไม่ต้องใส่หัวข้อระดับ #";
    vec![
        ChatMessage::new("system", system),
        ChatMessage::new("user", format!("{}\n\n----------\n\n{}", note("ก", a), note("ข", b))),
    ]
}

/// The merged note from the model's reply (one note; extra ones are folded into the first).
pub fn parse_merge(reply: &str, fallback_title: &str) -> Option<Draft> {
    let mut drafts = parse_drafts(reply, fallback_title);
    if drafts.is_empty() {
        return None;
    }
    let mut first = drafts.remove(0);
    for d in drafts {
        first.markdown.push_str("\n\n");
        first.markdown.push_str(&d.markdown);
    }
    Some(first)
}

/// Move a note aside instead of deleting it: `<its folder>\ซ่อนแล้ว\<name>.hidden`.
/// Only `*.knowledge.md` files; never over another file. Returns where it went.
pub fn hide_note(path: &Path) -> AppResult<PathBuf> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !crate::ingest::is_knowledge(&name) || !path.is_file() {
        return Err(AppError::new("invalid", "ซ่อนได้เฉพาะไฟล์บันทึกความรู้"));
    }
    let dir = path.parent().ok_or_else(|| AppError::new("invalid", "ไม่พบโฟลเดอร์ของบันทึก"))?.join(HIDDEN_DIR);
    std::fs::create_dir_all(&dir)?;
    let mut dest = dir.join(format!("{name}{HIDDEN_EXT}"));
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{name}-{n}{HIDDEN_EXT}"));
        n += 1;
    }
    std::fs::rename(path, &dest)?;
    Ok(dest)
}

/// Pairs the user chose to keep apart ("เก็บทั้งคู่"), so they are not offered again.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KeptApart {
    pairs: BTreeSet<String>,
}

impl KeptApart {
    fn path() -> PathBuf {
        crate::config::data_dir().join("knowledge-kept.json")
    }
    fn key(a: &str, b: &str) -> String {
        let (a, b) = (a.to_lowercase(), b.to_lowercase());
        if a <= b { format!("{a}\n{b}") } else { format!("{b}\n{a}") }
    }
    pub fn load() -> Self {
        crate::config::read_json(&Self::path()).ok().flatten().unwrap_or_default()
    }
    pub fn contains(&self, a: &str, b: &str) -> bool {
        self.pairs.contains(&Self::key(a, b))
    }
    pub fn insert(&mut self, a: &str, b: &str) {
        self.pairs.insert(Self::key(a, b));
    }
    pub fn save(&self) -> AppResult<()> {
        crate::config::write_json(&Self::path(), self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::SourceRef;

    fn msg(id: i64, role: &str, content: &str) -> Message {
        Message {
            id, chat_id: "c".into(), role: role.into(), content: content.into(), model: None, reasoning_effort: None,
            sources: vec![], elapsed_ms: None, error: None, created_at: 0,
            search_query: None, steps: vec![], changes: None,
        }
    }

    #[test]
    fn transcript_stops_at_the_chosen_answer() {
        let mut a = msg(2, "assistant", "ตรวจตาราง PS_TL_PAYABLE_TIME [1]");
        a.sources = vec![SourceRef {
            n: 1, service_id: "s".into(), service_name: "S".into(), doc_path: "x".into(), file_name: "SQL.pdf".into(),
            page: Some(12), ord: 0, score: 0.5, rerank_score: None, keyword: false, knowledge: false, web: false, text: String::new(),
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
    fn note_actions_expand_first_drop_repeats() {
        use NoteAction::*;
        let a = "C:\\K\\a.knowledge.md";
        let b = "C:\\K\\b.knowledge.md";
        // same note reached by two casings counts once; documents are left alone
        let paths = [None, Some(a), Some("c:\\k\\A.knowledge.md"), Some(b), None];
        assert_eq!(note_actions(&paths), vec![Leave, Expand, Drop, Expand, Leave]);
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
    fn similar_notes_over_the_threshold_best_first() {
        let n = |x: f32, y: f32| {
            let l = (x * x + y * y).sqrt();
            vec![x / l, y / l]
        };
        let heads = vec![
            ("C:\\K\\new.knowledge.md".to_string(), n(1.0, 0.0)),
            ("C:\\K\\a.knowledge.md".to_string(), n(1.0, 0.1)),
            ("C:\\K\\b.knowledge.md".to_string(), n(1.0, 0.6)),
            ("C:\\K\\c.knowledge.md".to_string(), n(0.0, 1.0)),
        ];
        let got = pick_similar(&heads, &n(1.0, 0.0), "c:\\k\\NEW.knowledge.md", 0.85, 3);
        assert_eq!(got.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), vec!["C:\\K\\a.knowledge.md", "C:\\K\\b.knowledge.md"]);
        assert!(got[0].1 > got[1].1);
        assert_eq!(pick_similar(&heads, &n(1.0, 0.0), "x", 0.85, 1).len(), 1);
        assert!(pick_similar(&heads, &n(1.0, 0.0), "x", 1.01, 3).is_empty());
    }

    #[test]
    fn note_parts_for_merging() {
        let text = compose("OT ไม่ขึ้น", "ยังไม่ยืนยัน", "2026-11-12", "แชต", "## ปัญหา / อาการ\nOT หาย");
        assert_eq!(body_of(&text), "## ปัญหา / อาการ\nOT หาย");
        assert_eq!(status_of(&text), Some("ยังไม่ยืนยัน"));
        assert_eq!(status_of("# x\n\nไม่มีเมตา"), None);
        assert_eq!(body_of("# x\n\nเนื้อหา"), "เนื้อหา");
        assert_eq!(title_of_path("C:\\K\\2026-11-12_OT-ไม่ขึ้น.knowledge.md"), "OT ไม่ขึ้น");
        let p = merge_prompt(&text, "# ลา\n\n> บันทึกความรู้ · 2026-11-13 · สถานะ: แก้ได้แล้ว\n\n## วิธีแก้ไข\n1. รัน TA");
        assert!(p[1].content.contains("สถานะทั้งบันทึก: ยังไม่ยืนยัน") && p[1].content.contains("สถานะทั้งบันทึก: แก้ได้แล้ว"));
        assert!(p[1].content.contains("ชื่อเรื่อง: OT ไม่ขึ้น") && !p[1].content.contains("> บันทึกความรู้"));
        assert!(p[0].content.contains("(ไม่ได้ผล)"));
        let m = parse_merge("@@@ OT ไม่ขึ้น\n## วิธีแก้ไข\n1. รัน TA **(ได้ผล)**\n@@@ เกิน\n## อื่น\nx", "f").unwrap();
        assert_eq!(m.title, "OT ไม่ขึ้น");
        assert!(m.markdown.contains("(ได้ผล)") && m.markdown.contains("## อื่น"), "a second note is folded in, not lost");
        assert!(parse_merge("  ", "f").is_none());
    }

    #[test]
    fn hidden_notes_are_moved_aside_not_deleted() {
        let dir = std::env::temp_dir().join(format!("docask-hide-{}", uuid::Uuid::new_v4().simple()));
        let a = write_note(&dir, "OT", "fixed", "2026-11-12", "", "x").unwrap();
        let b = write_note(&dir, "OT", "fixed", "2026-11-12", "", "y").unwrap();
        let ha = hide_note(&a).unwrap();
        assert!(!a.exists() && ha.exists());
        assert_eq!(ha.parent().unwrap(), dir.join(HIDDEN_DIR));
        assert!(!crate::ingest::is_supported(&ha), "a hidden note is never indexed");
        // the same name again does not overwrite the first
        std::fs::rename(&b, &a).unwrap();
        let hb = hide_note(&a).unwrap();
        assert_ne!(ha, hb);
        assert!(std::fs::read_to_string(&ha).unwrap().contains("\nx\n"));
        let other = dir.join("plain.md");
        std::fs::write(&other, "x").unwrap();
        assert!(hide_note(&other).is_err(), "only knowledge notes");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn kept_apart_pairs_ignore_order_and_case() {
        let mut k = KeptApart::default();
        k.insert("C:\\A.knowledge.md", "c:\\b.knowledge.md");
        assert!(k.contains("C:\\B.knowledge.md", "c:\\a.knowledge.md"));
        assert!(!k.contains("a", "c"));
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
