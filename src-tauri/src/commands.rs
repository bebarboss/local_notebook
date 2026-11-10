//! Tauri commands — the only surface the frontend can reach.
//! Gateway calls, file access and the API key live exclusively on this side.

use crate::chats::{ChatDb, Chat, Message, NewMessage, SourceRef};
use crate::config::{self, GatewayConfig, GatewayInfo, ModelConfig, Settings};
use crate::embed::{self, DownloadProgress, Embedder, EmbeddingStatus, ModelId};
use crate::errors::{AppError, AppResult};
use crate::gateway_test::{self, TestResult};
use crate::ingest::watcher::Watchers;
use crate::ingest::{is_supported, IndexManager};
use crate::llm::{self, ChatMessage, StreamEnd};
use crate::secrets;
use crate::services::{now_ms, NewService, Service, ServiceRegistry, ServiceStats};
use crate::store::{DocRow, Stores, VectorStore};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub state: String, // unknown | ok | unreachable | error
    pub message: Option<String>,
    pub checked_at: i64,
}

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub gateway: Mutex<GatewayConfig>,
    pub models: Mutex<Vec<ModelConfig>>,
    pub services: Mutex<ServiceRegistry>,
    pub stores: Stores,
    pub chats: Mutex<ChatDb>,
    pub embedder: Arc<Embedder>,
    pub indexer: Arc<IndexManager>,
    pub watchers: Arc<Watchers>,
    pub streams: Mutex<HashMap<String, CancellationToken>>,
    pub status: Mutex<GatewayStatus>,
}

impl AppState {
    pub fn new(app: &AppHandle) -> AppResult<Self> {
        std::fs::create_dir_all(config::data_dir())?;
        let settings = Settings::load().sanitize();
        let gateway = GatewayConfig::load();
        let models = config::load_models(&gateway.base());
        let chats = ChatDb::open(&config::data_dir().join("chats.db"))?;
        let _ = chats.purge_older_than(settings.auto_delete_days);
        let indexer = Arc::new(IndexManager::default());
        Ok(Self {
            settings: Mutex::new(settings),
            gateway: Mutex::new(gateway),
            models: Mutex::new(models),
            services: Mutex::new(ServiceRegistry::load()),
            stores: Stores::default(),
            chats: Mutex::new(chats),
            embedder: Arc::new(Embedder::default()),
            watchers: Watchers::new(app.clone(), indexer.clone()),
            indexer,
            streams: Mutex::new(HashMap::new()),
            status: Mutex::new(GatewayStatus { state: "unknown".into(), message: None, checked_at: 0 }),
        })
    }

    fn embedding_model(&self) -> AppResult<ModelId> {
        ModelId::parse(&self.settings.lock().embedding_model)
    }
}

type S<'a> = State<'a, AppState>;

// ------------------------------------------------------------------ app / settings

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    data_dir: String,
    mock: bool,
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
    AppInfo { version: app.package_info().version.to_string(), data_dir: config::data_dir().to_string_lossy().into_owned(), mock: false }
}

