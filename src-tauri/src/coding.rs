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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub const MAX_ROUNDS: usize = 24;
/// Wall-clock budget for a whole coding turn (includes time spent waiting for a file-permission reply).
pub const TIME_BUDGET: Duration = Duration::from_secs(300);
/// Most plan steps kept from one `update_plan` call.
const MAX_PLAN_STEPS: usize = 12;

/// One item in the agent's visible plan checklist. `status` is "pending" | "running" | "done".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub text: String,
    pub status: String,
}

/// Result of a `read_reference_file` call once the user has (or hasn't) approved it.
#[derive(Debug, Clone, PartialEq)]
pub enum ReferenceOutcome {
    /// extracted text of the approved outside file
    Allowed(String),
    /// the user skipped it (or didn't answer in time)
    Denied,
}

/// Everything a coding turn produces: files changed, the final summary text, an optional stop reason
/// (set only when it ended abnormally without a clean summary), and the latest plan checklist.
pub struct CodeOutcome {
    pub changes: Vec<CodeChange>,
    pub answer: String,
    pub stop: Option<String>,
    pub plan: Vec<PlanStep>,
}
/// Each Gateway round's own timeout. Code replies can be long, and a round that feeds a whole file
/// back to a reasoning model is slow — 60s was tight enough to time out mid-turn on big files and
/// get mis-reported as "can't reach Gateway". The whole turn is still bounded by TIME_BUDGET.
pub const PER_CALL: Duration = Duration::from_secs(120);
/// Most files a single turn may change.
pub const MAX_FILES: usize = 40;
/// A tool result shown back to the model is trimmed to this many characters per call.
const RESULT_CHARS: usize = 6000;
/// read_file gets a larger slice than grep/list so the model can see a whole file before editing it
/// (edit_file matches the exact original text). Files are capped at 256 KB on disk (codefs); a file
/// longer than this still truncates, and the model is told to grep for the parts it needs.
const READ_RESULT_CHARS: usize = 24_000;

/// Result of a `run_command` call. `blocked` means the command was refused for not being in the
/// project's allowlist (the model is told so and should try an allowed one or stop); otherwise
/// `exit_code`/`text` carry the real outcome. The allowlist check and the run both live in the host
/// impl (CodeAgent), so the loop stays testable.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub blocked: bool,
    pub exit_code: Option<i32>,
    pub text: String,
}

