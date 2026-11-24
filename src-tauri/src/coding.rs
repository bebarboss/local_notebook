//! Bounded coding loop for the "เขียนโค้ด" toggle. Mirrors `web_research::research` but the tools act
//! on a project folder: `list_dir` / `read_file` / `grep` to explore, and `write_file` / `edit_file`
//! to change code. Writes land on disk immediately (so VS Code, open on the same folder, shows them
//! at once); the original content of each file is captured the first time a turn touches it, so the
//! whole turn can be undone. Hard limits: `MAX_ROUNDS`, a wall-clock budget, and a cap on files
//! changed. On any trouble (no tool call, an error, a cancel) the loop ends and we answer with the
//! changes made so far. Never logs file contents — step details are paths/counts only.

use crate::agent::Step;
use crate::codefs::{CodeChange, DirEntry, Hit, WriteOutcome};
use crate::llm::Reply;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub const MAX_ROUNDS: usize = 16;
/// Wall-clock budget for a whole coding turn.
pub const TIME_BUDGET: Duration = Duration::from_secs(180);
/// Each Gateway round's own timeout (code replies can be long).
pub const PER_CALL: Duration = Duration::from_secs(60);
/// Most files a single turn may change.
pub const MAX_FILES: usize = 40;
/// A tool result shown back to the model is trimmed to this many characters per call.
const RESULT_CHARS: usize = 6000;

/// What the host provides: a Gateway round and the project-scoped filesystem. A trait so the loop can
/// be tested with scripted replies over a temp folder.
#[allow(async_fn_in_trait)]
pub trait CodeBackend {
    async fn complete(&self, messages: &Value, tools: &Value) -> crate::errors::AppResult<Reply>;
    fn list_dir(&self, path: &str) -> crate::errors::AppResult<Vec<DirEntry>>;
    fn read_file(&self, path: &str) -> crate::errors::AppResult<String>;
    fn grep(&self, pattern: &str, sub: Option<&str>) -> crate::errors::AppResult<Vec<Hit>>;
    fn write_file(&self, path: &str, content: &str) -> crate::errors::AppResult<WriteOutcome>;
    fn edit_file(&self, path: &str, old: &str, new: &str) -> crate::errors::AppResult<WriteOutcome>;
}

pub fn tools_schema() -> Value {
    json!([
        {"type": "function", "function": {
            "name": "list_dir",
            "description": "ดูรายชื่อไฟล์/โฟลเดอร์ในพาธที่ระบุ (relative กับรากโปรเจค, ใช้ \"\" หรือ \".\" สำหรับราก)",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string", "description": "พาธโฟลเดอร์ relative กับรากโปรเจค"}
            }, "required": ["path"]}
        }},
        {"type": "function", "function": {
            "name": "read_file",
            "description": "อ่านเนื้อหาไฟล์ข้อความ (relative กับรากโปรเจค) อ่านก่อนแก้เสมอ",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string"}
            }, "required": ["path"]}
        }},
        {"type": "function", "function": {
            "name": "grep",
            "description": "ค้นข้อความ (regex) ในไฟล์ทั้งโปรเจค คืนพาธ+เลขบรรทัด ใช้หาว่าโค้ดอยู่ไฟล์ไหน",
            "parameters": {"type": "object", "properties": {
                "pattern": {"type": "string", "description": "regex ที่จะค้น"},
                "path": {"type": "string", "description": "จำกัดเฉพาะโฟลเดอร์นี้ (ไม่ใส่ = ทั้งโปรเจค)"}
            }, "required": ["pattern"]}
        }},
        {"type": "function", "function": {
            "name": "write_file",
            "description": "สร้างไฟล์ใหม่หรือเขียนทับทั้งไฟล์ด้วยเนื้อหาที่ให้ ใช้กับไฟล์ใหม่หรือเขียนใหม่ทั้งไฟล์",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string", "description": "เนื้อหาเต็มของไฟล์"}
            }, "required": ["path", "content"]}
        }},
        {"type": "function", "function": {
            "name": "edit_file",
            "description": "แก้บางส่วนของไฟล์ที่มีอยู่: แทนที่ข้อความเดิม (old) ด้วยข้อความใหม่ (new) · old ต้องตรงอักขระและมีที่เดียวในไฟล์ (ใส่บริบทรอบ ๆ ให้ยาวพอจะเจาะจง)",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string"},
                "old": {"type": "string", "description": "ข้อความเดิมที่จะถูกแทน (ต้องมีที่เดียว)"},
                "new": {"type": "string", "description": "ข้อความใหม่"}
            }, "required": ["path", "old", "new"]}
        }}
    ])
}

