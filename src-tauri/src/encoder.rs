//! Transformer text encoder on ONNX Runtime, shared by embedding (bi-encoder) and
//! reranking (cross-encoder).
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// mean of token vectors, ignoring padding (e5)
    Mean,
    /// first token ([CLS] / <s>) (bge-m3)
    Cls,
}

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
        max_workers: usize,
        f: impl Fn(&[Encoding], Vec<f32>, Vec<usize>) -> Vec<T> + Sync,
    ) -> AppResult<Vec<T>> {
        let mut order: Vec<usize> = (0..encodings.len()).collect();
        order.sort_by_key(|&i| encodings[i].get_ids().len());
        let batches: Vec<Vec<usize>> = order.chunks(batch.max(1)).map(<[usize]>::to_vec).collect();
        let next = AtomicUsize::new(0);
        let results: parking_lot::Mutex<Vec<(usize, AppResult<Vec<T>>)>> = parking_lot::Mutex::new(Vec::new());
        let encodings = &encodings;
        std::thread::scope(|s| {
            for _ in 0..self.layout.workers.min(max_workers).min(batches.len()).max(1) {
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

    /// Sentence embeddings, L2-normalised.
    pub fn embed(&self, texts: &[String], pooling: Pooling, batch: usize) -> AppResult<Vec<Vec<f32>>> {
        self.embed_with(texts, pooling, batch, usize::MAX)
    }

    /// Like `embed`, on at most `max_workers` parallel inferences (lighter on the CPU).
    pub fn embed_with(&self, texts: &[String], pooling: Pooling, batch: usize, max_workers: usize) -> AppResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let enc = self.encode_unpadded(texts.iter().map(|t| t.as_str().into()).collect())?;
        self.run_batches(enc, batch, max_workers, |items, data, shape| {
            // [batch, tokens, dim]
            let (n, d) = (shape[1], shape[2]);
            items
                .iter()
                .enumerate()
                .map(|(b, e)| {
                    let base = b * n * d;
                    let mut v = vec![0f32; d];
                    match pooling {
                        Pooling::Cls => v.copy_from_slice(&data[base..base + d]),
                        Pooling::Mean => {
                            let mut cnt = 0f32;
                            for (t, &m) in e.get_attention_mask().iter().enumerate() {
                                if m == 1 {
                                    let row = &data[base + t * d..base + (t + 1) * d];
                                    v.iter_mut().zip(row).for_each(|(a, x)| *a += x);
                                    cnt += 1.0;
                                }
                            }
                            v.iter_mut().for_each(|a| *a /= cnt.max(1.0));
                        }
                    }
                    crate::search::normalize(&mut v);
                    v
                })
                .collect()
        })
    }

    /// Cross-encoder relevance of each document to the query (raw logits, higher = better).
    pub fn score_pairs(&self, query: &str, docs: &[String], batch: usize) -> AppResult<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let enc = self.encode_unpadded(docs.iter().map(|d| (query, d.as_str()).into()).collect())?;
        self.run_batches(enc, batch, usize::MAX, |items, data, shape| {
            let per = shape.get(1).copied().unwrap_or(1).max(1);
            (0..items.len()).map(|b| data[b * per]).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    /// Thai retrieval check: does a reranker put the right passage first more often than
    /// vectors alone? `cargo test --release bench_rerank -- --ignored --nocapture`
    /// with DOCASK_TEST_EMBED (e5-small folder) and DOCASK_TEST_RERANK (folder of model folders).
    #[test]
    #[ignore]
    fn bench_rerank() {
        let passages: Vec<String> = [
            "การลาพักผ่อนประจำปี พนักงานที่ทำงานครบ 1 ปีมีสิทธิ์ลาพักผ่อนได้ 10 วันทำการต่อปี และเพิ่มขึ้นปีละ 1 วันเมื่ออายุงานครบ 3 ปี สูงสุดไม่เกิน 15 วัน ต้องยื่นใบลาผ่านระบบล่วงหน้าอย่างน้อย 3 วันทำการ",
            "การลาป่วย พนักงานลาป่วยได้ตามจริงไม่เกิน 30 วันทำการต่อปีโดยได้รับค่าจ้าง หากลาป่วยติดต่อกันตั้งแต่ 3 วันขึ้นไปต้องแนบใบรับรองแพทย์",
            "การลากิจ พนักงานลากิจธุระจำเป็นได้ไม่เกิน 3 วันทำการต่อปีโดยได้รับค่าจ้าง ต้องแจ้งหัวหน้างานล่วงหน้าอย่างน้อย 1 วัน ยกเว้นกรณีฉุกเฉิน",
            "การลาคลอด พนักงานหญิงมีสิทธิ์ลาคลอดได้ 98 วัน รวมวันหยุด โดยบริษัทจ่ายค่าจ้าง 45 วัน และสามารถเบิกส่วนที่เหลือจากประกันสังคม",
            "การลาออก พนักงานต้องยื่นใบลาออกเป็นลายลักษณ์อักษรล่วงหน้าไม่น้อยกว่า 30 วันก่อนวันที่มีผล และคืนทรัพย์สินของบริษัท เช่น บัตรพนักงานและโน้ตบุ๊ก ในวันทำงานสุดท้าย",
            "การเบิกค่ารักษาพยาบาล พนักงานเบิกค่ารักษาผู้ป่วยนอกได้ไม่เกิน 20,000 บาทต่อปี ผู้ป่วยในเบิกตามจริงไม่เกิน 100,000 บาทต่อครั้ง ต้องส่งใบเสร็จภายใน 30 วัน",
            "การเบิกค่าเดินทาง การเดินทางไปปฏิบัติงานต่างจังหวัด เบิกค่าเบี้ยเลี้ยงวันละ 300 บาท ค่าที่พักไม่เกินคืนละ 1,200 บาท และค่าน้ำมันกิโลเมตรละ 5 บาท",
            "เวลาทำงาน บริษัทกำหนดเวลาทำงานวันจันทร์ถึงศุกร์ 08.30 ถึง 17.30 น. พักกลางวัน 12.00 ถึง 13.00 น. มาสายเกิน 15 นาทีถือว่าสาย",
            "การทำงานล่วงเวลา การทำงานล่วงเวลาต้องได้รับอนุมัติจากหัวหน้างานก่อน ค่าล่วงเวลาวันทำงานปกติ 1.5 เท่า วันหยุด 3 เท่าของค่าจ้างต่อชั่วโมง",
            "การรีเซ็ตรหัสผ่าน หากลืมรหัสผ่านเข้าระบบ ให้กดลืมรหัสผ่านที่หน้า login ระบบจะส่งลิงก์ไปที่อีเมลบริษัท ลิงก์มีอายุ 15 นาที รหัสใหม่ต้องยาวอย่างน้อย 12 ตัวอักษร",
            "การขอสิทธิ์เข้าใช้ระบบ POS พนักงานใหม่ต้องให้ผู้จัดการสาขากรอกแบบฟอร์มขอสิทธิ์ใน IT Service Portal และรออนุมัติ 2 วันทำการ",
            "การปิดยอดขายประจำวันที่หน้าร้าน พนักงานต้องพิมพ์รายงานปิดยอดจากเครื่อง POS นับเงินสดในลิ้นชักให้ตรงกับรายงาน และนำส่งเงินภายในเวลา 22.00 น.",
            "การคืนสินค้า ลูกค้าคืนสินค้าได้ภายใน 7 วันพร้อมใบเสร็จ สินค้าต้องอยู่ในสภาพเดิม ยกเว้นสินค้าสดและสินค้าลดราคา ไม่รับคืน",
            "การจัดซื้อ การซื้อสินค้าหรือบริการมูลค่าเกิน 100,000 บาท ต้องเปรียบเทียบราคาจากผู้ขายอย่างน้อย 3 ราย และได้รับอนุมัติจากผู้อำนวยการฝ่าย",
            "การประเมินผลงาน บริษัทประเมินผลงานปีละ 2 ครั้ง ในเดือนมิถุนายนและธันวาคม ผลการประเมินใช้พิจารณาขึ้นเงินเดือนและโบนัสประจำปี",
            "สวัสดิการกองทุนสำรองเลี้ยงชีพ พนักงานสมัครเข้ากองทุนได้หลังผ่านทดลองงาน สะสมได้ร้อยละ 3 ถึง 15 ของเงินเดือน บริษัทสมทบร้อยละ 5",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // (question, index of the passage that answers it)
        let qs: [(&str, usize); 12] = [
            ("ลาป่วยกี่วันต้องมีใบแพทย์", 1),
            ("ถ้าจะลาออกต้องแจ้งล่วงหน้ากี่วัน", 4),
            ("ลาพักร้อนต้องขอล่วงหน้ากี่วัน", 0),
            ("ทำโอทีวันเสาร์อาทิตย์ได้กี่เท่า", 8),
            ("ลืม password ทำยังไง", 9),
            ("ไปทำงานต่างจังหวัดเบิกค่าโรงแรมได้เท่าไร", 6),
            ("พนักงานใหม่อยากใช้เครื่องขายหน้าร้านต้องทำอะไร", 10),
            ("ซื้อของราคาสองแสนต้องทำอะไรบ้าง", 13),
            ("ลูกค้าเอาผักมาคืนได้ไหม", 12),
            ("บริษัทสมทบ provident fund กี่เปอร์เซ็นต์", 15),
            ("เข้างานกี่โมง", 7),
            ("ส่งเงินสดหลังปิดร้านภายในกี่โมง", 11),
        ];

        let emb_dir = PathBuf::from(std::env::var("DOCASK_TEST_EMBED").expect("DOCASK_TEST_EMBED"));
        let rr_root = PathBuf::from(std::env::var("DOCASK_TEST_RERANK").expect("DOCASK_TEST_RERANK"));
        let emb = Encoder::load(&emb_dir.join("onnx").join("model_quantized.onnx"), &emb_dir, 512, false, Layout::auto()).unwrap();
        let pv = emb.embed(&passages.iter().map(|p| format!("passage: {p}")).collect::<Vec<_>>(), Pooling::Mean, 8).unwrap();

        let rank_of = |scores: &[f32], gold: usize| scores.iter().filter(|&&s| s > scores[gold]).count() + 1;
        let report = |name: &str, ranks: &[usize]| {
            let top1 = ranks.iter().filter(|&&r| r == 1).count();
            let mrr: f64 = ranks.iter().map(|&r| 1.0 / r as f64).sum::<f64>() / ranks.len() as f64;
            println!("{name:<28} top-1 {top1}/{}  MRR {mrr:.3}  ranks {ranks:?}", ranks.len());
        };

        let mut base = Vec::new();
        let mut vec_scores = Vec::new();
        for (q, gold) in qs {
            let qv = emb.embed(&[format!("query: {q}")], Pooling::Mean, 1).unwrap().pop().unwrap();
            let s: Vec<f32> = pv.iter().map(|p| crate::search::dot(&qv, p)).collect();
            base.push(rank_of(&s, gold));
            vec_scores.push(s);
        }
        report("vector only (e5-small)", &base);

        // long passages for latency: 20 candidates of ~1000 Thai characters
        let long: Vec<String> = (0..20).map(|i| passages.iter().cycle().skip(i).take(6).cloned().collect::<Vec<_>>().join(" ").chars().take(1000).collect()).collect();

        for name in ["mm", "gte", "bge"] {
            let dir = rr_root.join(name);
            let onnx = dir.join("onnx").join("model_quantized.onnx");
            if !onnx.exists() {
                continue;
            }
            let t = Instant::now();
            let rr = match Encoder::load(&onnx, &dir, 512, true, Layout { intra: Layout::auto().intra * Layout::auto().workers, workers: 1 }) {
                Ok(r) => r,
                Err(e) => {
                    println!("{name}: load failed: {}", e.message);
                    continue;
                }
            };
            let load = t.elapsed();
            // rerank the 10 best vector hits (what the app would do)
            let mut ranks = Vec::new();
            for (i, (q, gold)) in qs.iter().enumerate() {
                let mut cand: Vec<usize> = (0..passages.len()).collect();
                cand.sort_by(|a, b| vec_scores[i][*b].total_cmp(&vec_scores[i][*a]));
                cand.truncate(10);
                let docs: Vec<String> = cand.iter().map(|&k| passages[k].clone()).collect();
                let s = rr.score_pairs(q, &docs, 4).unwrap();
                let mut full = vec![f32::NEG_INFINITY; passages.len()];
                for (k, v) in cand.iter().zip(s) {
                    full[*k] = v;
                }
                ranks.push(if full[*gold].is_finite() { rank_of(&full, *gold) } else { 99 });
            }
            report(&format!("rerank top-10 with {name}"), &ranks);
            for (intra, workers, batch) in [(Layout::auto().intra * Layout::auto().workers, 1, 4), (Layout::auto().intra, Layout::auto().workers, 2)] {
                let rr = Encoder::load(&onnx, &dir, 512, true, Layout { intra, workers }).unwrap();
                rr.score_pairs("ลาพักร้อนได้กี่วัน", &long[..2], 2).unwrap(); // warm-up
                let t = Instant::now();
                rr.score_pairs("ลาพักร้อนได้กี่วัน", &long, batch).unwrap();
                println!("   {name}: load {load:?}, rerank 20 × ~1000 chars: {:?}  (intra {intra} × {workers})", t.elapsed());
            }
        }
    }
}
