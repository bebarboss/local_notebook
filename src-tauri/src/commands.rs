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
use std::time::{Duration, Instant};
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
    /// coding mode: in-flight "allow reading this outside file?" prompts, awaiting the user's reply
    pub pending_perms: Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    /// coding mode: outside files the user already approved, per chat (canonical paths)
    pub code_allowed: Mutex<HashMap<String, std::collections::HashSet<String>>>,
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
            pending_perms: Mutex::new(HashMap::new()),
            code_allowed: Mutex::new(HashMap::new()),
        })
    }

    pub fn embedding_source(&self) -> AppResult<Source> {
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

/// Usage/budget for the current virtual key, read from the Gateway (LiteLLM `/key/info`).
/// `Ok(None)` means this gateway does not expose it — the UI shows a "not available" note.
#[tauri::command]
pub async fn gateway_key_usage(app: AppHandle) -> AppResult<Option<llm::KeyUsage>> {
    let base = app.state::<AppState>().gateway.lock().base();
    let key = match secrets::get_key()? {
        Some(k) => k,
        None => return Err(AppError::no_key()),
    };
    llm::key_info(&base, &key).await
}

/// Per-day spend/tokens/requests + per-model breakdown over a date range (LiteLLM /user/daily/activity).
/// `start_date`/`end_date` are YYYY-MM-DD, computed on the frontend from the user's clock.
/// `Ok(None)` means the gateway does not expose the endpoint for this key.
#[tauri::command]
pub async fn gateway_key_activity(app: AppHandle, start_date: String, end_date: String) -> AppResult<Option<llm::KeyActivity>> {
    let base = app.state::<AppState>().gateway.lock().base();
    let key = match secrets::get_key()? {
        Some(k) => k,
        None => return Err(AppError::no_key()),
    };
    llm::key_activity(&base, &key, &start_date, &end_date).await
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
pub async fn source_images(app: AppHandle, service_id: String, doc_path: String, page: Option<u32>, ord: i64) -> AppResult<Vec<crate::store::ImageInfo>> {
    let state = app.state::<AppState>();
    let local = {
        let store = state.stores.get(&service_id)?;
        let imgs = store.lock().images_for(&doc_path, page, ord)?;
        imgs
    };
    // a shared document indexed on another PC: its pictures are on the server
    let shared = state.services.lock().get(&service_id).is_ok_and(|s| s.shared);
    if local.is_empty() && shared {
        return crate::team::remote_images(&service_id, &doc_path, page, ord).await.or_else(|_| Ok(vec![]));
    }
    Ok(local)
}

/// One kept picture as a `data:image/jpeg;base64,…` URL (CSP allows `img-src data:`).
#[tauri::command]
pub async fn image_data(app: AppHandle, service_id: String, id: i64) -> AppResult<String> {
    use base64::Engine;
    let bytes = if id < 0 {
        crate::team::remote_image(&service_id, -id).await?
    } else {
        let store = app.state::<AppState>().stores.get(&service_id)?;
        let b = store.lock().image_data(id)?;
        b.ok_or_else(|| AppError::new("not_found", "ไม่พบรูปนี้ (อาจถูก index ใหม่แล้ว)"))?
    };
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
pub fn list_chats(state: S<'_>, service_id: Option<String>, mode: Option<String>, query: String) -> AppResult<Vec<Chat>> {
    // for a floating list (no service) `mode` picks the menu: "chat" (general) or "code" (coding)
    let floating = mode.as_deref().unwrap_or("chat");
    state.chats.lock().list_chats(service_id.as_deref(), floating, &query)
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

fn mode_docs() -> String {
    "docs".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskRequest {
    pub chat_id: Option<String>,
    /// required for a docs chat; omitted for a general ("chat" mode) chat
    #[serde(default)]
    pub service_id: Option<String>,
    pub question: String,
    pub model: String,
    pub effort: String,
    /// `"docs"` (default, RAG) or `"chat"` (general) — only used when creating a new chat
    #[serde(default = "mode_docs")]
    pub mode: String,
    #[serde(default)]
    pub all_services: bool,
    /// generated by the frontend so it can match events that arrive before ask() returns
    #[serde(default)]
    pub request_id: Option<String>,
    /// file paths attached to this one question (general chat only): their text — images via
    /// OCR — is read fresh and injected as context, not indexed or saved.
    #[serde(default)]
    pub attachments: Vec<String>,
    /// current date/time from the user's own clock (Thai-formatted by the frontend), injected
    /// into the system prompt so the model can answer "what day is it" questions.
    #[serde(default)]
    pub now: Option<String>,
    /// general chat only: run a web-research step (DuckDuckGo) before answering.
    #[serde(default)]
    pub web: bool,
    /// general chat only: run the "เขียนโค้ด" agent on `project_dir` before answering.
    #[serde(default)]
    pub code: bool,
    /// the project folder the coding agent may read/edit (required when `code` is on).
    #[serde(default)]
    pub project_dir: Option<String>,
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
#[serde(rename_all = "camelCase")]
struct Sources {
    sources: Vec<SourceRef>,
    /// a follow-up question was searched with this standalone query
    #[serde(skip_serializing_if = "Option::is_none")]
    search_query: Option<String>,
}
#[derive(Serialize, Clone)]
struct Done {
    message: Message,
}
#[derive(Serialize, Clone)]
struct Failed {
    error: AppError,
}
#[derive(Serialize, Clone)]
struct StepEvent {
    step: crate::agent::Step,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CodeChangesEvent {
    root: String,
    changes: Vec<crate::codefs::CodeChange>,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CodePlanEvent {
    plan: Vec<crate::coding::PlanStep>,
}
/// "Allow reading this outside file?" — emitted to the UI; the user's reply comes back via
/// `resolve_code_permission(request_id, allow)`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CodePermissionRequest {
    request_id: String,
    chat_id: String,
    path: String,
}

#[tauri::command]
pub async fn ask(app: AppHandle, state: S<'_>, req: AskRequest) -> AppResult<AskStarted> {
    let typed = req.question.trim().to_string();
    // a question with only an attachment and no text: the image/file's own text (OCR) becomes the
    // query — see AskJob::run. The stored message shows a short label instead of the raw OCR text.
    let from_attachment = typed.is_empty() && !req.attachments.is_empty();
    if typed.is_empty() && !from_attachment {
        return Err(AppError::new("invalid", "กรุณาพิมพ์คำถาม หรือแนบรูป/ไฟล์"));
    }
    let question = if from_attachment { "ค้นหาจากรูป/ไฟล์ที่แนบ".to_string() } else { typed };
    let key = secrets::require_key()?;
    let model = state
        .models
        .lock()
        .iter()
        .find(|m| m.id == req.model && m.enabled)
        .cloned()
        .ok_or_else(|| AppError::new("no_model", "ไม่พบโมเดลนี้หรือถูกปิดไว้ — ตรวจในหมวดโมเดล"))?;
    let turns = state.settings.lock().history_turns;

    let (chat, history, summary, previous, user_message) = {
        let db = state.chats.lock();
        let chat = match &req.chat_id {
            // existing chat: its mode and service are authoritative (can't switch mid-chat)
            Some(id) => db.get_chat(id)?,
            // new chat: "chat" = general/floating, "code" = coding on a project folder, "docs" = a service
            None => match req.mode.as_str() {
                "chat" => db.create_chat(None, "chat", &question, None)?,
                "code" => {
                    let pd = req.project_dir.as_deref().filter(|p| !p.trim().is_empty())
                        .ok_or_else(|| AppError::new("invalid", "ต้องเลือกโฟลเดอร์โปรเจคก่อน"))?;
                    db.create_chat(None, "code", &question, Some(pd))?
                }
                _ => {
                    let sid = req.service_id.as_deref().ok_or_else(|| AppError::new("invalid", "ต้องเลือก service ก่อน"))?;
                    db.create_chat(Some(sid), "docs", &question, None)?
                }
            },
        };
        // history first (text only), then the new question
        let history = crate::memory::fit(&db.recent_turns(&chat.id, turns)?, crate::memory::HISTORY_BUDGET);
        // history off (0 turns) = no memory of the chat at all
        let summary = if turns > 0 { db.summary(&chat.id)? } else { None };
        let previous = db.last_answer(&chat.id)?;
        let um = db.add_message(NewMessage {
            chat_id: &chat.id, role: "user", content: &question, model: None, reasoning_effort: None,
            sources: &[], elapsed_ms: None, error: None, search_query: None, steps: &[], changes: None, plan: &[],
        })?;
        (chat, history, summary, previous, um)
    };

    // a docs chat needs its service loaded; general ("chat") and "code" chats have none
    let docs = chat.mode == "docs";
    let svc = if docs {
        let sid = chat.service_id.as_deref().ok_or_else(|| AppError::new("invalid", "แชตนี้ไม่มี service"))?;
        Some(state.services.lock().get(sid)?)
    } else {
        None
    };

    let request_id = req
        .request_id
        .clone()
        .filter(|r| !r.is_empty() && r.len() <= 64)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let token = CancellationToken::new();
    state.streams.lock().insert(request_id.clone(), token.clone());

    // attachments are per-question context in both modes (docs chats also search the service)
    let attachments = req.attachments;
    let now = req.now.filter(|s| !s.trim().is_empty());
    // "code" chats run the coding agent on the chat's project folder (persisted at creation; a
    // re-pick mid-chat updates it). "chat" chats may run web research. Both need a tool-calling model.
    let project_dir = if chat.mode == "code" {
        let req_pd = req.project_dir.as_deref().map(str::trim).filter(|p| !p.is_empty());
        match req_pd {
            Some(pd) if chat.project_dir.as_deref() != Some(pd) => {
                state.chats.lock().set_project_dir(&chat.id, pd)?;
                Some(pd.to_string())
            }
            _ => chat.project_dir.clone(),
        }
    } else {
        None
    };
    let code = chat.mode == "code" && model.tool_calling && project_dir.is_some();
    let web = chat.mode == "chat" && req.web && model.tool_calling;
    // extra commands the coding agent may run, beyond the ones auto-detected from the project
    let code_commands = if code { state.settings.lock().code_commands.clone() } else { Vec::new() };
    let job = AskJob { app: app.clone(), request_id: request_id.clone(), chat_id: chat.id.clone(), svc, docs, model, effort: req.effort, all: req.all_services, question, history, summary, turns, previous, search_query: None, steps: Vec::new(), attachments, from_attachment, now, web, code, project_dir, code_commands, changes: None, plan: Vec::new(), key, token };
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
    /// `None` for a general chat (no documents)
    svc: Option<Service>,
    /// `true` = docs mode (RAG), `false` = general chat
    docs: bool,
    model: ModelConfig,
    effort: String,
    all: bool,
    question: String,
    history: Vec<(String, String)>,
    /// summary of the turns before `history` (memory.rs)
    summary: Option<String>,
    turns: u32,
    /// the last good answer and its sources, for follow-up questions
    previous: Option<(String, Vec<SourceRef>)>,
    search_query: Option<String>,
    /// the web-research / coding loop's live activity, saved with the answer
    steps: Vec<crate::agent::Step>,
    /// file paths attached to this question (general chat only); read fresh, never saved
    attachments: Vec<String>,
    /// the box was empty and only an attachment was sent: use the attachment's text (OCR) as the
    /// question/query instead of injecting it as extra context (see `run`)
    from_attachment: bool,
    /// current date/time from the user's clock, injected into the system prompt
    now: Option<String>,
    /// general chat only: run the web-research step before answering
    web: bool,
    /// general chat only: run the coding agent on `project_dir` before answering
    code: bool,
    /// coding mode: extra allowed command prefixes from settings (merged with project defaults)
    code_commands: Vec<String>,
    /// project folder for the coding agent (absolute path; set only when `code` is on)
    project_dir: Option<String>,
    /// files the coding turn changed, saved with the answer for undo
    changes: Option<crate::codefs::Changeset>,
    /// the coding turn's plan checklist, saved with the answer
    plan: Vec<crate::coding::PlanStep>,
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
            search_query: self.search_query.as_deref(),
            steps: &self.steps,
            changes: self.changes.as_ref(),
            plan: &self.plan,
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
            search_query: self.search_query.clone(),
            steps: self.steps.clone(),
            changes: self.changes.clone(),
            plan: self.plan.clone(),
        })
    }

    fn fail(&self, e: AppError, sources: &[SourceRef], elapsed: i64) {
        if e.is_unreachable() {
            set_status(&self.app, "unreachable", Some(e.message.clone()));
        }
        self.save("", sources, elapsed, Some(&e.message));
        self.emit("chat-error", Failed { error: e });
    }

    /// A follow-up question rewritten into a standalone search query (falls back to the
    /// previous question + this one when the model is slow or fails).
    async fn search_query(&self) -> String {
        let messages = crate::followup::rewrite_prompt(&self.history, &self.question);
        let req = llm::Request { url: &self.model.url, key: &self.key, model: &self.model.id, effort: None, max_tokens: self.model.max_output_tokens.min(400), messages: &messages };
        let reply = tokio::select! {
            r = llm::complete_within(&req, Duration::from_secs(10)) => r,
            _ = self.token.cancelled() => return crate::followup::fallback(&self.history, &self.question),
        };
        match reply.map(|r| crate::followup::clean_rewrite(&r)) {
            Ok(Some(q)) => q,
            Ok(None) => crate::followup::fallback(&self.history, &self.question),
            Err(e) => {
                // the code only — the question and chat are never logged
                log::warn!("follow-up rewrite failed ({}), using the previous question", e.code);
                crate::followup::fallback(&self.history, &self.question)
            }
        }
    }

    async fn run(mut self) {
        let started = Instant::now();
        let elapsed = || started.elapsed().as_millis() as i64;

        // files/images attached to this one question (both modes): read fresh (OCR for images) as
        // extra context, never indexed or saved. In docs mode they sit alongside the retrieved
        // passages; in general mode they are the only context.
        let mut attached = if self.attachments.is_empty() {
            None
        } else {
            let opts = {
                let st = self.app.state::<AppState>();
                let s = st.settings.lock();
                crate::ingest::chunker::ChunkOptions { size: s.chunk_size, overlap: s.chunk_overlap }
            };
            let paths = self.attachments.clone();
            match tauri::async_runtime::spawn_blocking(move || read_attachments(&paths, opts)).await {
                Ok(t) if !t.is_empty() => Some(t),
                _ => None,
            }
        };

        // the box was empty and only an attachment was sent (e.g. a pasted screenshot): its own
        // text (OCR) is the query — not extra context, so `take` it out of `attached`. No text in
        // the image/file → nothing to search with.
        if self.from_attachment {
            match attached.take().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
                Some(t) => self.question = cap_chars(&t, ATTACH_QUERY_CAP),
                None => {
                    self.fail(AppError::new("invalid", "รูป/ไฟล์ที่แนบไม่มีตัวอักษรให้ค้นหา — พิมพ์คำถามแทน"), &[], elapsed());
                    self.cleanup();
                    return;
                }
            }
        }

        // general (floating) chat: no documents, no retrieval — just answer with the model's
        // own knowledge, keeping the chat's own history.
        if !self.docs {
            // coding: let the model read/edit files in the chosen project folder a few bounded
            // rounds (writes land on disk at once). The loop's own final text is the answer; its
            // changes are emitted for the diff view and saved for undo.
            if self.code {
                let root = self.project_dir.clone().unwrap_or_default();
                let ws = match crate::codefs::Workspace::new(&root) {
                    Ok(w) => w,
                    Err(e) => {
                        self.fail(e, &[], elapsed());
                        self.cleanup();
                        return;
                    }
                };
                let agent = CodeAgent {
                    app: &self.app, chat_id: &self.chat_id, request_id: &self.request_id,
                    model: &self.model, key: self.key.as_str(), effort: None,
                    tools_max: self.model.max_output_tokens, ws, extra_allow: &self.code_commands, cancel: &self.token,
                };
                let (rid, cid, app2) = (self.request_id.clone(), self.chat_id.clone(), self.app.clone());
                let mut collected: Vec<crate::agent::Step> = Vec::new();
                let out = crate::coding::edit_codebase(&agent, &self.question, self.now.as_deref(), &self.token, |step| {
                    let _ = app2.emit("chat-step", ChatEvent { request_id: rid.clone(), chat_id: cid.clone(), data: StepEvent { step: step.clone() } });
                    collected.push(step);
                })
                .await;
                self.steps = collected;
                self.plan = out.plan;
                self.emit("chat-plan", CodePlanEvent { plan: self.plan.clone() });
                let changeset = crate::codefs::Changeset { root: agent.ws.root_display(), items: out.changes };
                self.emit("chat-changes", CodeChangesEvent { root: changeset.root.clone(), changes: changeset.items.clone() });
                self.changes = (!changeset.items.is_empty()).then_some(changeset.clone());
                let (answer_text, stop) = (out.answer, out.stop);
                if answer_text.trim().is_empty() {
                    // the loop ended without a summary (hit a limit/cancel/error) — ask for one from the
                    // files changed, passing the real stop reason so the model states it instead of guessing
                    let messages = build_code_prompt(&self.history, self.summary.as_deref(), self.now.as_deref(), &changeset, &self.question, stop.as_deref());
                    self.answer(messages, Vec::new(), started).await;
                } else {
                    self.deliver(&answer_text, started);
                }
                return;
            }
            // web research: let the model search the public web (DuckDuckGo) a few bounded rounds,
            // collect sources, then answer citing them. Trouble just yields fewer/zero sources.
            if self.web {
                let backend = WebAgent {
                    app: &self.app,
                    model: &self.model,
                    key: self.key.as_str(),
                    effort: None,
                    tools_max: self.model.max_output_tokens.min(2000),
                };
                let (rid, cid, app2) = (self.request_id.clone(), self.chat_id.clone(), self.app.clone());
                let mut collected: Vec<crate::agent::Step> = Vec::new();
                let sources = crate::web_research::research(&backend, &self.question, self.now.as_deref(), &self.token, |step| {
                    let _ = app2.emit("chat-step", ChatEvent { request_id: rid.clone(), chat_id: cid.clone(), data: StepEvent { step: step.clone() } });
                    collected.push(step);
                })
                .await;
                self.steps = collected;
                self.emit("chat-sources", Sources { sources: sources.clone(), search_query: None });
                let messages = build_web_prompt(&self.history, self.summary.as_deref(), attached.as_deref(), self.now.as_deref(), &sources, &self.question);
                self.answer(messages, sources, started).await;
                return;
            }
            let messages = build_chat_prompt(&self.history, self.summary.as_deref(), attached.as_deref(), self.now.as_deref(), &self.question);
            self.answer(messages, Vec::new(), started).await;
            return;
        }

        // docs chat from here on — a service is guaranteed to be present
        let svc = self.svc.clone().expect("docs chat has a service");

        // a follow-up ("แล้วตรวจยังไง") is searched as a standalone query, together with the
        // passages the previous answer cited; the answer still uses the user's own words
        let mut queries = vec![self.question.clone()];
        let mut carried = Vec::new();
        if crate::followup::is_follow_up(&self.question, !self.history.is_empty()) {
            let q = self.search_query().await;
            if q.trim() != self.question.trim() {
                queries.insert(0, q.clone());
                self.search_query = Some(q);
            }
            if let Some((answer, prev)) = &self.previous {
                carried = crate::followup::carried(answer, prev);
            }
        }

        let sources = match retrieve(&self.app, &svc, self.all, &queries, &carried).await {
            Ok(s) => s,
            Err(e) => {
                self.fail(e, &[], elapsed());
                self.cleanup();
                return;
            }
        };

        self.emit("chat-sources", Sources { sources: sources.clone(), search_query: self.search_query.clone() });

        let messages = build_prompt(&svc, &sources, &self.history, self.summary.as_deref(), &self.question, self.all, self.now.as_deref(), attached.as_deref());
        self.answer(messages, sources, started).await;
    }

    /// Emit sources was already done by the caller; this streams the model's reply, saves it,
    /// and cleans up. Shared by both docs and general chats.
    async fn answer(&mut self, messages: Vec<ChatMessage>, sources: Vec<SourceRef>, started: Instant) {
        let elapsed = || started.elapsed().as_millis() as i64;
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
                self.summarize_later();
            }
            Ok(StreamEnd::Cancelled(text)) => {
                let m = self.save(&text, &sources, elapsed(), Some("หยุดโดยผู้ใช้"));
                self.emit("chat-done", Done { message: m });
            }
            Err(e) => self.fail(e, &sources, elapsed()),
        }
        self.cleanup();
    }

    /// Deliver an answer that is already complete (the coding loop's final summary): show it, save
    /// it with this turn's changes, finish — no extra Gateway call.
    fn deliver(&self, text: &str, started: Instant) {
        let elapsed = started.elapsed().as_millis() as i64;
        self.emit("chat-delta", Delta { delta: text.to_string() });
        let m = self.save(text, &[], elapsed, None);
        self.emit("chat-done", Done { message: m });
        set_status(&self.app, "ok", None);
        self.summarize_later();
        self.cleanup();
    }

    fn cleanup(&self) {
        self.app.state::<AppState>().streams.lock().remove(&self.request_id);
    }

    /// Turns that left the history window get folded into the chat's summary — in the
    /// background, so no question waits for it. Failures only mean the summary stays as it was.
    fn summarize_later(&self) {
        if self.turns == 0 {
            return;
        }
        let (app, chat_id, turns) = (self.app.clone(), self.chat_id.clone(), self.turns);
        let (url, key, model) = (self.model.url.clone(), self.key.clone(), self.model.id.clone());
        let max_tokens = self.model.max_output_tokens.min(1200);
        tauri::async_runtime::spawn(async move {
            let (previous, dropped) = match app.state::<AppState>().chats.lock().to_summarize(&chat_id, turns) {
                Ok(x) => x,
                Err(_) => return,
            };
            let Some(upto) = dropped.last().map(|(id, ..)| *id) else { return };
            let turns_text: Vec<(String, String)> = dropped.into_iter().map(|(_, q, a)| (q, a)).collect();
            let messages = crate::memory::summary_prompt(previous.as_deref(), &turns_text);
            let req = llm::Request { url: &url, key: &key, model: &model, effort: None, max_tokens, messages: &messages };
            match llm::complete_within(&req, Duration::from_secs(90)).await.map(|r| crate::memory::clean_summary(&r)) {
                Ok(Some(s)) => {
                    let _ = app.state::<AppState>().chats.lock().set_summary(&chat_id, &s, upto);
                    log::info!("chat summary updated ({} turns folded in)", turns_text.len());
                }
                Ok(None) => log::warn!("chat summary: empty reply"),
                Err(e) => log::warn!("chat summary failed ({})", e.code),
            }
        });
    }
}

/// Embed the question locally and search the service's index (or all services).
fn has_local_index(state: &AppState, targets: &[Service]) -> bool {
    targets.iter().any(|s| state.stores.get(&s.id).ok().and_then(|st| st.lock().stats().ok()).is_some_and(|(_, chunks, ..)| chunks > 0))
}

/// A whole knowledge note is trimmed to this many characters before it goes to the model.
const WHOLE_NOTE_CAP: usize = 8000;

/// A note's text for the prompt, trimmed on a char boundary so a huge note can't flood context.
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…(ตัดเนื้อโน้ตบางส่วน)");
    out
}