fn system_prompt(now: Option<&str>) -> String {
    let today = now.map(|n| format!(" วันนี้คือ {n}.")).unwrap_or_default();
    format!(
        "คุณเป็นผู้ช่วยเขียนโค้ดที่ทำงานในโฟลเดอร์โปรเจคของผู้ใช้โดยตรง{today}\n\
         - สำรวจก่อนด้วย list_dir/read_file/grep ให้เข้าใจโครงสร้างและสไตล์โค้ดเดิม แล้วค่อยแก้\n\
         - แก้ไฟล์ที่มีอยู่ด้วย edit_file (old ต้องตรงอักขระเป๊ะและมีที่เดียว) · สร้างไฟล์ใหม่หรือเขียนใหม่ทั้งไฟล์ด้วย write_file\n\
         - ทำให้ตรงกับที่ผู้ใช้ขอ แก้เท่าที่จำเป็น รักษาสไตล์เดิม อย่าเดาพาธหรือชื่อที่ยังไม่ได้อ่าน\n\
         - ถ้าเครื่องมือคืน error ให้อ่านสาเหตุแล้วแก้วิธีเรียกใหม่\n\
         - ทำงานได้สูงสุด {MAX_ROUNDS} รอบ พอแก้เสร็จแล้วให้ตอบสรุปสั้น ๆ ว่าแก้อะไรไปบ้าง (ไม่ต้องเรียกเครื่องมือ)"
    )
}

fn initial_messages(question: &str, now: Option<&str>) -> Value {
    json!([
        {"role": "system", "content": system_prompt(now)},
        {"role": "user", "content": format!("งานที่ต้องทำ: {question}")}
    ])
}

fn arg<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

fn trim_result(s: &str) -> String {
    if s.chars().count() <= RESULT_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(RESULT_CHARS).collect();
    out.push_str("\n…(ตัดบางส่วน)");
    out
}

fn step(kind: &str, detail: String) -> Step {
    Step { kind: kind.into(), detail, found: 0 }
}

fn push(messages: &mut Value, msg: Value) {
    if let Some(arr) = messages.as_array_mut() {
        arr.push(msg);
    }
}

/// Tracks the files a turn changed, keeping each file's *original* content (first touch) and its
/// latest content, so repeated edits to one file collapse into a single change.
#[derive(Default)]
struct Changes {
    order: Vec<String>,
    original: HashMap<String, Option<String>>,
    latest: HashMap<String, String>,
}

impl Changes {
    fn record(&mut self, path: &str, outcome: WriteOutcome) {
        if !self.original.contains_key(path) {
            self.original.insert(path.to_string(), outcome.before);
            self.order.push(path.to_string());
        }
        self.latest.insert(path.to_string(), outcome.after);
    }
    fn len(&self) -> usize {
        self.order.len()
    }
    fn into_vec(self) -> Vec<CodeChange> {
        let Changes { order, mut original, mut latest } = self;
        order
            .into_iter()
            .map(|p| {
                let before = original.remove(&p).flatten();
                let after = latest.remove(&p).unwrap_or_default();
                let kind = if before.is_none() { "create" } else { "modify" };
                CodeChange { path: p, kind: kind.into(), before, after }
            })
            .collect()
    }
}

/// Run the bounded coding loop. Emits a `Step` per action for the live activity list. Returns the
/// files changed and the model's final summary text (its last turn with no tool calls). Never
/// errors: any trouble ends the loop and returns what was done so far.
pub async fn edit_codebase<B: CodeBackend>(
    backend: &B,
    question: &str,
    now: Option<&str>,
    cancel: &CancellationToken,
    mut on_step: impl FnMut(Step),
) -> (Vec<CodeChange>, String) {
    let tools = tools_schema();
    let mut messages = initial_messages(question, now);
    let mut changes = Changes::default();
    let mut final_text = String::new();
    let start = Instant::now();

    for _ in 0..MAX_ROUNDS {
        if cancel.is_cancelled() || start.elapsed() > TIME_BUDGET {
            break;
        }
        let reply = tokio::select! {
            r = backend.complete(&messages, &tools) => match r {
                Ok(r) => r,
                Err(e) => { on_step(step("error", e.code.clone())); break; }
            },
            _ = cancel.cancelled() => break,
        };
        if !reply.content.trim().is_empty() {
            final_text = reply.content.clone();
        }
        if reply.tool_calls.is_empty() {
            on_step(step("done", String::new()));
            break;
        }

        let echoed: Vec<Value> = reply
            .tool_calls
            .iter()
            .map(|t| json!({"id": t.id, "type": "function", "function": {"name": t.name, "arguments": t.arguments}}))
            .collect();
        push(&mut messages, json!({"role": "assistant", "content": reply.content, "tool_calls": echoed}));

        for tc in &reply.tool_calls {
            let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(Value::Null);
            let (result, emitted) = run_tool(backend, &tc.name, &args, &mut changes);
            if let Some(s) = emitted {
                on_step(s);
            }
            push(&mut messages, json!({"role": "tool", "tool_call_id": tc.id, "content": result}));
        }
    }
    (changes.into_vec(), final_text)
}