/// What the host provides: a Gateway round, the project-scoped filesystem, and an allowlisted command
/// runner. A trait so the loop can be tested with scripted replies over a temp folder.
#[allow(async_fn_in_trait)]
pub trait CodeBackend {
    async fn complete(&self, messages: &Value, tools: &Value) -> crate::errors::AppResult<Reply>;
    fn list_dir(&self, path: &str) -> crate::errors::AppResult<Vec<DirEntry>>;
    fn read_file(&self, path: &str, offset: Option<usize>, limit: Option<usize>) -> crate::errors::AppResult<String>;
    fn grep(&self, pattern: &str, sub: Option<&str>) -> crate::errors::AppResult<Vec<Hit>>;
    fn write_file(&self, path: &str, content: &str) -> crate::errors::AppResult<WriteOutcome>;
    fn edit_file(&self, path: &str, old: &str, new: &str) -> crate::errors::AppResult<WriteOutcome>;
    async fn run_command(&self, command: &str) -> crate::errors::AppResult<RunResult>;
    /// Read a file outside the project root — asks the user to approve it first (read-only).
    async fn read_reference(&self, path: &str) -> crate::errors::AppResult<ReferenceOutcome>;
    /// Publish the agent's plan checklist to the UI (and let the host remember it).
    async fn update_plan(&self, steps: &[PlanStep]);
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
            "description": "อ่านไฟล์ในโปรเจค (relative กับราก) อ่านก่อนแก้เสมอ · ไฟล์เอกสาร (pdf/xlsx/docx/pptx/รูป) จะถูกดึงข้อความให้อัตโนมัติ · ไฟล์ยาวใส่ offset/limit เพื่ออ่านเป็นช่วง",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "description": "เริ่มอ่านจากบรรทัดที่ (เริ่มที่ 1) — ใช้กับไฟล์ข้อความยาว"},
                "limit": {"type": "integer", "description": "จำนวนบรรทัดที่จะอ่าน (ไม่ใส่ = จนจบหรือจนเต็มขีดจำกัด)"}
            }, "required": ["path"]}
        }},
        {"type": "function", "function": {
            "name": "read_reference_file",
            "description": "อ่านไฟล์อ้างอิงที่อยู่ \"นอก\" โฟลเดอร์โปรเจค (เช่น ไฟล์ใน Downloads) ใส่พาธเต็มของไฟล์ · ระบบจะถามผู้ใช้ให้กดอนุญาตก่อน และอ่านได้อย่างเดียว (เขียนไม่ได้) · รองรับ xlsx/pdf/docx/csv/ข้อความ — ใช้เมื่อจำเป็นต้องดูเนื้อหาไฟล์ภายนอก อย่าเดาเอง",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string", "description": "พาธเต็มของไฟล์ภายนอก เช่น C:\\Users\\me\\Downloads\\data.xlsx"}
            }, "required": ["path"]}
        }},
        {"type": "function", "function": {
            "name": "update_plan",
            "description": "ตั้ง/อัปเดตเช็กลิสต์แผนงานที่ผู้ใช้เห็นความคืบหน้า · เรียกครั้งแรกตอนเริ่มงานเพื่อวางแผนทั้งหมด (ข้อแรก status=\"running\" ที่เหลือ \"pending\") แล้วเรียกซ้ำทุกครั้งที่ทำข้อหนึ่งเสร็จ (ข้อนั้นเป็น \"done\" และตั้งข้อถัดไปเป็น \"running\") · ส่งรายการทั้งหมดทุกครั้ง",
            "parameters": {"type": "object", "properties": {
                "steps": {"type": "array", "description": "ลำดับขั้นของงาน", "items": {"type": "object", "properties": {
                    "text": {"type": "string", "description": "ชื่อขั้นสั้น ๆ"},
                    "status": {"type": "string", "enum": ["pending", "running", "done"]}
                }, "required": ["text", "status"]}}
            }, "required": ["steps"]}
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
        }},
        {"type": "function", "function": {
            "name": "run_command",
            "description": "รันคำสั่งตรวจงานในรากโปรเจค (build/test/lint/typecheck) แล้วอ่าน output กับข้อผิดพลาดกลับมาแก้จนผ่าน · รันได้เฉพาะคำสั่งในรายการที่อนุญาตของโปรเจคนี้ (เช่น cargo test, npm run build, pytest) คำสั่งนอกรายการจะถูกบล็อก · ทีละคำสั่งเท่านั้น ห้ามใช้ | & ; > หรือเครื่องหมายต่อคำสั่ง",
            "parameters": {"type": "object", "properties": {
                "command": {"type": "string", "description": "คำสั่งเดียว เช่น \"cargo test\" หรือ \"npm run build\""}
            }, "required": ["command"]}
        }}
    ])
}

