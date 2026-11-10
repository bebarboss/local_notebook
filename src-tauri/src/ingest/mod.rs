//! Document ingestion: extract → chunk → embed → store, incrementally, in the background.

pub mod chunker;
pub mod docx;
pub mod image;
pub mod ocr;
pub mod pdf;
pub mod pptx;
pub mod text;
pub mod watcher;

use crate::commands::AppState;
use crate::embed::{Embedder, ModelId};
use crate::errors::{AppError, AppResult};
use crate::store::{NewChunk, VectorStore};
use chunker::{chunk, normalize, ChunkOptions};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

pub struct Page {
    pub page: Option<u32>,
    pub text: String,
}

/// Add a file type by implementing this and registering it in `extractors()`.
pub trait Extractor: Send + Sync {
    fn extensions(&self) -> &'static [&'static str];
    fn extract(&self, path: &Path) -> AppResult<Vec<Page>>;
}

fn extractors() -> Vec<Box<dyn Extractor>> {
    vec![
        Box::new(pdf::PdfExtractor),
        Box::new(docx::DocxExtractor),
        Box::new(pptx::PptxExtractor),
        Box::new(text::TextExtractor),
        Box::new(image::ImageExtractor),
    ]
}

pub fn extractor_for(path: &Path) -> Option<Box<dyn Extractor>> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    extractors().into_iter().find(|e| e.extensions().contains(&ext.as_str()))
}

pub fn is_supported(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    // skip Office lock files and hidden files
    !name.starts_with("~$") && !name.starts_with('.') && extractor_for(path).is_some()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub service_id: String,
    pub job_id: String,
    pub doc_path: Option<String>,
    pub file_name: Option<String>,
    pub done: usize,
    pub total: usize,
    pub doc_progress: f32,
    pub phase: &'static str,
    pub message: Option<String>,
}

/// Walk folders (recursively) + individual files and return supported files.
pub fn collect_files(folders: &[String], files: &[String]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 20 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let hidden = p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false);
                if !hidden {
                    walk(&p, depth + 1, out);
                }
            } else if ft.is_file() && is_supported(&p) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for f in folders {
        walk(Path::new(f), 0, &mut out);
    }
    for f in files {
        let p = PathBuf::from(f);
        if p.is_file() && is_supported(&p) {
            out.push(p);
        }
    }
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(p.to_string_lossy().to_lowercase()));
    out
}

