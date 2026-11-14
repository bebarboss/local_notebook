//! Chat history: `%APPDATA%\DocAsk\chats.db`.

use crate::errors::{AppError, AppResult};
use crate::services::now_ms;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    pub service_id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub n: usize,
    pub service_id: String,
    pub service_name: String,
    pub doc_path: String,
    pub file_name: String,
    pub page: Option<u32>,
    pub ord: i64,
    pub score: f32,
    /// cross-encoder relevance 0–1 when reranking was used
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f32>,
    /// found by keyword (exact words) search
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keyword: bool,
    /// a knowledge note saved from an earlier chat (`*.knowledge.md`)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub knowledge: bool,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub chat_id: String,
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub sources: Vec<SourceRef>,
    pub elapsed_ms: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
    /// the standalone query a follow-up question was searched with
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_query: Option<String>,
}

pub struct NewMessage<'a> {
    pub chat_id: &'a str,
    pub role: &'a str,
    pub content: &'a str,
    pub model: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
    pub sources: &'a [SourceRef],
    pub elapsed_ms: Option<i64>,
    pub error: Option<&'a str>,
    pub search_query: Option<&'a str>,
}

/// (answer message id, question, answer)
pub type Turn = (i64, String, String);

pub struct ChatDb {
    conn: Connection,
}

/// First 40 characters (grapheme-safe) of the first question.
pub fn title_from(question: &str) -> String {
    let q = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let g: Vec<&str> = q.graphemes(true).collect();
    if g.len() <= 40 {
        q
    } else {
        format!("{}…", g[..40].concat().trim_end())
    }
}

impl ChatDb {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> AppResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> AppResult<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS chats (
               id TEXT PRIMARY KEY,
               service_id TEXT NOT NULL,
               title TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS chats_service ON chats(service_id, updated_at DESC);
             CREATE TABLE IF NOT EXISTS messages (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
               role TEXT NOT NULL,
               content TEXT NOT NULL,
               model TEXT,
               reasoning_effort TEXT,
               sources TEXT NOT NULL DEFAULT '[]',
               elapsed_ms INTEGER,
               error TEXT,
               created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS messages_chat ON messages(chat_id, id);",
        )?;
        // v0.12.4: the rewritten query of a follow-up question
        let has: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name='search_query'")?
            .exists([])?;
        if !has {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN search_query TEXT;")?;
        }
        // v0.15: a summary of the turns that left the history window (memory.rs)
        let has: bool = conn.prepare("SELECT 1 FROM pragma_table_info('chats') WHERE name='summary'")?.exists([])?;
        if !has {
            conn.execute_batch("ALTER TABLE chats ADD COLUMN summary TEXT; ALTER TABLE chats ADD COLUMN summary_upto INTEGER NOT NULL DEFAULT 0;")?;
        }
        Ok(Self { conn })
    }

