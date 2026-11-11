//! Local embeddings on CPU (ONNX Runtime, see `encoder.rs`). Model files are NOT bundled:
//! they are downloaded from Hugging Face with progress, or picked from a folder.

use crate::config::{data_dir, read_json, write_json};
use crate::errors::{AppError, AppResult};
use crate::encoder::{Encoder, Layout, Pooling};
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelId {
    MultilingualE5Small,
    MultilingualE5Base,
    BgeM3,
}

pub struct Spec {
    pub repo: &'static str,
    pub onnx: &'static str,
    pub pooling: Pooling,
    /// e5 models need "passage: " / "query: " prefixes
    pub e5_prefix: bool,
    pub max_length: usize,
}

pub const TOKENIZER_FILES: [&str; 4] = ["tokenizer.json", "config.json", "special_tokens_map.json", "tokenizer_config.json"];

impl ModelId {
    pub fn parse(s: &str) -> AppResult<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string()))
            .map_err(|_| AppError::new("invalid", format!("ไม่รู้จักโมเดล embedding: {s}")))
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelId::MultilingualE5Small => "multilingual-e5-small",
            ModelId::MultilingualE5Base => "multilingual-e5-base",
            ModelId::BgeM3 => "bge-m3",
        }
    }

    pub fn spec(&self) -> Spec {
        match self {
            ModelId::MultilingualE5Small => Spec { repo: "Xenova/multilingual-e5-small", onnx: "onnx/model_quantized.onnx", pooling: Pooling::Mean, e5_prefix: true, max_length: 512 },
            ModelId::MultilingualE5Base => Spec { repo: "Xenova/multilingual-e5-base", onnx: "onnx/model_quantized.onnx", pooling: Pooling::Mean, e5_prefix: true, max_length: 512 },
            ModelId::BgeM3 => Spec { repo: "Xenova/bge-m3", onnx: "onnx/model_quantized.onnx", pooling: Pooling::Cls, e5_prefix: false, max_length: 1024 },
        }
    }
}

pub fn models_root() -> PathBuf {
    data_dir().join("models")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Choice {
    model: Option<String>,
    path: Option<String>,
}

fn choice_path() -> PathBuf {
    data_dir().join("embedding.json")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingStatus {
    pub model: String,
    pub ready: bool,
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub model: String,
    pub file: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
}

/// Locate the onnx file inside a model folder (several common layouts).
pub fn find_onnx(dir: &Path) -> Option<PathBuf> {
    ["onnx/model_quantized.onnx", "onnx/model_int8.onnx", "model_quantized.onnx", "onnx/model.onnx", "model.onnx"]
        .iter()
        .map(|p| dir.join(p))
        .find(|p| p.is_file())
}

fn folder_ok(dir: &Path) -> bool {
    find_onnx(dir).is_some() && dir.join("tokenizer.json").is_file()
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            total += if p.is_dir() { dir_size(&p) } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
        }
    }
    total
}

struct Loaded {
    model: ModelId,
    dir: PathBuf,
    enc: Arc<Encoder>,
}

pub struct Embedder {
    loaded: Mutex<Option<Loaded>>,
}

impl Default for Embedder {
    fn default() -> Self {
        Self { loaded: Mutex::new(None) }
    }
}