/// Total characters of attached-file text handed to the model, across all files. Large enough to
/// fit a long slide deck / document (incl. per-slide OCR text) so the model doesn't see it cut
/// mid-way and report it as incomplete; a file bigger than this is still truncated (with a marker).
const ATTACH_CAP: usize = 100_000;

/// When a question is only an attachment (no typed text), how much of its text (OCR) to use as the
/// query. The embed server truncates a query to its own token limit anyway; this keeps the model's
/// user turn sane when a scanned page holds a lot of text.
const ATTACH_QUERY_CAP: usize = 4000;

/// Read the text of each attached file (images via OCR) for a general chat, as one context
/// block with a header per file. Runs on a blocking thread. Never logs file contents — only an
/// error code when a file can't be read; a file that yields nothing is marked inline so the model
/// can tell the user instead of failing the whole question.
fn read_attachments(paths: &[String], opts: crate::ingest::chunker::ChunkOptions) -> String {
    let mut out = String::new();
    for p in paths {
        let path = std::path::Path::new(p);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("ไฟล์แนบ");
        let body = match crate::ingest::extract_chunks(path, opts) {
            Ok(ex) => {
                let mut t = String::new();
                for (_page, c) in &ex.chunks {
                    t.push_str(c);
                    t.push('\n');
                }
                t.trim().to_string()
            }
            Err(e) => {
                log::warn!("attachment read failed: {}", e.code);
                String::new()
            }
        };
        out.push_str(&format!("\n--- {name} ---\n"));
        if body.is_empty() {
            out.push_str("(อ่านไฟล์นี้ไม่ได้ หรือเป็นรูปที่ไม่มีตัวอักษรให้ OCR)\n");
        } else {
            out.push_str(&body);
            out.push('\n');
        }
        if out.chars().count() > ATTACH_CAP {
            out = out.chars().take(ATTACH_CAP).collect::<String>();
            out.push_str("\n…(ไฟล์แนบยาวมาก ส่งให้โมเดลเฉพาะช่วงต้น — ส่วนที่เหลือไม่ได้ส่ง ถ้าต้องดูครบให้เพิ่มไฟล์เข้า service แล้วค้นแทน)");
            break;
        }
    }
    out.trim().to_string()
}