    pub fn create_chat(&self, service_id: &str, first_question: &str) -> AppResult<Chat> {
        let now = now_ms();
        let c = Chat {
            id: uuid::Uuid::new_v4().to_string(),
            service_id: service_id.to_string(),
            title: title_from(first_question),
            created_at: now,
            updated_at: now,
        };
        self.conn.execute(
            "INSERT INTO chats(id,service_id,title,created_at,updated_at) VALUES(?1,?2,?3,?4,?5)",
            params![c.id, c.service_id, c.title, c.created_at, c.updated_at],
        )?;
        Ok(c)
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Chat> {
        Ok(Chat { id: r.get(0)?, service_id: r.get(1)?, title: r.get(2)?, created_at: r.get(3)?, updated_at: r.get(4)? })
    }

    pub fn get_chat(&self, id: &str) -> AppResult<Chat> {
        self.conn
            .query_row("SELECT id,service_id,title,created_at,updated_at FROM chats WHERE id=?1", [id], Self::row)
            .optional()?
            .ok_or_else(|| AppError::new("not_found", "ไม่พบแชตนี้"))
    }

    pub fn list_chats(&self, service_id: Option<&str>, query: &str) -> AppResult<Vec<Chat>> {
        let like = format!("%{}%", query.trim().replace('%', "\\%").replace('_', "\\_"));
        let mut st = self.conn.prepare(
            "SELECT id,service_id,title,created_at,updated_at FROM chats
             WHERE (?1 IS NULL OR service_id=?1) AND title LIKE ?2 ESCAPE '\\'
             ORDER BY updated_at DESC LIMIT 200",
        )?;
        let rows = st.query_map(params![service_id, like], Self::row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn rename(&self, id: &str, title: &str) -> AppResult<Chat> {
        let t: String = title.trim().chars().take(120).collect();
        if t.is_empty() {
            return Err(AppError::new("invalid", "ชื่อแชตต้องไม่ว่าง"));
        }
        self.conn.execute("UPDATE chats SET title=?2 WHERE id=?1", params![id, t])?;
        self.get_chat(id)
    }

    pub fn touch(&self, id: &str) -> AppResult<()> {
        self.conn.execute("UPDATE chats SET updated_at=?2 WHERE id=?1", params![id, now_ms()])?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM chats WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn delete_by_service(&self, service_id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM chats WHERE service_id=?1", [service_id])?;
        Ok(())
    }

    pub fn delete_all(&self) -> AppResult<()> {
        self.conn.execute_batch("DELETE FROM messages; DELETE FROM chats;")?;
        Ok(())
    }

    /// Auto-delete chats not updated in `days` days.
    pub fn purge_older_than(&self, days: u32) -> AppResult<usize> {
        if days == 0 {
            return Ok(0);
        }
        let cutoff = now_ms() - days as i64 * 86_400_000;
        Ok(self.conn.execute("DELETE FROM chats WHERE updated_at < ?1", [cutoff])?)
    }

    pub fn add_message(&self, m: NewMessage<'_>) -> AppResult<Message> {
        let now = now_ms();
        let sources = serde_json::to_string(m.sources)?;
        self.conn.execute(
            "INSERT INTO messages(chat_id,role,content,model,reasoning_effort,sources,elapsed_ms,error,created_at,search_query)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![m.chat_id, m.role, m.content, m.model, m.reasoning_effort, sources, m.elapsed_ms, m.error, now, m.search_query],
        )?;
        let id = self.conn.last_insert_rowid();
        self.touch(m.chat_id)?;
        Ok(Message {
            id,
            chat_id: m.chat_id.into(),
            role: m.role.into(),
            content: m.content.into(),
            model: m.model.map(Into::into),
            reasoning_effort: m.reasoning_effort.map(Into::into),
            sources: m.sources.to_vec(),
            elapsed_ms: m.elapsed_ms,
            error: m.error.map(Into::into),
            created_at: now,
            search_query: m.search_query.map(Into::into),
        })
    }

    pub fn messages(&self, chat_id: &str) -> AppResult<Vec<Message>> {
        let mut st = self.conn.prepare(
            "SELECT id,chat_id,role,content,model,reasoning_effort,sources,elapsed_ms,error,created_at,search_query
             FROM messages WHERE chat_id=?1 ORDER BY id",
        )?;
        let rows = st.query_map([chat_id], |r| {
            let src: String = r.get(6)?;
            Ok(Message {
                id: r.get(0)?,
                chat_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                model: r.get(4)?,
                reasoning_effort: r.get(5)?,
                sources: serde_json::from_str(&src).unwrap_or_default(),
                elapsed_ms: r.get(7)?,
                error: r.get(8)?,
                created_at: r.get(9)?,
                search_query: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The last answer that worked (text + its sources) — what a follow-up question builds on.
    pub fn last_answer(&self, chat_id: &str) -> AppResult<Option<(String, Vec<SourceRef>)>> {
        Ok(self
            .messages(chat_id)?
            .into_iter()
            .rev()
            .find(|m| m.role == "assistant" && m.error.is_none() && !m.content.trim().is_empty())
            .map(|m| (m.content, m.sources)))
    }

    /// Every good question/answer pair, oldest first, with the answer's message id.
    /// Failed or empty answers are skipped together with their question.
    fn pairs(&self, chat_id: &str) -> AppResult<Vec<Turn>> {
        let mut pairs = Vec::new();
        let mut pending_q: Option<String> = None;
        for m in self.messages(chat_id)? {
            match m.role.as_str() {
                "user" => pending_q = Some(m.content),
                "assistant" => {
                    if let Some(q) = pending_q.take() {
                        if m.error.is_none() && !m.content.trim().is_empty() {
                            pairs.push((m.id, q, m.content));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(pairs)
    }

    /// Last `turns` question/answer pairs (text only — no document chunks), oldest first.
    pub fn recent_turns(&self, chat_id: &str, turns: u32) -> AppResult<Vec<(String, String)>> {
        if turns == 0 {
            return Ok(vec![]);
        }
        let pairs = self.pairs(chat_id)?;
        let skip = pairs.len().saturating_sub(turns as usize);
        Ok(pairs.into_iter().skip(skip).map(|(_, q, a)| (q, a)).collect())
    }

    /// The summary of the turns before the history window (memory.rs).
    pub fn summary(&self, chat_id: &str) -> AppResult<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT summary FROM chats WHERE id=?1", [chat_id], |r| r.get::<_, Option<String>>(0))
            .optional()?
            .flatten()
            .filter(|s| !s.trim().is_empty()))
    }

    /// The current summary and the turns that left the `turns` window since it was written.
    pub fn to_summarize(&self, chat_id: &str, turns: u32) -> AppResult<(Option<String>, Vec<Turn>)> {
        let upto: i64 = self.conn.query_row("SELECT summary_upto FROM chats WHERE id=?1", [chat_id], |r| r.get(0)).optional()?.unwrap_or(0);
        let pairs = self.pairs(chat_id)?;
        let keep = pairs.len().saturating_sub(turns as usize);
        let dropped = pairs.into_iter().take(keep).filter(|(id, ..)| *id > upto).collect();
        Ok((self.summary(chat_id)?, dropped))
    }

    /// Save a summary that covers every answer up to message `upto`.
    pub fn set_summary(&self, chat_id: &str, summary: &str, upto: i64) -> AppResult<()> {
        self.conn.execute("UPDATE chats SET summary=?2, summary_upto=?3 WHERE id=?1", params![chat_id, summary, upto])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg<'a>(chat: &'a str, role: &'a str, content: &'a str, error: Option<&'a str>) -> NewMessage<'a> {
        NewMessage { chat_id: chat, role, content, model: None, reasoning_effort: None, sources: &[], elapsed_ms: None, error, search_query: None }
    }

    #[test]
    fn old_databases_get_the_search_query_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chats (id TEXT PRIMARY KEY, service_id TEXT NOT NULL, title TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL,
               model TEXT, reasoning_effort TEXT, sources TEXT NOT NULL DEFAULT '[]', elapsed_ms INTEGER, error TEXT, created_at INTEGER NOT NULL);
             INSERT INTO chats VALUES('c','hr','t',0,0);
             INSERT INTO messages(chat_id,role,content,created_at) VALUES('c','assistant','เก่า',0);",
        )
        .unwrap();
        let db = ChatDb::init(conn).unwrap();
        assert_eq!(db.messages("c").unwrap()[0].search_query, None);
        let mut m = msg("c", "assistant", "ใหม่ [1]", None);
        m.search_query = Some("คำค้นที่เขียนใหม่");
        db.add_message(m).unwrap();
        let all = db.messages("c").unwrap();
        assert_eq!(all[1].search_query.as_deref(), Some("คำค้นที่เขียนใหม่"));
        assert_eq!(db.last_answer("c").unwrap().unwrap().0, "ใหม่ [1]");
    }

    #[test]
    fn turns_leaving_the_window_are_summarized_once() {
        let db = ChatDb::open_in_memory().unwrap();
        let c = db.create_chat("hr", "q").unwrap();
        for i in 0..5 {
            db.add_message(msg(&c.id, "user", &format!("q{i}"), None)).unwrap();
            db.add_message(msg(&c.id, "assistant", &format!("a{i}"), None)).unwrap();
        }
        let (prev, dropped) = db.to_summarize(&c.id, 3).unwrap();
        assert!(prev.is_none());
        assert_eq!(dropped.iter().map(|(_, q, _)| q.as_str()).collect::<Vec<_>>(), vec!["q0", "q1"]);
        db.set_summary(&c.id, "- สรุป q0 q1", dropped.last().unwrap().0).unwrap();
        let (prev, dropped) = db.to_summarize(&c.id, 3).unwrap();
        assert_eq!(prev.as_deref(), Some("- สรุป q0 q1"));
        assert!(dropped.is_empty(), "nothing new left the window");
        db.add_message(msg(&c.id, "user", "q5", None)).unwrap();
        db.add_message(msg(&c.id, "assistant", "a5", None)).unwrap();
        let (_, dropped) = db.to_summarize(&c.id, 3).unwrap();
        assert_eq!(dropped.iter().map(|(_, q, _)| q.as_str()).collect::<Vec<_>>(), vec!["q2"], "only the newly dropped turn");
        assert_eq!(db.summary(&c.id).unwrap().as_deref(), Some("- สรุป q0 q1"));
    }

    #[test]
    fn history_roundtrip_and_turns() {
        let db = ChatDb::open_in_memory().unwrap();
        let c = db.create_chat("hr", "พนักงานลาพักร้อนได้กี่วันต่อปี และต้องแจ้งล่วงหน้ากี่วันทำการ ตามนโยบายบริษัท").unwrap();
        assert!(c.title.ends_with('…'));
        for i in 0..4 {
            db.add_message(msg(&c.id, "user", &format!("q{i}"), None)).unwrap();
            db.add_message(msg(&c.id, "assistant", &format!("a{i}"), if i == 2 { Some("401") } else { None })).unwrap();
        }
        let t = db.recent_turns(&c.id, 2).unwrap();
        assert_eq!(t, vec![("q1".into(), "a1".into()), ("q3".into(), "a3".into())]);
        assert_eq!(db.list_chats(Some("hr"), "ลาพัก").unwrap().len(), 1);
        assert!(db.list_chats(Some("pos"), "").unwrap().is_empty());
        db.delete_by_service("hr").unwrap();
        assert!(db.messages(&c.id).unwrap().is_empty());
    }
}