impl Embedder {
    /// Folder currently chosen for `model` (custom path or %APPDATA%\DocAsk\models\<id>).
    pub fn folder_for(model: ModelId) -> PathBuf {
        let c: Choice = read_json(&choice_path()).ok().flatten().unwrap_or_default();
        if c.model.as_deref() == Some(model.as_str()) {
            if let Some(p) = c.path {
                return PathBuf::from(p);
            }
        }
        let user = models_root().join(model.as_str());
        // `--features bundled-model`: fall back to the copy shipped next to the exe
        #[cfg(feature = "bundled-model")]
        if !folder_ok(&user) {
            if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
                let shipped = exe_dir.join("models").join(model.as_str());
                if folder_ok(&shipped) {
                    return shipped;
                }
            }
        }
        user
    }

    pub fn status(model: ModelId) -> EmbeddingStatus {
        let dir = Self::folder_for(model);
        let ready = folder_ok(&dir);
        EmbeddingStatus {
            model: model.as_str().into(),
            ready,
            path: Some(dir.to_string_lossy().into_owned()),
            size_bytes: ready.then(|| dir_size(&dir)),
        }
    }

    pub fn use_folder(&self, model: ModelId, folder: &Path) -> AppResult<EmbeddingStatus> {
        if !folder_ok(folder) {
            return Err(AppError::new(
                "model_folder",
                "โฟลเดอร์นี้ไม่มีไฟล์โมเดลที่ต้องการ (onnx/model_quantized.onnx หรือ model.onnx และ tokenizer.json)",
            ));
        }
        write_json(&choice_path(), &Choice { model: Some(model.as_str().into()), path: Some(folder.to_string_lossy().into_owned()) })?;
        *self.loaded.lock() = None;
        Ok(Self::status(model))
    }

    /// The loaded model (loads it on first use). The lock is held only to fetch it, so a
    /// question can be embedded while the indexer is busy embedding a document.
    fn encoder(&self, model: ModelId) -> AppResult<Arc<Encoder>> {
        let dir = Self::folder_for(model);
        let mut g = self.loaded.lock();
        if let Some(l) = g.as_ref() {
            if l.model == model && l.dir == dir {
                return Ok(l.enc.clone());
            }
        }
        if !folder_ok(&dir) {
            return Err(AppError::new("no_embedding_model", "ยังไม่มีโมเดล embedding — ไปที่ ตั้งค่า › Embedding และ Index เพื่อดาวน์โหลดหรือเลือกโฟลเดอร์"));
        }
        let enc = Arc::new(Self::load(model, &dir)?);
        *g = Some(Loaded { model, dir, enc: enc.clone() });
        Ok(enc)
    }

    /// Load a model folder (onnx + tokenizer).
    pub fn load(model: ModelId, dir: &Path) -> AppResult<Encoder> {
        let onnx = find_onnx(dir).ok_or_else(|| AppError::new("no_embedding_model", "ไม่พบไฟล์ onnx ของโมเดล"))?;
        Encoder::load(&onnx, dir, model.spec().max_length, false, Layout::auto())
            .map_err(|e| AppError::new("embedding_load", format!("โหลดโมเดล embedding ไม่ได้: {}", e.message)))
    }

    fn embed(&self, model: ModelId, texts: Vec<String>) -> AppResult<Vec<Vec<f32>>> {
        let enc = self.encoder(model)?;
        enc.embed(&texts, model.spec().pooling, 4)
            .map_err(|e| AppError::new("embedding", format!("ทำ embedding ไม่สำเร็จ: {}", e.message)))
    }

    /// Blocking — call from a background thread.
    pub fn embed_passages(&self, model: ModelId, texts: &[String]) -> AppResult<Vec<Vec<f32>>> {
        let p = model.spec().e5_prefix;
        self.embed(model, texts.iter().map(|t| if p { format!("passage: {t}") } else { t.clone() }).collect())
    }

    /// Blocking — call from a background thread.
    pub fn embed_query(&self, model: ModelId, q: &str) -> AppResult<Vec<f32>> {
        let t = if model.spec().e5_prefix { format!("query: {q}") } else { q.to_string() };
        Ok(self.embed(model, vec![t])?.pop().unwrap_or_default())
    }

    pub fn unload(&self) {
        *self.loaded.lock() = None;
    }
}

/// Download the model from Hugging Face into %APPDATA%\DocAsk\models\<id>\ with progress.
pub async fn download(model: ModelId, progress: impl FnMut(DownloadProgress)) -> AppResult<EmbeddingStatus> {
    let spec = model.spec();
    let dir = models_root().join(model.as_str());
    let mut files: Vec<(&str, &str, bool)> = vec![(spec.onnx, spec.onnx, true)];
    files.extend(TOKENIZER_FILES.iter().map(|f| (*f, *f, *f == "tokenizer.json")));
    let mut progress = progress;
    fetch_hf(spec.repo, "main", &files, &dir, model.as_str(), &mut progress).await?;
    // downloading resets any custom folder choice for this model
    write_json(&choice_path(), &Choice { model: Some(model.as_str().into()), path: None })?;
    let st = Embedder::status(model);
    progress(DownloadProgress { model: model.as_str().into(), file: String::new(), downloaded: st.size_bytes.unwrap_or(0), total: st.size_bytes, done: true, error: None });
    Ok(st)
}

/// Fetch `(remote path, local path, required)` files of a Hugging Face repo at `revision`
/// into `dir`, reporting progress under `label`. Files land via `.part` + rename.
pub async fn fetch_hf(
    repo: &str,
    revision: &str,
    files: &[(&str, &str, bool)],
    dir: &Path,
    label: &str,
    progress: &mut impl FnMut(DownloadProgress),
) -> AppResult<()> {
    for (remote, local, required) in files.iter().copied() {
        let file = local;
        if let Some(parent) = dir.join(local).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = format!("https://huggingface.co/{repo}/resolve/{revision}/{remote}");
        let resp = crate::llm::CLIENT.get(&url).send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                AppError::new("download", "ต่อ huggingface.co ไม่ได้ (เครือข่ายบริษัทอาจบล็อก) — ใช้ตัวเลือก (ข) เลือกโฟลเดอร์โมเดลแทน")
            } else {
                AppError::from(e)
            }
        })?;
        if !resp.status().is_success() {
            if required {
                return Err(AppError::new("download", format!("ดาวน์โหลด {file} ไม่สำเร็จ (HTTP {})", resp.status().as_u16())));
            }
            continue;
        }
        let total = resp.content_length();
        let dest = dir.join(file);
        let part = dest.with_extension("part");
        let mut out = std::fs::File::create(&part)?;
        let mut got: u64 = 0;
        let mut last_emit = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(AppError::from)?;
            out.write_all(&chunk)?;
            got += chunk.len() as u64;
            if got - last_emit > 512 * 1024 {
                last_emit = got;
                progress(DownloadProgress { model: label.into(), file: file.into(), downloaded: got, total, done: false, error: None });
            }
        }
        out.flush()?;
        drop(out);
        std::fs::rename(&part, &dest)?;
    }
    Ok(())
}