fn system_prompt(now: Option<&str>) -> String {
    let today = now.map(|n| format!(" วันนี้คือ {n}.")).unwrap_or_default();
    format!(
        "คุณเป็นผู้ช่วยเขียนโค้ดที่ทำงานในโฟลเดอร์โปรเจคของผู้ใช้โดยตรง{today}\n\
         - เริ่มงานด้วยการวางแผนเป็นขั้น ๆ ผ่าน update_plan (2–6 ข้อ · ข้อแรก status \"running\" ที่เหลือ \"pending\") แล้วอัปเดตแผนทุกครั้งที่ทำแต่ละข้อเสร็จ (ข้อนั้นเป็น \"done\" ตั้งข้อถัดไปเป็น \"running\") เพื่อให้ผู้ใช้เห็นความคืบหน้า\n\
         - สำรวจก่อนด้วย list_dir/read_file/grep ให้เข้าใจโครงสร้างและสไตล์โค้ดเดิม แล้วค่อยแก้ · ไฟล์ยาวใช้ read_file พร้อม offset/limit อ่านเป็นช่วง · ไฟล์เอกสาร (pdf/xlsx/docx) read_file ดึงข้อความให้เอง\n\
         - ไฟล์อ้างอิงที่อยู่นอกโฟลเดอร์โปรเจค (เช่นใน Downloads) อ่านด้วย read_reference_file ใส่พาธเต็ม — ระบบจะถามผู้ใช้ให้อนุญาตก่อน (อ่านอย่างเดียว) · ถ้าผู้ใช้อ้างถึงไฟล์ภายนอก ให้อ่านด้วยเครื่องมือนี้ อย่าเดาเนื้อหา\n\
         - แก้ไฟล์ที่มีอยู่ด้วย edit_file (old ต้องตรงอักขระเป๊ะและมีที่เดียว) · สร้างไฟล์ใหม่หรือเขียนใหม่ทั้งไฟล์ด้วย write_file\n\
         - ทำให้ตรงกับที่ผู้ใช้ขอ แก้เท่าที่จำเป็น รักษาสไตล์เดิม อย่าเดาพาธหรือชื่อที่ยังไม่ได้อ่าน\n\
         - ถ้าเครื่องมือคืน error ให้อ่านสาเหตุแล้วแก้วิธีเรียกใหม่\n\
         - หลังแก้โค้ดแล้ว ให้ตรวจงานด้วย run_command เสมอก่อนบอกว่าเสร็จ: รัน build/typecheck และ test ของโปรเจค (เช่น cargo check/test · npm run build · tsc · pytest) อ่าน error ที่ได้มาแก้แล้วรันซ้ำจนผ่าน · อย่าสรุปว่าเสร็จถ้ายังไม่ได้รันตรวจ\n\
         - ถ้าโปรเจคมี git ใช้ git diff/git status (ดูได้อย่างเดียว) ทบทวนว่าแก้ตรงที่ตั้งใจ ไม่แก้เกินจำเป็น · ห้าม commit/reset/checkout (ถูกบล็อก)\n\
         - รันได้เฉพาะคำสั่งที่อนุญาต ถ้าคำสั่งถูกบล็อกให้ข้ามไป อย่าพยายามเลี่ยง\n\
         - อย่าจบเทิร์นด้วยการประกาศแผน เช่น \"เดี๋ยวจะอ่าน/แก้/ลองต่อ\" — ถ้างานยังไม่เสร็จให้เรียกเครื่องมือทำต่อจนจบในเทิร์นนี้ · ถ้าไฟล์ยาวถูกตัด (ขึ้น \"…(ตัดบางส่วน)\") ให้ใช้ grep หาส่วนที่ต้องแก้แทนการพยายามวัดขนาดไฟล์ · ถ้าติดจริงทำต่อไม่ได้ ให้บอกชัดว่าติดอะไรและทำอะไรสำเร็จไปแล้ว อย่าพิมพ์ขั้นตอนถัดไปเป็นคำตอบ\n\
         - ทำงานได้สูงสุด {MAX_ROUNDS} รอบ พอแก้เสร็จและตรวจผ่านแล้วให้ตอบสรุปสั้น ๆ ว่าแก้อะไรไปบ้างและผลการตรวจ (build/test ผ่านไหม) (ไม่ต้องเรียกเครื่องมือ)"
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
    trim_to(s, RESULT_CHARS)
}

/// Cut a tool result to `cap` characters so a huge file or match list can't blow the model's context.
/// read_file passes a larger cap (READ_RESULT_CHARS) than grep/list: to edit an existing file the model
/// must see it whole, because edit_file needs the exact original text.
fn trim_to(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String = s.chars().take(cap).collect();
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
/// files changed, the model's final summary text (its last turn with no tool calls), and — when the
/// loop ended abnormally without that summary — a short Thai reason why it stopped, so the caller's
/// fallback can state it accurately instead of letting the model guess (e.g. blaming a "silent write
/// failure" when the real cause was a Gateway timeout and nothing was ever written). Never errors:
/// any trouble ends the loop and returns what was done so far.
pub async fn edit_codebase<B: CodeBackend>(
    backend: &B,
    question: &str,
    now: Option<&str>,
    cancel: &CancellationToken,
    mut on_step: impl FnMut(Step),
) -> CodeOutcome {
    let tools = tools_schema();
    let mut messages = initial_messages(question, now);
    let mut changes = Changes::default();
    let mut final_text = String::new();
    let mut stop: Option<String> = None;
    let mut plan: Vec<PlanStep> = Vec::new();
    let start = Instant::now();

    for _ in 0..MAX_ROUNDS {
        if cancel.is_cancelled() {
            break;
        }
        if start.elapsed() > TIME_BUDGET {
            stop = Some("หมดเวลารวมที่กำหนดสำหรับงานนี้".into());
            break;
        }
        let reply = tokio::select! {
            r = backend.complete(&messages, &tools) => match r {
                Ok(r) => r,
                Err(e) => { on_step(step("error", e.code.clone())); stop = Some(stop_reason_for(&e)); break; }
            },
            _ = cancel.cancelled() => break,
        };
        if reply.tool_calls.is_empty() {
            // Only a reply with no tool calls is a genuine final summary. Content that rode along
            // with tool calls is mid-loop narration ("I'll read the file first…") — never deliver it
            // as the answer, or a round that errors out mid-way would surface stale planning text.
            // When the loop instead breaks on error/budget/cancel, final_text stays empty and the
            // caller asks for an honest summary from the files actually changed.
            if !reply.content.trim().is_empty() {
                final_text = reply.content.clone();
            }
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
            // run_command, read_reference and update_plan are async (process / UI prompt / event);
            // the project file tools are sync
            let (result, emitted) = match tc.name.as_str() {
                "run_command" => run_command_tool(backend, &args).await,
                "read_reference_file" => run_reference_tool(backend, &args).await,
                "update_plan" => {
                    plan = parse_plan(&args);
                    backend.update_plan(&plan).await;
                    ("บันทึกแผนแล้ว ทำตามแผนต่อได้เลย และอัปเดต update_plan เมื่อทำแต่ละข้อเสร็จ".to_string(), None)
                }
                _ => run_tool(backend, &tc.name, &args, &mut changes),
            };
            if let Some(s) = emitted {
                on_step(s);
            }
            push(&mut messages, json!({"role": "tool", "tool_call_id": tc.id, "content": result}));
        }
    }
    // ran out of rounds while still calling tools (no clean summary, no earlier stop reason)
    if final_text.trim().is_empty() && stop.is_none() && !cancel.is_cancelled() {
        stop = Some(format!("ทำครบ {MAX_ROUNDS} รอบสูงสุดแล้วแต่งานยังไม่จบ"));
    }
    // mark the plan finished when the turn ended cleanly, so the checklist doesn't stay mid-progress
    if stop.is_none() && !cancel.is_cancelled() {
        for s in &mut plan {
            if s.status != "done" {
                s.status = "done".into();
            }
        }
    }
    CodeOutcome { changes: changes.into_vec(), answer: final_text, stop, plan }
}

/// Parse an `update_plan` tool call into a bounded list of plan steps.
fn parse_plan(args: &Value) -> Vec<PlanStep> {
    args.get("steps")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|it| {
                    let text = it.get("text").and_then(|t| t.as_str())?.trim();
                    if text.is_empty() {
                        return None;
                    }
                    let status = match it.get("status").and_then(|s| s.as_str()) {
                        Some("running") => "running",
                        Some("done") => "done",
                        _ => "pending",
                    };
                    Some(PlanStep { text: text.chars().take(120).collect(), status: status.into() })
                })
                .take(MAX_PLAN_STEPS)
                .collect()
        })
        .unwrap_or_default()
}