/// Execute one tool call; returns the text result for the model and the step to emit (if any).
fn run_tool<B: CodeBackend>(backend: &B, name: &str, args: &Value, changes: &mut Changes) -> (String, Option<Step>) {
    match name {
        "list_dir" => {
            let path = arg(args, "path").unwrap_or("");
            match backend.list_dir(path) {
                Ok(entries) => {
                    let body = entries.iter().map(|e| if e.dir { format!("{}/", e.name) } else { e.name.clone() }).collect::<Vec<_>>().join("\n");
                    (format!("เนื้อหาใน \"{}\":\n{}", disp(path), trim_result(&body)), Some(step("codelist", disp(path))))
                }
                Err(e) => (format!("list_dir ไม่สำเร็จ: {}", e.message), Some(step("codelist", disp(path)))),
            }
        }
        "read_file" => {
            let path = arg(args, "path").unwrap_or("");
            match backend.read_file(path) {
                Ok(text) => (format!("เนื้อหา {}:\n{}", path, trim_result(&text)), Some(step("coderead", path.to_string()))),
                Err(e) => (format!("อ่าน {} ไม่ได้: {}", path, e.message), Some(step("coderead", path.to_string()))),
            }
        }
        "grep" => {
            let Some(pattern) = arg(args, "pattern") else { return ("ต้องระบุ pattern".into(), None) };
            let sub = arg(args, "path").filter(|s| !s.is_empty());
            match backend.grep(pattern, sub) {
                Ok(hits) if hits.is_empty() => ("ไม่พบที่ตรงกับ pattern".into(), Some(step("codegrep", pattern.to_string()))),
                Ok(hits) => {
                    let body = hits.iter().map(|h| format!("{}:{}: {}", h.path, h.line, h.text)).collect::<Vec<_>>().join("\n");
                    (format!("ผลค้น:\n{}", trim_result(&body)), Some(step("codegrep", pattern.to_string())))
                }
                Err(e) => (format!("grep ไม่สำเร็จ: {}", e.message), Some(step("codegrep", pattern.to_string()))),
            }
        }
        "write_file" => {
            let (Some(path), Some(content)) = (arg(args, "path"), arg(args, "content")) else {
                return ("ต้องระบุ path และ content".into(), None);
            };
            if changes.len() >= MAX_FILES && !changes.original.contains_key(path) {
                return (format!("แก้ไฟล์ครบ {MAX_FILES} ไฟล์ตามขีดจำกัดต่อครั้งแล้ว"), None);
            }
            match backend.write_file(path, content) {
                Ok(outcome) => {
                    let created = outcome.before.is_none();
                    changes.record(path, outcome);
                    (format!("เขียน {} สำเร็จ", path), Some(step(if created { "codewrite" } else { "codeedit" }, path.to_string())))
                }
                Err(e) => (format!("เขียน {} ไม่ได้: {}", path, e.message), Some(step("error", e.code))),
            }
        }
        "edit_file" => {
            let (Some(path), Some(old), Some(new)) = (arg(args, "path"), arg(args, "old"), arg(args, "new")) else {
                return ("ต้องระบุ path, old และ new".into(), None);
            };
            if changes.len() >= MAX_FILES && !changes.original.contains_key(path) {
                return (format!("แก้ไฟล์ครบ {MAX_FILES} ไฟล์ตามขีดจำกัดต่อครั้งแล้ว"), None);
            }
            match backend.edit_file(path, old, new) {
                Ok(outcome) => {
                    changes.record(path, outcome);
                    (format!("แก้ {} สำเร็จ", path), Some(step("codeedit", path.to_string())))
                }
                // an edit error (not found / ambiguous) is handed back so the model can retry
                Err(e) => (format!("แก้ {} ไม่ได้: {}", path, e.message), Some(step("codeedit", path.to_string()))),
            }
        }
        other => (format!("ไม่รู้จักเครื่องมือ {other}"), None),
    }
}