/// Max bytes accepted for one pasted image (guards against a giant clipboard payload).
const PASTE_MAX: usize = 25 * 1024 * 1024;

/// Save an image pasted into a general chat to a temp file and return its path, so it flows
/// through the same attachment pipeline (OCR → context) as a picked file. Pastes older than two
/// hours are pruned so temp files don't pile up. Never logs the image bytes.
#[tauri::command]
pub fn save_pasted_image(bytes: Vec<u8>, ext: String) -> AppResult<String> {
    if bytes.is_empty() {
        return Err(AppError::new("invalid", "ไม่มีรูปในคลิปบอร์ด"));
    }
    if bytes.len() > PASTE_MAX {
        return Err(AppError::new("too_big", "รูปที่วางใหญ่เกินไป (เกิน 25MB)"));
    }
    let ext = match ext.to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "bmp" | "webp" | "tif" | "tiff" | "gif" => ext.to_lowercase(),
        _ => "png".to_string(),
    };
    let dir = std::env::temp_dir().join("docask-paste");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::new("io", format!("สร้างโฟลเดอร์ชั่วคราวไม่ได้: {e}")))?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let cutoff = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        for e in rd.flatten() {
            if e.metadata().and_then(|m| m.modified()).map(|m| m < cutoff).unwrap_or(false) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let name = format!("pasted-{}.{ext}", &uuid::Uuid::new_v4().to_string()[..8]);
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| AppError::new("io", format!("บันทึกรูปที่วางไม่ได้: {e}")))?;
    Ok(path.to_string_lossy().into_owned())
}

/// `queries[0]` is the main query (reranks and labels); later ones only add candidates.
/// `carried`: passages cited by the previous answer, re-ranked along with the new ones.
async fn retrieve(app: &AppHandle, svc: &Service, all: bool, queries: &[String], carried: &[SourceRef]) -> AppResult<Vec<SourceRef>> {
    let question = queries.first().map(String::as_str).unwrap_or_default();
    let state = app.state::<AppState>();
    let src = state.embedding_source()?;
    let mut threshold = state.settings.lock().threshold;
    if src.is_remote() {
        // other models spread cosine scores lower than e5 (OpenAI: unrelated ≈ 0.1, related
        // ≈ 0.3–0.5) — keep the cut-off from hiding good passages; top-k still limits
        threshold = threshold.min(0.15);
    }
    let targets: Vec<Service> = if all { state.services.lock().list() } else { vec![svc.clone()] };
    let k = svc.top_k as usize;
    // the server's reranker (bge-reranker-v2-m3) when embeddings come from the server, else this PC's
    let rerank_on = state.settings.lock().rerank;
    let server_rerank = rerank_on && matches!(src, Source::Server(_)) && crate::embed_server::rerank_available();
    let local_rerank = rerank_on && crate::rerank::Reranker::status().ready;
    let rerank = server_rerank || local_rerank;
    let hybrid = state.settings.lock().hybrid;
    let want = if rerank { crate::rerank::candidates(k) } else { k };
    let mut hits: Vec<(Service, crate::store::Hit)> = Vec::new();

    // shared services are searched on the server (each with its own key); the rest here.
    // When the server is down, a shared service with a local index is searched here instead.
    let (remote, mut local_targets): (Vec<Service>, Vec<Service>) = targets.iter().cloned().partition(|s| s.shared);
    if !remote.is_empty() {
        match crate::team::search(&remote, queries, want, threshold, hybrid).await {
            Ok(h) => hits = h,
            Err(e) if crate::embed_server::is_down(&e) && has_local_index(&state, &remote) => {
                let _ = app.emit("embed-fallback", "ฐานข้อมูลของทีมติดต่อไม่ได้ — ค้นจาก index ในเครื่องนี้แทน".to_string());
                local_targets.extend(remote);
            }
            Err(e) => return Err(e),
        }
    }
    let local = !local_targets.is_empty();
    let mut vectors = Vec::with_capacity(queries.len());
    if local {
        for q in queries {
            let (qv, notice) = state.embedder.clone().embed_query_from(src.clone(), q.clone()).await?;
            if let Some(n) = notice {
                let _ = app.emit("embed-fallback", n);
            }
            vectors.push((q, qv));
        }
    }
    for s in local_targets.iter() {
        let store = state.stores.get(&s.id)?;
        for (q, qv) in &vectors {
            let found = if hybrid {
                store.lock().hybrid_search(qv, q, want, threshold)?
            } else {
                store.lock().search(qv, want, threshold)?
            };
            for h in found {
                // the same passage found by both queries: keep its better score
                match hits.iter_mut().find(|(hs, x)| hs.id == s.id && x.chunk_id == h.chunk_id) {
                    Some((_, x)) if h.fused > x.fused => *x = h,
                    Some(_) => {}
                    None => hits.push((s.clone(), h)),
                }
            }
        }
    }
    hits.sort_by(|a, b| b.1.fused.total_cmp(&a.1.fused).then(b.1.score.total_cmp(&a.1.score)));
    hits.truncate(want);
    // the previous answer's passages compete again (last without a reranker)
    for c in carried {
        let Some(s) = targets.iter().find(|s| s.id == c.service_id) else { continue };
        let dup = hits.iter().any(|(hs, h)| hs.id == c.service_id && h.doc_path == c.doc_path && h.ord == c.ord);
        if dup || !Path::new(&c.doc_path).exists() {
            continue;
        }
        hits.push((s.clone(), crate::store::Hit {
            chunk_id: -1, score: c.score, fused: 0.0, keyword: false,
            doc_path: c.doc_path.clone(), page: c.page, ord: c.ord, text: c.text.clone(),
        }));
    }
    // the same text twice (the same file in two folders, a renamed copy) takes one place
    {
        let keep = crate::context::first_of_each(&hits.iter().map(|(_, h)| h.text.as_str()).collect::<Vec<_>>());
        let mut slots: Vec<Option<_>> = hits.into_iter().map(Some).collect();
        hits = keep.into_iter().map(|i| slots[i].take().expect("each index once")).collect();
    }

    // second stage: the cross-encoder reads question + passage together
    let mut rr: Vec<Option<f32>> = vec![None; hits.len()];
    if rerank && hits.len() > 1 {
        let texts: Vec<String> = hits.iter().map(|(_, h)| h.text.clone()).collect();
        let mut scored: Option<Vec<f32>> = None;
        if server_rerank {
            match crate::embed_server::rerank(question, &texts).await {
                Ok(s) if s.len() == texts.len() => scored = Some(s),
                Ok(_) => log::warn!("server rerank: wrong number of scores"),
                // reranking is an improvement, never a reason to fail the question
                Err(e) => log::warn!("server rerank failed ({}), trying this PC's", e.code),
            }
        }
        if scored.is_none() && local_rerank {
            let reranker = state.reranker.clone();
            let q = question.to_string();
            match tauri::async_runtime::spawn_blocking(move || reranker.score(&q, &texts)).await {
                Ok(Ok(s)) => scored = Some(s),
                Ok(Err(e)) => log::warn!("rerank failed, using vector order: {}", e.message),
                Err(e) => log::warn!("rerank task failed: {e}"),
            }
        }
        if let Some(scores) = scored {
            let mut order: Vec<usize> = (0..hits.len()).collect();
            order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
            let mut slots: Vec<Option<_>> = hits.into_iter().map(Some).collect();
            hits = order.iter().map(|&i| slots[i].take().expect("each index once")).collect();
            rr = order.iter().map(|&i| Some(scores[i])).collect();
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

    let mut merged: std::collections::HashSet<(String, String, i64)> = std::collections::HashSet::new();

    // a knowledge note is sent whole, not just the chunk the question matched: a saved SQL
    // query or fix usually sits further down the note than the "problem" line that matched.
    // Notes are short and prefer_notes keeps at most two, so this stays small. Later chunks of
    // the same note drop out (the retain below); a note the model can't read keeps its chunk.
    {
        let actions = {
            let note_paths: Vec<Option<&str>> = hits
                .iter()
                .map(|(_, h)| crate::ingest::is_knowledge(&h.doc_path).then_some(h.doc_path.as_str()))
                .collect();
            crate::knowledge::note_actions(&note_paths)
        };
        for (i, action) in actions.into_iter().enumerate() {
            match action {
                crate::knowledge::NoteAction::Expand => {
                    let (s, path) = { let (s, h) = &hits[i]; (s.clone(), h.doc_path.clone()) };
                    match note_of(&s, &path).await {
                        Ok(t) => hits[i].1.text = cap_chars(&t, WHOLE_NOTE_CAP),
                        // can't read it (moved/hidden/server down): keep the matched chunk, never fail
                        Err(e) => log::warn!("whole-note skipped: {}", e.code),
                    }
                }
                crate::knowledge::NoteAction::Drop => {
                    let (s, h) = &hits[i];
                    merged.insert((s.id.clone(), h.doc_path.clone(), h.ord));
                }
                crate::knowledge::NoteAction::Leave => {}
            }
        }
    }

    // the best hits get the chunk before and after them (a SQL statement or a procedure cut
    // across two chunks reaches the model whole); a neighbour that is itself a hit is merged
    for i in 0..hits.len().min(crate::context::NEIGHBOUR_TOP) {
        let (s, h) = &hits[i];
        if crate::ingest::is_knowledge(&h.doc_path) || merged.contains(&(s.id.clone(), h.doc_path.clone(), h.ord)) {
            continue;
        }
        let ords = [h.ord - 1, h.ord + 1];
        let mut got = state.stores.get(&s.id).ok().and_then(|st| st.lock().chunk_texts_at(&h.doc_path, &ords).ok()).unwrap_or_default();
        if got.is_empty() && s.shared {
            got = crate::team::neighbours(&s.id, &h.doc_path, &ords).await.unwrap_or_default();
        }
        let before = got.iter().find(|(o, _)| *o == h.ord - 1).map(|(_, t)| t.as_str());
        let after = got.iter().find(|(o, _)| *o == h.ord + 1).map(|(_, t)| t.as_str());
        if before.is_none() && after.is_none() {
            continue;
        }
        let widened = crate::context::widen(before, &h.text, after);
        let (sid, path, ord) = (s.id.clone(), h.doc_path.clone(), h.ord);
        if before.is_some() {
            merged.insert((sid.clone(), path.clone(), ord - 1));
        }
        if after.is_some() {
            merged.insert((sid.clone(), path.clone(), ord + 1));
        }
        hits[i].1.text = widened;
    }
    if !merged.is_empty() {
        let keep: Vec<bool> = hits.iter().map(|(s, h)| !merged.contains(&(s.id.clone(), h.doc_path.clone(), h.ord))).collect();
        let mut i = 0;
        hits.retain(|_| { i += 1; keep[i - 1] });
        let mut j = 0;
        rr.retain(|_| { j += 1; keep[j - 1] });
    }

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
            web: false,
            text: h.text,
        })
        .collect())
}

