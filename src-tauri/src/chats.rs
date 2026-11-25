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
    /// `None` for a general (floating) chat that isn't tied to any service
    pub service_id: Option<String>,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// `"docs"` (RAG over a service), `"chat"` (general, no documents), or `"code"` (coding agent
    /// over a project folder) — locked at creation
    pub mode: String,
    /// the project folder a `"code"` chat works in (absolute path); `None` for other modes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
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
    /// a web result/page from the "ค้นเว็บ" research step (`doc_path` is the URL)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web: bool,
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
    /// the agentic gather step's activity (what it searched / read); empty when it didn't run
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<crate::agent::Step>,
    /// files changed by the "เขียนโค้ด" (coding) turn — kept so they can be undone later
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<crate::codefs::Changeset>,
    /// the coding turn's plan checklist (empty when it didn't run)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<crate::coding::PlanStep>,
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
    pub steps: &'a [crate::agent::Step],
    pub changes: Option<&'a crate::codefs::Changeset>,
    pub plan: &'a [crate::coding::PlanStep],
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
               service_id TEXT,
               title TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,
               mode TEXT NOT NULL DEFAULT 'docs'
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
        // v0.19: the agentic gather step's activity list (agent.rs), stored as JSON
        let has: bool = conn.prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name='steps'")?.exists([])?;
        if !has {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN steps TEXT;")?;
        }
        // v0.25: files changed by a "เขียนโค้ด" turn (codefs::Changeset), stored as JSON, for undo
        let has: bool = conn.prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name='changes'")?.exists([])?;
        if !has {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN changes TEXT;")?;
        }
        // v0.30: the coding turn's plan checklist (Vec<coding::PlanStep>), stored as JSON
        let has: bool = conn.prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name='plan'")?.exists([])?;
        if !has {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN plan TEXT;")?;
        }
        // v0.19: chat mode — "docs" (RAG, existing rows) or "chat" (general, no service)
        let has: bool = conn.prepare("SELECT 1 FROM pragma_table_info('chats') WHERE name='mode'")?.exists([])?;
        if !has {
            conn.execute_batch("ALTER TABLE chats ADD COLUMN mode TEXT NOT NULL DEFAULT 'docs';")?;
        }
        // v0.19: older DBs declared service_id as NOT NULL; general (floating) chats need it
        // nullable. SQLite can't drop a column constraint in place, so rebuild the table once.
        let service_notnull: i64 = conn.query_row(
            r#"SELECT "notnull" FROM pragma_table_info('chats') WHERE name='service_id'"#,
            [],
            |r| r.get(0),
        )?;
        if service_notnull == 1 {
            conn.execute_batch(
                "PRAGMA foreign_keys=OFF;
                 BEGIN;
                 CREATE TABLE chats_new (
                   id TEXT PRIMARY KEY,
                   service_id TEXT,
                   title TEXT NOT NULL,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL,
                   mode TEXT NOT NULL DEFAULT 'docs',
                   summary TEXT,
                   summary_upto INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO chats_new (id,service_id,title,created_at,updated_at,mode,summary,summary_upto)
                   SELECT id,service_id,title,created_at,updated_at,mode,summary,summary_upto FROM chats;
                 DROP TABLE chats;
                 ALTER TABLE chats_new RENAME TO chats;
                 CREATE INDEX IF NOT EXISTS chats_service ON chats(service_id, updated_at DESC);
                 COMMIT;
                 PRAGMA foreign_keys=ON;",
            )?;
        }
        // v0.25: the project folder a "code" chat works in — after the rebuild above, which would
        // otherwise drop a column added before it.
        let has: bool = conn.prepare("SELECT 1 FROM pragma_table_info('chats') WHERE name='project_dir'")?.exists([])?;
        if !has {
            conn.execute_batch("ALTER TABLE chats ADD COLUMN project_dir TEXT;")?;
        }
        Ok(Self { conn })
    }

    /// `service_id = None` + `mode = "chat"` makes a general (floating) chat; otherwise a docs chat.
    pub fn create_chat(&self, service_id: Option<&str>, mode: &str, first_question: &str, project_dir: Option<&str>) -> AppResult<Chat> {
        let now = now_ms();
        let c = Chat {
            id: uuid::Uuid::new_v4().to_string(),
            service_id: service_id.map(str::to_string),
            title: title_from(first_question),
            created_at: now,
            updated_at: now,
            mode: mode.to_string(),
            project_dir: project_dir.map(str::to_string),
        };
        self.conn.execute(
            "INSERT INTO chats(id,service_id,title,created_at,updated_at,mode,project_dir) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![c.id, c.service_id, c.title, c.created_at, c.updated_at, c.mode, c.project_dir],
        )?;
        Ok(c)
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Chat> {
        Ok(Chat { id: r.get(0)?, service_id: r.get(1)?, title: r.get(2)?, created_at: r.get(3)?, updated_at: r.get(4)?, mode: r.get(5)?, project_dir: r.get(6)? })
    }

    pub fn get_chat(&self, id: &str) -> AppResult<Chat> {
        self.conn
            .query_row("SELECT id,service_id,title,created_at,updated_at,mode,project_dir FROM chats WHERE id=?1", [id], Self::row)
            .optional()?
            .ok_or_else(|| AppError::new("not_found", "ไม่พบแชตนี้"))
    }

    /// `service_id = Some(id)` lists that service's docs chats; `None` lists the floating chats of
    /// the given `floating_mode` (`"chat"` general, or `"code"` coding) — the menus never mix.
    pub fn list_chats(&self, service_id: Option<&str>, floating_mode: &str, query: &str) -> AppResult<Vec<Chat>> {
        let like = format!("%{}%", query.trim().replace('%', "\\%").replace('_', "\\_"));
        let mut st = self.conn.prepare(
            "SELECT id,service_id,title,created_at,updated_at,mode,project_dir FROM chats
             WHERE ((?1 IS NULL AND mode=?3) OR service_id=?1) AND title LIKE ?2 ESCAPE '\\'
             ORDER BY updated_at DESC LIMIT 200",
        )?;
        let rows = st.query_map(params![service_id, like, floating_mode], Self::row)?;
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

    /// Change the project folder of a "code" chat (when the user re-picks it mid-chat).
    pub fn set_project_dir(&self, id: &str, dir: &str) -> AppResult<()> {
        self.conn.execute("UPDATE chats SET project_dir=?2 WHERE id=?1", params![id, dir])?;
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
        let steps = if m.steps.is_empty() { None } else { Some(serde_json::to_string(m.steps)?) };
        let changes = m.changes.map(serde_json::to_string).transpose()?;
        let plan = if m.plan.is_empty() { None } else { Some(serde_json::to_string(m.plan)?) };
        self.conn.execute(
            "INSERT INTO messages(chat_id,role,content,model,reasoning_effort,sources,elapsed_ms,error,created_at,search_query,steps,changes,plan)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![m.chat_id, m.role, m.content, m.model, m.reasoning_effort, sources, m.elapsed_ms, m.error, now, m.search_query, steps, changes, plan],
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
            steps: m.steps.to_vec(),
            changes: m.changes.cloned(),
            plan: m.plan.to_vec(),
        })
    }

    pub fn messages(&self, chat_id: &str) -> AppResult<Vec<Message>> {
        let mut st = self.conn.prepare(
            "SELECT id,chat_id,role,content,model,reasoning_effort,sources,elapsed_ms,error,created_at,search_query,steps,changes,plan
             FROM messages WHERE chat_id=?1 ORDER BY id",
        )?;
        let rows = st.query_map([chat_id], |r| {
            let src: String = r.get(6)?;
            let steps: Option<String> = r.get(11)?;
            let changes: Option<String> = r.get(12)?;
            let plan: Option<String> = r.get(13)?;
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
                steps: steps.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                changes: changes.and_then(|s| serde_json::from_str(&s).ok()),
                plan: plan.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The changeset a coding message saved (for undo), or `None` if that message changed nothing.
    pub fn changeset(&self, chat_id: &str, message_id: i64) -> AppResult<Option<crate::codefs::Changeset>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT changes FROM messages WHERE id=?1 AND chat_id=?2", params![message_id, chat_id], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
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
        NewMessage { chat_id: chat, role, content, model: None, reasoning_effort: None, sources: &[], elapsed_ms: None, error, search_query: None, steps: &[], changes: None, plan: &[] }
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
        // the old chats table had no `mode` column — the migration adds it as 'docs'
        assert_eq!(db.get_chat("c").unwrap().mode, "docs");
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
        let c = db.create_chat(Some("hr"), "docs", "q", None).unwrap();
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
        let c = db.create_chat(Some("hr"), "docs", "พนักงานลาพักร้อนได้กี่วันต่อปี และต้องแจ้งล่วงหน้ากี่วันทำการ ตามนโยบายบริษัท", None).unwrap();
        assert!(c.title.ends_with('…'));
        for i in 0..4 {
            db.add_message(msg(&c.id, "user", &format!("q{i}"), None)).unwrap();
            db.add_message(msg(&c.id, "assistant", &format!("a{i}"), if i == 2 { Some("401") } else { None })).unwrap();
        }
        let t = db.recent_turns(&c.id, 2).unwrap();
        assert_eq!(t, vec![("q1".into(), "a1".into()), ("q3".into(), "a3".into())]);
        assert_eq!(db.list_chats(Some("hr"), "chat", "ลาพัก").unwrap().len(), 1);
        assert!(db.list_chats(Some("pos"), "chat", "").unwrap().is_empty());
        db.delete_by_service("hr").unwrap();
        assert!(db.messages(&c.id).unwrap().is_empty());
    }

    #[test]
    fn general_and_docs_chats_never_mix() {
        let db = ChatDb::open_in_memory().unwrap();
        let g = db.create_chat(None, "chat", "สรุปอีเมลให้หน่อย", None).unwrap();
        let d = db.create_chat(Some("hr"), "docs", "สิทธิลาพักร้อน", None).unwrap();
        assert_eq!(g.service_id, None);
        assert_eq!(g.mode, "chat");
        assert_eq!(d.service_id.as_deref(), Some("hr"));
        // general menu (None) lists only the floating chat; the service menu only its own
        let general = db.list_chats(None, "chat", "").unwrap();
        assert_eq!(general.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec![g.id.as_str()]);
        let hr = db.list_chats(Some("hr"), "chat", "").unwrap();
        assert_eq!(hr.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec![d.id.as_str()]);
        // reload keeps the mode
        assert_eq!(db.get_chat(&g.id).unwrap().mode, "chat");
    }

    #[test]
    fn code_chats_persist_their_folder_and_list_separately() {
        let db = ChatDb::open_in_memory().unwrap();
        let c = db.create_chat(None, "code", "แก้บั๊ก", Some("D:\\proj")).unwrap();
        assert_eq!(c.mode, "code");
        assert_eq!(c.project_dir.as_deref(), Some("D:\\proj"));
        let general = db.create_chat(None, "chat", "คุย", None).unwrap();
        // the code menu lists only code chats; the general menu only general ones (never mix)
        let code = db.list_chats(None, "code", "").unwrap();
        assert_eq!(code.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec![c.id.as_str()]);
        let chat = db.list_chats(None, "chat", "").unwrap();
        assert_eq!(chat.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec![general.id.as_str()]);
        // reload keeps the folder
        assert_eq!(db.get_chat(&c.id).unwrap().project_dir.as_deref(), Some("D:\\proj"));
    }

    #[test]
    fn legacy_not_null_service_id_is_relaxed_for_general_chats() {
        // a pre-v0.19 DB declared service_id NOT NULL; general chats need it nullable
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chats (id TEXT PRIMARY KEY, service_id TEXT NOT NULL, title TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL,
               model TEXT, reasoning_effort TEXT, sources TEXT NOT NULL DEFAULT '[]', elapsed_ms INTEGER, error TEXT, created_at INTEGER NOT NULL);
             INSERT INTO chats VALUES('old','hr','เดิม',0,0);
             INSERT INTO messages(chat_id,role,content,created_at) VALUES('old','assistant','คำตอบเก่า',0);",
        )
        .unwrap();
        let db = ChatDb::init(conn).unwrap();
        // the migration must drop NOT NULL so a floating chat can be created
        let g = db.create_chat(None, "chat", "สรุปให้หน่อย", None).unwrap();
        assert_eq!(g.service_id, None);
        assert_eq!(g.mode, "chat");
        // the old docs chat and its message survive the table rebuild
        assert_eq!(db.get_chat("old").unwrap().service_id.as_deref(), Some("hr"));
        assert_eq!(db.get_chat("old").unwrap().mode, "docs");
        assert_eq!(db.messages("old").unwrap()[0].content, "คำตอบเก่า");
        // and the two kinds still don't mix
        assert_eq!(db.list_chats(None, "chat", "").unwrap().len(), 1);
        assert_eq!(db.list_chats(Some("hr"), "chat", "").unwrap().len(), 1);
    }
}