/// Run a `read_reference_file` call: the host asks the user to approve the outside path, then reads
/// it read-only. A denial is handed back to the model as text so it can carry on without the file.
async fn run_reference_tool<B: CodeBackend>(backend: &B, args: &Value) -> (String, Option<Step>) {
    let Some(path) = arg(args, "path").map(str::trim).filter(|p| !p.is_empty()) else {
        return ("ต้องระบุ path ของไฟล์อ้างอิง (พาธเต็ม)".into(), None);
    };
    match backend.read_reference(path).await {
        Ok(ReferenceOutcome::Allowed(text)) => (
            format!("เนื้อหาไฟล์อ้างอิง {}:\n{}", path, trim_to(&text, READ_RESULT_CHARS)),
            Some(step("coderead", path.to_string())),
        ),
        Ok(ReferenceOutcome::Denied) => (
            format!("ผู้ใช้ไม่อนุญาตให้อ่าน {path} — ทำงานต่อโดยไม่ใช้ไฟล์นี้ หรือถามผู้ใช้ว่าจะให้ทำอย่างไร"),
            Some(step("codeskip", path.to_string())),
        ),
        Err(e) => (format!("อ่านไฟล์อ้างอิง {} ไม่ได้: {}", path, e.message), Some(step("codeskip", path.to_string()))),
    }
}