/// Backend for the web-research loop: a Gateway tool round plus the DuckDuckGo tools.
struct WebAgent<'a> {
    app: &'a AppHandle,
    model: &'a ModelConfig,
    key: &'a str,
    effort: Option<&'a str>,
    tools_max: u64,
}

/// Backend for the coding loop: a Gateway tool round plus the project-scoped filesystem.
struct CodeAgent<'a> {
    app: &'a AppHandle,
    /// the chat this turn belongs to — for permission events and the per-chat approved-files list
    chat_id: &'a str,
    request_id: &'a str,
    model: &'a ModelConfig,
    key: &'a str,
    effort: Option<&'a str>,
    tools_max: u64,
    ws: crate::codefs::Workspace,
    /// extra allowed command prefixes from settings, merged with the project defaults
    extra_allow: &'a [String],
    /// cancellation for the turn — also aborts a file-permission wait
    cancel: &'a CancellationToken,
}

impl crate::coding::CodeBackend for CodeAgent<'_> {
    async fn complete(&self, messages: &serde_json::Value, tools: &serde_json::Value) -> AppResult<llm::Reply> {
        llm::complete_tools(&self.model.url, self.key, &self.model.id, self.effort, self.tools_max, messages, tools, crate::coding::PER_CALL).await
    }
    fn list_dir(&self, path: &str) -> AppResult<Vec<crate::codefs::DirEntry>> {
        self.ws.list(path)
    }
    fn read_file(&self, path: &str, offset: Option<usize>, limit: Option<usize>) -> AppResult<String> {
        self.ws.read(path, offset, limit)
    }
    fn grep(&self, pattern: &str, sub: Option<&str>) -> AppResult<Vec<crate::codefs::Hit>> {
        self.ws.grep(pattern, sub)
    }
    fn write_file(&self, path: &str, content: &str) -> AppResult<crate::codefs::WriteOutcome> {
        self.ws.write(path, content)
    }
    fn edit_file(&self, path: &str, old: &str, new: &str) -> AppResult<crate::codefs::WriteOutcome> {
        self.ws.edit(path, old, new)
    }
    async fn run_command(&self, command: &str) -> AppResult<crate::coding::RunResult> {
        use crate::coderun;
        // bad characters are a usage error handed to the model (not a silent block)
        coderun::sanitize(command)?;
        let root = self.ws.root();
        let mut allow = coderun::default_allow(root);
        allow.extend(self.extra_allow.iter().cloned());
        if !coderun::is_allowed(command, &allow) {
            let list = if allow.is_empty() { "(ยังไม่มีคำสั่งที่อนุญาตสำหรับโปรเจคนี้)".to_string() } else { allow.join(", ") };
            return Ok(crate::coding::RunResult {
                blocked: true,
                exit_code: None,
                text: format!("คำสั่งนี้ไม่อยู่ในรายการที่อนุญาต จึงไม่รัน ให้ใช้คำสั่งตรวจงานที่อนุญาตแทน หรือข้ามขั้นนี้ · ที่อนุญาต: {list}"),
            });
        }
        let out = coderun::run(root, command, coderun::PER_CMD).await?;
        Ok(crate::coding::RunResult { blocked: false, exit_code: out.exit_code, text: out.output })
    }

    async fn read_reference(&self, path: &str) -> AppResult<crate::coding::ReferenceOutcome> {
        use crate::coding::ReferenceOutcome;
        // stable key for the per-chat approved list (if the file exists); the read itself re-validates
        let canon = std::path::Path::new(path).canonicalize().ok().map(|p| p.to_string_lossy().into_owned());
        let already = canon.as_ref().is_some_and(|c| {
            self.app.state::<AppState>().code_allowed.lock().get(self.chat_id).is_some_and(|s| s.contains(c))
        });
        if !already && !self.ask_permission(path).await {
            return Ok(ReferenceOutcome::Denied);
        }
        let text = crate::codefs::read_external(path)?;
        if let Some(c) = canon {
            self.app.state::<AppState>().code_allowed.lock().entry(self.chat_id.to_string()).or_default().insert(c);
        }
        Ok(ReferenceOutcome::Allowed(text))
    }

    async fn update_plan(&self, steps: &[crate::coding::PlanStep]) {
        let _ = self.app.emit(
            "chat-plan",
            ChatEvent { request_id: self.request_id.to_string(), chat_id: self.chat_id.to_string(), data: CodePlanEvent { plan: steps.to_vec() } },
        );
    }
}

impl CodeAgent<'_> {
    /// Ask the UI to approve reading an outside file; resolves to the user's choice, or `false` on
    /// cancel/timeout. Never logs the file content — only that a prompt was raised.
    async fn ask_permission(&self, path: &str) -> bool {
        let request_id = format!("perm-{}", uuid::Uuid::new_v4());
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        self.app.state::<AppState>().pending_perms.lock().insert(request_id.clone(), tx);
        let _ = self.app.emit(
            "code-permission-request",
            CodePermissionRequest { request_id: request_id.clone(), chat_id: self.chat_id.to_string(), path: path.to_string() },
        );
        let allowed = tokio::select! {
            r = rx => r.unwrap_or(false),
            _ = self.cancel.cancelled() => false,
            _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => false,
        };
        // drop the sender if we bailed out on cancel/timeout (the resolver may never come)
        self.app.state::<AppState>().pending_perms.lock().remove(&request_id);
        allowed
    }
}

/// A static fetch under this many characters is treated as "thin" — likely a JS-rendered page —
/// so we re-read it with the headless renderer.
const THIN_PAGE: usize = 400;

