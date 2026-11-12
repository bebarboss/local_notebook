//! Transformer cross-encoder on ONNX Runtime for reranking (embeddings come from the
//! Gateway).
//!
//! Why not fastembed's runner: it runs batches in parallel with rayon *and* gives each
//! ONNX session every CPU thread, so the threads fight and batching gets slower than
//! one-at-a-time. Here one session uses a few threads and a few workers share it,
//! texts are sorted by length so batches carry little padding.

use crate::errors::{AppError, AppResult};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokenizers::{Encoding, PaddingParams, PaddingStrategy, Tokenizer, TruncationParams, TruncationStrategy};

/// How to split the CPU: (threads per inference, inferences in parallel).
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub intra: usize,
    pub workers: usize,
}

impl Layout {
    /// Leave one core for the UI; 2 threads per inference scales best for small models.
    pub fn auto() -> Self {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let usable = cores.saturating_sub(1).max(1);
        let intra = usable.min(2);
        Self { intra, workers: (usable / intra).clamp(1, 6) }
    }
}

pub struct Encoder {
    session: Session,
    tokenizer: Tokenizer,
    type_ids: bool,
    layout: Layout,
}

fn err(e: impl std::fmt::Display) -> AppError {
    AppError::new("encoder", format!("โมเดลข้อความทำงานผิดพลาด: {e}"))
}

impl Encoder {
    /// `dir` holds tokenizer.json (+ tokenizer_config.json); `onnx` is the model file.
    pub fn load(onnx: &Path, dir: &Path, max_len: usize, pairs: bool, layout: Layout) -> AppResult<Self> {
        let session = Session::builder()
            .map_err(err)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(err)?
            .with_intra_threads(layout.intra)
            .map_err(err)?
            .commit_from_file(onnx)
            .map_err(err)?;
        let type_ids = session.inputs.iter().any(|i| i.name == "token_type_ids");

        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(err)?;
        let cfg: serde_json::Value = std::fs::read(dir.join("tokenizer_config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let pad_token = cfg
            .get("pad_token")
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.get("content").and_then(|c| c.as_str()).map(str::to_string)))
            .unwrap_or_else(|| "<pad>".into());
        let pad_id = tokenizer.token_to_id(&pad_token).unwrap_or(1);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: max_len,
                // cross-encoder: keep the whole question, cut the passage
                strategy: if pairs { TruncationStrategy::OnlySecond } else { TruncationStrategy::LongestFirst },
                ..Default::default()
            }))
            .map_err(err)?;
        tokenizer.with_padding(Some(PaddingParams { strategy: PaddingStrategy::BatchLongest, pad_id, pad_token, ..Default::default() }));
        Ok(Self { session, tokenizer, type_ids, layout })
    }

    pub fn token_count(&self, text: &str) -> usize {
        self.tokenizer.encode(text, true).map(|e| e.len()).unwrap_or(0)
    }

    /// Run one padded batch; returns the first output and its shape.
    fn forward(&self, enc: &[Encoding]) -> AppResult<(Vec<f32>, Vec<usize>)> {
        let (b, n) = (enc.len(), enc[0].len());
        let mut ids = Vec::with_capacity(b * n);
        let mut mask = Vec::with_capacity(b * n);
        let mut types = Vec::with_capacity(b * n);
        for e in enc {
            ids.extend(e.get_ids().iter().map(|&x| x as i64));
            mask.extend(e.get_attention_mask().iter().map(|&x| x as i64));
            types.extend(e.get_type_ids().iter().map(|&x| x as i64));
        }
        let t = |v: Vec<i64>| Tensor::from_array(([b, n], v.into_boxed_slice())).map_err(err);
        let (ids, mask) = (t(ids)?, t(mask)?);
        let outputs = if self.type_ids {
            let types = t(types)?;
            self.session.run(ort::inputs!["input_ids" => ids, "attention_mask" => mask, "token_type_ids" => types].map_err(err)?)
        } else {
            self.session.run(ort::inputs!["input_ids" => ids, "attention_mask" => mask].map_err(err)?)
        }
        .map_err(err)?;
        let out = outputs[0].try_extract_tensor::<f32>().map_err(err)?;
        Ok((out.iter().copied().collect(), out.shape().to_vec()))
    }

    /// Encode in length-sorted batches on `layout.workers` threads; `f` turns one batch
    /// into one value per item. Results come back in input order.
    fn run_batches<T: Send + Default + Clone>(
        &self,
        encodings: Vec<Encoding>,
        batch: usize,
        f: impl Fn(&[Encoding], Vec<f32>, Vec<usize>) -> Vec<T> + Sync,
    ) -> AppResult<Vec<T>> {
        let mut order: Vec<usize> = (0..encodings.len()).collect();
        order.sort_by_key(|&i| encodings[i].get_ids().len());
        let batches: Vec<Vec<usize>> = order.chunks(batch.max(1)).map(<[usize]>::to_vec).collect();
        let next = AtomicUsize::new(0);
        let results: parking_lot::Mutex<Vec<(usize, AppResult<Vec<T>>)>> = parking_lot::Mutex::new(Vec::new());
        let encodings = &encodings;
        std::thread::scope(|s| {
            for _ in 0..self.layout.workers.min(batches.len()).max(1) {
                s.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(idx) = batches.get(k) else { break };
                    // re-pad this batch to its own longest item
                    let mut items: Vec<Encoding> = idx.iter().map(|&i| encodings[i].clone()).collect();
                    let longest = items.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
                    for e in items.iter_mut() {
                        e.pad(longest, self.tokenizer.get_padding().map(|p| p.pad_id).unwrap_or(1), 0, "<pad>", tokenizers::PaddingDirection::Right);
                    }
                    let r = self.forward(&items).map(|(data, shape)| f(&items, data, shape));
                    results.lock().push((k, r));
                });
            }
        });
        let mut out = vec![T::default(); encodings.len()];
        for (k, r) in results.into_inner() {
            for (pos, v) in batches[k].iter().zip(r?) {
                out[*pos] = v;
            }
        }
        Ok(out)
    }

    fn encode_unpadded(&self, inputs: Vec<tokenizers::EncodeInput>) -> AppResult<Vec<Encoding>> {
        // pad per batch later (sorted by length) instead of to the longest overall
        let mut tok = self.tokenizer.clone();
        tok.with_padding(None);
        tok.encode_batch(inputs, true).map_err(err)
    }

    /// Cross-encoder relevance of each document to the query (raw logits, higher = better).
    pub fn score_pairs(&self, query: &str, docs: &[String], batch: usize) -> AppResult<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let enc = self.encode_unpadded(docs.iter().map(|d| (query, d.as_str()).into()).collect())?;
        self.run_batches(enc, batch, |items, data, shape| {
            let per = shape.get(1).copied().unwrap_or(1).max(1);
            (0..items.len()).map(|b| data[b * per]).collect()
        })
    }
}
