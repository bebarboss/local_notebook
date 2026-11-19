//! Bounded web-research loop for the "ค้นเว็บ" toggle. Mirrors `agent::gather` but the tools are
//! `web_search` (DuckDuckGo) and `read_url` (fetch a page as text). Hard limits: `MAX_ROUNDS`,
//! a wall-clock budget, and a cap on pages collected. On any trouble (no tool call, an error, a
//! cancel) the loop ends and we answer on what we have. Reuses `agent::Step` so the UI shows the
//! activity the same way, with two web-flavoured kinds. Never logs the query, page text or answer.

use crate::chats::SourceRef;
use crate::agent::Step;
use crate::llm::Reply;
use crate::websearch::{self, WebHit};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub const MAX_ROUNDS: usize = 3;
/// Wall-clock budget for the whole research step (web is slower than the index).
pub const TIME_BUDGET: Duration = Duration::from_secs(60);
/// Each Gateway round's own timeout.
pub const PER_CALL: Duration = Duration::from_secs(25);
/// Most web pages/results the step may collect as sources.
pub const MAX_PAGES: usize = 8;
/// Results requested per `web_search` query.
pub const SEARCH_RESULTS: usize = 5;
/// Characters kept from a fetched page (shown to the answer step as its source text).
pub const PAGE_CHARS: usize = 4000;
/// A page/snippet is trimmed to this in the loop's tool result, to spend fewer tokens per round.
const LOOP_CHARS: usize = 1500;

/// What the host must provide: a Gateway round and the two web tools. A trait so the loop can be
/// tested with scripted replies and no network.
#[allow(async_fn_in_trait)]
pub trait WebBackend {
    async fn complete(&self, messages: &Value, tools: &Value) -> crate::errors::AppResult<Reply>;
    async fn search_web(&self, query: &str) -> Vec<WebHit>;
    async fn read_url(&self, url: &str) -> Option<String>;
}

pub fn tools_schema() -> Value {
    json!([
        {"type": "function", "function": {
            "name": "web_search",
            "description": "ค้นเว็บสาธารณะด้วย DuckDuckGo เพื่อหาข้อมูลล่าสุด/ข้อเท็จจริงที่เปลี่ยนตามเวลา คืนรายการผล (ชื่อ, ลิงก์, ตัวอย่าง)",
            "parameters": {"type": "object", "properties": {
                "queries": {"type": "array", "items": {"type": "string"}, "description": "คำค้น 1-3 คำ"}
            }, "required": ["queries"]}
        }},
        {"type": "function", "function": {
            "name": "read_url",
            "description": "เปิดลิงก์จากผลการค้นเพื่ออ่านเนื้อหาเต็มของหน้านั้น ใช้เมื่อตัวอย่างสั้นยังไม่พอ",
            "parameters": {"type": "object", "properties": {
                "url": {"type": "string", "description": "URL http/https ที่ได้จากผลการค้น"}
            }, "required": ["url"]}
        }}
    ])
}

fn system_prompt(now: Option<&str>) -> String {
    let today = now.map(|n| format!(" วันนี้คือ {n}.")).unwrap_or_default();
    format!(
        "คุณกำลังค้นเว็บเพื่อให้ตอบคำถามได้เป็นปัจจุบันและถูกต้อง{today}\n\
         - เรียก web_search เพื่อหาแหล่ง แล้ว read_url เปิดหน้าที่น่าเชื่อถือเมื่อตัวอย่างยังไม่พอ\n\
         - ค้นได้สูงสุด {MAX_ROUNDS} รอบ พอได้ข้อมูลพอจะตอบแล้วให้ตอบสั้น ๆ ว่า DONE โดยไม่เรียกเครื่องมือ\n\
         - อย่าเดา ให้ยึดจากแหล่งที่เปิดจริง และจำหมายเลข [n] ของแหล่งไว้อ้างอิง"
    )
}

fn initial_messages(question: &str, now: Option<&str>) -> Value {
    json!([
        {"role": "system", "content": system_prompt(now)},
        {"role": "user", "content": format!("คำถาม: {question}")}
    ])
}

fn parse_read_url(args: &str) -> Option<String> {
    let v: Value = serde_json::from_str(args).ok()?;
    let u = v.get("url").and_then(|x| x.as_str())?.trim().to_string();
    (u.starts_with("http://") || u.starts_with("https://")).then_some(u)
}