impl crate::web_research::WebBackend for WebAgent<'_> {
    async fn complete(&self, messages: &serde_json::Value, tools: &serde_json::Value) -> AppResult<llm::Reply> {
        llm::complete_tools(&self.model.url, self.key, &self.model.id, self.effort, self.tools_max, messages, tools, crate::web_research::PER_CALL).await
    }
    async fn search_web(&self, query: &str) -> Vec<crate::websearch::WebHit> {
        // fast path: keyless HTTP search. Works until DuckDuckGo bot-blocks the IP (HTTP 202).
        if let Ok(hits) = crate::websearch::search(query, crate::web_research::SEARCH_RESULTS).await {
            if !hits.is_empty() {
                return hits;
            }
        }
        // blocked/empty → run the search in a real browser (DDG HTML endpoint, then Bing)
        crate::headless::browser_search(self.app, query, crate::web_research::SEARCH_RESULTS).await.unwrap_or_default()
    }
    async fn read_url(&self, url: &str) -> Option<String> {
        // fast path: plain HTTP fetch + readable-text pass
        let statik = crate::websearch::fetch_text(url, crate::web_research::PAGE_CHARS).await.ok();
        let thin = statik.as_ref().map(|t| t.chars().count() < THIN_PAGE).unwrap_or(true);
        // only pay the render cost when the static text looks empty/JS-rendered
        if thin {
            if let Ok(rendered) = crate::headless::render_text(self.app, url, crate::web_research::PAGE_CHARS).await {
                return Some(match statik {
                    Some(s) if s.chars().count() >= rendered.chars().count() => s,
                    _ => rendered,
                });
            }
        }
        statik
    }
}

/// General-chat prompt: a plain, helpful assistant with no documents and no citation rules.
/// Keeps the chat's own history and summary, but never mentions "เอกสารที่เกี่ยวข้อง".
pub fn build_chat_prompt(history: &[(String, String)], summary: Option<&str>, attachments: Option<&str>, now: Option<&str>, question: &str) -> Vec<ChatMessage> {
    let mut system = String::from(
        "คุณคือผู้ช่วยทั่วไป พูดคุยและช่วยงานได้หลากหลาย\n\
         - ตอบเป็นภาษาเดียวกับคำถาม\n\
         - ใช้ความรู้ทั่วไปของคุณตอบได้ ตอบให้กระชับ ตรงประเด็น\n\
         - ใช้ **ตัวหนา** กับคำสำคัญ และใช้โค้ดบล็อกเมื่อเหมาะสม\n\
         - โหมดนี้ไม่ได้ค้นเอกสารภายในองค์กร ถ้าคำถามต้องอ้างอิงข้อมูล/นโยบายของบริษัทโดยเฉพาะ \
         ให้บอกผู้ใช้ว่าควรสลับไปโหมด \"คุยจากเอกสาร\" เพื่อให้ได้คำตอบที่อ้างอิงเอกสารจริง\n\
         - ถ้าไม่แน่ใจหรือไม่รู้ ให้บอกตรงๆ อย่าเดา",
    );
    if let Some(n) = now {
        system.push_str(&format!("\n- วันเวลาปัจจุบัน (ตามนาฬิกาเครื่องผู้ใช้): {n} — ใช้ตอบคำถามเกี่ยวกับวัน/เวลาได้"));
    }
    if attachments.is_some() {
        system.push_str(
            "\n- ผู้ใช้แนบไฟล์มากับคำถามนี้ ข้อความในส่วน \"ไฟล์แนบ\" คือเนื้อหาที่ดึงจากไฟล์ \
             (รูปภาพผ่าน OCR จึงอาจมีคำผิดบ้าง) ให้ใช้เนื้อหานั้นประกอบการตอบ \
             ถ้าไฟล์ไหนอ่านไม่ได้ให้บอกผู้ใช้ตรงๆ",
        );
    }
    if let Some(s) = summary {
        system.push_str(&crate::memory::system_note(s));
    }
    let mut msgs = vec![ChatMessage::new("system", system)];
    for (q, a) in history {
        msgs.push(ChatMessage::new("user", q.clone()));
        msgs.push(ChatMessage::new("assistant", a.clone()));
    }
    if let Some(att) = attachments {
        msgs.push(ChatMessage::new("user", format!("ไฟล์แนบสำหรับคำถามนี้:\n{att}")));
    }
    msgs.push(ChatMessage::new("user", question.to_string()));
    msgs
}

/// Web-research prompt: answer from the collected web sources, citing them with [n] → URL. Used
/// by the "ค้นเว็บ" toggle in a general chat.
pub fn build_web_prompt(history: &[(String, String)], summary: Option<&str>, attachments: Option<&str>, now: Option<&str>, sources: &[SourceRef], question: &str) -> Vec<ChatMessage> {
    let mut system = String::from(
        "คุณคือผู้ช่วยที่ตอบคำถามจากผลการค้นเว็บด้านล่าง\n\
         - ตอบเป็นภาษาเดียวกับคำถาม ตอบกระชับ ตรงประเด็น ใช้ **ตัวหนา** กับคำสำคัญได้\n\
         - ใช้ข้อมูลจาก \"แหล่งจากเว็บ\" เป็นหลัก อ้างอิงด้วย [1], [2] ตามหมายเลขแหล่ง ต่อท้ายข้อความที่ใช้ และห้ามสร้างหมายเลขที่ไม่มีอยู่\n\
         - จัดระเบียบให้อ่านง่าย: ถ้าเป็นรายการที่มีเวลา/ลำดับ (ตารางแข่ง กำหนดการ ขั้นตอน) ให้จัดกลุ่มตามเวลา/หมวดแล้วเรียงให้ชัด ใช้หัวข้อย่อยหรือบุลเลตเมื่อช่วยให้อ่านง่าย\n\
         - ดึงรายละเอียดที่เป็นรูปธรรมจากแหล่งมาให้ครบ: ตัวเลข เวลา ช่องถ่ายทอด ราคา/เปอร์เซ็นต์ สถิติ ฟอร์มล่าสุด อันดับ — อย่าตัดทิ้ง · แต่ห้ามแต่งตัวเลขที่แหล่งไม่มี (เช่น % โอกาสชนะ ใส่ได้เฉพาะที่ปรากฏในแหล่ง ถ้าไม่มีก็ไม่ต้องเดา)\n\
         - ถ้าคำถามให้เลือกหรือแนะนำ (เช่น \"อันไหนน่าดู/ดีที่สุด\") ให้ปิดท้ายด้วยข้อสรุป \"ที่แนะนำ\" ให้ชัดเจน พร้อมเหตุผลสั้น ๆ อิงจากแหล่ง\n\
         - ข้อมูลจากเว็บอาจไม่ถูกต้องเสมอไป ถ้าแหล่งขัดกันให้บอก และเตือนให้ผู้ใช้ตรวจแหล่งต้นทางก่อนใช้งานจริง\n\
         - ถ้าผลการค้นไม่พอจะตอบ ให้บอกตรงๆ ว่ายังไม่พบข้อมูลที่ชัดเจนจากเว็บ อย่าเดา",
    );
    if let Some(n) = now {
        system.push_str(&format!("\n- วันเวลาปัจจุบัน: {n}"));
    }
    if attachments.is_some() {
        system.push_str("\n- ผู้ใช้แนบไฟล์มาด้วย (ส่วน \"ไฟล์แนบ\") ใช้ประกอบการตอบได้");
    }
    if let Some(s) = summary {
        system.push_str(&crate::memory::system_note(s));
    }
    let mut msgs = vec![ChatMessage::new("system", system)];
    for (q, a) in history {
        msgs.push(ChatMessage::new("user", q.clone()));
        msgs.push(ChatMessage::new("assistant", a.clone()));
    }
    if let Some(att) = attachments {
        msgs.push(ChatMessage::new("user", format!("ไฟล์แนบสำหรับคำถามนี้:\n{att}")));
    }
    let mut user = format!("คำถาม: {question}\n\nแหล่งจากเว็บ:\n");
    if sources.is_empty() {
        user.push_str("(ค้นเว็บไม่พบผล หรือค้นไม่สำเร็จ)\n");
    }
    for s in sources {
        user.push_str(&format!("[{}] {} ({})\n{}\n\n", s.n, s.file_name, s.doc_path, s.text));
    }
    msgs.push(ChatMessage::new("user", user));
    msgs
}

/// Coding summary prompt: used only when the coding loop ended without its own final text (hit a
/// limit/cancel). Asks the model to summarize the files it changed. The normal path delivers the
/// loop's own last message instead.
pub fn build_code_prompt(history: &[(String, String)], summary: Option<&str>, now: Option<&str>, changeset: &crate::codefs::Changeset, question: &str, stop: Option<&str>) -> Vec<ChatMessage> {
    let mut system = String::from(
        "คุณเพิ่งพยายามแก้โค้ดในโปรเจคของผู้ใช้ตามคำขอ ด้านล่างคือรายการไฟล์ที่เปลี่ยน\n\
         - สรุปสั้น ๆ ว่าแก้อะไรไปบ้าง และถ้าทำไม่ครบให้บอกตรง ๆ ว่าติดอะไร ตอบเป็นภาษาเดียวกับคำขอ\n\
         - ถ้ามีบรรทัด \"เหตุที่หยุด\" ให้บอกเหตุนั้นตรง ๆ ตามที่ระบุ ห้ามเดาสาเหตุอื่นเอง (เช่น อย่าบอกว่าเขียนไฟล์ล้มเหลวถ้ารายการไฟล์ที่เปลี่ยนเป็นค่าว่าง = ยังไม่เคยพยายามเขียน) · ถ้าเหตุคือ Gateway ให้บอกผู้ใช้ลองส่งคำสั่งใหม่อีกครั้ง\n\
         - เตือนให้ผู้ใช้ตรวจ diff ก่อนใช้ (มีปุ่มย้อนกลับถ้าไม่ต้องการ)\n\
         - ใช้ **ตัวหนา** กับชื่อไฟล์ได้ อย่าแปะเนื้อไฟล์ทั้งหมดซ้ำ",
    );
    if let Some(n) = now {
        system.push_str(&format!("\n- วันเวลาปัจจุบัน: {n}"));
    }
    if let Some(s) = summary {
        system.push_str(&crate::memory::system_note(s));
    }
    let mut msgs = vec![ChatMessage::new("system", system)];
    for (q, a) in history {
        msgs.push(ChatMessage::new("user", q.clone()));
        msgs.push(ChatMessage::new("assistant", a.clone()));
    }
    let mut user = format!("คำขอ: {question}\n\nไฟล์ที่เปลี่ยน (ราก {}):\n", changeset.root);
    if changeset.items.is_empty() {
        user.push_str("(ยังไม่ได้แก้ไฟล์ใด)\n");
    }
    for c in &changeset.items {
        let act = if c.kind == "create" { "สร้าง" } else { "แก้" };
        user.push_str(&format!("- {act} {}\n", c.path));
    }
    if let Some(reason) = stop {
        user.push_str(&format!("\nเหตุที่หยุด: {reason}\n"));
    }
    msgs.push(ChatMessage::new("user", user));
    msgs
}

