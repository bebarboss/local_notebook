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
use crate::store::{NewChunk, NewImage, VectorStore};
use chunker::{chunk, normalize, section_title, ChunkOptions};
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
    /// Pictures kept for display (only when `keep_images()` is on).
    pub images: Vec<PageImage>,
}

pub struct PageImage {
    pub jpeg: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// true → a marker was placed in `text` where the picture sits, so it can be tied
    /// to the chunk around it; false → belongs to the whole page (scanned page, image file).
    pub inline: bool,
}

static KEEP_IMAGES: AtomicBool = AtomicBool::new(true);

/// Keep pictures from documents so answers can show them.
pub fn set_keep_images(on: bool) {
    KEEP_IMAGES.store(on, Ordering::Relaxed);
}

pub fn keep_images() -> bool {
    KEEP_IMAGES.load(Ordering::Relaxed)
}

/// Longest side of a stored picture (px) and JPEG quality.
const IMAGE_MAX_SIDE: u32 = 1280;
const IMAGE_QUALITY: u8 = 80;
/// Pictures smaller than this are icons/bullets — not kept, not OCR'd.
pub const MIN_IMAGE_W: u32 = 64;
pub const MIN_IMAGE_H: u32 = 24;
/// Private-use code points mark where picture N sits inside page text.
const MARKER_BASE: u32 = 0xF0000;

fn marker(i: usize) -> char {
    char::from_u32(MARKER_BASE + i as u32).unwrap_or('\u{FFFD}')
}

fn marker_index(c: char) -> Option<usize> {
    let v = c as u32;
    (MARKER_BASE..MARKER_BASE + 0xFFFE).contains(&v).then(|| (v - MARKER_BASE) as usize)
}

fn encode_jpeg(img: &::image::DynamicImage) -> Option<(Vec<u8>, u32, u32)> {
    use ::image::GenericImageView;
    let (w, h) = img.dimensions();
    if w < MIN_IMAGE_W || h < MIN_IMAGE_H {
        return None;
    }
    let small = if w.max(h) > IMAGE_MAX_SIDE { img.thumbnail(IMAGE_MAX_SIDE, IMAGE_MAX_SIDE) } else { img.clone() };
    let rgb = small.to_rgb8();
    let mut out = Vec::new();
    ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, IMAGE_QUALITY).encode_image(&rgb).ok()?;
    Some((out, rgb.width(), rgb.height()))
}

impl Page {
    pub fn new(page: Option<u32>, text: String) -> Self {
        Self { page, text, images: Vec::new() }
    }

    /// Append a picture found in the flow of the page, with its OCR text (if any).
    pub fn push_image(&mut self, img: Option<&::image::DynamicImage>, ocr_text: Option<&str>) {
        if let Some((jpeg, w, h)) = img.filter(|_| keep_images()).and_then(encode_jpeg) {
            let m = marker(self.images.len());
            self.images.push(PageImage { jpeg, w, h, inline: true });
            self.text.push('\n');
            self.text.push(m);
        }
        match ocr_text.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => self.text.push_str(&format!("\n{}\n{t}\n", pdf::IMAGE_MARK)),
            None => self.text.push('\n'),
        }
    }

    /// A picture of the whole page (scanned page, standalone image file).
    pub fn set_page_image(&mut self, img: &::image::DynamicImage) {
        if !keep_images() {
            return;
        }
        if let Some((jpeg, w, h)) = encode_jpeg(img) {
            self.images.push(PageImage { jpeg, w, h, inline: false });
        }
    }
}