/// A short Thai reason for an aborted round, so the fallback summary is honest about the cause. A
/// timeout maps to `gateway_unreachable`, so we say "can't reach / too slow" and rule out a write
/// problem (the loop may have stopped before ever trying to write).
fn stop_reason_for(e: &crate::errors::AppError) -> String {
    if e.is_unreachable() {
        "หยุดกลางคันเพราะต่อ Gateway ไม่ได้ หรือ Gateway ตอบช้าเกินกำหนด — ไม่ใช่ปัญหาการเขียนไฟล์".into()
    } else {
        format!("หยุดกลางคันเพราะ Gateway ขัดข้อง ({})", e.code)
    }
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
            let offset = args.get("offset").and_then(|v| v.as_u64()).map(|n| n as usize).filter(|&n| n > 0);
            let limit = args.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize).filter(|&n| n > 0);
            match backend.read_file(path, offset, limit) {
                Ok(text) => (format!("เนื้อหา {}:\n{}", path, trim_to(&text, READ_RESULT_CHARS)), Some(step("coderead", path.to_string()))),
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

/// Run a `run_command` tool call: the host checks the allowlist and runs it. A blocked command and a
/// failing command both come back as text for the model to act on; the step records the command and
/// (for a command that ran) whether it exited cleanly — never the output.
async fn run_command_tool<B: CodeBackend>(backend: &B, args: &Value) -> (String, Option<Step>) {
    let Some(command) = arg(args, "command").map(str::trim).filter(|c| !c.is_empty()) else {
        return ("ต้องระบุ command".into(), None);
    };
    match backend.run_command(command).await {
        Ok(r) if r.blocked => (r.text, Some(step("codeblocked", command.to_string()))),
        Ok(r) => {
            let mut s = step("coderun", command.to_string());
            s.found = usize::from(r.exit_code == Some(0));
            let head = match r.exit_code {
                Some(0) => "คำสั่งรันเสร็จ (exit 0)".to_string(),
                Some(code) => format!("คำสั่งจบด้วย exit {code}"),
                None => "คำสั่งถูกหยุด".to_string(),
            };
            (format!("{head}\n{}", r.text), Some(s))
        }
        Err(e) => (format!("รันคำสั่งไม่ได้: {}", e.message), Some(step("error", e.code))),
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
        /// scripted run_command outcomes, popped in order (empty → a clean exit-0)
        cmd: RefCell<Vec<RunResult>>,
        /// commands the loop asked to run, in order
        ran: RefCell<Vec<String>>,
        /// whether read_reference approves (true) or denies (false) outside reads
        ref_allow: bool,
        /// the latest plan the loop published
        plan: RefCell<Vec<PlanStep>>,
    }
    impl CodeBackend for Fake {
        async fn complete(&self, _m: &Value, _t: &Value) -> crate::errors::AppResult<Reply> {
            *self.calls.borrow_mut() += 1;
            let r = self.replies.borrow_mut().remove(0);
            // a scripted Gateway-round failure (mirrors complete_tools returning Err). Uses the
            // unreachable/timeout variant — the realistic mid-loop failure when a big file read makes
            // the next round too slow.
            if r.content == "__ERR__" {
                return Err(crate::errors::AppError::unreachable());
            }
            Ok(r)
        }
        fn list_dir(&self, p: &str) -> crate::errors::AppResult<Vec<DirEntry>> {
            self.ws.list(p)
        }
        fn read_file(&self, p: &str, offset: Option<usize>, limit: Option<usize>) -> crate::errors::AppResult<String> {
            self.ws.read(p, offset, limit)
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
        async fn run_command(&self, command: &str) -> crate::errors::AppResult<RunResult> {
            self.ran.borrow_mut().push(command.to_string());
            Ok(self.cmd.borrow_mut().pop_front_or(RunResult { blocked: false, exit_code: Some(0), text: "ok".into() }))
        }
        async fn read_reference(&self, path: &str) -> crate::errors::AppResult<ReferenceOutcome> {
            if self.ref_allow {
                Ok(ReferenceOutcome::Allowed(crate::codefs::read_external(path)?))
            } else {
                Ok(ReferenceOutcome::Denied)
            }
        }
        async fn update_plan(&self, steps: &[PlanStep]) {
            *self.plan.borrow_mut() = steps.to_vec();
        }
    }

    // tiny helper so the Fake can pop the next scripted command result (or a default)
    trait PopFront {
        fn pop_front_or(&mut self, default: RunResult) -> RunResult;
    }
    impl PopFront for Vec<RunResult> {
        fn pop_front_or(&mut self, default: RunResult) -> RunResult {
            if self.is_empty() {
                default
            } else {
                self.remove(0)
            }
        }
    }

    fn fake(replies: Vec<Reply>) -> (tempfile::TempDir, Fake) {
        fake_with_cmds(replies, vec![])
    }
    fn fake_with_cmds(replies: Vec<Reply>, cmd: Vec<RunResult>) -> (tempfile::TempDir, Fake) {
        let (dir, mut f) = fake_base(replies);
        f.cmd = RefCell::new(cmd);
        (dir, f)
    }
    fn fake_base(replies: Vec<Reply>) -> (tempfile::TempDir, Fake) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.py"), "print('hi')\n").unwrap();
        let ws = Workspace::new(&dir.path().to_string_lossy()).unwrap();
        (dir, Fake { ws, replies: RefCell::new(replies), calls: RefCell::new(0), cmd: RefCell::new(Vec::new()), ran: RefCell::new(Vec::new()), ref_allow: false, plan: RefCell::new(Vec::new()) })
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
        let out = edit_codebase(&f, "เปลี่ยน hi เป็น world", None, &rt(), |s| steps.push(s)).await;
        assert_eq!(out.changes.len(), 1);
        assert_eq!(out.changes[0].path, "main.py");
        assert_eq!(out.changes[0].kind, "modify");
        assert_eq!(out.changes[0].before.as_deref(), Some("print('hi')\n"));
        assert_eq!(out.changes[0].after, "print('world')\n");
        assert_eq!(f.ws.read("main.py", None, None).unwrap(), "print('world')\n", "write hit the disk");
        assert_eq!(out.answer, "แก้ให้แล้ว", "the final no-tool reply is the summary");
        assert_eq!(out.stop, None, "a clean finish has no stop reason");
        assert_eq!(steps.last().unwrap().kind, "done");
    }

    #[tokio::test]
    async fn creates_a_new_file() {
        let (_d, f) = fake(vec![
            Reply { content: String::new(), tool_calls: vec![tool("write_file", r#"{"path":"a/b.txt","content":"ok\n"}"#)] },
            Reply { content: "สร้างแล้ว".into(), tool_calls: vec![] },
        ]);
        let out = edit_codebase(&f, "สร้างไฟล์", None, &rt(), |_| {}).await;
        assert_eq!(out.changes.len(), 1);
        assert_eq!(out.changes[0].kind, "create");
        assert_eq!(out.changes[0].before, None);
        assert_eq!(f.ws.read("a/b.txt", None, None).unwrap(), "ok\n");
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
        let out = edit_codebase(&f, "แก้", None, &rt(), |_| {}).await;
        assert_eq!(out.changes.len(), 1, "one file changed, the failed attempt left nothing");
        assert_eq!(out.changes[0].before.as_deref(), Some("print('hi')\n"));
        assert_eq!(out.changes[0].after, "print('world')\n");
    }

    #[tokio::test]
    async fn mid_loop_narration_is_not_delivered_when_a_round_errors() {
        // round 1: reasoning text rides along with a tool call; round 2: the Gateway round fails.
        // The narration must NOT come back as the answer (the caller then builds an honest summary).
        let (_d, f) = fake(vec![
            Reply { content: "ผู้ใช้อยากได้ X ผมจะอ่านไฟล์ก่อน".into(), tool_calls: vec![tool("read_file", r#"{"path":"main.py"}"#)] },
            Reply { content: "__ERR__".into(), tool_calls: vec![] },
        ]);
        let mut steps = Vec::new();
        let out = edit_codebase(&f, "ทำ X", None, &rt(), |s| steps.push(s)).await;
        assert!(out.changes.is_empty(), "nothing was written");
        assert_eq!(out.answer, "", "narration that accompanied a tool call is not a final summary");
        assert!(steps.iter().any(|s| s.kind == "error"), "the failed round shows as an error step");
        let reason = out.stop.expect("an aborted round reports why it stopped");
        assert!(reason.contains("ไม่ใช่ปัญหาการเขียนไฟล์"), "the reason rules out a write failure so the fallback won't invent one");
    }

    #[tokio::test]
    async fn bad_tool_json_does_not_crash() {
        let (_d, f) = fake(vec![
            Reply { content: String::new(), tool_calls: vec![tool("edit_file", "not json")] },
            Reply { content: "จบ".into(), tool_calls: vec![] },
        ]);
        let out = edit_codebase(&f, "q", None, &rt(), |_| {}).await;
        assert!(out.changes.is_empty());
    }

    #[tokio::test]
    async fn runs_a_command_and_feeds_the_output_back() {
        let (_d, f) = fake_with_cmds(
            vec![
                Reply { content: String::new(), tool_calls: vec![tool("run_command", r#"{"command":"pytest -q"}"#)] },
                Reply { content: "เทสต์ผ่านแล้ว".into(), tool_calls: vec![] },
            ],
            vec![RunResult { blocked: false, exit_code: Some(0), text: "2 passed".into() }],
        );
        let mut steps = Vec::new();
        let out = edit_codebase(&f, "รันเทสต์", None, &rt(), |s| steps.push(s)).await;
        assert!(out.changes.is_empty(), "running a command changes no files");
        assert_eq!(f.ran.borrow().as_slice(), ["pytest -q"]);
        let run_step = steps.iter().find(|s| s.kind == "coderun").expect("a coderun step was emitted");
        assert_eq!(run_step.detail, "pytest -q");
        assert_eq!(run_step.found, 1, "exit 0 → marked ok");
        assert_eq!(out.answer, "เทสต์ผ่านแล้ว");
    }

    #[tokio::test]
    async fn a_blocked_command_is_reported_not_run() {
        let (_d, f) = fake_with_cmds(
            vec![
                Reply { content: String::new(), tool_calls: vec![tool("run_command", r#"{"command":"rm -rf ."}"#)] },
                Reply { content: "ข้ามคำสั่งนั้น".into(), tool_calls: vec![] },
            ],
            vec![RunResult { blocked: true, exit_code: None, text: "คำสั่งนี้ไม่อยู่ในรายการที่อนุญาต".into() }],
        );
        let mut steps = Vec::new();
        edit_codebase(&f, "ลบทิ้ง", None, &rt(), |s| steps.push(s)).await;
        assert!(steps.iter().any(|s| s.kind == "codeblocked" && s.detail == "rm -rf ."));
        assert!(!steps.iter().any(|s| s.kind == "coderun"), "a blocked command never becomes a coderun");
    }

    #[tokio::test]
    async fn reads_an_external_reference_when_allowed() {
        let outside = tempfile::tempdir().unwrap();
        let ref_path = outside.path().join("ref.txt");
        fs::write(&ref_path, "REFERENCE CONTENT\n").unwrap();
        let (_d, mut f) = fake_base(vec![
            Reply { content: String::new(), tool_calls: vec![tool("read_reference_file", &format!(r#"{{"path":{:?}}}"#, ref_path.to_string_lossy()))] },
            Reply { content: "ใช้ไฟล์อ้างอิงแล้ว".into(), tool_calls: vec![] },
        ]);
        f.ref_allow = true;
        let mut steps = Vec::new();
        let out = edit_codebase(&f, "อ่านไฟล์อ้างอิง", None, &rt(), |s| steps.push(s)).await;
        assert_eq!(out.answer, "ใช้ไฟล์อ้างอิงแล้ว");
        assert!(steps.iter().any(|s| s.kind == "coderead"), "an allowed reference read shows as coderead");
    }

    #[tokio::test]
    async fn a_denied_reference_is_handed_back_not_fatal() {
        let (_d, f) = fake_base(vec![
            // ref_allow defaults to false → the host denies it
            Reply { content: String::new(), tool_calls: vec![tool("read_reference_file", r#"{"path":"C:\\somewhere\\secret.xlsx"}"#)] },
            Reply { content: "ทำต่อโดยไม่ใช้ไฟล์นั้น".into(), tool_calls: vec![] },
        ]);
        let mut steps = Vec::new();
        let out = edit_codebase(&f, "q", None, &rt(), |s| steps.push(s)).await;
        assert_eq!(out.answer, "ทำต่อโดยไม่ใช้ไฟล์นั้น");
        assert!(steps.iter().any(|s| s.kind == "codeskip"), "a denied read shows as codeskip and the loop continues");
    }

    #[tokio::test]
    async fn publishes_and_finalizes_the_plan() {
        let (_d, f) = fake(vec![
            Reply { content: String::new(), tool_calls: vec![tool("update_plan", r#"{"steps":[{"text":"อ่านโค้ด","status":"running"},{"text":"แก้","status":"pending"}]}"#)] },
            Reply { content: "เสร็จ".into(), tool_calls: vec![] },
        ]);
        let out = edit_codebase(&f, "ทำงาน", None, &rt(), |_| {}).await;
        assert_eq!(out.plan.len(), 2);
        assert_eq!(f.plan.borrow().len(), 2, "the host received the plan live");
        assert!(out.plan.iter().all(|s| s.status == "done"), "a clean finish completes the checklist");
    }

    #[tokio::test]
    async fn stops_at_the_round_limit() {
        let always = || Reply { content: String::new(), tool_calls: vec![tool("read_file", r#"{"path":"main.py"}"#)] };
        let (_d, f) = fake((0..MAX_ROUNDS + 4).map(|_| always()).collect());
        let out = edit_codebase(&f, "q", None, &rt(), |_| {}).await;
        assert_eq!(*f.calls.borrow(), MAX_ROUNDS);
        assert_eq!(out.answer, "", "no clean summary when it ran out of rounds");
        assert!(out.stop.expect("round-cap reports a stop reason").contains("รอบสูงสุด"));
    }
}