#[allow(clippy::too_many_arguments)]
pub fn build_prompt(svc: &Service, sources: &[SourceRef], history: &[(String, String)], summary: Option<&str>, question: &str, all: bool, now: Option<&str>, attachments: Option<&str>) -> Vec<ChatMessage> {
    let mut system = String::from(
        "คุณคือผู้ช่วยตอบคำถามจากเอกสารภายในองค์กร\n\
         - ตอบเป็นภาษาเดียวกับคำถาม\n\
         - ใช้เฉพาะข้อมูลจาก \"เอกสารที่เกี่ยวข้อง\" ในข้อความล่าสุดเท่านั้น ห้ามใช้ความรู้ภายนอก\n\
         - ถ้าเอกสารไม่มีคำตอบ ให้บอกตรงๆ ว่าไม่พบข้อมูลนี้ในเอกสาร\n\
         - อ้างอิงแหล่งด้วย [1], [2] ตามหมายเลขท่อนเอกสาร ต่อท้ายข้อความที่ใช้ข้อมูลนั้น และห้ามสร้างหมายเลขที่ไม่มีอยู่\n\
         - ใช้ **ตัวหนา** กับคำสำคัญได้ ตอบกระชับและเป็นธรรมชาติ\n\
         - ตอบให้ยืดหยุ่นตามเนื้อคำถาม ไม่ต้องใช้โครงหัวข้อตายตัวทุกครั้ง · คำถามตรง ๆ หรือถามข้อมูลสั้น ๆ ก็ตอบสั้น ๆ ได้เลย · ใช้หัวข้อหรือลำดับขั้นเฉพาะตอนที่ช่วยให้อ่านเข้าใจง่ายขึ้นจริง (เช่นปัญหาซับซ้อนหลายสาเหตุ) อย่าใส่หัวข้อว่าง ๆ หรือยืดให้ยาวเกินจำเป็น\n\
         - สิ่งที่มักช่วยได้เมื่อเป็นปัญหาในระบบคือ สาเหตุที่น่าจะเป็น (เรียงจากน่าจะเป็นมากสุด), SQL/วิธีตรวจข้อมูล, และวิธีแก้ — หยิบมาเท่าที่คำถามต้องการ ไม่ต้องครบทุกอย่างและไม่ต้องตามลำดับตายตัว\n\
         - เมื่อเสนอ SQL ให้อยู่ในบล็อก ```sql บอกว่าใช้ตรวจอะไร และที่มา: ถ้ามีในเอกสารให้คัดลอกตรงตัวแล้วอ้าง [n] · ถ้าไม่มีให้เขียนขึ้นได้โดยใช้เฉพาะชื่อตารางและคอลัมน์ที่ปรากฏในเอกสาร (เช่นท่อนโครงสร้างตาราง schema) อ้าง [n] ของตารางที่ใช้ และกำกับว่า \"เขียนขึ้นจากโครงสร้างตาราง ยังไม่เคยใช้จริง ตรวจก่อนรัน\" · ใช้ SELECT เป็นหลัก · ค่าที่ต้องแทนเขียนเป็น {EMPLID}, {YYYY-MM-DD} แล้วบอกว่าต้องแทนอะไร\n\
         - ห้ามแต่งชื่อตาราง คอลัมน์ หรือค่ารหัสที่ไม่มีในเอกสาร ถ้าเอกสารไม่พอจะเขียนคิวรี่ ให้บอกว่าต้องรู้ตารางหรือคอลัมน์อะไรเพิ่ม\n\
         - ถ้าเป็นคำสั่งที่แก้ข้อมูล (UPDATE, DELETE, INSERT, MERGE) ให้เตือนว่าเปลี่ยนข้อมูลจริง ให้ SELECT ตรวจเงื่อนไขก่อน และตรวจผลก่อน COMMIT\n\
         - ท่อนที่ขึ้นต้นว่า \"บันทึกความรู้\" คือวิธีที่เคยใช้แก้ปัญหานี้ได้ในแชตก่อน ใช้เป็นแนวทางได้ (อ้าง [n] เหมือนท่อนอื่น) \
         แต่ถ้าขัดกับเอกสาร ให้ยึดเอกสาร และบอกว่าบันทึกขัดกับเอกสารตรงไหน",
    );
    if let Some(n) = now {
        system.push_str(&format!("\n- วันเวลาปัจจุบัน (ตามนาฬิกาเครื่องผู้ใช้): {n} — ใช้ตอบคำถามเกี่ยวกับวัน/เวลาได้"));
    }
    if attachments.is_some() {
        system.push_str(
            "\n- ผู้ใช้แนบไฟล์มากับคำถามนี้ (ส่วน \"ไฟล์แนบ\") ข้อความนั้นดึงจากไฟล์ (รูปผ่าน OCR) \
             ใช้ประกอบคำตอบได้เพิ่มจาก \"เอกสารที่เกี่ยวข้อง\" ถ้าอ้างอิงเนื้อหาจากไฟล์แนบให้บอกว่ามาจากไฟล์แนบ",
        );
    }
    if !svc.system_prompt.trim().is_empty() {
        system.push_str("\n\nคำแนะนำเพิ่มเติมของ service นี้:\n");
        system.push_str(svc.system_prompt.trim());
    }
    if let Some(s) = summary {
        system.push_str(&crate::memory::system_note(s));
    }
    let mut msgs = vec![ChatMessage::new("system", system)];
    for (q, a) in history {
        msgs.push(ChatMessage::new("user", q.clone()));
        msgs.push(ChatMessage::new("assistant", a.clone()));
    }
    if let Some(att) = attachments {
        msgs.push(ChatMessage::new("user", format!("ไฟล์แนบสำหรับคำถามนี้:\n{att}")));
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
pub async fn open_source_file(app: AppHandle, path: String, service_id: Option<String>) -> AppResult<()> {
    let mut p = PathBuf::from(&path);
    if !p.is_file() || !is_supported(&p) {
        // a shared document whose file is on another PC: fetch the original from the server
        let shared = service_id.as_deref().and_then(|sid| app.state::<AppState>().services.lock().get(sid).ok()).filter(|s| s.shared);
        match shared {
            Some(s) => {
                p = crate::team::fetch_original(&s.id, &path)
                    .await
                    .map_err(|e| if e.status == Some(404) { AppError::new("not_found", "ไฟล์ต้นฉบับยังไม่อยู่บนเซิร์ฟเวอร์ (เครื่องที่มีไฟล์ยังไม่ได้ซิงก์ หรือไฟล์ใหญ่เกิน 50 MB)") } else { e })?;
            }
            None => return Err(AppError::new("not_found", "ไม่พบไฟล์ต้นฉบับ (อาจถูกย้ายหรือลบ)")),
        }
    }
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(&p).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&p).spawn();
    r.map(|_| ()).map_err(|e| AppError::new("open", format!("เปิดไฟล์ไม่ได้: {e}")))
}

/// Open a web source (from "ค้นเว็บ") in the system default browser. http/https only.
#[tauri::command]
pub fn open_url(url: String) -> AppResult<()> {
    let u = url::Url::parse(&url).map_err(|_| AppError::new("invalid", "ลิงก์ไม่ถูกต้อง"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(AppError::new("invalid", "เปิดได้เฉพาะลิงก์ http/https"));
    }
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(u.as_str()).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(u.as_str()).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(u.as_str()).spawn();
    r.map(|_| ()).map_err(|e| AppError::new("open", format!("เปิดลิงก์ไม่ได้: {e}")))
}

/// Open the "เขียนโค้ด" project folder in the OS file manager. `path` is the chat's chosen project
/// folder (picked earlier through the directory dialog), so it is checked to still be a real folder.
#[tauri::command]
pub fn open_project_folder(path: String) -> AppResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err(AppError::new("not_found", "ไม่พบโฟลเดอร์โปรเจค (อาจถูกย้ายหรือลบ)"));
    }
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(&p).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&p).spawn();
    r.map(|_| ()).map_err(|e| AppError::new("open", format!("เปิดโฟลเดอร์ไม่ได้: {e}")))
}

/// Open the "เขียนโค้ด" project folder in VS Code (the `code` command on PATH). On Windows `code` is a
/// `.cmd` shim, so it is launched through `cmd /C` with no console window; stdio is detached so nothing
/// blocks. A missing `code` becomes a friendly message instead of a raw OS error.
#[tauri::command]
pub fn open_in_editor(path: String) -> AppResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err(AppError::new("not_found", "ไม่พบโฟลเดอร์โปรเจค (อาจถูกย้ายหรือลบ)"));
    }
    use std::process::Stdio;
    let null = || (Stdio::null(), Stdio::null(), Stdio::null());
    #[cfg(windows)]
    let r = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let (i, o, e) = null();
        std::process::Command::new("cmd")
            .arg("/C").arg("code").arg(&p)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(i).stdout(o).stderr(e)
            .spawn()
    };
    #[cfg(not(windows))]
    let r = {
        let (i, o, e) = null();
        std::process::Command::new("code").arg(&p).stdin(i).stdout(o).stderr(e).spawn()
    };
    r.map(|_| ()).map_err(|_| AppError::new(
        "no_editor",
        "เปิด VS Code ไม่ได้ — ตรวจว่าติดตั้ง VS Code แล้ว และมีคำสั่ง code ใน PATH (ใน VS Code กด Ctrl+Shift+P → \"Shell Command: Install 'code' command in PATH\")",
    ))
}

/// Undo the files a "เขียนโค้ด" turn changed: restore each original, delete files it created.
/// Loads the changeset saved with that assistant message.
#[tauri::command]
pub fn undo_code_changes(state: S<'_>, chat_id: String, message_id: i64) -> AppResult<crate::codefs::UndoResult> {
    let changeset = {
        let db = state.chats.lock();
        db.changeset(&chat_id, message_id)?
    }
    .ok_or_else(|| AppError::new("not_found", "ไม่พบรายการแก้ไขของข้อความนี้"))?;
    let ws = crate::codefs::Workspace::new(&changeset.root)?;
    Ok(ws.undo(&changeset.items))
}

/// Save an answer as a file the user picked in the Save dialog. `content` is the answer
/// Markdown already on screen, `format` is `"md" | "txt" | "csv" | "xlsx"`, and `dest` is
/// the full path chosen on the frontend. Returns the path written. The answer text is never
/// logged — only built into bytes and written where the user asked.
#[tauri::command]
pub fn export_answer(content: String, format: String, dest: String) -> AppResult<String> {
    let bytes = crate::export::build(&content, &format)?;
    std::fs::write(&dest, &bytes)?;
    Ok(dest)
}

/// The user's answer to an "Allow reading this outside file?" prompt from the coding agent.
#[tauri::command]
pub fn resolve_code_permission(state: S<'_>, request_id: String, allow: bool) -> AppResult<()> {
    if let Some(tx) = state.pending_perms.lock().remove(&request_id) {
        let _ = tx.send(allow);
    }
    Ok(())
}

// ------------------------------------------------------------------ knowledge

