//! Sharing services through the team database on the server (server/embed/db.py,
//! PostgreSQL + pgvector).
//!
//! - The **owner** (this PC has the server token, from a setup link) turns on "แชร์ service นี้":
//!   the service is registered on the server, gets a **share key**, and its index is synced up.
//!   "คัดลอกลิงก์เชิญ" gives `docask://join?…` with that key; "สร้างคีย์ใหม่" revokes old links.
//! - A **member** clicks the invite: the service appears here (`shared`), its key goes to the
//!   credential store. The key opens only that service — search it, add documents to it.
//! - A shared service is searched on the server. A PC that has files for it indexes them here
//!   (vectors from the server model), then syncs: new / changed documents go up with their
//!   vectors; only documents this PC uploaded are removed when they disappear here.
//!   If the server is down and this PC has a local index, it searches that instead.
//! - Chats stay on each PC.
//!
//! Texts and keys are never logged — only counts.

use crate::commands::AppState;
use crate::embed::{ModelId, Source};
use crate::embed_server::{api, api_as, credential_for, is_owner, server_url};
use crate::errors::{AppError, AppResult};
use crate::services::Service;
use crate::store::{Hit, VectorStore};
use parking_lot::Mutex;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// Chunks per upload request (the server takes up to 64).
const BATCH: usize = 64;
const SHORT: Duration = Duration::from_secs(20);
const LONG: Duration = Duration::from_secs(120);

