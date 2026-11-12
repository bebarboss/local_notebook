//! Tauri commands — the only surface the frontend can reach.
//! Gateway calls, file access and the API key live exclusively on this side.

use crate::chats::{ChatDb, Chat, Message, NewMessage, SourceRef};
use crate::config::{self, GatewayConfig, GatewayInfo, ModelConfig, Settings};
use crate::embed::{self, DownloadProgress, Embedder, EmbeddingStatus, ModelId, Source};
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
    pub embedder: Arc<Embedder>,
    pub settings: Mutex<Settings>,
    pub gateway: Mutex<GatewayConfig>,
    pub models: Mutex<Vec<ModelConfig>>,
    pub services: Mutex<ServiceRegistry>,
    pub stores: Stores,
    pub chats: Mutex<ChatDb>,
    pub reranker: Arc<crate::rerank::Reranker>,
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
            embedder: Arc::new(Embedder::default()),
            settings: Mutex::new(settings),
            gateway: Mutex::new(gateway),
            models: Mutex::new(models),
            services: Mutex::new(ServiceRegistry::load()),
            stores: Stores::default(),
            chats: Mutex::new(chats),
            reranker: Arc::new(crate::rerank::Reranker::default()),
            watchers: Watchers::new(app.clone(), indexer.clone()),
            indexer,
            streams: Mutex::new(HashMap::new()),
            status: Mutex::new(GatewayStatus { state: "unknown".into(), message: None, checked_at: 0 }),
        })
    }

    fn embedding_source(&self) -> AppResult<Source> {
        Source::parse(&self.settings.lock().embedding_model)
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

/// Is DocAsk started by Windows at sign-in? (Run key in the registry, current user only.)
#[tauri::command]
pub fn get_autostart(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> AppResult<bool> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    let r = if enabled { al.enable() } else { al.disable() };
    r.map_err(|e| AppError::new("autostart", format!("ตั้งค่าเปิดพร้อม Windows ไม่ได้: {e}")))?;
    crate::diag::log(format!("autostart {}", if enabled { "on" } else { "off" }));
    Ok(al.is_enabled().unwrap_or(enabled))
}

#[tauri::command]
pub fn get_settings(state: S<'_>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, state: S<'_>, settings: Settings) -> AppResult<Settings> {
    let s = settings.sanitize();
    s.save()?;
    let ocr_changed = {
        let cur = state.settings.lock();
        cur.ocr_enabled != s.ocr_enabled || cur.keep_images != s.keep_images
    };
    let purge = s.auto_delete_days;
    crate::ingest::ocr::set_enabled(s.ocr_enabled);
    crate::ingest::set_keep_images(s.keep_images);
    crate::ingest::set_load(&s.index_load);
    *state.settings.lock() = s.clone();
    let _ = state.chats.lock().purge_older_than(purge);
    if ocr_changed {
        // what gets extracted changed → re-index every service
        for svc in state.services.lock().list() {
            state.indexer.start(app.clone(), svc.id, false);
        }
    }
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

/// Size on disk of a service's index (SQLite file + write-ahead log).
fn index_size(id: &str) -> i64 {
    let p = crate::services::index_path(id);
    let wal = p.with_extension("db-wal");
    [p, wal].iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len() as i64).sum()
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
            index_bytes: index_size(&s.id),
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
        return Err(AppError::new("unsupported", "รองรับไฟล์ .pdf .docx .pptx .txt .md และรูปภาพ .png .jpg .bmp .webp .tif"));
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

/// Put a document that was removed from a watched folder back into the service.
#[tauri::command]
pub fn restore_document(app: AppHandle, state: S<'_>, service_id: String, path: String) -> AppResult<Service> {
    let svc = state.services.lock().restore_file(&service_id, &path)?;
    state.indexer.start(app, service_id, false);
    Ok(svc)
}

/// Pictures kept from the document around a cited chunk.
#[tauri::command]
pub fn source_images(state: S<'_>, service_id: String, doc_path: String, page: Option<u32>, ord: i64) -> AppResult<Vec<crate::store::ImageInfo>> {
    let store = state.stores.get(&service_id)?;
    let imgs = store.lock().images_for(&doc_path, page, ord)?;
    Ok(imgs)
}

/// One kept picture as a `data:image/jpeg;base64,…` URL (CSP allows `img-src data:`).
#[tauri::command]
pub fn image_data(state: S<'_>, service_id: String, id: i64) -> AppResult<String> {
    use base64::Engine;
    let store = state.stores.get(&service_id)?;
    let bytes = store.lock().image_data(id)?.ok_or_else(|| AppError::new("not_found", "ไม่พบรูปนี้ (อาจถูก index ใหม่แล้ว)"))?;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareProgress {
    phase: &'static str,
    done: usize,
    total: usize,
}

/// One .zip with the index and the documents (see share.rs).
#[tauri::command]
pub async fn export_service_zip(app: AppHandle, service_id: String, dest_dir: String) -> AppResult<crate::share::ExportResult> {
    let state = app.state::<AppState>();
    let svc = state.services.lock().get(&service_id)?;
    let store = state.stores.get(&service_id)?;
    let embedding = state.embedding_source()?.id();
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::share::export_zip(&svc, &store, &embedding, Path::new(&dest_dir), |done, total| {
            let _ = a.emit("share-progress", ShareProgress { phase: "export", done, total });
        })
    })
    .await
    .map_err(|e| AppError::new("export", e.to_string()))?
}

/// Unpack a service .zip into `dest_dir` and add it as a service. The vectors are kept
/// when the embedding model matches; with `reembed` the documents are indexed again here.
#[tauri::command]
pub async fn import_service_zip(app: AppHandle, zip_path: String, dest_dir: String, reembed: bool) -> AppResult<Service> {
    let zip = PathBuf::from(&zip_path);
    let manifest = crate::share::read_manifest(&zip)?;
    let state = app.state::<AppState>();
    let ours = state.embedding_source()?.id();
    if manifest.embedding != ours && !reembed {
        return Err(AppError::new(
            "embedding_mismatch",
            format!("ไฟล์นี้ทำ index ด้วยโมเดล {} แต่เครื่องนี้ใช้ {} — เปลี่ยนโมเดลให้ตรงกันก่อน หรือนำเข้าแล้วทำ index ใหม่ในเครื่องนี้", manifest.embedding, ours),
        ));
    }
    let target = crate::share::free_folder(Path::new(&dest_dir), &manifest.name);
    let svc = state.services.lock().create(NewService {
        name: manifest.name.clone(),
        code: manifest.code.clone(),
        color: manifest.color.clone(),
        folders: vec![target.join("docs").to_string_lossy().into_owned()],
        model: manifest.model.clone(),
        effort: manifest.effort.clone(),
        top_k: manifest.top_k,
        system_prompt: manifest.system_prompt.clone(),
    })?;
    let id = svc.id.clone();
    let keep_index = manifest.embedding == ours;
    let a = app.clone();
    let unpacked = tauri::async_runtime::spawn_blocking(move || {
        let db = crate::services::index_path(&id);
        crate::share::unpack(&zip, &target, keep_index.then_some(db.as_path()), |done, total| {
            let _ = a.emit("share-progress", ShareProgress { phase: "import", done, total });
        })
    })
    .await
    .map_err(|e| AppError::new("import", e.to_string()))?;
    if let Err(e) = unpacked {
        // leave nothing half-made behind (the unpacked files stay for the user to look at)
        state.stores.close(&svc.id);
        let _ = state.services.lock().delete(&svc.id);
        return Err(e);
    }
    crate::diag::log(format!("[{}] imported from {} ({} documents, vectors {})", svc.code, zip_path, manifest.documents, if keep_index { "kept" } else { "redone" }));
    state.watchers.watch(&svc);
    state.indexer.start(app.clone(), svc.id.clone(), false);
    Ok(svc)
}

/// The files of a service folder were moved to `to`: follow them without re-indexing.
#[tauri::command]
pub fn move_service_folder(app: AppHandle, state: S<'_>, service_id: String, from: String, to: String) -> AppResult<Service> {
    if !Path::new(&to).is_dir() {
        return Err(AppError::new("not_found", "ไม่พบโฟลเดอร์ใหม่"));
    }
    let mut svc = state.services.lock().get(&service_id)?;
    let Some(slot) = svc.folders.iter_mut().find(|f| f.eq_ignore_ascii_case(&from)) else {
        return Err(AppError::new("not_found", "ไม่พบโฟลเดอร์นี้ใน service"));
    };
    *slot = to.clone();
    if svc.knowledge_dir.eq_ignore_ascii_case(&from) {
        svc.knowledge_dir = to.clone();
    }
    let to_dir = PathBuf::from(&to);
    state.indexer.cancel(&service_id);
    let moved = state.stores.get(&service_id)?.lock().rewrite_paths(|p| {
        crate::share::relative_to(p, &from).map(|rel| to_dir.join(rel.replace('/', "\\")).to_string_lossy().into_owned())
    })?;
    let svc = state.services.lock().update(svc)?;
    crate::diag::log(format!("[{}] folder moved {from} -> {to} ({moved} documents)", svc.code));
    state.watchers.watch(&svc);
    state.indexer.start(app, svc.id.clone(), false);
    Ok(svc)
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
    /// generated by the frontend so it can match events that arrive before ask() returns
    #[serde(default)]
    pub request_id: Option<String>,
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

    let request_id = req
        .request_id
        .clone()
        .filter(|r| !r.is_empty() && r.len() <= 64)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
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

    /// Save the answer. Always returns a message so the UI can leave the streaming
    /// state even if the database write fails.
    fn save(&self, content: &str, sources: &[SourceRef], elapsed: i64, error: Option<&str>) -> Message {
        let state = self.app.state::<AppState>();
        let db = state.chats.lock();
        let saved = db.add_message(NewMessage {
            chat_id: &self.chat_id,
            role: "assistant",
            content,
            model: Some(&self.model.id),
            reasoning_effort: Some(&self.effort),
            sources,
            elapsed_ms: Some(elapsed),
            error,
        });
        saved.unwrap_or_else(|e| Message {
            id: -now_ms(),
            chat_id: self.chat_id.clone(),
            role: "assistant".into(),
            content: content.into(),
            model: Some(self.model.id.clone()),
            reasoning_effort: Some(self.effort.clone()),
            sources: sources.to_vec(),
            elapsed_ms: Some(elapsed),
            error: Some(error.map(str::to_string).unwrap_or_else(|| format!("บันทึกประวัติไม่สำเร็จ: {}", e.message))),
            created_at: now_ms(),
        })
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
                let text = if text.trim().is_empty() { "(Gateway ไม่ได้ส่งคำตอบกลับมา)".to_string() } else { text };
                let m = self.save(&text, &sources, elapsed(), None);
                self.emit("chat-done", Done { message: m });
                // after chat-done, so a UI re-render can never swallow the done event
                set_status(&self.app, "ok", None);
            }
            Ok(StreamEnd::Cancelled(text)) => {
                let m = self.save(&text, &sources, elapsed(), Some("หยุดโดยผู้ใช้"));
                self.emit("chat-done", Done { message: m });
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
    let src = state.embedding_source()?;
    let mut threshold = state.settings.lock().threshold;
    if src.is_remote() {
        // other models spread cosine scores lower than e5 (OpenAI: unrelated ≈ 0.1, related
        // ≈ 0.3–0.5) — keep the cut-off from hiding good passages; top-k still limits
        threshold = threshold.min(0.15);
    }
    let qv = state.embedder.clone().embed_query_from(src, question.to_string()).await?;

    let targets: Vec<Service> = if all { state.services.lock().list() } else { vec![svc.clone()] };
    let k = svc.top_k as usize;
    let rerank = state.settings.lock().rerank && crate::rerank::Reranker::status().ready;
    let hybrid = state.settings.lock().hybrid;
    let want = if rerank { crate::rerank::candidates(k) } else { k };
    let mut hits = Vec::new();
    for s in targets {
        let store = state.stores.get(&s.id)?;
        let found = if hybrid {
            store.lock().hybrid_search(&qv, question, want, threshold)?
        } else {
            store.lock().search(&qv, want, threshold)?
        };
        hits.extend(found.into_iter().map(|h| (s.clone(), h)));
    }
    hits.sort_by(|a, b| b.1.fused.total_cmp(&a.1.fused).then(b.1.score.total_cmp(&a.1.score)));
    hits.truncate(want);

    // second stage: the cross-encoder reads question + passage together
    let mut rr: Vec<Option<f32>> = vec![None; hits.len()];
    if rerank && hits.len() > 1 {
        let reranker = state.reranker.clone();
        let q = question.to_string();
        let texts: Vec<String> = hits.iter().map(|(_, h)| h.text.clone()).collect();
        match tauri::async_runtime::spawn_blocking(move || reranker.score(&q, &texts)).await {
            Ok(Ok(scores)) => {
                let mut order: Vec<usize> = (0..hits.len()).collect();
                order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
                let mut slots: Vec<Option<_>> = hits.into_iter().map(Some).collect();
                hits = order.iter().map(|&i| slots[i].take().expect("each index once")).collect();
                rr = order.iter().map(|&i| Some(scores[i])).collect();
            }
            // reranking is an improvement, never a reason to fail the question
            Ok(Err(e)) => log::warn!("rerank failed, using vector order: {}", e.message),
            Err(e) => log::warn!("rerank task failed: {e}"),
        }
    }
    // a knowledge note that matches well goes first (at most 2), documents fill the rest
    let note: Vec<bool> = hits.iter().map(|(_, h)| crate::ingest::is_knowledge(&h.doc_path)).collect();
    if note.iter().any(|n| *n) {
        let strong: Vec<bool> = hits
            .iter()
            .zip(&rr)
            .map(|((_, h), r)| match r {
                Some(r) => *r >= 0.3,
                None => h.score >= threshold + 0.05 || (h.keyword && h.score >= threshold),
            })
            .collect();
        let order = crate::knowledge::prefer_notes(&note, &strong);
        let mut slots: Vec<Option<_>> = hits.into_iter().zip(rr).map(Some).collect();
        (hits, rr) = order.iter().map(|&i| slots[i].take().expect("each index once")).unzip();
    }
    hits.truncate(k);
    rr.truncate(k);
    Ok(hits
        .into_iter()
        .zip(rr)
        .enumerate()
        .map(|(i, ((s, h), rerank_score))| SourceRef {
            n: i + 1,
            service_id: s.id,
            service_name: s.name,
            file_name: Path::new(&h.doc_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            knowledge: crate::ingest::is_knowledge(&h.doc_path),
            doc_path: h.doc_path,
            page: h.page,
            ord: h.ord,
            score: h.score,
            rerank_score,
            keyword: h.keyword,
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
         - ใช้ **ตัวหนา** กับคำสำคัญได้ ตอบกระชับ\n\
         - ถ้าคำตอบมีคำสั่ง SQL หรือโค้ดจากเอกสาร ให้คัดลอกตรงตัวจากเอกสารในบล็อก ```sql (หรือภาษานั้น) แล้วอ้าง [n] ใต้บล็อก \
         ห้ามแต่งชื่อตาราง ฟิลด์ หรือเงื่อนไขที่ไม่มีในเอกสาร ถ้าต้องปรับค่า (เช่น {EMPLID} หรือวันที่) ให้บอกว่าต้องแทนค่าอะไร\n\
         - ถ้าเป็นคำสั่งที่แก้ข้อมูล (UPDATE, DELETE, INSERT, MERGE) ให้เตือนว่าเปลี่ยนข้อมูลจริง ให้ SELECT ตรวจเงื่อนไขก่อน และตรวจผลก่อน COMMIT\n\
         - ท่อนที่ขึ้นต้นว่า \"บันทึกความรู้\" คือวิธีที่เคยใช้แก้ปัญหานี้ได้ในแชตก่อน ใช้เป็นแนวทางได้ (อ้าง [n] เหมือนท่อนอื่น) \
         แต่ถ้าขัดกับเอกสาร ให้ยึดเอกสาร และบอกว่าบันทึกขัดกับเอกสารตรงไหน",
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
        let unit = if crate::ingest::is_spreadsheet(&s.file_name) { "แถว" } else { "หน้า" };
        let page = s.page.map(|p| format!(" · {unit} {p}")).unwrap_or_default();
        let from = if all { format!(" · service {}", s.service_name) } else { String::new() };
        if s.knowledge {
            let title = crate::knowledge::title_of(&s.file_name);
            user.push_str(&format!("\n[{}] บันทึกความรู้: {title}{from}\n{}\n", s.n, s.text));
        } else {
            user.push_str(&format!("\n[{}] {}{}{}\n{}\n", s.n, s.file_name, page, from, s.text));
        }
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

// ------------------------------------------------------------------ knowledge

/// Where notes of this service go and how many there are.
#[tauri::command]
pub fn knowledge_info(state: S<'_>, service_id: String) -> AppResult<crate::knowledge::KnowledgeInfo> {
    let svc = state.services.lock().get(&service_id)?;
    let dir = if svc.knowledge_dir.is_empty() { crate::services::default_knowledge_dir(&svc.name) } else { PathBuf::from(&svc.knowledge_dir) };
    Ok(crate::knowledge::KnowledgeInfo { notes: crate::knowledge::count_notes(&dir), dir: dir.to_string_lossy().into_owned() })
}

/// Ask the model to write notes (one per problem) from a chat, up to message `upto`.
/// Nothing is saved: the user checks and edits the drafts first.
#[tauri::command]
pub async fn draft_knowledge(app: AppHandle, chat_id: String, upto: Option<i64>) -> AppResult<Vec<crate::knowledge::Draft>> {
    let state = app.state::<AppState>();
    let key = secrets::require_key()?;
    let (chat, messages) = {
        let db = state.chats.lock();
        (db.get_chat(&chat_id)?, db.messages(&chat_id)?)
    };
    let transcript = crate::knowledge::transcript(&messages, upto);
    if !messages.iter().any(|m| m.role == "assistant" && m.error.is_none() && !m.content.trim().is_empty()) {
        return Err(AppError::new("invalid", "แชตนี้ยังไม่มีคำตอบให้สรุป"));
    }
    // the model of the last answer, else the service's, else any enabled one
    let svc = state.services.lock().get(&chat.service_id)?;
    let last_model = messages.iter().rev().find_map(|m| m.model.clone());
    let model = {
        let models = state.models.lock();
        let pick = |id: &str| models.iter().find(|m| m.enabled && m.id == id).cloned();
        last_model.as_deref().and_then(pick).or_else(|| pick(&svc.model)).or_else(|| models.iter().find(|m| m.enabled).cloned())
    }
    .ok_or_else(|| AppError::new("no_model", "ไม่มีโมเดลที่เปิดใช้ — ตรวจในหมวดโมเดล"))?;
    let messages = crate::knowledge::draft_prompt(&transcript);
    let req = llm::Request { url: &model.url, key: &key, model: &model.id, effort: None, max_tokens: model.max_output_tokens, messages: &messages };
    let reply = llm::complete_within(&req, std::time::Duration::from_secs(240)).await?;
    let drafts = crate::knowledge::parse_drafts(&reply, &chat.title);
    crate::diag::log(format!("[{}] knowledge draft: {} note(s) from {} chars", svc.code, drafts.len(), transcript.chars().count()));
    if drafts.is_empty() {
        return Err(AppError::new("empty", "โมเดลไม่ได้ส่งบันทึกกลับมา — ลองอีกครั้ง"));
    }
    Ok(drafts)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewNote {
    pub title: String,
    pub markdown: String,
    /// "fixed" | "unverified"
    pub status: String,
}

/// Write the checked notes into the service's knowledge folder and index them.
#[tauri::command]
pub fn save_knowledge(app: AppHandle, state: S<'_>, service_id: String, chat_id: Option<String>, date: String, notes: Vec<NewNote>) -> AppResult<Vec<String>> {
    if notes.is_empty() {
        return Err(AppError::new("invalid", "ยังไม่ได้เลือกบันทึก"));
    }
    let chat_title = match &chat_id {
        Some(id) => state.chats.lock().get_chat(id).map(|c| c.title).unwrap_or_default(),
        None => String::new(),
    };
    let (svc, folders_changed) = state.services.lock().ensure_knowledge_dir(&service_id)?;
    let dir = PathBuf::from(&svc.knowledge_dir);
    let mut saved = Vec::new();
    for n in &notes {
        let p = crate::knowledge::write_note(&dir, &n.title, &n.status, &date, &chat_title, &n.markdown)?;
        saved.push(p.to_string_lossy().into_owned());
    }
    crate::diag::log(format!("[{}] knowledge: saved {} note(s)", svc.code, saved.len()));
    if folders_changed {
        state.watchers.watch(&svc);
    }
    state.indexer.start(app, svc.id.clone(), false);
    Ok(saved)
}

/// Open the knowledge folder in Explorer (created if needed).
#[tauri::command]
pub fn open_knowledge_folder(state: S<'_>, service_id: String) -> AppResult<()> {
    let svc = state.services.lock().get(&service_id)?;
    let dir = if svc.knowledge_dir.is_empty() { crate::services::default_knowledge_dir(&svc.name) } else { PathBuf::from(&svc.knowledge_dir) };
    std::fs::create_dir_all(&dir).map_err(|e| AppError::new("io", format!("สร้างโฟลเดอร์ไม่ได้: {e}")))?;
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(&dir).spawn();
    r.map(|_| ()).map_err(|e| AppError::new("open", format!("เปิดโฟลเดอร์ไม่ได้: {e}")))
}

// ------------------------------------------------------------------ embedding

#[tauri::command]
pub fn embedding_status(state: S<'_>) -> AppResult<EmbeddingStatus> {
    Ok(Embedder::status_of(&state.embedding_source()?))
}

/// Embedding models offered by the Gateway.
#[tauri::command]
pub async fn gateway_embedding_models() -> AppResult<Vec<String>> {
    crate::gateway_embed::list_models().await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingTest {
    pub dims: usize,
    pub ms: u64,
    /// cosine of two Thai sentences with the same meaning vs. an unrelated one
    pub similar: f32,
    pub unrelated: f32,
}

/// Try a Gateway embedding model with three short Thai sentences.
#[tauri::command]
pub async fn test_gateway_embedding(model: String) -> AppResult<EmbeddingTest> {
    let texts = ["ลาพักร้อนได้กี่วันต่อปี", "สิทธิ์วันหยุดพักผ่อนประจำปีมีกี่วัน", "วิธีเปลี่ยนรหัสผ่านอีเมล"].map(String::from);
    let t = Instant::now();
    let v = crate::gateway_embed::embed(model.trim(), &texts).await?;
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    Ok(EmbeddingTest { dims: v[0].len(), ms: t.elapsed().as_millis() as u64, similar: dot(&v[0], &v[1]), unrelated: dot(&v[0], &v[2]) })
}

/// Switch embeddings to a Gateway model (after a test call) and re-index every service.
#[tauri::command]
pub async fn use_gateway_embedding(app: AppHandle, model: String) -> AppResult<EmbeddingStatus> {
    let model = model.trim().to_string();
    crate::gateway_embed::embed(&model, &["ทดสอบ".to_string()]).await?;
    let state = app.state::<AppState>();
    let s = {
        let mut cur = state.settings.lock();
        cur.embedding_model = format!("{}{model}", crate::gateway_embed::PREFIX);
        cur.clone()
    };
    s.save()?;
    state.embedder.unload();
    for svc in state.services.lock().list() {
        state.indexer.start(app.clone(), svc.id, false);
    }
    Ok(Embedder::status_of(&state.embedding_source()?))
}

// ------------------------------------------------- own embedding server (home Mac mini)

#[tauri::command]
pub fn embed_server_info() -> crate::embed_server::ServerInfo {
    crate::embed_server::info()
}

/// Save the URL and (when given) a new token. The token goes straight to the credential store.
#[tauri::command]
pub fn set_embed_server(url: String, token: Option<String>) -> AppResult<crate::embed_server::ServerInfo> {
    let base = crate::embed_server::validate_url(&url)?;
    if let Some(t) = token.filter(|t| !t.trim().is_empty()) {
        secrets::set_server_token(&t)?;
    }
    crate::embed_server::ServerConfig { url: base }.save()?;
    Ok(crate::embed_server::info())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerTest {
    pub model: String,
    pub device: String,
    #[serde(flatten)]
    pub test: EmbeddingTest,
}

/// `/health` for the model name, then the same three Thai sentences as the Gateway test.
#[tauri::command]
pub async fn test_embed_server() -> AppResult<ServerTest> {
    let info = crate::embed_server::info();
    if info.url.is_empty() {
        return Err(AppError::new("no_server", "ใส่ URL ของเซิร์ฟเวอร์ก่อน"));
    }
    let h = crate::embed_server::health(&info.url).await?;
    let texts = ["ลาพักร้อนได้กี่วันต่อปี", "สิทธิ์วันหยุดพักผ่อนประจำปีมีกี่วัน", "วิธีเปลี่ยนรหัสผ่านอีเมล"].map(String::from);
    let t = Instant::now();
    let q = crate::embed_server::embed_query(&h.model, &texts[0]).await?;
    let p = crate::embed_server::embed_passages(&h.model, &texts[1..]).await?;
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    if q.len() != h.dim {
        return Err(AppError::new("server", format!("เซิร์ฟเวอร์บอกว่า {} มิติ แต่ส่งมา {} มิติ", h.dim, q.len())));
    }
    Ok(ServerTest {
        model: h.model,
        device: h.device,
        test: EmbeddingTest { dims: q.len(), ms: t.elapsed().as_millis() as u64, similar: dot(&q, &p[0]), unrelated: dot(&q, &p[1]) },
    })
}

/// Switch embeddings to the server's model (after a test call) and re-index every service.
#[tauri::command]
pub async fn use_embed_server(app: AppHandle, model: String) -> AppResult<EmbeddingStatus> {
    let model = model.trim().to_string();
    crate::embed_server::embed_query(&model, "ทดสอบ").await?;
    let state = app.state::<AppState>();
    let s = {
        let mut cur = state.settings.lock();
        cur.embedding_model = format!("{}{model}", crate::embed_server::PREFIX);
        cur.clone()
    };
    s.save()?;
    state.embedder.unload();
    crate::diag::log(format!("embeddings: server model {model}"));
    for svc in state.services.lock().list() {
        state.indexer.start(app.clone(), svc.id, false);
    }
    Ok(Embedder::status_of(&state.embedding_source()?))
}

/// Remove the URL and token (not while the server is the embedding source).
#[tauri::command]
pub fn forget_embed_server(state: S<'_>) -> AppResult<crate::embed_server::ServerInfo> {
    if matches!(state.embedding_source()?, Source::Server(_)) {
        return Err(AppError::new("in_use", "กำลังใช้เซิร์ฟเวอร์นี้ทำ embedding — เปลี่ยนไปใช้โมเดลในเครื่องก่อน"));
    }
    secrets::delete_server_token()?;
    crate::embed_server::ServerConfig::default().save()?;
    Ok(crate::embed_server::info())
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

// ------------------------------------------------------------------ reranker

#[tauri::command]
pub fn rerank_status() -> crate::rerank::RerankStatus {
    crate::rerank::Reranker::status()
}

#[tauri::command]
pub async fn download_reranker(app: AppHandle) -> AppResult<()> {
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        let res = crate::rerank::download(|p: DownloadProgress| {
            let _ = a.emit("embed-download", p);
        })
        .await;
        match res {
            Ok(_) => a.state::<AppState>().reranker.unload(),
            Err(e) => {
                let _ = a.emit("embed-download", DownloadProgress { model: crate::rerank::MODEL.into(), file: String::new(), downloaded: 0, total: None, done: false, error: Some(e.message) });
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn use_reranker_folder(state: S<'_>, folder: String) -> AppResult<crate::rerank::RerankStatus> {
    state.reranker.use_folder(Path::new(&folder))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrStatus {
    available: bool,
    enabled: bool,
}

#[tauri::command]
pub fn ocr_status(state: S<'_>) -> OcrStatus {
    OcrStatus { available: crate::ingest::ocr::available(), enabled: state.settings.lock().ocr_enabled }
}

// ------------------------------------------------------------------ wipe

#[tauri::command]
pub fn wipe_all_data(state: S<'_>) -> AppResult<()> {
    state.indexer.cancel_all();
    state.watchers.clear();
    state.stores.close_all();
    state.embedder.unload();
    state.reranker.unload();
    for t in state.streams.lock().values() {
        t.cancel();
    }
    state.chats.lock().delete_all()?;
    state.services.lock().clear();
    secrets::delete_key()?;
    secrets::delete_server_token()?;
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
            id: "hr".into(), name: "HR".into(), code: "HR".into(), color: "#1F4FD1".into(), folders: vec![], files: vec![], excluded: vec![],
            model: "godev1".into(), effort: "medium".into(), top_k: 5, system_prompt: "ตอบสั้น".into(), knowledge_dir: String::new(), created_at: 0, updated_at: 0,
        };
        let src = vec![SourceRef { n: 1, service_id: "hr".into(), service_name: "HR".into(), doc_path: "a.pdf".into(), file_name: "a.pdf".into(), page: Some(4), ord: 0, score: 0.9, rerank_score: None, keyword: false, knowledge: false, text: "ลา 10 วัน".into() }];
        let hist = vec![("q0".to_string(), "a0".to_string())];
        let m = build_prompt(&svc, &src, &hist, "ลาได้กี่วัน", false);
        assert_eq!(m.len(), 4);
        assert!(m[0].content.contains("ตอบสั้น"));
        assert_eq!(m[1].content, "q0");
        assert!(m[3].content.contains("[1] a.pdf · หน้า 4"));

        let mut note = src[0].clone();
        note.file_name = "2026-11-12_OT-ไม่ขึ้น.knowledge.md".into();
        note.knowledge = true;
        let m = build_prompt(&svc, &[note], &[], "OT ไม่ขึ้น", false);
        assert!(m[1].content.contains("[1] บันทึกความรู้: OT ไม่ขึ้น\n"), "{}", m[1].content);
        assert!(m[0].content.contains("ให้ยึดเอกสาร"));
    }
}