/// Decode an embedded picture (DOCX/PPTX media). EMF/WMF and broken files → None.
pub fn decode_image(bytes: &[u8]) -> Option<::image::DynamicImage> {
    ::image::load_from_memory(bytes).ok()
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
pub fn collect_files(folders: &[String], files: &[String], excluded: &[String]) -> Vec<PathBuf> {
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
    let mut seen: HashSet<String> = excluded.iter().map(|x| x.to_lowercase()).collect();
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

pub struct Extracted {
    /// (page, chunk text); the index is the chunk's `ord`
    pub chunks: Vec<(Option<u32>, String)>,
    pub images: Vec<NewImage>,
}

/// Extract + chunk one file. Chunks never span pages. Picture markers are removed from
/// the chunk text and turned into (page, chunk ord) links for each kept picture.
pub fn extract_chunks(path: &Path, opts: ChunkOptions) -> AppResult<Extracted> {
    let ex = extractor_for(path).ok_or_else(|| AppError::new("unsupported", "ไม่รองรับไฟล์ชนิดนี้"))?;
    Ok(chunk_pages(ex.extract(path)?, opts))
}

pub fn chunk_pages(pages: Vec<Page>, opts: ChunkOptions) -> Extracted {
    let mut out = Extracted { chunks: Vec::new(), images: Vec::new() };
    let mut seen: HashSet<[u8; 16]> = HashSet::new();
    // last top-level section heading seen ("07 Time & Labor …"), carried across pages
    let mut section: Option<String> = None;
    for p in pages {
        let first_ord = out.chunks.len();
        let mut at: HashMap<usize, usize> = HashMap::new(); // picture → chunk ord
        let mut pending: Vec<usize> = Vec::new();
        for c in chunk(&normalize(&p.text), opts) {
            let mut clean = String::with_capacity(c.len());
            for ch in c.chars() {
                match marker_index(ch) {
                    Some(i) => pending.push(i),
                    None => clean.push(ch),
                }
            }
            let mut clean = normalize(&clean);
            if clean.is_empty() {
                continue; // markers carry over to the next chunk
            }
            // a chunk from the middle of a section gets the section's title in front, so
            // "7.4 Reported Time" is found by questions about "Time & Labor" too
            // (skipping a page-number line at the top)
            let starts_with_section = clean
                .lines()
                .find(|l| !l.trim().chars().all(|c| c.is_ascii_digit()))
                .and_then(section_title)
                .is_some();
            let next_section = clean.lines().filter_map(section_title).last().map(str::to_string);
            if let (Some(s), false) = (&section, starts_with_section) {
                clean = format!("{s} ›\n{clean}");
            }
            if next_section.is_some() {
                section = next_section;
            }
            let ord = out.chunks.len();
            for i in pending.drain(..) {
                at.entry(i).or_insert(ord);
            }
            out.chunks.push((p.page, clean));
        }
        if out.chunks.len() == first_ord {
            continue; // nothing to cite on this page → its pictures could never be shown
        }
        let last = out.chunks.len() - 1;
        for i in pending {
            at.entry(i).or_insert(last);
        }
        for (i, img) in p.images.into_iter().enumerate() {
            let digest: [u8; 16] = Sha256::digest(&img.jpeg)[..16].try_into().expect("16 bytes");
            if !seen.insert(digest) {
                continue; // same picture again (logo, repeated screenshot)
            }
            let chunk_ord = if img.inline { at.get(&i).map(|o| *o as i64) } else { None };
            out.images.push(NewImage { page: p.page, chunk_ord, w: img.w, h: img.h, jpeg: img.jpeg });
        }
    }
    out
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
                    let idle = jobs.is_empty();
                    drop(jobs);
                    if idle {
                        crate::background::on_indexing_idle(&app);
                    }
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

    pub fn any_running(&self) -> bool {
        !self.jobs.lock().is_empty()
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
    let signature = format!(
        // chunk2: heading-aware chunks with section titles (v0.6)
        "{}|{}|{}|ocr{}|img{}|chunk2",
        model.as_str(),
        opts.size,
        opts.overlap,
        u8::from(ocr::engine().is_some()),
        u8::from(keep_images())
    );
    {
        let mut s = store.lock();
        let old = s.meta("signature")?;
        let old_model = old.as_deref().and_then(|o| o.split('|').next());
        if old_model != Some(model.as_str()) {
            // other model → old vectors are useless
            s.clear()?;
        } else if full || old.as_deref() != Some(signature.as_str()) {
            // same model: re-extract every file, but keep vectors of unchanged text
            s.requeue_all()?;
        }
        s.set_meta("signature", &signature)?;
    }

    let files = collect_files(&svc.folders, &svc.files, &svc.excluded);
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

    let store_ref: &Mutex<crate::store::SqliteStore> = &store;
    let finished = index_files(store_ref, &work, opts, cancel, &|texts| state.embedder.embed_passages(model, texts), &progress)?;
    if finished {
        state.services.lock().touch(service_id);
    }
    Ok(())
}

/// One file extracted and chunked, waiting for its missing vectors.
struct Prepared {
    path: PathBuf,
    p: String,
    hash: String,
    mtime: i64,
    bytes: i64,
    doc_id: i64,
    pieces: Vec<(Option<u32>, String)>,
    images: Vec<NewImage>,
    vecs: Vec<Option<Vec<f32>>>,
    /// chunk indexes that still need a vector
    todo: Vec<usize>,
    /// how many of `todo` were handed to the embedder / came back
    sent: usize,
    embedded: usize,
}

/// Extract + chunk one file and look up the vectors it already has.
/// `None` → nothing to embed (skipped / error recorded on the document).
fn prepare(store: &Mutex<crate::store::SqliteStore>, path: &Path, mtime: i64, bytes: i64, opts: ChunkOptions) -> AppResult<Option<Prepared>> {
    let p = path.to_string_lossy().into_owned();
    let doc_id = store.lock().upsert_document(&p, "", 0, bytes, "indexing", None)?;
    let hash = match file_hash(path) {
        Ok(h) => h,
        Err(e) => {
            store.lock().set_status(doc_id, "error", Some(&e.message))?;
            return Ok(None);
        }
    };
    let Extracted { chunks: pieces, images } = match extract_chunks(path, opts) {
        Ok(c) if c.chunks.is_empty() => {
            store.lock().upsert_document(&p, &hash, mtime, bytes, "skipped", Some("ข้าม · ไม่พบข้อความในไฟล์"))?;
            return Ok(None);
        }
        Ok(c) => c,
        Err(e) if matches!(e.code.as_str(), "scanned_pdf" | "ocr_off" | "no_text") => {
            store.lock().upsert_document(&p, &hash, mtime, bytes, "skipped", Some(&e.message))?;
            return Ok(None);
        }
        Err(e) => {
            store.lock().upsert_document(&p, &hash, mtime, bytes, "error", Some(&e.message))?;
            return Ok(None);
        }
    };
    // text that was already embedded (file edited, OCR/picture setting changed) keeps its vector
    let mut known = store.lock().chunk_vectors(doc_id)?;
    let vecs: Vec<Option<Vec<f32>>> = pieces.iter().map(|(_, t)| known.remove(t)).collect();
    let todo: Vec<usize> = (0..pieces.len()).filter(|&j| vecs[j].is_none()).collect();
    Ok(Some(Prepared { path: path.to_path_buf(), p, hash, mtime, bytes, doc_id, pieces, images, vecs, todo, sent: 0, embedded: 0 }))
}

fn save(store: &Mutex<crate::store::SqliteStore>, d: Prepared) -> AppResult<()> {
    let chunks: Vec<NewChunk> = d
        .pieces
        .into_iter()
        .zip(d.vecs)
        .enumerate()
        .map(|(ord, ((page, text), emb))| NewChunk { page, ord: ord as i64, text, embedding: emb.expect("every chunk embedded") })
        .collect();
    let mut s = store.lock();
    s.upsert_document(&d.p, &d.hash, d.mtime, d.bytes, "indexing", None)?;
    s.replace_chunks(d.doc_id, &chunks, &d.images)
}

/// Chunks per embedder call (the encoder splits it further across its workers).
const EMBED_BATCH: usize = 32;
/// Files extracted ahead of the embedder.
const PREFETCH: usize = 2;

type Progress<'a> = dyn Fn(Option<&Path>, usize, usize, f32, &'static str, Option<String>) + 'a;

/// Index `work` as a pipeline: one thread extracts and chunks the next files while this
/// thread embeds, and chunks of several small files share one embedder batch so every
/// encoder worker has something to do. Files are saved in order as soon as they are
/// complete. Returns false when cancelled.
fn index_files(
    store: &Mutex<crate::store::SqliteStore>,
    work: &[(PathBuf, i64, i64)],
    opts: ChunkOptions,
    cancel: &AtomicBool,
    embed: &dyn Fn(&[String]) -> AppResult<Vec<Vec<f32>>>,
    progress: &Progress<'_>,
) -> AppResult<bool> {
    use std::collections::VecDeque;
    use std::sync::mpsc::{sync_channel, TryRecvError};
    let total = work.len();
    let extracting: Mutex<Option<PathBuf>> = Mutex::new(None);
    let (tx, rx) = sync_channel::<AppResult<Option<Prepared>>>(PREFETCH);

    std::thread::scope(|scope| {
        let ext = &extracting;
        scope.spawn(move || {
            for (path, mtime, bytes) in work {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                *ext.lock() = Some(path.clone());
                let r = prepare(store, path, *mtime, *bytes, opts);
                let failed = r.is_err();
                // receiver gone → the embedder stopped (error / cancel)
                if tx.send(r).is_err() || failed {
                    break;
                }
            }
        });

        // owned here so it is dropped on return: an extractor blocked on `send` then stops
        let rx = rx;
        let mut queue: VecDeque<Prepared> = VecDeque::new();
        let mut done = 0usize;
        let mut open = true;
        let requeue = |queue: &VecDeque<Prepared>| -> AppResult<()> {
            for d in queue {
                store.lock().upsert_document(&d.p, &d.hash, 0, d.bytes, "queued", None)?;
            }
            Ok(())
        };
        loop {
            if cancel.load(Ordering::Relaxed) {
                requeue(&queue)?;
                progress(None, done, total, 0.0, "cancelled", Some("ยกเลิกการทำ index แล้ว".into()));
                return Ok(false);
            }
            // take what the extractor already has, without waiting
            while open {
                match rx.try_recv() {
                    Ok(Ok(Some(d))) => queue.push_back(d),
                    Ok(Ok(None)) => done += 1,
                    Ok(Err(e)) => {
                        requeue(&queue)?;
                        return Err(e);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => open = false,
                }
            }
            // files whose every chunk has a vector are saved, in order
            while queue.front().is_some_and(|d| d.embedded == d.todo.len()) {
                let d = queue.pop_front().expect("front");
                save(store, d)?;
                done += 1;
            }
            let waiting: usize = queue.iter().map(|d| d.todo.len() - d.sent).sum();
            if waiting == 0 {
                if queue.is_empty() && !open {
                    break;
                }
                if open {
                    let now = extracting.lock().clone();
                    progress(now.as_deref(), done, total, 0.0, "extracting", None);
                    match rx.recv() {
                        Ok(Ok(Some(d))) => queue.push_back(d),
                        Ok(Ok(None)) => done += 1,
                        Ok(Err(e)) => {
                            requeue(&queue)?;
                            return Err(e);
                        }
                        Err(_) => open = false,
                    }
                }
                continue;
            }
            // one batch across files, front first
            let mut batch: Vec<(usize, usize)> = Vec::with_capacity(EMBED_BATCH);
            for (qi, d) in queue.iter_mut().enumerate() {
                while d.sent < d.todo.len() && batch.len() < EMBED_BATCH {
                    batch.push((qi, d.todo[d.sent]));
                    d.sent += 1;
                }
            }
            let texts: Vec<String> = batch.iter().map(|&(qi, j)| queue[qi].pieces[j].1.clone()).collect();
            let vecs = match embed(&texts) {
                Ok(v) => v,
                Err(e) => {
                    requeue(&queue)?;
                    return Err(e);
                }
            };
            for (&(qi, j), v) in batch.iter().zip(vecs) {
                queue[qi].vecs[j] = Some(v);
                queue[qi].embedded += 1;
            }
            if let Some(d) = queue.front() {
                let dp = d.embedded as f32 / d.todo.len().max(1) as f32;
                progress(Some(d.path.as_path()), done, total, dp, "embedding", None);
            }
        }
        progress(None, total, total, 1.0, "done", None);
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_chunks_txt() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.md");
        std::fs::write(&f, "หัวข้อ\n\n".to_string() + &"เนื้อหาภาษาไทยยาวๆ ".repeat(200)).unwrap();
        let c = extract_chunks(&f, ChunkOptions { size: 500, overlap: 50 }).unwrap().chunks;
        assert!(c.len() > 3);
        assert!(c.iter().all(|(p, _)| p.is_none()));
    }

    fn picture(w: u32, h: u32, shade: u8) -> ::image::DynamicImage {
        ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_pixel(w, h, ::image::Rgb([shade, 0, 0])))
    }

    #[test]
    fn pictures_follow_their_chunk() {
        set_keep_images(true);
        let opts = ChunkOptions { size: 300, overlap: 30 };
        let mut p = Page::new(None, "ส่วนแรก ".repeat(80));
        p.push_image(Some(&picture(200, 100, 1)), Some("ข้อความในรูปแรก"));
        p.text.push_str(&"ส่วนท้าย ".repeat(80));
        p.push_image(Some(&picture(200, 100, 2)), None);
        p.push_image(Some(&picture(200, 100, 1)), None); // duplicate → dropped
        p.push_image(Some(&picture(10, 10, 3)), None); // icon → not kept
        let mut scan = Page::new(Some(2), "หน้าสแกน".into());
        scan.set_page_image(&picture(300, 400, 9));
        let ex = chunk_pages(vec![p, scan], opts);

        assert!(ex.chunks.iter().all(|(_, t)| t.chars().all(|c| marker_index(c).is_none())), "markers leaked");
        assert!(ex.chunks.iter().any(|(_, t)| t.contains("ข้อความในรูปแรก")));
        assert_eq!(ex.images.len(), 3);
        let first = ex.images[0].chunk_ord.unwrap() as usize;
        assert!(ex.chunks[first].1.contains("ส่วนแรก"));
        let second = ex.images[1].chunk_ord.unwrap() as usize;
        assert!(second > first && ex.chunks[second].1.contains("ส่วนท้าย"));
        assert_eq!((ex.images[2].page, ex.images[2].chunk_ord), (Some(2), None));
        assert!(ex.images.iter().all(|i| i.jpeg.starts_with(&[0xFF, 0xD8])));

        // switched off: OCR text stays, no picture kept
        set_keep_images(false);
        let mut p = Page::new(None, "ข้อความ".into());
        p.push_image(Some(&picture(200, 100, 1)), Some("ในรูป"));
        set_keep_images(true);
        assert!(p.images.is_empty());
        assert!(p.text.contains("ในรูป"));
    }

    /// Print the pages and chunks the app makes from one file:
    /// `cargo test --lib dump_document -- --ignored --nocapture` with DOCASK_TEST_RES and DOCASK_TEST_FILE.
    #[test]
    #[ignore]
    fn dump_document() {
        let res = PathBuf::from(std::env::var("DOCASK_TEST_RES").expect("DOCASK_TEST_RES"));
        let file = PathBuf::from(std::env::var("DOCASK_TEST_FILE").expect("DOCASK_TEST_FILE"));
        ocr::init(res.join("ocr"));
        pdf::init(res.join("pdfium").join("pdfium.dll"));
        let pages = extractor_for(&file).unwrap().extract(&file).unwrap();
        let show_pages = std::env::var("DOCASK_TEST_PAGES").is_ok();
        if show_pages {
            for p in &pages {
                println!("--- page {:?}\n{}", p.page, p.text.trim());
            }
        }
        let ex = chunk_pages(pages, ChunkOptions::default());
        for (ord, (page, text)) in ex.chunks.iter().enumerate() {
            println!("===== chunk {ord} (page {page:?}, {} chars)\n{text}", text.chars().count());
        }
    }

    /// Search quality on a real SQL manual: does the right entry come first?
    /// `cargo test --release --lib bench_manual -- --ignored --nocapture` with DOCASK_TEST_RES,
    /// DOCASK_TEST_FILE (SQL_Query_Manual.pdf), DOCASK_TEST_EMBED, optional DOCASK_TEST_RERANK.
    #[test]
    #[ignore]
    fn bench_manual() {
        use crate::encoder::{Encoder, Layout, Pooling};
        use crate::store::SqliteStore;
        let res = PathBuf::from(std::env::var("DOCASK_TEST_RES").expect("DOCASK_TEST_RES"));
        let file = PathBuf::from(std::env::var("DOCASK_TEST_FILE").expect("DOCASK_TEST_FILE"));
        pdf::init(res.join("pdfium").join("pdfium.dll"));
        let ex = chunk_pages(extractor_for(&file).unwrap().extract(&file).unwrap(), ChunkOptions::default());
        let emb_dir = PathBuf::from(std::env::var("DOCASK_TEST_EMBED").expect("DOCASK_TEST_EMBED"));
        let emb = Encoder::load(&emb_dir.join("onnx").join("model_quantized.onnx"), &emb_dir, 512, false, Layout::auto()).unwrap();
        let texts: Vec<String> = ex.chunks.iter().map(|(_, t)| t.clone()).collect();
        let pv = emb.embed(&texts.iter().map(|t| format!("passage: {t}")).collect::<Vec<_>>(), Pooling::Mean, 4).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut st = SqliteStore::open(&dir.path().join("index.db")).unwrap();
        let d = st.upsert_document("C:\\m.pdf", "h", 1, 1, "indexing", None).unwrap();
        let chunks: Vec<NewChunk> = ex.chunks.iter().zip(&pv).enumerate()
            .map(|(i, ((page, t), e))| NewChunk { page: *page, ord: i as i64, text: t.clone(), embedding: e.clone() }).collect();
        st.replace_chunks(d, &chunks, &[]).unwrap();
        let rr = std::env::var("DOCASK_TEST_RERANK").ok().map(|r| PathBuf::from(r).join("mm")).and_then(|dir| {
            Encoder::load(&dir.join("onnx").join("model_quantized.onnx"), &dir, 512, true, Layout { intra: Layout::auto().intra * Layout::auto().workers, workers: 1 }).ok()
        });
        // (question, text that only the right entry contains)
        let qs = [
            ("ดูผู้อนุมัติการลาของพนักงานใช้ตารางไหน", "PS_GP_ABS_SS_STA"),
            ("ขอ SQL ดูการยกเลิกลา", "PS_FS_ABS_CANCEL"),
            ("ตรวจสอบเวลาเข้างาน clock time ของพนักงานในวันหนึ่ง", "PS_FS_RAW_PUNCH\nWHERE"),
            ("ดู OT ของพนักงานวันที่กำหนดดูตารางอะไรบ้าง", "PS_XPS_TL_OT_VW"),
            ("PS_TL_PAYABLE_TIME OT10 สถานะ SP", "OT10"),
            ("ดูสิทธิ์ role ของ user", "PSROLEUSER"),
            ("ข้อมูล consent ศาสนาตาม PDPA", "PS_GS_PDPA_REG_CON"),
            ("ดูรูปโปรไฟล์ปัจจุบันของพนักงาน", "PS_EMPL_PHOTO"),
            ("ปรับยอดเงินสดขาดเกินเป็น 0", "PS_FS_CASHIER_DAT"),
            ("ผล payroll แถบ accumulators", "PS_GP_RSLT_ACUM_VW"),
            ("หาชื่อคอลัมน์ของตาราง", "DBA_TAB_COLUMNS"),
            ("ประวัติการอบรมของพนักงาน", "PS_TRAINING"),
            ("กะการทำงาน shift table", "PS_SCH_SHIFT_TBL"),
            ("ค่าความยากลำบาก hardship WHARD", "PS_JPM_JP_ITEMS"),
            ("Retro trigger ของพนักงาน", "PS_GP_RTO_TRGR"),
            ("รายชื่อพนักงาน ณ วันที่กำหนดตามบริษัทและ paygroup", "GP_PAYGROUP = '263FS'"),
        ];
        println!("{} chunks", texts.len());
        for mode in ["vector", "hybrid", "hybrid+rerank"] {
            if mode.ends_with("rerank") && rr.is_none() {
                continue;
            }
            let mut ranks = Vec::new();
            for (q, needle) in qs {
                let qv = emb.embed(&[format!("query: {q}")], Pooling::Mean, 1).unwrap().pop().unwrap();
                let want = if mode.ends_with("rerank") { 15 } else { 5 };
                let mut hits = if mode == "vector" { st.search(&qv, want, 0.3).unwrap() } else { st.hybrid_search(&qv, q, want, 0.3).unwrap() };
                if let Some(rr) = rr.as_ref().filter(|_| mode.ends_with("rerank")) {
                    let s = rr.score_pairs(q, &hits.iter().map(|h| h.text.clone()).collect::<Vec<_>>(), 4).unwrap();
                    let mut idx: Vec<usize> = (0..hits.len()).collect();
                    idx.sort_by(|a, b| s[*b].total_cmp(&s[*a]));
                    hits = idx.into_iter().map(|i| hits[i].clone()).collect();
                }
                hits.truncate(5);
                ranks.push(hits.iter().position(|h| h.text.contains(needle)).map(|p| p + 1).unwrap_or(99));
            }
            let top1 = ranks.iter().filter(|&&r| r == 1).count();
            let top5 = ranks.iter().filter(|&&r| r <= 5).count();
            println!("{mode:<14} top-1 {top1}/{n}  in top-5 {top5}/{n}  ranks {ranks:?}", n = ranks.len());
        }
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
        for name in ["mixed.pdf", "scanned.pdf", "screen.png", "sample.docx", "sample.pptx"] {
            if !dir.join(name).exists() {
                println!("===== {name}: not found, skipped");
                continue;
            }
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

            // pictures kept for display, tied to the chunk/page they belong to
            let ex = chunk_pages(pages, ChunkOptions::default());
            for i in &ex.images {
                println!("  picture page={:?} chunk={:?} {}x{} {} KB", i.page, i.chunk_ord, i.w, i.h, i.jpeg.len() / 1024);
            }
            assert!(!ex.images.is_empty(), "{name}: no picture kept");
            if name == "sample.docx" {
                let ord = ex.images[0].chunk_ord.expect("docx picture tied to a chunk") as usize;
                assert!(ex.chunks[ord].1.contains("Job Data"), "docx picture not in the chunk with its OCR text");
            }
            if name == "sample.pptx" {
                assert_eq!(ex.images.len(), 1);
                assert_eq!(ex.images[0].page, Some(1), "pptx picture must stay on slide 1");
            }
        }
    }

    /// Where does indexing time go? `cargo test --release bench_index -- --ignored --nocapture`
    /// with DOCASK_TEST_RES, DOCASK_TEST_DIR and DOCASK_TEST_EMBED (multilingual-e5-small folder).
    #[test]
    #[ignore]
    fn bench_index() {
        use std::time::Instant;
        let res = PathBuf::from(std::env::var("DOCASK_TEST_RES").expect("DOCASK_TEST_RES"));
        let dir = PathBuf::from(std::env::var("DOCASK_TEST_DIR").expect("DOCASK_TEST_DIR"));
        let emb = PathBuf::from(std::env::var("DOCASK_TEST_EMBED").expect("DOCASK_TEST_EMBED"));
        ocr::init(res.join("ocr"));
        pdf::init(res.join("pdfium").join("pdfium.dll"));
        let t = Instant::now();
        ocr::engine().expect("ocr");
        println!("OCR model load: {:?}", t.elapsed());
        let t = Instant::now();
        let te = crate::embed::Embedder::load(ModelId::MultilingualE5Small, &emb).unwrap();
        println!("embedding model load: {:?}", t.elapsed());

        let mut all_chunks = Vec::new();
        for name in ["mixed.pdf", "scanned.pdf", "screen.png", "sample.docx", "sample.pptx"] {
            let p = dir.join(name);
            if !p.exists() {
                continue;
            }
            let t = Instant::now();
            let ex = extract_chunks(&p, ChunkOptions::default()).unwrap();
            println!("extract {name}: {:?} → {} chunks, {} pictures", t.elapsed(), ex.chunks.len(), ex.images.len());
            all_chunks.extend(ex.chunks.into_iter().map(|(_, c)| c));
        }

        // a typical text-heavy document: 100 chunks of ~1000 Thai characters
        let base: String = all_chunks.join(" ");
        let mut thai = Vec::new();
        let chars: Vec<char> = base.chars().collect();
        for i in 0..100 {
            let start = (i * 997) % chars.len().max(1);
            let s: String = chars.iter().cycle().skip(start).take(1000).collect();
            thai.push(format!("passage: {s}"));
        }
        let tok = te.token_count(&thai[0]);
        println!("tokens in one 1000-char Thai chunk: {tok} (model max 512)");
        for round in 0..2 {
            let t = Instant::now();
            te.embed(&thai, crate::encoder::Pooling::Mean, 4).unwrap();
            let e = t.elapsed();
            println!("embed 100 chunks (round {round}, {:?}): {e:?} ({:.1} chunks/s)", crate::encoder::Layout::auto(), 100.0 / e.as_secs_f64());
        }
    }

    /// Pipeline vs one file at a time: `cargo test --release --lib bench_pipeline -- --ignored --nocapture`
    /// with DOCASK_TEST_RES, DOCASK_TEST_DIR (every file in it is copied DOCASK_TEST_COPIES times,
    /// default 10) and DOCASK_TEST_EMBED.
    #[test]
    #[ignore]
    fn bench_pipeline() {
        use crate::store::SqliteStore;
        use std::time::Instant;
        let res = PathBuf::from(std::env::var("DOCASK_TEST_RES").expect("DOCASK_TEST_RES"));
        let dir = PathBuf::from(std::env::var("DOCASK_TEST_DIR").expect("DOCASK_TEST_DIR"));
        let emb = PathBuf::from(std::env::var("DOCASK_TEST_EMBED").expect("DOCASK_TEST_EMBED"));
        let copies: usize = std::env::var("DOCASK_TEST_COPIES").ok().and_then(|c| c.parse().ok()).unwrap_or(10);
        ocr::init(res.join("ocr"));
        pdf::init(res.join("pdfium").join("pdfium.dll"));
        let enc = crate::embed::Embedder::load(ModelId::MultilingualE5Small, &emb).unwrap();
        let embed = |t: &[String]| enc.embed(&t.iter().map(|x| format!("passage: {x}")).collect::<Vec<_>>(), crate::encoder::Pooling::Mean, 4);

        let tmp = tempfile::tempdir().unwrap();
        let mut work = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if !p.is_file() || !is_supported(&p) {
                continue;
            }
            let t = Instant::now();
            let ex = extract_chunks(&p, ChunkOptions::default()).unwrap();
            println!("extract {:?}: {:?} → {} chunks", p.file_name().unwrap(), t.elapsed(), ex.chunks.len());
            for k in 0..copies {
                let to = tmp.path().join(format!("{k}_{}", p.file_name().unwrap().to_string_lossy()));
                std::fs::copy(&p, &to).unwrap();
                let bytes = std::fs::metadata(&to).unwrap().len() as i64;
                work.push((to, 1, bytes));
            }
        }
        println!("{} files, {:?}", work.len(), crate::encoder::Layout::auto());
        let cancel = AtomicBool::new(false);
        let quiet = |_: Option<&Path>, _: usize, _: usize, _: f32, _: &'static str, _: Option<String>| {};
        let mut counts = Vec::new();
        for mode in ["one file at a time", "pipeline"] {
            let store = Mutex::new(SqliteStore::open(&tmp.path().join(format!("{}.db", counts.len()))).unwrap());
            let t = Instant::now();
            if mode == "pipeline" {
                assert!(index_files(&store, &work, ChunkOptions::default(), &cancel, &embed, &quiet).unwrap());
            } else {
                for w in &work {
                    index_files(&store, std::slice::from_ref(w), ChunkOptions::default(), &cancel, &embed, &quiet).unwrap();
                }
            }
            let docs = store.lock().list_documents().unwrap();
            let chunks: i64 = docs.iter().map(|d| d.chunk_count).sum();
            println!("{mode}: {:?} ({chunks} chunks)", t.elapsed());
            counts.push(chunks);
        }
        assert_eq!(counts[0], counts[1]);
    }

    #[test]
    fn index_files_saves_every_file() {
        use crate::store::SqliteStore;
        let tmp = tempfile::tempdir().unwrap();
        let mut work = Vec::new();
        for i in 0..7 {
            let p = tmp.path().join(format!("{i}.txt"));
            let body = if i == 3 { String::new() } else { format!("เอกสาร {i} ").repeat(40 * (i + 1)) };
            std::fs::write(&p, body).unwrap();
            work.push((p, 5, 1));
        }
        let store = Mutex::new(SqliteStore::open(&tmp.path().join("t.db")).unwrap());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let embed = |t: &[String]| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(t.iter().map(|x| vec![x.len() as f32, 1.0]).collect())
        };
        let cancel = AtomicBool::new(false);
        let quiet = |_: Option<&Path>, _: usize, _: usize, _: f32, _: &'static str, _: Option<String>| {};
        let opts = ChunkOptions { size: 200, overlap: 20 };
        assert!(index_files(&store, &work, opts, &cancel, &embed, &quiet).unwrap());
        let docs = store.lock().list_documents().unwrap();
        assert_eq!(docs.len(), 7);
        for d in &docs {
            if d.path.ends_with("3.txt") {
                assert_eq!(d.status, "skipped");
            } else {
                assert_eq!(d.status, "ready", "{}", d.path);
                assert_eq!(d.mtime, 5);
                assert!(d.chunk_count > 0);
            }
        }
        let total: i64 = docs.iter().map(|d| d.chunk_count).sum();
        // small files share batches → fewer calls than one per file
        assert!(calls.load(Ordering::Relaxed) <= (total as usize).div_ceil(EMBED_BATCH) + 6);

        // an embedder failure leaves unfinished files queued for next time
        let store = Mutex::new(SqliteStore::open(&tmp.path().join("f.db")).unwrap());
        let fail = |_: &[String]| -> AppResult<Vec<Vec<f32>>> { Err(AppError::new("embedding", "boom")) };
        assert!(index_files(&store, &work, opts, &cancel, &fail, &quiet).is_err());
        assert!(store.lock().list_documents().unwrap().iter().all(|d| d.status != "ready"));
    }

    #[test]
    fn collects_supported_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        std::fs::write(dir.path().join("b.exe"), "x").unwrap();
        std::fs::write(dir.path().join("~$c.docx"), "x").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("d.MD"), "x").unwrap();
        let files = collect_files(&[dir.path().to_string_lossy().into_owned()], &[], &[]);
        assert_eq!(files.len(), 2);
        let removed = dir.path().join("sub").join("d.MD").to_string_lossy().to_uppercase();
        let files = collect_files(&[dir.path().to_string_lossy().into_owned()], &[], &[removed]);
        assert_eq!(files.len(), 1, "excluded file must be skipped (case-insensitive)");
    }
}