/// Services being synced right now (one sync per service at a time).
static SYNCING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreInfo {
    pub model: String,
    pub dim: usize,
    pub documents: i64,
    pub chunks: i64,
    pub chats: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDoc {
    id: i64,
    path: String,
    hash: String,
    status: String,
    chunk_count: i64,
    indexed_by: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteService {
    id: String,
    data: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteHit {
    chunk_id: i64,
    service_id: String,
    doc_path: String,
    page: Option<u32>,
    ord: i64,
    text: String,
    score: f32,
    fused: f32,
    keyword: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub service_id: String,
    pub uploaded: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub error: Option<String>,
}

/// This PC's name, recorded on the documents it uploads.
pub fn machine() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "pc".into())
}

/// Totals over the whole database — the owner only.
pub async fn info() -> AppResult<StoreInfo> {
    api(Method::GET, "/v1/store", None, SHORT).await
}

/// The model the server's database uses (from /health: no credential needed).
async fn server_model() -> AppResult<String> {
    let h = crate::embed_server::health(&server_url()?).await?;
    Ok(h.model)
}

/// Vectors made here can go up only if they come from the server's model.
pub fn compatible(src: &Source, server_model: &str) -> AppResult<()> {
    let ok = match src {
        Source::Server(m) => m == server_model,
        Source::Local(m) => ModelId::from_server_model(server_model) == Some(*m),
        Source::Gateway(_) => false,
    };
    if ok {
        Ok(())
    } else {
        Err(AppError::new(
            "model_mismatch",
            format!("index ในเครื่องทำด้วย {} แต่ฐานข้อมูลบน server ใช้ {server_model} — เปลี่ยนไปใช้เซิร์ฟเวอร์ embedding ก่อน", src.id()),
        ))
    }
}

/// The settings a team shares (not folders: those are paths on one PC).
fn shared_settings(s: &Service) -> serde_json::Value {
    json!({ "name": s.name, "code": s.code, "color": s.color, "model": s.model, "effort": s.effort,
            "topK": s.top_k, "systemPrompt": s.system_prompt })
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn invite_link(url: &str, service_id: &str, key: &str, name: &str) -> String {
    format!("docask://join?url={}&service={}&key={}&name={}", urlencode(url), urlencode(service_id), urlencode(key), urlencode(name))
}

// ------------------------------------------------------------------ owner

fn require_owner() -> AppResult<()> {
    if is_owner() {
        Ok(())
    } else {
        Err(AppError::new("not_owner", "เครื่องนี้ไม่ใช่เจ้าของเซิร์ฟเวอร์ — แชร์หรือสร้างคีย์ได้เฉพาะเครื่องที่ตั้งค่าด้วยลิงก์ตั้งค่าของผู้ดูแล"))
    }
}

async fn new_key(service_id: &str) -> AppResult<String> {
    #[derive(Deserialize)]
    struct K {
        key: String,
    }
    let k: K = api(Method::POST, &format!("/v1/services/{service_id}/key"), None, SHORT).await?;
    crate::secrets::set_service_key(service_id, &k.key)?;
    Ok(k.key)
}

/// "แชร์ service นี้": register it on the server, make its share key, sync its index up.
pub async fn share(app: &AppHandle, service_id: &str) -> AppResult<String> {
    require_owner()?;
    let state = app.state::<AppState>();
    let svc = state.services.lock().get(service_id)?;
    compatible(&state.embedding_source()?, &server_model().await?)?;
    let _: serde_json::Value = api(Method::PUT, &format!("/v1/services/{service_id}"), Some(&json!({ "data": shared_settings(&svc) })), SHORT).await?;
    let key = match crate::secrets::get_service_key(service_id)? {
        Some(k) => k,
        None => new_key(service_id).await?,
    };
    state.services.lock().set_shared(service_id, true)?;
    let _ = app.emit("services-changed", ());
    crate::diag::log(format!("[{}] shared on the team database", svc.code));
    spawn_sync(app, vec![service_id.to_string()]);
    Ok(invite_link(&server_url()?, service_id, &key, &svc.name))
}

/// The invite link of a shared service (a key is made if it has none yet).
pub async fn invite(app: &AppHandle, service_id: &str) -> AppResult<String> {
    let svc = app.state::<AppState>().services.lock().get(service_id)?;
    let key = match crate::secrets::get_service_key(service_id)? {
        Some(k) => k,
        None => {
            require_owner()?;
            new_key(service_id).await?
        }
    };
    Ok(invite_link(&server_url()?, service_id, &key, &svc.name))
}

/// "สร้างคีย์ใหม่": old invite links stop working at once.
pub async fn rotate(app: &AppHandle, service_id: &str) -> AppResult<String> {
    require_owner()?;
    let svc = app.state::<AppState>().services.lock().get(service_id)?;
    let key = new_key(service_id).await?;
    crate::diag::log(format!("[{}] new share key", svc.code));
    Ok(invite_link(&server_url()?, service_id, &key, &svc.name))
}

/// Stop sharing on this PC (searches go back to the local index). The server keeps its copy
/// unless `delete` — then the owner removes the service and its documents there too.
pub async fn unshare(app: &AppHandle, service_id: &str, delete: bool) -> AppResult<()> {
    if delete {
        require_owner()?;
        let _: serde_json::Value = api(Method::DELETE, &format!("/v1/services/{service_id}?data=true"), None, SHORT).await?;
        crate::secrets::delete_service_key(service_id)?;
    }
    app.state::<AppState>().services.lock().set_shared(service_id, false)?;
    let _ = app.emit("services-changed", ());
    Ok(())
}

// ------------------------------------------------------------------ member

/// Accept an invite: the service appears here, its key goes to the credential store, and the
/// server becomes this PC's embedding server.
pub async fn join(app: &AppHandle, url: &str, service_id: &str, key: &str) -> AppResult<Service> {
    let state = app.state::<AppState>();
    let health = crate::embed_server::health(url).await?;
    // the invite's server becomes this PC's server (an owner keeps its own URL)
    let mut cfg = crate::embed_server::ServerConfig::load();
    if cfg.url.trim().is_empty() || !is_owner() {
        cfg.url = url.to_string();
    }
    if !is_owner() && cfg.member_service.is_none() {
        cfg.member_service = Some(service_id.to_string());
    }
    cfg.save()?;
    let r: RemoteService = api_as(key, Method::GET, &format!("/v1/services/{service_id}"), None, SHORT).await?;
    crate::secrets::set_service_key(service_id, key)?;
    let d = &r.data;
    let text = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let top_k = d.get("topK").and_then(|v| v.as_u64()).unwrap_or(5) as u32;
    let added = state.services.lock().adopt(&r.id, &text("name"), &text("code"), &text("color"), &text("model"), &text("effort"), top_k, &text("systemPrompt"))?;
    if !added {
        state.services.lock().set_shared(&r.id, true)?;
    }
    // questions must be embedded with the database's model
    if !matches!(state.embedding_source()?, Source::Server(ref m) if *m == health.model) {
        let s = {
            let mut cur = state.settings.lock();
            cur.embedding_model = format!("{}{}", crate::embed_server::PREFIX, health.model);
            cur.clone()
        };
        s.save()?;
        state.embedder.unload();
    }
    crate::diag::log("joined a shared service");
    let _ = app.emit("services-changed", ());
    let joined = state.services.lock().get(&r.id);
    joined
}

/// Leave a joined service: it disappears from this PC, its key is forgotten. The team keeps it.
pub fn leave(app: &AppHandle, service_id: &str) -> AppResult<()> {
    let state = app.state::<AppState>();
    state.services.lock().forget(service_id)?;
    crate::secrets::delete_service_key(service_id)?;
    let mut cfg = crate::embed_server::ServerConfig::load();
    if cfg.member_service.as_deref() == Some(service_id) {
        // another joined service's key can carry the embeddings
        cfg.member_service = state
            .services
            .lock()
            .list()
            .into_iter()
            .find(|s| s.shared && matches!(crate::secrets::get_service_key(&s.id), Ok(Some(_))))
            .map(|s| s.id);
        cfg.save()?;
    }
    let _ = app.emit("services-changed", ());
    Ok(())
}

// ------------------------------------------------------------------ sync

/// Upload what changed in one service's local index; delete what this PC removed.
pub async fn sync_service(app: &AppHandle, service_id: &str) -> AppResult<SyncReport> {
    {
        let mut g = SYNCING.lock();
        let set = g.get_or_insert_with(HashSet::new);
        if !set.insert(service_id.to_string()) {
            return Ok(SyncReport { service_id: service_id.into(), ..Default::default() });
        }
    }
    let r = sync_inner(app, service_id).await;
    if let Some(set) = SYNCING.lock().as_mut() {
        set.remove(service_id);
    }
    r
}

async fn sync_inner(app: &AppHandle, sid: &str) -> AppResult<SyncReport> {
    let state = app.state::<AppState>();
    let svc = state.services.lock().get(sid)?;
    let cred = credential_for(sid)?;
    compatible(&state.embedding_source()?, &server_model().await?)?;
    if is_owner() {
        let _: serde_json::Value = api(Method::PUT, &format!("/v1/services/{sid}"), Some(&json!({ "data": shared_settings(&svc) })), SHORT).await?;
    }

    let store = state.stores.get(sid)?;
    let local: Vec<_> = store.lock().list_documents()?.into_iter().filter(|d| d.status == "ready").collect();
    let remote: Vec<RemoteDoc> = api_as(&cred, Method::GET, &format!("/v1/services/{sid}/documents"), None, SHORT).await?;
    let by_path: HashMap<&str, &RemoteDoc> = remote.iter().map(|d| (d.path.as_str(), d)).collect();
    let me = machine();
    let mut report = SyncReport { service_id: sid.into(), ..Default::default() };

    for d in &local {
        if let Some(r) = by_path.get(d.path.as_str()) {
            if r.hash == d.hash && r.status == "ready" && r.chunk_count == d.chunk_count {
                report.unchanged += 1;
                continue;
            }
        }
        let chunks = store.lock().export_chunks(d.id)?;
        if chunks.is_empty() {
            continue;
        }
        let doc: RemoteDoc = api_as(
            &cred,
            Method::PUT,
            &format!("/v1/services/{sid}/documents"),
            Some(&json!({ "path": d.path, "hash": d.hash, "mtime": d.mtime, "bytes": d.bytes, "status": "indexing", "indexedBy": me })),
            SHORT,
        )
        .await?;
        for (i, part) in chunks.chunks(BATCH).enumerate() {
            let items: Vec<_> = part.iter().map(|(page, ord, text, v)| json!({ "ord": ord, "page": page, "text": text, "embedding": v })).collect();
            let _: serde_json::Value =
                api_as(&cred, Method::POST, &format!("/v1/documents/{}/chunks", doc.id), Some(&json!({ "replace": i == 0, "chunks": items })), LONG).await?;
        }
        let _: serde_json::Value = api_as(&cred, Method::PATCH, &format!("/v1/documents/{}", doc.id), Some(&json!({ "status": "ready" })), SHORT).await?;
        report.uploaded += 1;
    }

    // only documents this PC uploaded: another PC's files are not ours to remove
    let here: HashSet<&str> = local.iter().map(|d| d.path.as_str()).collect();
    for r in remote.iter().filter(|r| r.indexed_by == me && !here.contains(r.path.as_str())) {
        let path = urlencode(&r.path);
        let _: serde_json::Value = api_as(&cred, Method::DELETE, &format!("/v1/services/{sid}/documents?path={path}"), None, SHORT).await?;
        report.deleted += 1;
    }
    crate::diag::log(format!("[{}] team sync: {} up, {} deleted, {} unchanged", svc.code, report.uploaded, report.deleted, report.unchanged));
    Ok(report)
}

/// Sync in the background and tell the UI ("team-sync"); after indexing and from the button.
pub fn spawn_sync(app: &AppHandle, service_ids: Vec<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for sid in service_ids {
            let report = match sync_service(&app, &sid).await {
                Ok(r) => r,
                Err(e) => {
                    crate::diag::log(format!("team sync failed: {}", e.code));
                    SyncReport { service_id: sid, error: Some(e.message), ..Default::default() }
                }
            };
            let _ = app.emit("team-sync", report);
        }
    });
}