/// Where notes of this service go and how many there are.
#[tauri::command]
pub fn knowledge_info(state: S<'_>, service_id: String) -> AppResult<crate::knowledge::KnowledgeInfo> {
    let svc = state.services.lock().get(&service_id)?;
    let dir = if svc.knowledge_dir.is_empty() { crate::services::default_knowledge_dir(&svc.name) } else { PathBuf::from(&svc.knowledge_dir) };
    Ok(crate::knowledge::KnowledgeInfo { notes: crate::knowledge::count_notes(&dir), dir: dir.to_string_lossy().into_owned() })
}

const DRAFT_MAX_TOKENS: u64 = 12_000;
const DRAFT_LIMIT: Duration = Duration::from_secs(240);
/// The note draft in progress (one dialog at a time); closing the dialog cancels it.
static DRAFT_CANCEL: Mutex<Option<CancellationToken>> = Mutex::new(None);

#[tauri::command]
pub fn cancel_knowledge_draft() {
    if let Some(t) = DRAFT_CANCEL.lock().take() {
        t.cancel();
    }
}

/// Let the model write a note (a draft or a merge), streamed: a long non-streamed reply hit
/// the client's idle timeout and looked frozen. The dialog sees the text grow
/// ("knowledge-progress") and closing it cancels this.
async fn stream_note(app: &AppHandle, model: &ModelConfig, key: &str, messages: &[ChatMessage], too_long: &str) -> AppResult<String> {
    // notes are short: a small output cap and low effort keep a thinking model from running on
    let effort = (model.reasoning_effort_format == "chat-completions" && model.supports_reasoning_effort.iter().any(|e| e == "low")).then_some("low");
    let req = llm::Request { url: &model.url, key, model: &model.id, effort, max_tokens: model.max_output_tokens.min(DRAFT_MAX_TOKENS), messages };
    let token = CancellationToken::new();
    if let Some(old) = DRAFT_CANCEL.lock().replace(token.clone()) {
        old.cancel();
    }
    let mut last_emit = Instant::now();
    let mut chars = 0usize;
    let streamed = tokio::time::timeout(DRAFT_LIMIT, llm::stream(&req, &token, |d| {
        chars += d.chars().count();
        if last_emit.elapsed() >= Duration::from_millis(400) {
            last_emit = Instant::now();
            let _ = app.emit("knowledge-progress", chars);
        }
    }))
    .await;
    match streamed {
        Err(_) => {
            token.cancel();
            Err(AppError::new("timeout", too_long))
        }
        Ok(Err(e)) => Err(e),
        Ok(Ok(StreamEnd::Cancelled(_))) => Err(AppError::new("cancelled", "ยกเลิกแล้ว")),
        Ok(Ok(StreamEnd::Finished(text))) => Ok(text),
    }
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
    let sid = chat.service_id.as_deref().ok_or_else(|| AppError::new("invalid", "แชตคุยทั่วไปไม่มีเอกสารให้บันทึกความรู้"))?;
    let svc = state.services.lock().get(sid)?;
    let last_model = messages.iter().rev().find_map(|m| m.model.clone());
    let model = {
        let models = state.models.lock();
        let pick = |id: &str| models.iter().find(|m| m.enabled && m.id == id).cloned();
        last_model.as_deref().and_then(pick).or_else(|| pick(&svc.model)).or_else(|| models.iter().find(|m| m.enabled).cloned())
    }
    .ok_or_else(|| AppError::new("no_model", "ไม่มีโมเดลที่เปิดใช้ — ตรวจในหมวดโมเดล"))?;
    let messages = crate::knowledge::draft_prompt(&transcript);
    let reply = stream_note(&app, &model, &key, &messages, "โมเดลใช้เวลานานเกิน 4 นาที — ลองอีกครั้ง หรือเลือกเฉพาะช่วงแชตที่สั้นลง").await?;
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
    // the same problem written down before? (in the background; says so with "knowledge-duplicate")
    crate::background::spawn_duplicate_check(&app, svc.id.clone(), saved.clone());
    state.indexer.start(app, svc.id.clone(), false);
    Ok(saved)
}

// ------------------------------------------------------------------ duplicate notes

/// A knowledge note's text as a passage vector, the way the index makes them: the server's
/// model, else the same model on this PC when the server is down. At most 10 seconds.
async fn embed_note(embedder: Arc<Embedder>, src: Source, text: String) -> AppResult<Vec<f32>> {
    let job = tauri::async_runtime::spawn_blocking(move || {
        let texts = [text];
        match embedder.embed_passages_from(&src, &texts) {
            Err(e) if crate::embed_server::is_down(&e) => match &src {
                Source::Server(m) => match ModelId::from_server_model(m).filter(|l| Embedder::status(*l).ready) {
                    Some(l) => embedder.embed_passages(l, &texts),
                    None => Err(e),
                },
                _ => Err(e),
            },
            r => r,
        }
    });
    let vectors = tokio::time::timeout(Duration::from_secs(10), job)
        .await
        .map_err(|_| AppError::new("timeout", "ทำ embedding ของบันทึกนานเกินไป"))?
        .map_err(|e| AppError::new("embedding", format!("ทำ embedding ไม่สำเร็จ: {e}")))??;
    vectors.into_iter().next().ok_or_else(|| AppError::new("embedding", "ไม่ได้ vector ของบันทึก"))
}

/// Notes of the service like the one at `path` (over `threshold`, pairs kept apart left out).
/// Logs counts and the best similarity only — never a title, path or text.
pub(crate) async fn find_duplicates(app: &AppHandle, service_id: &str, path: &str, threshold: f32) -> AppResult<Option<crate::knowledge::DuplicateFound>> {
    use crate::knowledge::{pick_similar, title_of_path, SimilarNote, MAX_SIMILAR};
    let state = app.state::<AppState>();
    let svc = state.services.lock().get(service_id)?;
    let opts = {
        let s = state.settings.lock();
        crate::ingest::chunker::ChunkOptions { size: s.chunk_size, overlap: s.chunk_overlap }
    };
    let p = PathBuf::from(path);
    let extracted = tauri::async_runtime::spawn_blocking(move || crate::ingest::extract_chunks(&p, opts))
        .await
        .map_err(|e| AppError::new("io", format!("อ่านบันทึกไม่ได้: {e}")))??;
    let Some(head) = extracted.chunks.into_iter().next().map(|(_, t)| t).filter(|t| !t.trim().is_empty()) else {
        return Ok(None);
    };
    let (found, compared): (Vec<SimilarNote>, Option<usize>) = if svc.shared {
        // every member's notes are on the server: compare there, passage against passage
        let got = crate::team::similar_notes(&svc.id, &head, path, threshold).await?;
        let notes = got.into_iter().map(|(p, similarity, id)| SimilarNote { title: title_of_path(&p), path: p, similarity, document_id: Some(id) }).collect();
        (notes, None)
    } else {
        let v = embed_note(state.embedder.clone(), state.embedding_source()?, head).await?;
        let heads = state.stores.get(&svc.id)?.lock().knowledge_heads()?;
        let notes = pick_similar(&heads, &v, path, threshold, MAX_SIMILAR)
            .into_iter()
            .map(|(p, similarity)| SimilarNote { title: title_of_path(&p), path: p, similarity, document_id: None })
            .collect();
        (notes, Some(heads.len()))
    };
    let kept = crate::knowledge::KeptApart::load();
    let found: Vec<SimilarNote> = found.into_iter().filter(|n| !kept.contains(path, &n.path)).collect();
    let best = found.first().map(|n| n.similarity).unwrap_or(0.0);
    match compared {
        Some(n) => crate::diag::log(format!("[{}] knowledge check: {} similar of {} note(s), best {best:.2}", svc.code, found.len(), n)),
        None => crate::diag::log(format!("[{}] knowledge check (server): {} similar, best {best:.2}", svc.code, found.len())),
    }
    if found.is_empty() {
        return Ok(None);
    }
    Ok(Some(crate::knowledge::DuplicateFound { service_id: svc.id, path: path.to_string(), title: title_of_path(path), similar: found }))
}

/// A note this PC may read for the service: a `*.knowledge.md` inside the service's folders,
/// else (shared services) the copy on the server.
async fn note_of(svc: &Service, path: &str) -> AppResult<String> {
    if !crate::ingest::is_knowledge(path) || path.contains("..") {
        return Err(AppError::new("invalid", "เปิดได้เฉพาะไฟล์บันทึกความรู้"));
    }
    let lower = path.to_lowercase();
    let inside = svc.folders.iter().chain(std::iter::once(&svc.knowledge_dir)).filter(|f| !f.is_empty()).any(|f| lower.starts_with(&f.to_lowercase()));
    let p = Path::new(path);
    if inside && p.is_file() {
        return Ok(std::fs::read_to_string(p)?);
    }
    if svc.shared {
        return crate::team::note_text(&svc.id, path).await;
    }
    Err(AppError::new("not_found", "ไม่พบไฟล์บันทึกนี้ (อาจถูกย้ายหรือซ่อนไปแล้ว)"))
}

#[tauri::command]
pub async fn read_note(app: AppHandle, service_id: String, path: String) -> AppResult<String> {
    let svc = app.state::<AppState>().services.lock().get(&service_id)?;
    note_of(&svc, &path).await
}

