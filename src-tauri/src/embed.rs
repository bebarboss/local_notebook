//! Embeddings come from the company Agent Gateway (`gateway_embed.rs`); nothing is
//! computed on this machine. `settings.embedding_model` = "gateway:<model>".
//! Also holds the Hugging Face download helpers the (local) reranker uses.

use crate::errors::{AppError, AppResult};
use futures_util::StreamExt;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn models_root() -> PathBuf {
    crate::config::data_dir().join("models")
}

/// The Gateway model chosen in Settings › Embedding.
pub fn gateway_model(setting: &str) -> AppResult<String> {
    match setting.strip_prefix(crate::gateway_embed::PREFIX).map(str::trim) {
        Some(m) if !m.is_empty() => Ok(m.to_string()),
        _ => Err(AppError::new(
            "no_embedding_model",
            "ยังไม่ได้เลือกโมเดล embedding บน Gateway — ไปที่ ตั้งค่า › Embedding และ Index",
        )),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingStatus {
    /// "gateway:<model>", or "" when none is chosen yet
    pub model: String,
    /// a model is chosen and the API key is saved
    pub ready: bool,
}

pub fn status(setting: &str) -> EmbeddingStatus {
    let chosen = gateway_model(setting).is_ok();
    EmbeddingStatus {
        model: if chosen { setting.to_string() } else { String::new() },
        ready: chosen && crate::secrets::get_key().ok().flatten().is_some(),
    }
}

/// Passages for the index. Blocking — call from a background thread (not a tokio task).
pub fn embed_passages(model: &str, texts: &[String]) -> AppResult<Vec<Vec<f32>>> {
    tauri::async_runtime::block_on(crate::gateway_embed::embed(model, texts))
}

/// A question to search with.
pub async fn embed_query(model: &str, q: &str) -> AppResult<Vec<f32>> {
    Ok(crate::gateway_embed::embed(model, &[q.to_string()]).await?.pop().unwrap_or_default())
}

/// Earlier versions downloaded embedding models here; they are no longer used.
pub fn remove_old_local_models() {
    for m in ["multilingual-e5-small", "multilingual-e5-base", "bge-m3"] {
        let dir = models_root().join(m);
        if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    let _ = std::fs::remove_file(crate::config::data_dir().join("embedding.json"));
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
                AppError::new("download", "ต่อ huggingface.co ไม่ได้ (เครือข่ายบริษัทอาจบล็อก) — ใช้ปุ่มเลือกโฟลเดอร์โมเดลแทน")
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