/// Every shared service that has files on this PC.
pub fn sync_all(app: &AppHandle) {
    let ids = app.state::<AppState>().services.lock().list().into_iter().filter(|s| s.shared && (!s.folders.is_empty() || !s.files.is_empty())).map(|s| s.id).collect();
    spawn_sync(app, ids);
}

/// After an index job: push a shared service up.
pub fn after_index(app: &AppHandle, service_id: &str) {
    if app.state::<AppState>().services.lock().get(service_id).is_ok_and(|s| s.shared) {
        spawn_sync(app, vec![service_id.to_string()]);
    }
}

/// v0.13.0 had one switch for everything: its services with files become shared services.
pub fn migrate_global_switch(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.settings.lock().team_server {
        return;
    }
    for s in state.services.lock().list() {
        if !s.folders.is_empty() || !s.files.is_empty() {
            let _ = state.services.lock().set_shared(&s.id, true);
        }
    }
    let s = {
        let mut cur = state.settings.lock();
        cur.team_server = false;
        cur.clone()
    };
    let _ = s.save();
}

// ------------------------------------------------------------------ search

/// Search shared services on the server, each with its own credential.
pub async fn search(targets: &[Service], queries: &[String], k: usize, threshold: f32, hybrid: bool) -> AppResult<Vec<(Service, Hit)>> {
    #[derive(Deserialize)]
    struct Found {
        hits: Vec<RemoteHit>,
    }
    let mut out = Vec::new();
    for s in targets {
        let cred = credential_for(&s.id)?;
        let body = json!({ "serviceIds": [s.id], "queries": queries.iter().take(4).collect::<Vec<_>>(), "k": k.clamp(1, 60), "threshold": threshold, "hybrid": hybrid });
        let found: Found = api_as(&cred, Method::POST, "/v1/search", Some(&body), Duration::from_secs(30)).await?;
        out.extend(found.hits.into_iter().filter(|h| h.service_id == s.id).map(|h| {
            (s.clone(), Hit { chunk_id: h.chunk_id, score: h.score, fused: h.fused, keyword: h.keyword, doc_path: h.doc_path, page: h.page, ord: h.ord, text: h.text })
        }));
    }
    out.sort_by(|a, b| b.1.fused.total_cmp(&a.1.fused).then(b.1.score.total_cmp(&a.1.score)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_server_model_may_go_up() {
        assert!(compatible(&Source::Server("bge-m3".into()), "bge-m3").is_ok());
        assert!(compatible(&Source::Local(ModelId::BgeM3), "bge-m3").is_ok());
        assert!(compatible(&Source::Local(ModelId::MultilingualE5Small), "bge-m3").is_err());
        assert!(compatible(&Source::Gateway("bge-m3".into()), "bge-m3").is_err());
        assert!(compatible(&Source::Server("other".into()), "bge-m3").is_err());
    }

    #[test]
    fn paths_are_encoded_for_the_query_string() {
        assert_eq!(urlencode("C:\\docs\\ใบลา v2.pdf"), "C%3A%5Cdocs%5C%E0%B9%83%E0%B8%9A%E0%B8%A5%E0%B8%B2%20v2.pdf");
        assert_eq!(urlencode("a-b_c.d~"), "a-b_c.d~");
    }

    #[test]
    fn invite_links_round_trip() {
        let l = invite_link("https://abc.trycloudflare.com", "a1b2c3", "Key_-0123456789abcdefXYZ", "HR · นโยบาย");
        let j = crate::connect::parse_join(&l).unwrap();
        assert_eq!((j.url.as_str(), j.service.as_str(), j.key.as_str(), j.name.as_str()), ("https://abc.trycloudflare.com", "a1b2c3", "Key_-0123456789abcdefXYZ", "HR · นโยบาย"));
    }

    #[test]
    fn shared_settings_leave_folders_out() {
        let s = Service {
            id: "hr".into(), name: "HR".into(), code: "HR".into(), color: "#1F4FD1".into(), folders: vec!["C:\\x".into()],
            files: vec![], excluded: vec![], model: "godev1".into(), effort: "medium".into(), top_k: 8,
            system_prompt: "p".into(), knowledge_dir: String::new(), shared: true, created_at: 0, updated_at: 0,
        };
        let v = shared_settings(&s);
        assert_eq!(v["topK"], 8);
        assert!(v.get("folders").is_none());
    }
}