/// One note from two of the same problem: every fix kept, each marked ได้ผล / ยังไม่ยืนยัน /
/// ไม่ได้ผล. Nothing is saved: the user checks and edits it first.
#[tauri::command]
pub async fn merge_notes(app: AppHandle, service_id: String, a: String, b: String) -> AppResult<crate::knowledge::Draft> {
    let state = app.state::<AppState>();
    let key = secrets::require_key()?;
    let svc = state.services.lock().get(&service_id)?;
    let (ta, tb) = (note_of(&svc, &a).await?, note_of(&svc, &b).await?);
    let model = {
        let models = state.models.lock();
        models.iter().find(|m| m.enabled && m.id == svc.model).or_else(|| models.iter().find(|m| m.enabled)).cloned()
    }
    .ok_or_else(|| AppError::new("no_model", "ไม่มีโมเดลที่เปิดใช้ — ตรวจในหมวดโมเดล"))?;
    let messages = crate::knowledge::merge_prompt(&ta, &tb);
    let reply = stream_note(&app, &model, &key, &messages, "โมเดลใช้เวลานานเกิน 4 นาที — ลองอีกครั้ง").await?;
    let merged = crate::knowledge::parse_merge(&reply, &crate::knowledge::title_of_path(&a));
    crate::diag::log(format!("[{}] knowledge merge: {}", svc.code, if merged.is_some() { "drafted" } else { "empty reply" }));
    merged.ok_or_else(|| AppError::new("empty", "โมเดลไม่ได้ส่งบันทึกกลับมา — ลองอีกครั้ง"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteToHide {
    pub path: String,
    pub document_id: Option<i64>,
}

/// Save the merged note, then hide the old ones (moved aside on this PC, hidden on the
/// server) — never deleted. For a shared service the server is told first, so a failure
/// there leaves everything as it was.
#[tauri::command]
pub async fn save_merged_note(app: AppHandle, service_id: String, date: String, note: NewNote, hide: Vec<NoteToHide>) -> AppResult<String> {
    let state = app.state::<AppState>();
    let svc = state.services.lock().get(&service_id)?;
    for h in &hide {
        if !crate::ingest::is_knowledge(&h.path) || h.path.contains("..") {
            return Err(AppError::new("invalid", "ซ่อนได้เฉพาะไฟล์บันทึกความรู้"));
        }
    }
    if svc.shared {
        for h in &hide {
            let id = match h.document_id {
                Some(id) => Some(id),
                None => crate::team::document_id(&svc.id, &h.path).await?,
            };
            if let Some(id) = id {
                crate::team::set_hidden(&svc.id, id, true).await?;
            }
        }
    }
    let (svc, folders_changed) = state.services.lock().ensure_knowledge_dir(&service_id)?;
    let saved = crate::knowledge::write_note(Path::new(&svc.knowledge_dir), &note.title, &note.status, &date, "", &note.markdown)?;
    let lower: Vec<String> = svc.folders.iter().chain(std::iter::once(&svc.knowledge_dir)).filter(|f| !f.is_empty()).map(|f| f.to_lowercase()).collect();
    let mut moved = 0;
    for h in &hide {
        let p = Path::new(&h.path);
        if p.is_file() && lower.iter().any(|f| h.path.to_lowercase().starts_with(f)) {
            crate::knowledge::hide_note(p)?;
            moved += 1;
        }
    }
    crate::diag::log(format!("[{}] knowledge merge: saved 1 note, {} hidden ({} on this PC)", svc.code, hide.len(), moved));
    if folders_changed {
        state.watchers.watch(&svc);
    }
    state.indexer.start(app.clone(), svc.id.clone(), false);
    Ok(saved.to_string_lossy().into_owned())
}

/// "เก็บทั้งคู่": these two notes are different problems — do not offer them again.
#[tauri::command]
pub fn keep_notes_apart(a: String, b: String) -> AppResult<()> {
    let mut k = crate::knowledge::KeptApart::load();
    k.insert(&a, &b);
    k.save()
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
        if crate::embed_server::token_locked() {
            return Err(AppError::new("invalid", "token ฝังมากับโปรแกรม เปลี่ยนได้แค่ URL"));
        }
        secrets::set_server_token(&t)?;
    }
    { let mut c = crate::embed_server::ServerConfig::load(); c.url = base; c.save()?; }
    Ok(crate::embed_server::info())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerTest {
    pub model: String,
    pub device: String,
    #[serde(flatten)]
    pub test: EmbeddingTest,
    /// the same model on this machine, used for questions while the server is down
    pub fallback_model: Option<String>,
    pub fallback_ready: bool,
    /// cosine of the server's and the local vector for the same question (≈ 1 = interchangeable)
    pub fallback_match: Option<f32>,
}

/// `/health` for the model name, then the same three Thai sentences as the Gateway test.
#[tauri::command]
pub async fn test_embed_server(app: AppHandle) -> AppResult<ServerTest> {
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
    let test = EmbeddingTest { dims: q.len(), ms: t.elapsed().as_millis() as u64, similar: dot(&q, &p[0]), unrelated: dot(&q, &p[1]) };
    let local = ModelId::from_server_model(&h.model);
    let fallback_ready = local.is_some_and(|l| Embedder::status(l).ready);
    let mut fallback_match = None;
    if let (Some(l), true) = (local, fallback_ready) {
        let embedder = app.state::<AppState>().embedder.clone();
        let lv = embedder.embed_query_local(l, texts[0].clone()).await?;
        fallback_match = Some(dot(&q, &lv));
    }
    Ok(ServerTest {
        model: h.model,
        device: h.device,
        test,
        fallback_model: local.map(|l| l.as_str().to_string()),
        fallback_ready,
        fallback_match,
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

// ------------------------------------------------------------------ team database

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatus {
    /// this PC has the server token: may share services and make keys
    pub owner: bool,
    /// totals on the server (owner only)
    pub info: Option<crate::team::StoreInfo>,
    pub error: Option<String>,
    pub machine: String,
    pub shared_services: usize,
}

#[tauri::command]
pub async fn team_status(app: AppHandle) -> TeamStatus {
    let owner = crate::embed_server::is_owner();
    let (info, error) = if owner {
        match crate::team::info().await {
            Ok(i) => (Some(i), None),
            Err(e) => (None, Some(e.message)),
        }
    } else {
        (None, None)
    };
    let shared_services = app.state::<AppState>().services.lock().list().iter().filter(|s| s.shared).count();
    TeamStatus { owner, info, error, machine: crate::team::machine(), shared_services }
}

/// "แชร์ service นี้" on → the invite link; off → searches go back to this PC's index
/// (`delete`: the owner also removes the service from the server).
#[tauri::command]
pub async fn share_service(app: AppHandle, service_id: String, shared: bool, delete: Option<bool>) -> AppResult<Option<String>> {
    if shared {
        Ok(Some(crate::team::share(&app, &service_id).await?))
    } else {
        crate::team::unshare(&app, &service_id, delete.unwrap_or(false)).await?;
        Ok(None)
    }
}

/// "คัดลอกลิงก์เชิญ" — the link carries the service's share key (for the team only).
#[tauri::command]
pub async fn service_invite(app: AppHandle, service_id: String) -> AppResult<String> {
    crate::team::invite(&app, &service_id).await
}

/// "สร้างคีย์ใหม่": the old invite links stop working.
#[tauri::command]
pub async fn rotate_service_key(app: AppHandle, service_id: String) -> AppResult<String> {
    crate::team::rotate(&app, &service_id).await
}

/// "ออกจาก service" (a joined service): gone from this PC, kept by the team.
#[tauri::command]
pub fn leave_service(app: AppHandle, service_id: String) -> AppResult<()> {
    crate::team::leave(&app, &service_id)
}

/// "ดาวน์โหลดเอกสารทั้งหมด" of a shared service into a folder the user picked.
#[tauri::command]
pub async fn download_service_files(service_id: String, dest: String) -> AppResult<usize> {
    crate::team::download_all(&service_id, Path::new(&dest)).await
}

/// "ซิงก์ตอนนี้": push every shared service that has files on this PC.
#[tauri::command]
pub fn team_sync_now(app: AppHandle) {
    crate::team::sync_all(&app);
}

// ------------------------------------------------------------------ setup / invite links

/// A docask:// link waiting for "เชื่อมต่อ?" (host / service name only — never the key).
#[tauri::command]
pub fn connect_pending() -> Option<crate::connect::LinkInfo> {
    crate::connect::pending()
}

/// A link pasted in Settings (when clicking it did not open DocAsk).
#[tauri::command]
pub fn connect_paste(app: AppHandle, link: String) -> AppResult<crate::connect::LinkInfo> {
    crate::connect::receive(&app, &link)
}

#[tauri::command]
pub fn connect_discard() {
    crate::connect::discard();
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResult {
    pub team: TeamStatus,
    /// a joined service
    pub service: Option<Service>,
}

/// The user said yes. Setup link: save URL + server token, embed on the server.
/// Invite: add the shared service with its key.
#[tauri::command]
pub async fn connect_apply(app: AppHandle) -> AppResult<ConnectResult> {
    let pending = crate::connect::take().ok_or_else(|| AppError::new("invalid", "ไม่มีลิงก์ที่รอเชื่อมต่อ — กดลิงก์อีกครั้ง"))?;
    let service = match pending {
        crate::connect::Pending::Connect(link) => {
            let h = crate::embed_server::health(&link.url).await?;
            secrets::set_server_token(&link.token)?;
            let mut c = crate::embed_server::ServerConfig::load();
            c.url = link.url.clone();
            c.save()?;
            crate::diag::log("setup link applied");
            if app.state::<AppState>().embedding_source()? != Source::Server(h.model.clone()) {
                use_embed_server(app.clone(), h.model.clone()).await?;
            }
            None
        }
        crate::connect::Pending::Join(j) => {
            let svc = crate::team::join(&app, &j.url, &j.service, &j.key).await?;
            Some(svc)
        }
    };
    Ok(ConnectResult { team: team_status(app).await, service })
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
            model: "godev1".into(), effort: "medium".into(), top_k: 5, system_prompt: "ตอบสั้น".into(), knowledge_dir: String::new(), shared: false, created_at: 0, updated_at: 0,
        };
        let src = vec![SourceRef { n: 1, service_id: "hr".into(), service_name: "HR".into(), doc_path: "a.pdf".into(), file_name: "a.pdf".into(), page: Some(4), ord: 0, score: 0.9, rerank_score: None, keyword: false, knowledge: false, web: false, text: "ลา 10 วัน".into() }];
        let hist = vec![("q0".to_string(), "a0".to_string())];
        let m = build_prompt(&svc, &src, &hist, None, "ลาได้กี่วัน", false, None, None);
        assert!(!m[0].content.contains("สรุปบทสนทนาช่วงก่อนหน้า"));
        let with = build_prompt(&svc, &src, &hist, Some("- รหัสพนักงาน 12345"), "ลาได้กี่วัน", false, None, None);
        assert!(with[0].content.contains("สรุปบทสนทนาช่วงก่อนหน้า") && with[0].content.contains("รหัสพนักงาน 12345"));
        assert_eq!(m.len(), 4);
        assert!(m[0].content.contains("ตอบสั้น"));
        assert_eq!(m[1].content, "q0");
        assert!(m[3].content.contains("[1] a.pdf · หน้า 4"));

        let mut note = src[0].clone();
        note.file_name = "2026-11-12_OT-ไม่ขึ้น.knowledge.md".into();
        note.knowledge = true;
        let m = build_prompt(&svc, &[note], &[], None, "OT ไม่ขึ้น", false, None, None);
        assert!(m[1].content.contains("[1] บันทึกความรู้: OT ไม่ขึ้น\n"), "{}", m[1].content);
        assert!(m[0].content.contains("ให้ยึดเอกสาร"));
        // queries may be written from the schema, but must say so and come with checks
        assert!(m[0].content.contains("เขียนขึ้นจากโครงสร้างตาราง") && m[0].content.contains("วิธีตรวจข้อมูล"));
        assert!(m[0].content.contains("ห้ามแต่งชื่อตาราง คอลัมน์"));
    }
}