fn file_hash(path: &Path) -> AppResult<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Extract + chunk one file. Chunks never span pages.
pub fn extract_chunks(path: &Path, opts: ChunkOptions) -> AppResult<Vec<(Option<u32>, String)>> {
    let ex = extractor_for(path).ok_or_else(|| AppError::new("unsupported", "ไม่รองรับไฟล์ชนิดนี้"))?;
    let pages = ex.extract(path)?;
    let mut out = Vec::new();
    for p in pages {
        for c in chunk(&normalize(&p.text), opts) {
            out.push((p.page, c));
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ manager

struct Job {
    cancel: Arc<AtomicBool>,
    rerun: bool,
    full: bool,
}

#[derive(Default)]
pub struct IndexManager {
    jobs: Mutex<HashMap<String, Job>>,
}

impl IndexManager {
    /// Start (or queue a re-run of) an index job for one service. Never blocks the UI.
    pub fn start(self: &Arc<Self>, app: AppHandle, service_id: String, full: bool) {
        {
            let mut jobs = self.jobs.lock();
            if let Some(j) = jobs.get_mut(&service_id) {
                j.rerun = true;
                j.full |= full;
                return;
            }
            jobs.insert(service_id.clone(), Job { cancel: Arc::new(AtomicBool::new(false)), rerun: false, full });
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name(format!("index-{service_id}"))
            .spawn(move || loop {
                let (cancel, full) = {
                    let mut jobs = me.jobs.lock();
                    let j = jobs.get_mut(&service_id).expect("job registered");
                    let r = (j.cancel.clone(), j.full);
                    j.full = false;
                    j.rerun = false;
                    r
                };
                let job_id = uuid::Uuid::new_v4().to_string();
                if let Err(e) = run_job(&app, &service_id, &job_id, full, &cancel) {
                    emit(&app, IndexProgress {
                        service_id: service_id.clone(), job_id: job_id.clone(), doc_path: None, file_name: None,
                        done: 0, total: 0, doc_progress: 0.0, phase: "error", message: Some(e.message),
                    });
                }
                let _ = app.emit("services-changed", ());
                let mut jobs = me.jobs.lock();
                let again = jobs.get(&service_id).map(|j| j.rerun && !j.cancel.load(Ordering::Relaxed)).unwrap_or(false);
                if !again {
                    jobs.remove(&service_id);
                    break;
                }
            })
            .expect("spawn index thread");
    }

    pub fn cancel(&self, service_id: &str) {
        if let Some(j) = self.jobs.lock().get_mut(service_id) {
            j.cancel.store(true, Ordering::Relaxed);
            j.rerun = false;
        }
    }

    pub fn cancel_all(&self) {
        for j in self.jobs.lock().values_mut() {
            j.cancel.store(true, Ordering::Relaxed);
            j.rerun = false;
        }
    }

    pub fn is_running(&self, service_id: &str) -> bool {
        self.jobs.lock().contains_key(service_id)
    }
}

fn emit(app: &AppHandle, p: IndexProgress) {
    let _ = app.emit("index-progress", p);
}

fn run_job(app: &AppHandle, service_id: &str, job_id: &str, full: bool, cancel: &AtomicBool) -> AppResult<()> {
    let state = app.state::<AppState>();
    let svc = state.services.lock().get(service_id)?;
    let settings = state.settings.lock().clone();
    let model = ModelId::parse(&settings.embedding_model)?;
    let opts = ChunkOptions { size: settings.chunk_size, overlap: settings.chunk_overlap };
    let store = state.stores.get(service_id)?;
    let progress = |doc: Option<&Path>, done: usize, total: usize, dp: f32, phase: &'static str, msg: Option<String>| {
        emit(app, IndexProgress {
            service_id: service_id.to_string(),
            job_id: job_id.to_string(),
            doc_path: doc.map(|p| p.to_string_lossy().into_owned()),
            file_name: doc.and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()),
            done, total, doc_progress: dp, phase, message: msg,
        })
    };
    progress(None, 0, 0, 0.0, "scanning", None);

    // a different embedding model (or chunk settings) invalidates every vector
    let signature = format!("{}|{}|{}|ocr{}", model.as_str(), opts.size, opts.overlap, u8::from(ocr::engine().is_some()));
    {
        let mut s = store.lock();
        let old = s.meta("signature")?;
        if full || old.as_deref() != Some(signature.as_str()) {
            s.clear()?;
            s.set_meta("signature", &signature)?;
        }
    }

    let files = collect_files(&svc.folders, &svc.files);
    let wanted: HashSet<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();

    // remove documents whose file is gone (or no longer part of this service)
    let existing = store.lock().list_documents()?;
    for d in &existing {
        if !wanted.contains(&d.path) {
            store.lock().delete_document(&d.path)?;
        }
    }

    // find new / modified files
    let mut work: Vec<(PathBuf, i64, i64)> = Vec::new();
    for p in &files {
        let Ok(meta) = std::fs::metadata(p) else { continue };
        let (mtime, bytes) = (mtime_of(&meta), meta.len() as i64);
        let path = p.to_string_lossy().into_owned();
        let prev = store.lock().get_document(&path)?;
        let unchanged = matches!(&prev, Some(d) if d.mtime == mtime && d.bytes == bytes && matches!(d.status.as_str(), "ready" | "skipped"));
        if unchanged {
            continue;
        }
        // mtime changed but content identical → just record the new mtime
        if let Some(d) = &prev {
            if d.status == "ready" && !d.hash.is_empty() && file_hash(p).ok().as_deref() == Some(d.hash.as_str()) {
                store.lock().upsert_document(&path, &d.hash, mtime, bytes, "ready", None)?;
                continue;
            }
        }
        // mtime 0 until finished, so a cancelled job is picked up next time
        let hash = prev.map(|d| d.hash).unwrap_or_default();
        store.lock().upsert_document(&path, &hash, 0, bytes, "queued", None)?;
        work.push((p.clone(), mtime, bytes));
    }

    let total = work.len();
    if total == 0 {
        progress(None, 0, 0, 1.0, "done", None);
        state.services.lock().touch(service_id);
        return Ok(());
    }

    if !Embedder::status(model).ready {
        return Err(AppError::new("no_embedding_model", "ยังไม่มีโมเดล embedding — เอกสารรอคิวไว้ ไปที่ ตั้งค่า › Embedding และ Index"));
    }

    for (i, (path, mtime, bytes)) in work.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            progress(None, i, total, 0.0, "cancelled", Some("ยกเลิกการทำ index แล้ว".into()));
            return Ok(());
        }
        let p = path.to_string_lossy().into_owned();
        progress(Some(path.as_path()), i, total, 0.0, "extracting", None);
        let doc_id = store.lock().upsert_document(&p, "", 0, *bytes, "indexing", None)?;
        let hash = match file_hash(path) {
            Ok(h) => h,
            Err(e) => {
                store.lock().set_status(doc_id, "error", Some(&e.message))?;
                continue;
            }
        };
        let pieces = match extract_chunks(path, opts) {
            Ok(c) if c.is_empty() => {
                store.lock().upsert_document(&p, &hash, *mtime, *bytes, "skipped", Some("ข้าม · ไม่พบข้อความในไฟล์"))?;
                continue;
            }
            Ok(c) => c,
            Err(e) if matches!(e.code.as_str(), "scanned_pdf" | "ocr_off" | "no_text") => {
                store.lock().upsert_document(&p, &hash, *mtime, *bytes, "skipped", Some(&e.message))?;
                continue;
            }
            Err(e) => {
                store.lock().upsert_document(&p, &hash, *mtime, *bytes, "error", Some(&e.message))?;
                continue;
            }
        };

        let mut chunks: Vec<NewChunk> = Vec::with_capacity(pieces.len());
        let n = pieces.len();
        for batch in pieces.chunks(32) {
            if cancel.load(Ordering::Relaxed) {
                store.lock().upsert_document(&p, &hash, 0, *bytes, "queued", None)?;
                progress(None, i, total, 0.0, "cancelled", Some("ยกเลิกการทำ index แล้ว".into()));
                return Ok(());
            }
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let vecs = state.embedder.embed_passages(model, &texts)?;
            for ((page, text), emb) in batch.iter().zip(vecs) {
                let ord = chunks.len() as i64;
                chunks.push(NewChunk { page: *page, ord, text: text.clone(), embedding: emb });
            }
            progress(Some(path.as_path()), i, total, chunks.len() as f32 / n as f32, "embedding", None);
        }
        {
            let mut s = store.lock();
            s.upsert_document(&p, &hash, *mtime, *bytes, "indexing", None)?;
            s.replace_chunks(doc_id, &chunks)?;
        }
    }
    state.services.lock().touch(service_id);
    progress(None, total, total, 1.0, "done", None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_chunks_txt() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.md");
        std::fs::write(&f, "หัวข้อ\n\n".to_string() + &"เนื้อหาภาษาไทยยาวๆ ".repeat(200)).unwrap();
        let c = extract_chunks(&f, ChunkOptions { size: 500, overlap: 50 }).unwrap();
        assert!(c.len() > 3);
        assert!(c.iter().all(|(p, _)| p.is_none()));
    }

    /// Real models + pdfium: `cargo test real_documents -- --ignored --nocapture`
    /// with DOCASK_TEST_RES=<src-tauri/resources> and DOCASK_TEST_DIR=<folder with mixed.pdf, scanned.pdf, screen.png>.
    #[test]
    #[ignore]
    fn real_documents() {
        let res = PathBuf::from(std::env::var("DOCASK_TEST_RES").expect("DOCASK_TEST_RES"));
        let dir = PathBuf::from(std::env::var("DOCASK_TEST_DIR").expect("DOCASK_TEST_DIR"));
        ocr::init(res.join("ocr"));
        pdf::init(res.join("pdfium").join("pdfium.dll"));
        for name in ["mixed.pdf", "scanned.pdf", "screen.png"] {
            let t = std::time::Instant::now();
            let pages = extractor_for(&dir.join(name)).unwrap().extract(&dir.join(name)).unwrap();
            println!("===== {name} ({:?})", t.elapsed());
            for p in &pages {
                println!("--- page {:?}
{}", p.page, p.text.trim());
            }
            let all: String = pages.iter().map(|p| p.text.as_str()).collect();
            assert!(all.contains("ลาออก"), "{name}: Thai text missing");
            if name == "mixed.pdf" {
                assert!(all.contains("คู่มือการแจ้งลาออก"), "Thai text layer not de-duplicated");
                assert!(all.contains("เอกสารนี้อธิบายขั้นตอน"), "Thai text layer not de-duplicated");
                assert!(all.contains("ผู้ดำเนินการ"), "SARA AM cluster not de-duplicated");
            }
            assert!(all.contains("Job Data"), "{name}: English text from image missing");
        }
    }

    #[test]
    fn collects_supported_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        std::fs::write(dir.path().join("b.exe"), "x").unwrap();
        std::fs::write(dir.path().join("~$c.docx"), "x").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("d.MD"), "x").unwrap();
        let files = collect_files(&[dir.path().to_string_lossy().into_owned()], &[]);
        assert_eq!(files.len(), 2);
    }
}