fn web_source(n: usize, title: &str, url: &str, text: &str) -> SourceRef {
    let name = if title.trim().is_empty() { websearch::host_of(url) } else { title.trim().to_string() };
    SourceRef {
        n,
        service_id: String::new(),
        service_name: "เว็บ".into(),
        doc_path: url.to_string(),
        file_name: name,
        page: None,
        ord: 0,
        score: 0.0,
        rerank_score: None,
        keyword: false,
        knowledge: false,
        web: true,
        text: cap(text, PAGE_CHARS),
    }
}

fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

fn trim(s: &str) -> String {
    cap(&s.replace('\n', " "), LOOP_CHARS)
}

fn search_step(detail: String, found: usize) -> Step {
    Step { kind: "websearch".into(), detail, found }
}
fn read_step(host: String) -> Step {
    Step { kind: "webread".into(), detail: host, found: 0 }
}
fn done_step() -> Step {
    Step { kind: "done".into(), detail: String::new(), found: 0 }
}
fn error_step(code: &str) -> Step {
    Step { kind: "error".into(), detail: code.into(), found: 0 }
}

fn push(messages: &mut Value, msg: Value) {
    if let Some(arr) = messages.as_array_mut() {
        arr.push(msg);
    }
}

/// Run the bounded web-research loop. Emits a `Step` per action for the live activity list.
/// Never errors: any trouble ends the loop and returns the sources collected so far.
pub async fn research<B: WebBackend>(
    backend: &B,
    question: &str,
    now: Option<&str>,
    cancel: &CancellationToken,
    mut on_step: impl FnMut(Step),
) -> Vec<SourceRef> {
    let tools = tools_schema();
    let mut messages = initial_messages(question, now);
    let mut sources: Vec<SourceRef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let start = Instant::now();

    for _ in 0..MAX_ROUNDS {
        if cancel.is_cancelled() || start.elapsed() > TIME_BUDGET || sources.len() >= MAX_PAGES {
            break;
        }
        let reply = tokio::select! {
            r = backend.complete(&messages, &tools) => match r {
                Ok(r) => r,
                Err(e) => { on_step(error_step(&e.code)); break; }
            },
            _ = cancel.cancelled() => break,
        };
        if reply.tool_calls.is_empty() {
            on_step(done_step());
            break;
        }

        let echoed: Vec<Value> = reply
            .tool_calls
            .iter()
            .map(|t| json!({"id": t.id, "type": "function", "function": {"name": t.name, "arguments": t.arguments}}))
            .collect();
        push(&mut messages, json!({"role": "assistant", "content": reply.content, "tool_calls": echoed}));

        for tc in &reply.tool_calls {
            let (result, step) = match tc.name.as_str() {
                "web_search" => {
                    let queries = crate::agent::parse_search_queries(&tc.arguments);
                    if queries.is_empty() {
                        ("คำค้นไม่ถูกต้อง".to_string(), None)
                    } else {
                        let mut lines = Vec::new();
                        let mut found = 0;
                        for q in &queries {
                            if sources.len() >= MAX_PAGES {
                                break;
                            }
                            for h in backend.search_web(q).await {
                                if sources.len() >= MAX_PAGES {
                                    break;
                                }
                                if seen.insert(h.url.to_lowercase()) {
                                    let n = sources.len() + 1;
                                    lines.push(format!("[{n}] {} — {}\n{}", h.title, h.url, trim(&h.snippet)));
                                    sources.push(web_source(n, &h.title, &h.url, &h.snippet));
                                    found += 1;
                                }
                            }
                        }
                        let body = if found == 0 { "ไม่พบผลใหม่".to_string() } else { format!("ผลการค้น:\n{}", lines.join("\n")) };
                        (body, Some(search_step(queries.join(", "), found)))
                    }
                }
                "read_url" => match parse_read_url(&tc.arguments) {
                    Some(u) => {
                        let host = websearch::host_of(&u);
                        match backend.read_url(&u).await {
                            Some(text) => {
                                let key = u.to_lowercase();
                                let n = if let Some(pos) = sources.iter().position(|s| s.doc_path.to_lowercase() == key) {
                                    sources[pos].text = cap(&text, PAGE_CHARS);
                                    pos + 1
                                } else if sources.len() < MAX_PAGES {
                                    seen.insert(key);
                                    let n = sources.len() + 1;
                                    sources.push(web_source(n, &host, &u, &text));
                                    n
                                } else {
                                    0
                                };
                                let body = if n > 0 {
                                    format!("เนื้อหาจาก [{n}] {u}:\n{}", trim(&text))
                                } else {
                                    "อ่านครบจำนวนหน้าที่กำหนดแล้ว".to_string()
                                };
                                (body, Some(read_step(host)))
                            }
                            None => (format!("อ่านหน้า {u} ไม่ได้"), Some(read_step(host))),
                        }
                    }
                    None => ("URL ไม่ถูกต้อง".to_string(), None),
                },
                other => (format!("ไม่รู้จักเครื่องมือ {other}"), None),
            };
            if let Some(s) = step {
                on_step(s);
            }
            push(&mut messages, json!({"role": "tool", "tool_call_id": tc.id, "content": result}));
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolCall;
    use std::cell::RefCell;

    struct Fake {
        replies: RefCell<Vec<Reply>>,
        hits: Vec<WebHit>,
        page: Option<String>,
        calls: RefCell<usize>,
    }
    impl WebBackend for Fake {
        async fn complete(&self, _m: &Value, _t: &Value) -> crate::errors::AppResult<Reply> {
            *self.calls.borrow_mut() += 1;
            Ok(self.replies.borrow_mut().remove(0))
        }
        async fn search_web(&self, _q: &str) -> Vec<WebHit> {
            self.hits.clone()
        }
        async fn read_url(&self, _u: &str) -> Option<String> {
            self.page.clone()
        }
    }
    fn tool(name: &str, args: &str) -> ToolCall {
        ToolCall { id: "c1".into(), name: name.into(), arguments: args.into() }
    }
    fn hit(title: &str, url: &str) -> WebHit {
        WebHit { title: title.into(), url: url.into(), snippet: "ตัวอย่างผลการค้นที่ยาวพอสมควร".into() }
    }

    #[tokio::test]
    async fn searches_then_reads_then_done() {
        let fake = Fake {
            replies: RefCell::new(vec![
                Reply { content: String::new(), tool_calls: vec![tool("web_search", r#"{"queries":["นายกไทย"]}"#)] },
                Reply { content: String::new(), tool_calls: vec![tool("read_url", r#"{"url":"https://a.com/x"}"#)] },
                Reply { content: "DONE".into(), tool_calls: vec![] },
            ]),
            hits: vec![hit("ผล A", "https://a.com/x"), hit("ผล B", "https://b.com/y")],
            page: Some("เนื้อหาเต็มของหน้า a.com ที่อ่านได้".into()),
            calls: RefCell::new(0),
        };
        let mut steps = Vec::new();
        let srcs = research(&fake, "นายกไทยคนปัจจุบัน", Some("20 พ.ย. 2026"), &CancellationToken::new(), |s| steps.push(s)).await;
        assert_eq!(srcs.len(), 2, "two unique results became sources");
        assert_eq!(srcs[0].doc_path, "https://a.com/x");
        assert!(srcs[0].web);
        assert!(srcs[0].text.contains("เนื้อหาเต็ม"), "read_url upgraded the source text");
        assert_eq!(*fake.calls.borrow(), 3);
        assert_eq!(steps.first().unwrap().kind, "websearch");
        assert!(steps.iter().any(|s| s.kind == "webread"));
        assert_eq!(steps.last().unwrap().kind, "done");
    }

    #[tokio::test]
    async fn dedupes_urls_and_caps_pages() {
        let many: Vec<WebHit> = (0..20).map(|i| hit(&format!("t{i}"), &format!("https://x.com/{i}"))).collect();
        let fake = Fake {
            replies: RefCell::new(vec![
                Reply { content: String::new(), tool_calls: vec![tool("web_search", r#"{"queries":["q"]}"#)] },
                Reply { content: String::new(), tool_calls: vec![tool("web_search", r#"{"queries":["q"]}"#)] },
                Reply { content: "DONE".into(), tool_calls: vec![] },
            ]),
            hits: many,
            page: None,
            calls: RefCell::new(0),
        };
        let srcs = research(&fake, "q", None, &CancellationToken::new(), |_| {}).await;
        assert_eq!(srcs.len(), MAX_PAGES, "capped at MAX_PAGES, duplicates across rounds dropped");
    }

    #[tokio::test]
    async fn gateway_error_ends_cleanly() {
        let fake = Fake {
            replies: RefCell::new(vec![Reply { content: String::new(), tool_calls: vec![tool("web_search", "not json")] }, Reply { content: "DONE".into(), tool_calls: vec![] }]),
            hits: vec![],
            page: None,
            calls: RefCell::new(0),
        };
        let srcs = research(&fake, "q", None, &CancellationToken::new(), |_| {}).await;
        assert!(srcs.is_empty());
    }

    #[test]
    fn parses_read_url() {
        assert_eq!(parse_read_url(r#"{"url":"https://a.com"}"#).as_deref(), Some("https://a.com"));
        assert_eq!(parse_read_url(r#"{"url":"ftp://a"}"#), None);
        assert_eq!(parse_read_url("broken"), None);
    }
}
