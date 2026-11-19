//! Bounded "gather" step before answering. The first search sometimes leaves the model short
//! of a specific it needs — a table name behind a field code, a schema in another document. When
//! the chosen model can call tools, it may search the same service a few more times (or read
//! around a passage), and we answer on everything gathered. Hard limits: at most `MAX_ROUNDS`
//! rounds, a wall-clock budget, and a cap on added passages. On any trouble (the model asks for
//! nothing, a round errors, the Gateway rejects tools, the user cancels) we answer with what we
//! have. Nothing of the question or the passages is logged — step kinds and counts only.

use crate::chats::SourceRef;
use crate::llm::Reply;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Rounds of tool calling before we answer no matter what.
pub const MAX_ROUNDS: usize = 3;
/// Wall-clock budget for the whole gather step.
pub const TIME_BUDGET: Duration = Duration::from_secs(45);
/// Each Gateway round's own timeout.
pub const PER_CALL: Duration = Duration::from_secs(20);
/// Most passages the gather step may add on top of the first search.
pub const MAX_ADDED: usize = 12;
/// Each passage shown to the model is trimmed to this many characters.
const PASSAGE_CHARS: usize = 600;

/// One line in the live activity list ("กำลังทำอะไร"), shown under "ค้นด้วย: …".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    /// "search" | "read" | "done" | "error"
    pub kind: String,
    /// the search terms, the source number, or an error code — never passage text
    pub detail: String,
    /// new passages this step added
    pub found: usize,
}

impl Step {
    fn search(detail: String, found: usize) -> Self {
        Self { kind: "search".into(), detail, found }
    }
    fn read(source: usize, found: usize) -> Self {
        Self { kind: "read".into(), detail: format!("[{source}]"), found }
    }
    fn done() -> Self {
        Self { kind: "done".into(), detail: String::new(), found: 0 }
    }
    fn error(code: &str) -> Self {
        Self { kind: "error".into(), detail: code.into(), found: 0 }
    }
}

/// What the gather step collected: extra passages to append, and the terms it searched (shown
/// next to "ค้นด้วย" and kept in history).
#[derive(Debug, Default, Clone)]
pub struct Gathered {
    pub added: Vec<SourceRef>,
    pub queries: Vec<String>,
}

/// The two things the loop needs from the host: a Gateway round and the retrieval tools.
/// Kept as a trait so the loop can be tested with scripted replies and no network or index.
#[allow(async_fn_in_trait)]
pub trait Backend {
    /// One tool-calling round over the running conversation.
    async fn complete(&self, messages: &Value, tools: &Value) -> crate::errors::AppResult<Reply>;
    /// Search the same service again; returns candidate passages (already ranked).
    async fn search(&self, queries: &[String]) -> Vec<SourceRef>;
    /// The chunk before and after a passage, joined — or `None` if there is nothing to add.
    async fn read_around(&self, src: &SourceRef) -> Option<String>;
}

pub fn tools_schema() -> Value {
    json!([
        {"type": "function", "function": {
            "name": "search_documents",
            "description": "ค้นเอกสารใน service เดียวกันเพิ่ม ด้วยคำค้นเจาะจง เช่น ชื่อตาราง ชื่อคอลัมน์ หรือโค้ดฟิลด์ ใช้เมื่อท่อนที่มีอยู่ยังไม่พอจะตอบ",
            "parameters": {"type": "object", "properties": {
                "queries": {"type": "array", "items": {"type": "string"}, "description": "คำค้น 1-3 คำ เจาะจงชื่อเฉพาะที่ยังขาด"}
            }, "required": ["queries"]}
        }},
        {"type": "function", "function": {
            "name": "read_around",
            "description": "อ่านท่อนก่อน/หลังของแหล่งที่อ้างด้วยหมายเลข [n] เพื่อดูบริบทต่อเนื่อง",
            "parameters": {"type": "object", "properties": {
                "source": {"type": "integer", "description": "หมายเลขแหล่ง [n] ที่ต้องการอ่านรอบ ๆ"}
            }, "required": ["source"]}
        }}
    ])
}

fn system_prompt() -> &'static str {
    "คุณกำลังช่วยรวบรวมเอกสารภายในองค์กรเพื่อให้ตอบคำถามได้ครบ ด้านล่างคือท่อนที่ค้นเจอรอบแรก\n\
     - ถ้าข้อมูลพอจะตอบคำถามได้แล้ว ให้ตอบสั้น ๆ ว่า DONE โดยไม่ต้องเรียกเครื่องมือ\n\
     - ถ้ายังขาด เช่น เอกสารพูดถึงชื่อคอลัมน์/ฟิลด์/โค้ด (เช่น THENT, PREF_PHONE_FLAG) แต่ไม่มีชื่อตารางหรือโครงสร้างที่ต้องใช้ ให้เรียก search_documents ค้นชื่อนั้นเพิ่ม หรือ read_around อ่านรอบ ๆ ท่อนที่เกี่ยว\n\
     - ค้นได้สูงสุด 3 รอบ ห้ามเดาชื่อตาราง/คอลัมน์เอง ให้ได้จากเอกสารเท่านั้น"
}

