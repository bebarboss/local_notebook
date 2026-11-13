//! The team database on the embedding server (server/embed/db.py, PostgreSQL + pgvector).
//!
//! Switched on in Settings ("ใช้ฐานข้อมูลของทีมบน server"):
//! - a PC that has the files still indexes them here (vectors come from the server model), then
//!   **syncs** each service up: new / changed documents with their vectors, deletions of the
//!   documents this PC uploaded and no longer has. The same sync uploads an existing index.
//! - every PC **searches** the server, so team members without the files can ask too;
//!   if the server is down and this PC has a local index, it searches that instead.
//! - services are shared: the ones on the server appear on every PC.
//!
//! Texts are never logged — only counts.

use crate::embed::{ModelId, Source};
use crate::embed_server::api;
use crate::errors::{AppError, AppResult};
use crate::services::Service;
use crate::commands::AppState;
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

pub fn enabled(app: &AppHandle) -> bool {
    app.state::<AppState>().settings.lock().team_server
}

pub async fn info() -> AppResult<StoreInfo> {
    api(Method::GET, "/v1/store", None, SHORT).await
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
fn shared(s: &Service) -> serde_json::Value {
    json!({ "name": s.name, "code": s.code, "color": s.color, "model": s.model, "effort": s.effort,
            "topK": s.top_k, "systemPrompt": s.system_prompt })
}

/// Services on the server that this PC does not have yet become local services (no folders).
pub async fn pull_services(app: &AppHandle) -> AppResult<usize> {
    let remote: Vec<RemoteService> = api(Method::GET, "/v1/services", None, SHORT).await?;
    let state = app.state::<AppState>();
    let mut added = 0;
    for r in remote {
        let d = &r.data;
        let text = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let top_k = d.get("topK").and_then(|v| v.as_u64()).unwrap_or(5) as u32;
        if state.services.lock().adopt(&r.id, &text("name"), &text("code"), &text("color"), &text("model"), &text("effort"), top_k, &text("systemPrompt"))? {
            added += 1;
        }
    }
    if added > 0 {
        let _ = app.emit("services-changed", ());
    }
    Ok(added)
}

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
    let src = state.embedding_source()?;
    let server = info().await?;
    compatible(&src, &server.model)?;
    let _: serde_json::Value = api(Method::PUT, &format!("/v1/services/{sid}"), Some(&json!({ "data": shared(&svc) })), SHORT).await?;

    let store = state.stores.get(sid)?;
    let local: Vec<_> = store.lock().list_documents()?.into_iter().filter(|d| d.status == "ready").collect();
    let remote: Vec<RemoteDoc> = api(Method::GET, &format!("/v1/services/{sid}/documents"), None, SHORT).await?;
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
        let doc: RemoteDoc = api(
            Method::PUT,
            &format!("/v1/services/{sid}/documents"),
            Some(&json!({ "path": d.path, "hash": d.hash, "mtime": d.mtime, "bytes": d.bytes, "status": "indexing", "indexedBy": me })),
            SHORT,
        )
        .await?;
        for (i, part) in chunks.chunks(BATCH).enumerate() {
            let items: Vec<_> = part.iter().map(|(page, ord, text, v)| json!({ "ord": ord, "page": page, "text": text, "embedding": v })).collect();
            let _: serde_json::Value =
                api(Method::POST, &format!("/v1/documents/{}/chunks", doc.id), Some(&json!({ "replace": i == 0, "chunks": items })), LONG).await?;
        }
        let _: serde_json::Value = api(Method::PATCH, &format!("/v1/documents/{}", doc.id), Some(&json!({ "status": "ready" })), SHORT).await?;
        report.uploaded += 1;
    }

    // only documents this PC uploaded: another PC's files are not ours to remove
    let here: HashSet<&str> = local.iter().map(|d| d.path.as_str()).collect();
    for r in remote.iter().filter(|r| r.indexed_by == me && !here.contains(r.path.as_str())) {
        let path = urlencode(&r.path);
        let _: serde_json::Value = api(Method::DELETE, &format!("/v1/services/{sid}/documents?path={path}"), None, SHORT).await?;
        report.deleted += 1;
    }
    crate::diag::log(format!("[{}] team sync: {} up, {} deleted, {} unchanged", svc.code, report.uploaded, report.deleted, report.unchanged));
    Ok(report)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Sync in the background and tell the UI ("team-sync"); used after indexing and by the button.
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

/// After an index job: push it up when the team database is on.
pub fn after_index(app: &AppHandle, service_id: &str) {
    if enabled(app) {
        spawn_sync(app, vec![service_id.to_string()]);
    }
}

/// Search the team database. Hits come back with their service from `targets`.
pub async fn search(targets: &[Service], queries: &[String], k: usize, threshold: f32, hybrid: bool) -> AppResult<Vec<(Service, Hit)>> {
    #[derive(Deserialize)]
    struct Found {
        hits: Vec<RemoteHit>,
    }
    let ids: Vec<&str> = targets.iter().map(|s| s.id.as_str()).collect();
    let body = json!({ "serviceIds": ids, "queries": queries.iter().take(4).collect::<Vec<_>>(), "k": k.clamp(1, 60), "threshold": threshold, "hybrid": hybrid });
    let found: Found = api(Method::POST, "/v1/search", Some(&body), Duration::from_secs(30)).await?;
    Ok(found
        .hits
        .into_iter()
        .filter_map(|h| {
            let s = targets.iter().find(|s| s.id == h.service_id)?.clone();
            Some((s, Hit { chunk_id: h.chunk_id, score: h.score, fused: h.fused, keyword: h.keyword, doc_path: h.doc_path, page: h.page, ord: h.ord, text: h.text }))
        })
        .collect())
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
    fn shared_settings_leave_folders_out() {
        let s = Service {
            id: "hr".into(), name: "HR".into(), code: "HR".into(), color: "#1F4FD1".into(), folders: vec!["C:\\x".into()],
            files: vec![], excluded: vec![], model: "godev1".into(), effort: "medium".into(), top_k: 8,
            system_prompt: "p".into(), knowledge_dir: String::new(), created_at: 0, updated_at: 0,
        };
        let v = shared(&s);
        assert_eq!(v["topK"], 8);
        assert!(v.get("folders").is_none());
    }
}