#[tauri::command]
pub fn get_settings(state: S<'_>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
pub fn save_settings(state: S<'_>, settings: Settings) -> AppResult<Settings> {
    let s = settings.sanitize();
    s.save()?;
    let purge = s.auto_delete_days;
    *state.settings.lock() = s.clone();
    let _ = state.chats.lock().purge_older_than(purge);
    Ok(s)
}

// ------------------------------------------------------------------ gateway + key

fn gateway_info(state: &AppState) -> GatewayInfo {
    let g = state.gateway.lock().clone();
    let (has_key, last4) = secrets::key_status();
    GatewayInfo {
        group_name: config::GROUP_NAME.into(),
        display_name: config::GROUP_NAME.into(),
        url: g.base(),
        default_url: config::DEFAULT_GATEWAY_URL.into(),
        api_type: config::API_TYPE.into(),
        has_key,
        key_last4: last4,
        last_test_at: g.last_test_at,
        last_test_passed: g.last_test_passed,
        last_test_total: g.last_test_total,
    }
}

#[tauri::command]
pub fn get_gateway(state: S<'_>) -> GatewayInfo {
    gateway_info(&state)
}

#[tauri::command]
pub fn set_gateway_url(state: S<'_>, url: String) -> AppResult<GatewayInfo> {
    let new_base = config::validate_gateway_url(&url)?;
    {
        let mut g = state.gateway.lock();
        let old = g.base();
        g.url = new_base.clone();
        g.save()?;
        let mut m = state.models.lock();
        config::rebase_models(&mut m, &old, &new_base);
        config::save_models(&m)?;
    }
    Ok(gateway_info(&state))
}

#[tauri::command]
pub fn reset_gateway_url(state: S<'_>) -> AppResult<GatewayInfo> {
    set_gateway_url(state, config::DEFAULT_GATEWAY_URL.into())
}

#[tauri::command]
pub fn save_api_key(state: S<'_>, key: String) -> AppResult<GatewayInfo> {
    secrets::set_key(&key)?;
    drop(key);
    Ok(gateway_info(&state))
}

#[tauri::command]
pub fn remove_provider(state: S<'_>) -> AppResult<()> {
    secrets::delete_key()?;
    let g = GatewayConfig::default();
    g.save()?;
    let models = config::default_models(&g.base());
    config::save_models(&models)?;
    *state.gateway.lock() = g;
    *state.models.lock() = models;
    let mut s = state.settings.lock();
    s.setup_done = false;
    s.save()?;
    *state.status.lock() = GatewayStatus { state: "unknown".into(), message: None, checked_at: now_ms() };
    Ok(())
}

async fn do_check(app: &AppHandle) -> GatewayStatus {
    let state = app.state::<AppState>();
    let base = state.gateway.lock().base();
    let st = match secrets::get_key() {
        Ok(Some(key)) => match llm::list_models(&base, &key).await {
            Ok(_) => GatewayStatus { state: "ok".into(), message: None, checked_at: now_ms() },
            Err(e) if e.is_unreachable() => GatewayStatus { state: "unreachable".into(), message: Some(e.message), checked_at: now_ms() },
            Err(e) => GatewayStatus { state: "error".into(), message: Some(e.message), checked_at: now_ms() },
        },
        Ok(None) => GatewayStatus { state: "error".into(), message: Some(AppError::no_key().message), checked_at: now_ms() },
        Err(e) => GatewayStatus { state: "error".into(), message: Some(e.message), checked_at: now_ms() },
    };
    *state.status.lock() = st.clone();
    let _ = app.emit("gateway-status", st.clone());
    st
}

fn set_status(app: &AppHandle, state: &str, message: Option<String>) {
    let st = GatewayStatus { state: state.into(), message, checked_at: now_ms() };
    *app.state::<AppState>().status.lock() = st.clone();
    let _ = app.emit("gateway-status", st);
}

#[tauri::command]
pub async fn check_gateway(app: AppHandle) -> AppResult<GatewayStatus> {
    Ok(do_check(&app).await)
}

#[tauri::command]
pub async fn gateway_status(app: AppHandle) -> AppResult<GatewayStatus> {
    let cached = app.state::<AppState>().status.lock().clone();
    if now_ms() - cached.checked_at < 25_000 {
        return Ok(cached);
    }
    Ok(do_check(&app).await)
}

// ------------------------------------------------------------------ models

#[tauri::command]
pub fn list_models(state: S<'_>) -> Vec<ModelConfig> {
    state.models.lock().clone()
}

#[tauri::command]
pub fn save_models(state: S<'_>, models: Vec<ModelConfig>) -> AppResult<Vec<ModelConfig>> {
    let models: Vec<ModelConfig> = models.into_iter().filter(|m| !m.id.trim().is_empty()).collect();
    if models.is_empty() {
        return Err(AppError::new("invalid", "ต้องมีอย่างน้อย 1 โมเดล"));
    }
    config::save_models(&models)?;
    *state.models.lock() = models.clone();
    Ok(models)
}

#[tauri::command]
pub fn reset_models(state: S<'_>) -> AppResult<Vec<ModelConfig>> {
    let m = config::default_models(&state.gateway.lock().base());
    save_models(state, m)
}

#[tauri::command]
pub fn import_vscode_models(state: S<'_>, path: String) -> AppResult<Vec<ModelConfig>> {
    let base = state.gateway.lock().base();
    let m = config::import_vscode_models(Path::new(&path), &base)?;
    save_models(state, m)
}

#[tauri::command]
pub async fn test_models(app: AppHandle, models: Option<Vec<String>>) -> AppResult<Vec<TestResult>> {
    let key = secrets::require_key()?;
    let list: Vec<ModelConfig> = {
        let state = app.state::<AppState>();
        let all = state.models.lock().clone();
        all.into_iter().filter(|m| m.enabled && models.as_ref().map_or(true, |only| only.contains(&m.id))).collect()
    };
    let mut results = Vec::new();
    for m in &list {
        let r = gateway_test::test_model(m, &key).await;
        let _ = app.emit("gateway-test-result", r.clone());
        let stop = r.unreachable; // VPN down: no point trying the rest
        results.push(r);
        if stop {
            for rest in list.iter().skip(results.len()) {
                let r = TestResult { model: rest.id.clone(), ok: false, reply: None, error: Some(crate::errors::MSG_UNREACHABLE.into()), status: None, elapsed_ms: 0, unreachable: true };
                let _ = app.emit("gateway-test-result", r.clone());
                results.push(r);
            }
            break;
        }
    }
    let state = app.state::<AppState>();
    {
        let mut ms = state.models.lock();
        for m in ms.iter_mut() {
            if let Some(r) = results.iter().find(|r| r.model == m.id) {
                // VPN failures don't hide models — only real per-model failures do
                if !r.unreachable {
                    m.hidden = !r.ok;
                }
            }
        }
        config::save_models(&ms)?;
    }
    {
        let mut g = state.gateway.lock();
        g.last_test_at = Some(now_ms());
        g.last_test_passed = Some(results.iter().filter(|r| r.ok).count() as u32);
        g.last_test_total = Some(results.len() as u32);
        g.save()?;
    }
    if results.iter().any(|r| r.ok) {
        set_status(&app, "ok", None);
    } else if results.iter().all(|r| r.unreachable) && !results.is_empty() {
        set_status(&app, "unreachable", Some(crate::errors::MSG_UNREACHABLE.into()));
    }
    Ok(results)
}

// ------------------------------------------------------------------ services + documents

#[tauri::command]
pub fn list_services(state: S<'_>) -> Vec<Service> {
    state.services.lock().list()
}

#[tauri::command]
pub fn service_stats(state: S<'_>) -> AppResult<Vec<ServiceStats>> {
    let list = state.services.lock().list();
    let mut out = Vec::new();
    for s in list {
        let store = state.stores.get(&s.id)?;
        let (documents, chunks, bytes, skipped, pending) = store.lock().stats()?;
        out.push(ServiceStats {
            service_id: s.id.clone(),
            documents,
            chunks,
            bytes,
            skipped,
            indexing: pending,
            last_indexed_at: Some(s.updated_at),
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn create_service(app: AppHandle, state: S<'_>, service: NewService) -> AppResult<Service> {
    let s = state.services.lock().create(service)?;
    state.watchers.watch(&s);
    state.indexer.start(app, s.id.clone(), false);
    Ok(s)
}

#[tauri::command]
pub fn update_service(app: AppHandle, state: S<'_>, service: Service) -> AppResult<Service> {
    let before = state.services.lock().get(&service.id)?;
    let s = state.services.lock().update(service)?;
    if before.folders != s.folders || before.files != s.files {
        state.watchers.watch(&s);
        state.indexer.start(app, s.id.clone(), false);
    }
    Ok(s)
}

#[tauri::command]
pub fn delete_service(state: S<'_>, id: String, delete_chats: bool) -> AppResult<()> {
    state.indexer.cancel(&id);
    state.watchers.unwatch(&id);
    state.stores.close(&id);
    // the indexer thread may still hold the store briefly; retry the folder removal
    let mut last = Ok(());
    for _ in 0..10 {
        last = state.services.lock().delete(&id);
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    last?;
    if delete_chats {
        state.chats.lock().delete_by_service(&id)?;
    }
    Ok(())
}

#[tauri::command]
pub fn list_documents(state: S<'_>, service_id: String) -> AppResult<Vec<DocRow>> {
    let store = state.stores.get(&service_id)?;
    let docs = store.lock().list_documents()?;
    Ok(docs)
}

#[tauri::command]
pub fn add_documents(app: AppHandle, state: S<'_>, service_id: String, paths: Vec<String>) -> AppResult<()> {
    let ok: Vec<String> = paths.into_iter().filter(|p| is_supported(Path::new(p)) && Path::new(p).is_file()).collect();
    if ok.is_empty() {
        return Err(AppError::new("unsupported", "รองรับเฉพาะไฟล์ .pdf .docx .txt .md"));
    }
    state.services.lock().add_files(&service_id, &ok)?;
    state.indexer.start(app, service_id, false);
    Ok(())
}

#[tauri::command]
pub fn remove_document(app: AppHandle, state: S<'_>, service_id: String, doc_id: i64) -> AppResult<()> {
    let store = state.stores.get(&service_id)?;
    let path = store.lock().list_documents()?.into_iter().find(|d| d.id == doc_id).map(|d| d.path);
    if let Some(p) = path {
        state.services.lock().remove_file(&service_id, &p)?;
        store.lock().delete_document(&p)?;
        state.indexer.start(app, service_id, false);
    }
    Ok(())
}

#[tauri::command]
pub fn reindex_service(app: AppHandle, state: S<'_>, service_id: String, full: bool) -> AppResult<()> {
    state.services.lock().get(&service_id)?;
    state.indexer.start(app, service_id, full);
    Ok(())
}

#[tauri::command]
pub fn reindex_all(app: AppHandle, state: S<'_>) -> AppResult<()> {
    for s in state.services.lock().list() {
        state.indexer.start(app.clone(), s.id, true);
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_index(state: S<'_>, service_id: String) {
    state.indexer.cancel(&service_id);
}

#[tauri::command]
pub fn export_service(state: S<'_>, service_id: String, dest_dir: String) -> AppResult<String> {
    let svc = state.services.lock().get(&service_id)?;
    let dir = PathBuf::from(dest_dir);
    let base = format!("DocAsk-{}-{}", svc.code, svc.id);
    let db = dir.join(format!("{base}.db"));
    state.stores.get(&service_id)?.lock().export_to(&db)?;
    config::write_json(&dir.join(format!("{base}.json")), &svc)?;
    Ok(db.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------ chats

#[tauri::command]
pub fn list_chats(state: S<'_>, service_id: Option<String>, query: String) -> AppResult<Vec<Chat>> {
    state.chats.lock().list_chats(service_id.as_deref(), &query)
}

#[tauri::command]
pub fn get_chat(state: S<'_>, chat_id: String) -> AppResult<Chat> {
    state.chats.lock().get_chat(&chat_id)
}

#[tauri::command]
pub fn get_messages(state: S<'_>, chat_id: String) -> AppResult<Vec<Message>> {
    state.chats.lock().messages(&chat_id)
}

#[tauri::command]
pub fn rename_chat(state: S<'_>, chat_id: String, title: String) -> AppResult<Chat> {
    state.chats.lock().rename(&chat_id, &title)
}

#[tauri::command]
pub fn delete_chat(state: S<'_>, chat_id: String) -> AppResult<()> {
    state.chats.lock().delete(&chat_id)
}

#[tauri::command]
pub fn delete_all_chats(state: S<'_>) -> AppResult<()> {
    state.chats.lock().delete_all()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskRequest {
    pub chat_id: Option<String>,
    pub service_id: String,
    pub question: String,
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub all_services: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskStarted {
    request_id: String,
    chat_id: String,
    user_message: Message,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatEvent<T: Serialize + Clone> {
    request_id: String,
    chat_id: String,
    #[serde(flatten)]
    data: T,
}

#[derive(Serialize, Clone)]
struct Delta {
    delta: String,
}
#[derive(Serialize, Clone)]
struct Sources {
    sources: Vec<SourceRef>,
}
#[derive(Serialize, Clone)]
struct Done {
    message: Message,
}
#[derive(Serialize, Clone)]
struct Failed {
    error: AppError,
}

#[tauri::command]
pub async fn ask(app: AppHandle, state: S<'_>, req: AskRequest) -> AppResult<AskStarted> {
    let question = req.question.trim().to_string();
    if question.is_empty() {
        return Err(AppError::new("invalid", "กรุณาพิมพ์คำถาม"));
    }
    let key = secrets::require_key()?;
    let model = state
        .models
        .lock()
        .iter()
        .find(|m| m.id == req.model && m.enabled)
        .cloned()
        .ok_or_else(|| AppError::new("no_model", "ไม่พบโมเดลนี้หรือถูกปิดไว้ — ตรวจในหมวดโมเดล"))?;
    let svc = state.services.lock().get(&req.service_id)?;
    let turns = state.settings.lock().history_turns;

    let (chat, history, user_message) = {
        let db = state.chats.lock();
        let chat = match &req.chat_id {
            Some(id) => {
                let c = db.get_chat(id)?;
                if c.service_id != svc.id {
                    return Err(AppError::new("invalid", "แชตนี้ผูกกับ service อื่น"));
                }
                c
            }
            None => db.create_chat(&svc.id, &question)?,
        };
        // history first (text only), then the new question
        let history = db.recent_turns(&chat.id, turns)?;
        let um = db.add_message(NewMessage {
            chat_id: &chat.id, role: "user", content: &question, model: None, reasoning_effort: None,
            sources: &[], elapsed_ms: None, error: None,
        })?;
        (chat, history, um)
    };

    let request_id = uuid::Uuid::new_v4().to_string();
    let token = CancellationToken::new();
    state.streams.lock().insert(request_id.clone(), token.clone());

    let job = AskJob { app: app.clone(), request_id: request_id.clone(), chat_id: chat.id.clone(), svc, model, effort: req.effort, all: req.all_services, question, history, key, token };
    tauri::async_runtime::spawn(job.run());

    Ok(AskStarted { request_id, chat_id: chat.id, user_message })
}

#[tauri::command]
pub fn stop_generation(state: S<'_>, request_id: String) {
    if let Some(t) = state.streams.lock().get(&request_id) {
        t.cancel();
    }
}

struct AskJob {
    app: AppHandle,
    request_id: String,
    chat_id: String,
    svc: Service,
    model: ModelConfig,
    effort: String,
    all: bool,
    question: String,
    history: Vec<(String, String)>,
    key: String,
    token: CancellationToken,
}

impl AskJob {
    fn emit<T: Serialize + Clone>(&self, event: &str, data: T) {
        let _ = self.app.emit(event, ChatEvent { request_id: self.request_id.clone(), chat_id: self.chat_id.clone(), data });
    }

    fn save(&self, content: &str, sources: &[SourceRef], elapsed: i64, error: Option<&str>) -> Option<Message> {
        let state = self.app.state::<AppState>();
        let db = state.chats.lock();
        db.add_message(NewMessage {
            chat_id: &self.chat_id,
            role: "assistant",
            content,
            model: Some(&self.model.id),
            reasoning_effort: Some(&self.effort),
            sources,
            elapsed_ms: Some(elapsed),
            error,
        })
        .ok()
    }

    fn fail(&self, e: AppError, sources: &[SourceRef], elapsed: i64) {
        if e.is_unreachable() {
            set_status(&self.app, "unreachable", Some(e.message.clone()));
        }
        self.save("", sources, elapsed, Some(&e.message));
        self.emit("chat-error", Failed { error: e });
    }

    async fn run(self) {
        let started = Instant::now();
        let elapsed = || started.elapsed().as_millis() as i64;

        let sources = match retrieve(&self.app, &self.svc, self.all, &self.question).await {
            Ok(s) => s,
            Err(e) => {
                self.fail(e, &[], elapsed());
                self.cleanup();
                return;
            }
        };
        self.emit("chat-sources", Sources { sources: sources.clone() });

        let messages = build_prompt(&self.svc, &sources, &self.history, &self.question, self.all);
        let effort = (self.model.reasoning_effort_format == "chat-completions"
            && self.model.supports_reasoning_effort.iter().any(|e| e == &self.effort))
            .then_some(self.effort.as_str());
        let req = llm::Request { url: &self.model.url, key: &self.key, model: &self.model.id, effort, max_tokens: self.model.max_output_tokens, messages: &messages };

        let result = llm::stream(&req, &self.token, |d| self.emit("chat-delta", Delta { delta: d.to_string() })).await;
        match result {
            Ok(StreamEnd::Finished(text)) => {
                set_status(&self.app, "ok", None);
                let text = if text.trim().is_empty() { "(Gateway ไม่ได้ส่งคำตอบกลับมา)".to_string() } else { text };
                if let Some(m) = self.save(&text, &sources, elapsed(), None) {
                    self.emit("chat-done", Done { message: m });
                }
            }
            Ok(StreamEnd::Cancelled(text)) => {
                if let Some(m) = self.save(&text, &sources, elapsed(), Some("หยุดโดยผู้ใช้")) {
                    self.emit("chat-done", Done { message: m });
                }
            }
            Err(e) => self.fail(e, &sources, elapsed()),
        }
        self.cleanup();
    }

    fn cleanup(&self) {
        self.app.state::<AppState>().streams.lock().remove(&self.request_id);
    }
}

/// Embed the question locally and search the service's index (or all services).
async fn retrieve(app: &AppHandle, svc: &Service, all: bool, question: &str) -> AppResult<Vec<SourceRef>> {
    let state = app.state::<AppState>();
    let model = state.embedding_model()?;
    let threshold = state.settings.lock().threshold;
    let embedder = state.embedder.clone();
    let q = question.to_string();
    let qv = tauri::async_runtime::spawn_blocking(move || embedder.embed_query(model, &q))
        .await
        .map_err(|e| AppError::new("embedding", format!("ทำ embedding ไม่สำเร็จ: {e}")))??;

    let targets: Vec<Service> = if all { state.services.lock().list() } else { vec![svc.clone()] };
    let k = svc.top_k as usize;
    let mut hits = Vec::new();
    for s in targets {
        let store = state.stores.get(&s.id)?;
        let found = store.lock().search(&qv, k, threshold)?;
        hits.extend(found.into_iter().map(|h| (s.clone(), h)));
    }
    hits.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    hits.truncate(k);
    Ok(hits
        .into_iter()
        .enumerate()
        .map(|(i, (s, h))| SourceRef {
            n: i + 1,
            service_id: s.id,
            service_name: s.name,
            file_name: Path::new(&h.doc_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            doc_path: h.doc_path,
            page: h.page,
            ord: h.ord,
            score: h.score,
            text: h.text,
        })
        .collect())
}

pub fn build_prompt(svc: &Service, sources: &[SourceRef], history: &[(String, String)], question: &str, all: bool) -> Vec<ChatMessage> {
    let mut system = String::from(
        "คุณคือผู้ช่วยตอบคำถามจากเอกสารภายในองค์กร\n\
         - ตอบเป็นภาษาเดียวกับคำถาม\n\
         - ใช้เฉพาะข้อมูลจาก \"เอกสารที่เกี่ยวข้อง\" ในข้อความล่าสุดเท่านั้น ห้ามใช้ความรู้ภายนอก\n\
         - ถ้าเอกสารไม่มีคำตอบ ให้บอกตรงๆ ว่าไม่พบข้อมูลนี้ในเอกสาร\n\
         - อ้างอิงแหล่งด้วย [1], [2] ตามหมายเลขท่อนเอกสาร ต่อท้ายข้อความที่ใช้ข้อมูลนั้น และห้ามสร้างหมายเลขที่ไม่มีอยู่\n\
         - ใช้ **ตัวหนา** กับคำสำคัญได้ ตอบกระชับ",
    );
    if !svc.system_prompt.trim().is_empty() {
        system.push_str("\n\nคำแนะนำเพิ่มเติมของ service นี้:\n");
        system.push_str(svc.system_prompt.trim());
    }
    let mut msgs = vec![ChatMessage::new("system", system)];
    for (q, a) in history {
        msgs.push(ChatMessage::new("user", q.clone()));
        msgs.push(ChatMessage::new("assistant", a.clone()));
    }
    let mut user = format!("คำถาม: {question}\n\nเอกสารที่เกี่ยวข้อง:\n");
    if sources.is_empty() {
        user.push_str("(ไม่พบท่อนเอกสารที่เกี่ยวข้อง)\n");
    }
    for s in sources {
        let page = s.page.map(|p| format!(" · หน้า {p}")).unwrap_or_default();
        let from = if all { format!(" · service {}", s.service_name) } else { String::new() };
        user.push_str(&format!("\n[{}] {}{}{}\n{}\n", s.n, s.file_name, page, from, s.text));
    }
    msgs.push(ChatMessage::new("user", user));
    msgs
}

#[tauri::command]
pub fn open_source_file(path: String) -> AppResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_file() || !is_supported(&p) {
        return Err(AppError::new("not_found", "ไม่พบไฟล์ต้นฉบับ (อาจถูกย้ายหรือลบ)"));
    }
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(&p).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&p).spawn();
    r.map(|_| ()).map_err(|e| AppError::new("open", format!("เปิดไฟล์ไม่ได้: {e}")))
}

// ------------------------------------------------------------------ embedding

#[tauri::command]
pub fn embedding_status(state: S<'_>) -> AppResult<EmbeddingStatus> {
    Ok(Embedder::status(state.embedding_model()?))
}

#[tauri::command]
pub async fn download_embedding(app: AppHandle, model: String) -> AppResult<()> {
    let id = ModelId::parse(&model)?;
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        let res = embed::download(id, |p: DownloadProgress| {
            let _ = a.emit("embed-download", p);
        })
        .await;
        match res {
            Ok(_) => {
                let st = a.state::<AppState>();
                st.embedder.unload();
                // documents waiting for a model can now be indexed
                for s in st.services.lock().list() {
                    st.indexer.start(a.clone(), s.id, false);
                }
            }
            Err(e) => {
                let _ = a.emit("embed-download", DownloadProgress { model: id.as_str().into(), file: String::new(), downloaded: 0, total: None, done: false, error: Some(e.message) });
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn use_embedding_folder(app: AppHandle, state: S<'_>, model: String, folder: String) -> AppResult<EmbeddingStatus> {
    let id = ModelId::parse(&model)?;
    let st = state.embedder.use_folder(id, Path::new(&folder))?;
    for s in state.services.lock().list() {
        state.indexer.start(app.clone(), s.id, false);
    }
    Ok(st)
}

// ------------------------------------------------------------------ wipe

#[tauri::command]
pub fn wipe_all_data(state: S<'_>) -> AppResult<()> {
    state.indexer.cancel_all();
    state.watchers.clear();
    state.stores.close_all();
    state.embedder.unload();
    for t in state.streams.lock().values() {
        t.cancel();
    }
    state.chats.lock().delete_all()?;
    state.services.lock().clear();
    secrets::delete_key()?;
    let dir = config::data_dir();
    std::thread::sleep(std::time::Duration::from_millis(300));
    for sub in ["services", "models"] {
        let p = dir.join(sub);
        if p.exists() {
            let _ = std::fs::remove_dir_all(p);
        }
    }
    for f in ["settings.json", "gateway.json", "models.json", "services.json", "embedding.json"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    *state.settings.lock() = Settings::default();
    *state.gateway.lock() = GatewayConfig::default();
    *state.models.lock() = config::default_models(config::DEFAULT_GATEWAY_URL);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_has_numbered_sources_and_no_old_chunks() {
        let svc = Service {
            id: "hr".into(), name: "HR".into(), code: "HR".into(), color: "#1F4FD1".into(), folders: vec![], files: vec![],
            model: "godev1".into(), effort: "medium".into(), top_k: 5, system_prompt: "ตอบสั้น".into(), created_at: 0, updated_at: 0,
        };
        let src = vec![SourceRef { n: 1, service_id: "hr".into(), service_name: "HR".into(), doc_path: "a.pdf".into(), file_name: "a.pdf".into(), page: Some(4), ord: 0, score: 0.9, text: "ลา 10 วัน".into() }];
        let hist = vec![("q0".to_string(), "a0".to_string())];
        let m = build_prompt(&svc, &src, &hist, "ลาได้กี่วัน", false);
        assert_eq!(m.len(), 4);
        assert!(m[0].content.contains("ตอบสั้น"));
        assert_eq!(m[1].content, "q0");
        assert!(m[3].content.contains("[1] a.pdf · หน้า 4"));
    }
}