fn trim(s: &str) -> String {
    if s.chars().count() <= PASSAGE_CHARS {
        return s.replace('\n', " ");
    }
    s.chars().take(PASSAGE_CHARS).collect::<String>().replace('\n', " ")
}

/// "[n] (file) text" for each passage, numbered from 1.
fn passages_block(all: &[SourceRef]) -> String {
    all.iter()
        .enumerate()
        .map(|(i, s)| format!("[{}] ({}) {}", i + 1, s.file_name, trim(&s.text)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn initial_messages(question: &str, all: &[SourceRef]) -> Value {
    json!([
        {"role": "system", "content": system_prompt()},
        {"role": "user", "content": format!("คำถาม: {question}\n\nท่อนที่มีอยู่:\n{}", passages_block(all))}
    ])
}

pub fn parse_search_queries(args: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(args) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    if let Some(arr) = v.get("queries").and_then(|q| q.as_array()) {
        out.extend(arr.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()));
    }
    // tolerate a single {"query":"..."} too
    if let Some(q) = v.get("query").and_then(|x| x.as_str()) {
        out.push(q.trim().to_string());
    }
    out.retain(|s| !s.is_empty());
    out.truncate(3);
    out
}

pub fn parse_read_source(args: &str) -> Option<usize> {
    let v: Value = serde_json::from_str(args).ok()?;
    let n = v.get("source").and_then(|s| s.as_u64())?;
    (n >= 1).then_some(n as usize)
}

fn key_of(s: &SourceRef) -> (String, String, i64) {
    (s.service_id.clone(), s.doc_path.to_lowercase(), s.ord)
}

/// Run the bounded tool-calling loop. `initial` is the first search's passages. Emits a `Step`
/// for each action so the UI can show progress live. Never errors: trouble ends the loop.
pub async fn gather<B: Backend>(
    backend: &B,
    question: &str,
    initial: &[SourceRef],
    cancel: &CancellationToken,
    mut on_step: impl FnMut(Step),
) -> Gathered {
    let tools = tools_schema();
    let mut messages = initial_messages(question, initial);
    let mut all: Vec<SourceRef> = initial.to_vec();
    let mut seen: std::collections::HashSet<(String, String, i64)> = initial.iter().map(key_of).collect();
    let mut out = Gathered::default();
    let start = Instant::now();

    for _ in 0..MAX_ROUNDS {
        if cancel.is_cancelled() || start.elapsed() > TIME_BUDGET || out.added.len() >= MAX_ADDED {
            break;
        }
        let reply = tokio::select! {
            r = backend.complete(&messages, &tools) => match r {
                Ok(r) => r,
                Err(e) => { on_step(Step::error(&e.code)); break; }
            },
            _ = cancel.cancelled() => break,
        };
        if reply.tool_calls.is_empty() {
            on_step(Step::done());
            break;
        }

        // echo the assistant turn (with its tool_calls) before answering each call
        let echoed: Vec<Value> = reply
            .tool_calls
            .iter()
            .map(|t| json!({"id": t.id, "type": "function", "function": {"name": t.name, "arguments": t.arguments}}))
            .collect();
        push(&mut messages, json!({"role": "assistant", "content": reply.content, "tool_calls": echoed}));

        for tc in &reply.tool_calls {
            let (result, step) = match tc.name.as_str() {
                "search_documents" => {
                    let queries = parse_search_queries(&tc.arguments);
                    if queries.is_empty() {
                        ("คำค้นไม่ถูกต้อง".to_string(), None)
                    } else {
                        let hits = backend.search(&queries).await;
                        let mut lines = Vec::new();
                        let mut found = 0;
                        for h in hits {
                            if out.added.len() >= MAX_ADDED {
                                break;
                            }
                            if seen.insert(key_of(&h)) {
                                all.push(h.clone());
                                lines.push(format!("[{}] ({}) {}", all.len(), h.file_name, trim(&h.text)));
                                out.added.push(h);
                                found += 1;
                            }
                        }
                        out.queries.extend(queries.iter().cloned());
                        let body = if found == 0 { "ไม่พบท่อนใหม่".to_string() } else { format!("เจอ {found} ท่อนใหม่:\n{}", lines.join("\n")) };
                        (body, Some(Step::search(queries.join(", "), found)))
                    }
                }
                "read_around" => match parse_read_source(&tc.arguments).and_then(|n| all.get(n - 1).cloned().map(|s| (n, s))) {
                    Some((n, src)) => match backend.read_around(&src).await {
                        Some(text) => (format!("บริบทรอบแหล่ง [{n}]:\n{}", trim(&text)), Some(Step::read(n, 0))),
                        None => (format!("แหล่ง [{n}] ไม่มีท่อนข้างเคียงเพิ่ม"), Some(Step::read(n, 0))),
                    },
                    None => ("หมายเลขแหล่งไม่ถูกต้อง".to_string(), None),
                },
                other => (format!("ไม่รู้จักเครื่องมือ {other}"), None),
            };
            if let Some(s) = step {
                on_step(s);
            }
            push(&mut messages, json!({"role": "tool", "tool_call_id": tc.id, "content": result}));
        }
    }
    out
}

fn push(messages: &mut Value, msg: Value) {
    if let Some(arr) = messages.as_array_mut() {
        arr.push(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolCall;
    use std::cell::RefCell;

    fn src(id: &str, path: &str, ord: i64, text: &str) -> SourceRef {
        SourceRef {
            n: 1, service_id: id.into(), service_name: "S".into(), doc_path: path.into(), file_name: "f".into(),
            page: None, ord, score: 0.5, rerank_score: None, keyword: false, knowledge: false, web: false, text: text.into(),
        }
    }

    /// Replies are handed out one per round; search returns a fixed candidate set.
    struct Fake {
        replies: RefCell<Vec<Reply>>,
        search_hits: Vec<SourceRef>,
        calls: RefCell<usize>,
    }

    impl Backend for Fake {
        async fn complete(&self, _m: &Value, _t: &Value) -> crate::errors::AppResult<Reply> {
            *self.calls.borrow_mut() += 1;
            Ok(self.replies.borrow_mut().remove(0))
        }
        async fn search(&self, _q: &[String]) -> Vec<SourceRef> {
            self.search_hits.clone()
        }
        async fn read_around(&self, _s: &SourceRef) -> Option<String> {
            Some("ก่อน … หลัง".into())
        }
    }

    fn tool(name: &str, args: &str) -> ToolCall {
        ToolCall { id: "c1".into(), name: name.into(), arguments: args.into() }
    }

    #[tokio::test]
    async fn searches_then_stops_when_model_is_done() {
        let fake = Fake {
            replies: RefCell::new(vec![
                Reply { content: String::new(), tool_calls: vec![tool("search_documents", r#"{"queries":["PS_X"]}"#)] },
                Reply { content: "DONE".into(), tool_calls: vec![] },
            ]),
            search_hits: vec![src("s", "schema.md", 0, "PS_X คือ...")],
            calls: RefCell::new(0),
        };
        let mut steps = Vec::new();
        let g = gather(&fake, "ถามเรื่อง PS_X", &[], &CancellationToken::new(), |s| steps.push(s)).await;
        assert_eq!(g.added.len(), 1);
        assert_eq!(g.queries, vec!["PS_X"]);
        assert_eq!(*fake.calls.borrow(), 2);
        assert_eq!(steps.last().unwrap().kind, "done");
    }

    #[tokio::test]
    async fn drops_duplicates_of_passages_already_shown() {
        let dup = src("s", "schema.md", 0, "เดิม");
        let fake = Fake {
            replies: RefCell::new(vec![
                Reply { content: String::new(), tool_calls: vec![tool("search_documents", r#"{"queries":["x"]}"#)] },
                Reply { content: "DONE".into(), tool_calls: vec![] },
            ]),
            search_hits: vec![dup.clone()],
            calls: RefCell::new(0),
        };
        let g = gather(&fake, "q", std::slice::from_ref(&dup), &CancellationToken::new(), |_| {}).await;
        assert!(g.added.is_empty(), "a passage already in the first search is not added again");
    }

    #[tokio::test]
    async fn stops_at_the_round_limit() {
        // every round asks to search again; the loop must still stop at MAX_ROUNDS
        let always = || Reply { content: String::new(), tool_calls: vec![tool("search_documents", r#"{"queries":["x"]}"#)] };
        let fake = Fake {
            replies: RefCell::new(vec![always(), always(), always(), always()]),
            search_hits: vec![],
            calls: RefCell::new(0),
        };
        gather(&fake, "q", &[], &CancellationToken::new(), |_| {}).await;
        assert_eq!(*fake.calls.borrow(), MAX_ROUNDS);
    }

    #[tokio::test]
    async fn bad_tool_json_does_not_crash() {
        let fake = Fake {
            replies: RefCell::new(vec![
                Reply { content: String::new(), tool_calls: vec![tool("search_documents", "not json")] },
                Reply { content: "DONE".into(), tool_calls: vec![] },
            ]),
            search_hits: vec![src("s", "a.md", 0, "x")],
            calls: RefCell::new(0),
        };
        let g = gather(&fake, "q", &[], &CancellationToken::new(), |_| {}).await;
        assert!(g.added.is_empty());
    }

    #[test]
    fn parses_tool_arguments() {
        assert_eq!(parse_search_queries(r#"{"queries":["A"," B ",""]}"#), vec!["A", "B"]);
        assert_eq!(parse_search_queries(r#"{"query":"solo"}"#), vec!["solo"]);
        assert!(parse_search_queries("broken").is_empty());
        assert_eq!(parse_read_source(r#"{"source":3}"#), Some(3));
        assert_eq!(parse_read_source(r#"{"source":0}"#), None);
        assert_eq!(parse_read_source("x"), None);
    }
}