fn disp(path: &str) -> String {
    if path.trim().is_empty() { ".".into() } else { path.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codefs::Workspace;
    use crate::llm::ToolCall;
    use std::cell::RefCell;
    use std::fs;

    struct Fake {
        ws: Workspace,
        replies: RefCell<Vec<Reply>>,
        calls: RefCell<usize>,
    }
    impl CodeBackend for Fake {
        async fn complete(&self, _m: &Value, _t: &Value) -> crate::errors::AppResult<Reply> {
            *self.calls.borrow_mut() += 1;
            Ok(self.replies.borrow_mut().remove(0))
        }
        fn list_dir(&self, p: &str) -> crate::errors::AppResult<Vec<DirEntry>> {
            self.ws.list(p)
        }
        fn read_file(&self, p: &str) -> crate::errors::AppResult<String> {
            self.ws.read(p)
        }
        fn grep(&self, pat: &str, sub: Option<&str>) -> crate::errors::AppResult<Vec<Hit>> {
            self.ws.grep(pat, sub)
        }
        fn write_file(&self, p: &str, c: &str) -> crate::errors::AppResult<WriteOutcome> {
            self.ws.write(p, c)
        }
        fn edit_file(&self, p: &str, old: &str, new: &str) -> crate::errors::AppResult<WriteOutcome> {
            self.ws.edit(p, old, new)
        }
    }

    fn fake(replies: Vec<Reply>) -> (tempfile::TempDir, Fake) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.py"), "print('hi')\n").unwrap();
        let ws = Workspace::new(&dir.path().to_string_lossy()).unwrap();
        (dir, Fake { ws, replies: RefCell::new(replies), calls: RefCell::new(0) })
    }
    fn tool(name: &str, args: &str) -> ToolCall {
        ToolCall { id: "c1".into(), name: name.into(), arguments: args.into() }
    }
    fn rt() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn reads_then_edits_then_done() {
        let (_d, f) = fake(vec![
            Reply { content: String::new(), tool_calls: vec![tool("read_file", r#"{"path":"main.py"}"#)] },
            Reply { content: String::new(), tool_calls: vec![tool("edit_file", r#"{"path":"main.py","old":"hi","new":"world"}"#)] },
            Reply { content: "แก้ให้แล้ว".into(), tool_calls: vec![] },
        ]);
        let mut steps = Vec::new();
        let (changes, answer) = edit_codebase(&f, "เปลี่ยน hi เป็น world", None, &rt(), |s| steps.push(s)).await;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "main.py");
        assert_eq!(changes[0].kind, "modify");
        assert_eq!(changes[0].before.as_deref(), Some("print('hi')\n"));
        assert_eq!(changes[0].after, "print('world')\n");
        assert_eq!(f.ws.read("main.py").unwrap(), "print('world')\n", "write hit the disk");
        assert_eq!(answer, "แก้ให้แล้ว", "the final no-tool reply is the summary");
        assert_eq!(steps.last().unwrap().kind, "done");
    }

    #[tokio::test]
    async fn creates_a_new_file() {
        let (_d, f) = fake(vec![
            Reply { content: String::new(), tool_calls: vec![tool("write_file", r#"{"path":"a/b.txt","content":"ok\n"}"#)] },
            Reply { content: "สร้างแล้ว".into(), tool_calls: vec![] },
        ]);
        let (changes, _) = edit_codebase(&f, "สร้างไฟล์", None, &rt(), |_| {}).await;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "create");
        assert_eq!(changes[0].before, None);
        assert_eq!(f.ws.read("a/b.txt").unwrap(), "ok\n");
    }

    #[tokio::test]
    async fn failed_edit_is_retried_and_collapses_to_one_change() {
        let (_d, f) = fake(vec![
            // wrong old → error handed back
            Reply { content: String::new(), tool_calls: vec![tool("edit_file", r#"{"path":"main.py","old":"nope","new":"x"}"#)] },
            // corrected → succeeds
            Reply { content: String::new(), tool_calls: vec![tool("edit_file", r#"{"path":"main.py","old":"hi","new":"world"}"#)] },
            Reply { content: "เสร็จ".into(), tool_calls: vec![] },
        ]);
        let (changes, _) = edit_codebase(&f, "แก้", None, &rt(), |_| {}).await;
        assert_eq!(changes.len(), 1, "one file changed, the failed attempt left nothing");
        assert_eq!(changes[0].before.as_deref(), Some("print('hi')\n"));
        assert_eq!(changes[0].after, "print('world')\n");
    }

    #[tokio::test]
    async fn bad_tool_json_does_not_crash() {
        let (_d, f) = fake(vec![
            Reply { content: String::new(), tool_calls: vec![tool("edit_file", "not json")] },
            Reply { content: "จบ".into(), tool_calls: vec![] },
        ]);
        let (changes, _) = edit_codebase(&f, "q", None, &rt(), |_| {}).await;
        assert!(changes.is_empty());
    }

    #[tokio::test]
    async fn stops_at_the_round_limit() {
        let always = || Reply { content: String::new(), tool_calls: vec![tool("read_file", r#"{"path":"main.py"}"#)] };
        let (_d, f) = fake((0..MAX_ROUNDS + 4).map(|_| always()).collect());
        edit_codebase(&f, "q", None, &rt(), |_| {}).await;
        assert_eq!(*f.calls.borrow(), MAX_ROUNDS);
    }
}
