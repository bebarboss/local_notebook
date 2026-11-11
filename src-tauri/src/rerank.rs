//! Optional second-stage ranking with a cross-encoder: vector search finds candidates,
//! the reranker reads question + passage together and re-orders them.
//!
//! Model: cross-encoder/mmarco-mMiniLMv2-L12-H384-v1 (multilingual MiniLM, Apache-2.0,
//! int8 ≈ 119 MB). On a 12-question Thai test set it put the right passage first 12/12
//! times vs 9/12 for vectors alone, reranking 20 long passages in ≈ 2 s on 8 CPU threads
//! (bge-reranker-v2-m3 was equally accurate but ≈ 6× slower). Like the embedding model it
//! is downloaded on demand or picked from a folder — never bundled.

use crate::config::{data_dir, read_json, write_json};
use crate::embed::{fetch_hf, find_onnx, models_root, DownloadProgress};
use crate::encoder::{Encoder, Layout};
use crate::errors::{AppError, AppResult};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Folder name under models\ and the label used in download progress events.
pub const MODEL: &str = "mmarco-mMiniLMv2-L12";
const REPO: &str = "cross-encoder/mmarco-mMiniLMv2-L12-H384-v1";
const REVISION: &str = "1427fd652930e4ba29e8149678df786c240d8825";
const MAX_TOKENS: usize = 512;

/// How many vector hits to hand to the reranker for a final top-k.
pub fn candidates(k: usize) -> usize {
    (k * 3).clamp(15, 30)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankStatus {
    pub model: String,
    pub ready: bool,
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Choice {
    path: Option<String>,
}

fn choice_path() -> PathBuf {
    data_dir().join("reranker.json")
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

#[derive(Default)]
pub struct Reranker {
    loaded: Mutex<Option<(PathBuf, Arc<Encoder>)>>,
}

impl Reranker {
    pub fn folder() -> PathBuf {
        let c: Choice = read_json(&choice_path()).ok().flatten().unwrap_or_default();
        c.path.map(PathBuf::from).unwrap_or_else(|| models_root().join(MODEL))
    }

    pub fn status() -> RerankStatus {
        let dir = Self::folder();
        let ready = folder_ok(&dir);
        RerankStatus { model: MODEL.into(), ready, path: Some(dir.to_string_lossy().into_owned()), size_bytes: ready.then(|| dir_size(&dir)) }
    }

    pub fn use_folder(&self, folder: &Path) -> AppResult<RerankStatus> {
        if !folder_ok(folder) {
            return Err(AppError::new("model_folder", "โฟลเดอร์นี้ไม่มีไฟล์โมเดล reranker (onnx/model_quantized.onnx หรือ model.onnx และ tokenizer.json)"));
        }
        write_json(&choice_path(), &Choice { path: Some(folder.to_string_lossy().into_owned()) })?;
        self.unload();
        Ok(Self::status())
    }

    pub fn unload(&self) {
        *self.loaded.lock() = None;
    }

    fn encoder(&self) -> AppResult<Arc<Encoder>> {
        let dir = Self::folder();
        let mut g = self.loaded.lock();
        if let Some((d, e)) = g.as_ref() {
            if *d == dir {
                return Ok(e.clone());
            }
        }
        let onnx = find_onnx(&dir).ok_or_else(|| AppError::new("no_reranker", "ยังไม่มีโมเดล reranker"))?;
        let enc = Arc::new(Encoder::load(&onnx, &dir, MAX_TOKENS, true, Layout::auto())?);
        *g = Some((dir, enc.clone()));
        Ok(enc)
    }

    /// Relevance 0–1 of each passage to the question (sigmoid of the model's logit).
    /// Blocking — call from a background thread.
    pub fn score(&self, question: &str, passages: &[String]) -> AppResult<Vec<f32>> {
        let enc = self.encoder()?;
        Ok(enc.score_pairs(question, passages, 2)?.into_iter().map(|x| 1.0 / (1.0 + (-x).exp())).collect())
    }
}

/// Download the reranker from Hugging Face (pinned revision) into models\<MODEL>\.
pub async fn download(mut progress: impl FnMut(DownloadProgress)) -> AppResult<RerankStatus> {
    let dir = models_root().join(MODEL);
    let files = [
        // int8 build for AVX2 CPUs (every x64 PC from the last ~10 years)
        ("onnx/model_quint8_avx2.onnx", "onnx/model_quantized.onnx", true),
        ("tokenizer.json", "tokenizer.json", true),
        ("tokenizer_config.json", "tokenizer_config.json", false),
        ("special_tokens_map.json", "special_tokens_map.json", false),
        ("config.json", "config.json", false),
    ];
    fetch_hf(REPO, REVISION, &files, &dir, MODEL, &mut progress).await?;
    write_json(&choice_path(), &Choice { path: None })?;
    let st = Reranker::status();
    progress(DownloadProgress { model: MODEL.into(), file: String::new(), downloaded: st.size_bytes.unwrap_or(0), total: st.size_bytes, done: true, error: None });
    Ok(st)
}
