//! OCR with PaddleOCR models on ONNX Runtime (CPU), bundled in the installer:
//!   det.onnx  — PP-OCRv3 mobile text detection (DB)
//!   rec.onnx  — PP-OCRv5 Thai recognition (Thai + English), dict.txt
//! Pipeline: detect text regions → crop → recognize (CTC greedy) → group into lines.

use crate::errors::{AppError, AppResult};
use image::{imageops::FilterType, DynamicImage, GenericImageView, RgbImage};
use ndarray::Array4;
use once_cell::sync::OnceCell;
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static DIR: OnceCell<PathBuf> = OnceCell::new();
static ENGINE: OnceCell<Option<Ocr>> = OnceCell::new();
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Called once at startup with the folder that holds det.onnx / rec.onnx / dict.txt.
pub fn init(dir: PathBuf) {
    let _ = DIR.set(dir);
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// The shared engine, loaded on first use. None when OCR is off or the models are missing.
pub fn engine() -> Option<&'static Ocr> {
    if !ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    DIR.get()?; // not initialised yet → don't cache "unavailable"
    ENGINE
        .get_or_init(|| {
            let dir = DIR.get()?;
            match Ocr::load(dir) {
                Ok(o) => Some(o),
                Err(e) => {
                    log::warn!("OCR unavailable: {}", e.message);
                    None
                }
            }
        })
        .as_ref()
}

pub fn available() -> bool {
    DIR.get().map(|d| d.join("det.onnx").is_file() && d.join("rec.onnx").is_file() && d.join("dict.txt").is_file()).unwrap_or(false)
}

pub struct Ocr {
    det: Session,
    rec: Session,
    dict: Vec<String>,
    rec_height: usize,
    /// text lines recognised in parallel (rec session uses few threads each)
    rec_workers: usize,
}

#[derive(Debug, Clone)]
struct TextBox {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    text: String,
}

const DET_MAX_SIDE: u32 = 1280;
const DET_THRESH: f32 = 0.3;
const BOX_THRESH: f32 = 0.5;
const UNCLIP_RATIO: f32 = 1.6;
const REC_MIN_SCORE: f32 = 0.5;

fn ort_err(e: impl std::fmt::Display) -> AppError {
    AppError::new("ocr", format!("OCR ผิดพลาด: {e}"))
}

fn session(path: &Path, threads: usize) -> AppResult<Session> {
    Session::builder()
        .map_err(ort_err)?
        .with_intra_threads(threads)
        .map_err(ort_err)?
        .commit_from_file(path)
        .map_err(ort_err)
}

impl Ocr {
    pub fn load(dir: &Path) -> AppResult<Self> {
        // measured: one line at a time on all threads beats lines in parallel
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
        Self::load_with(dir, crate::encoder::Layout { intra: cores, workers: 1 })
    }

    /// Detection is one big image → all threads; recognition is many small line images →
    /// a few threads each, several lines at once.
    pub fn load_with(dir: &Path, layout: crate::encoder::Layout) -> AppResult<Self> {
        let all = (layout.intra * layout.workers).max(1);
        let det = session(&dir.join("det.onnx"), all)?;
        let rec = session(&dir.join("rec.onnx"), layout.intra)?;
        let rec_workers = layout.workers;
        let dict: Vec<String> = std::fs::read_to_string(dir.join("dict.txt"))?
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        // PP-OCRv5 rec uses height 48; honour a fixed height if the model declares one
        let rec_height = rec
            .inputs
            .first()
            .and_then(|i| match &i.input_type {
                ort::value::ValueType::Tensor { dimensions, .. } => dimensions.get(2).copied(),
                _ => None,
            })
            .filter(|h| *h > 0)
            .map(|h| h as usize)
            .unwrap_or(48);
        Ok(Self { det, rec, dict, rec_height, rec_workers })
    }

    /// Recognize all text in an image, returned as lines (top → bottom, left → right).
    pub fn recognize(&self, img: &DynamicImage) -> AppResult<String> {
        let (w, h) = img.dimensions();
        if w < 16 || h < 8 {
            return Ok(String::new());
        }
        let rgb = img.to_rgb8();
        let mut boxes = self.detect(&rgb)?;
        self.read_boxes(&rgb, &mut boxes)?;
        boxes.retain(|b| !b.text.trim().is_empty());
        Ok(group_lines(boxes))
    }

    fn detect(&self, img: &RgbImage) -> AppResult<Vec<TextBox>> {
        let (w, h) = img.dimensions();
        let scale = (DET_MAX_SIDE as f32 / w.max(h) as f32).min(1.0);
        // upscale tiny images a little so small UI text is still detected
        let scale = if w.max(h) < 480 { 480.0 / w.max(h) as f32 } else { scale };
        let rw = (((w as f32 * scale) / 32.0).round() as u32).max(1) * 32;
        let rh = (((h as f32 * scale) / 32.0).round() as u32).max(1) * 32;
        let resized = image::imageops::resize(img, rw, rh, FilterType::Triangle);

        // PaddleOCR feeds BGR, (x/255 - mean) / std
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let mut input = Array4::<f32>::zeros((1, 3, rh as usize, rw as usize));
        for (x, y, p) in resized.enumerate_pixels() {
            let bgr = [p[2], p[1], p[0]];
            for c in 0..3 {
                input[[0, c, y as usize, x as usize]] = (bgr[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
        let tensor = Tensor::from_array(input).map_err(ort_err)?;
        let outputs = self.det.run(ort::inputs![tensor].map_err(ort_err)?).map_err(ort_err)?;
        let prob = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;
        let shape = prob.shape().to_vec();
        let (mh, mw) = (shape[shape.len() - 2], shape[shape.len() - 1]);
        let prob: Vec<f32> = prob.iter().copied().collect();

        let comps = components(&prob, mw, mh, DET_THRESH);
        let sx = w as f32 / mw as f32;
        let sy = h as f32 / mh as f32;
        let mut out = Vec::new();
        for c in comps {
            if c.score < BOX_THRESH || c.x1 - c.x0 < 3 || c.y1 - c.y0 < 3 {
                continue;
            }
            let bw = (c.x1 - c.x0 + 1) as f32;
            let bh = (c.y1 - c.y0 + 1) as f32;
            // DB "unclip": grow the shrunk region back by area * ratio / perimeter
            let d = bw * bh * UNCLIP_RATIO / (2.0 * (bw + bh));
            let x0 = ((c.x0 as f32 - d) * sx).max(0.0) as u32;
            let y0 = ((c.y0 as f32 - d) * sy).max(0.0) as u32;
            let x1 = (((c.x1 as f32 + 1.0 + d) * sx) as u32).min(w - 1);
            let y1 = (((c.y1 as f32 + 1.0 + d) * sy) as u32).min(h - 1);
            if x1 > x0 + 3 && y1 > y0 + 3 {
                out.push(TextBox { x0, y0, x1, y1, text: String::new() });
            }
        }
        Ok(out)
    }

    /// Recognise every box, `rec_workers` at a time (the session is shared).
    fn read_boxes(&self, img: &RgbImage, boxes: &mut [TextBox]) -> AppResult<()> {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let texts: parking_lot::Mutex<Vec<(usize, AppResult<String>)>> = parking_lot::Mutex::new(Vec::new());
        let snapshot: &[TextBox] = boxes;
        std::thread::scope(|s| {
            for _ in 0..self.rec_workers.min(snapshot.len()).max(1) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(b) = snapshot.get(i) else { break };
                    let r = self.read_box(img, b);
                    texts.lock().push((i, r));
                });
            }
        });
        for (i, t) in texts.into_inner() {
            boxes[i].text = t?;
        }
        Ok(())
    }

    fn read_box(&self, img: &RgbImage, b: &TextBox) -> AppResult<String> {
        let (bw, bh) = (b.x1 - b.x0 + 1, b.y1 - b.y0 + 1);
        if bh < 6 || (bh as f32) > (bw as f32) * 1.6 {
            return Ok(String::new()); // too small or vertical text — skip
        }
        let crop = image::imageops::crop_imm(img, b.x0, b.y0, bw, bh).to_image();
        let th = self.rec_height as u32;
        let tw = ((bw as f32 * th as f32 / bh as f32).ceil() as u32).clamp(16, 3200);
        let resized = image::imageops::resize(&crop, tw, th, FilterType::Triangle);
        let mut input = Array4::<f32>::zeros((1, 3, th as usize, tw as usize));
        for (x, y, p) in resized.enumerate_pixels() {
            let bgr = [p[2], p[1], p[0]];
            for c in 0..3 {
                input[[0, c, y as usize, x as usize]] = (bgr[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
        let tensor = Tensor::from_array(input).map_err(ort_err)?;
        let outputs = self.rec.run(ort::inputs![tensor].map_err(ort_err)?).map_err(ort_err)?;
        let logits = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;
        let shape = logits.shape().to_vec(); // [1, T, C]
        let (t, c) = (shape[1], shape[2]);
        let data: Vec<f32> = logits.iter().copied().collect();
        Ok(ctc_decode(&data, t, c, &self.dict))
    }
}

/// CTC greedy decode: index 0 = blank, 1..=dict.len() = dict chars, dict.len()+1 = space.
fn ctc_decode(data: &[f32], t: usize, c: usize, dict: &[String]) -> String {
    let mut out = String::new();
    let mut prev = 0usize;
    let mut score_sum = 0.0f32;
    let mut n = 0usize;
    for step in 0..t {
        let row = &data[step * c..(step + 1) * c];
        let (idx, p) = row.iter().enumerate().fold((0, f32::MIN), |acc, (i, &v)| if v > acc.1 { (i, v) } else { acc });
        if idx != 0 && idx != prev {
            let ch = if idx - 1 < dict.len() { dict[idx - 1].as_str() } else { " " };
            out.push_str(ch);
            score_sum += p;
            n += 1;
        }
        prev = idx;
    }
    if n == 0 || score_sum / (n as f32) < REC_MIN_SCORE {
        return String::new();
    }
    out.trim().to_string()
}

struct Comp {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    score: f32,
}

/// Connected components (4-neighbour) of the thresholded probability map.
fn components(prob: &[f32], w: usize, h: usize, thresh: f32) -> Vec<Comp> {
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if seen[start] || prob[start] <= thresh {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        let (mut sum, mut cnt) = (0.0f32, 0usize);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            sum += prob[i];
            cnt += 1;
            let mut push = |j: usize| {
                if !seen[j] && prob[j] > thresh {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 { push(i - 1); }
            if x + 1 < w { push(i + 1); }
            if y > 0 { push(i - w); }
            if y + 1 < h { push(i + w); }
        }
        if cnt >= 4 {
            out.push(Comp { x0, y0, x1, y1, score: sum / cnt as f32 });
        }
    }
    out
}

/// Sort boxes into reading order and join boxes on the same line with spaces.
fn group_lines(mut boxes: Vec<TextBox>) -> String {
    boxes.sort_by_key(|b| (b.y0 + b.y1) / 2);
    let mut lines: Vec<Vec<TextBox>> = Vec::new();
    for b in boxes {
        let cy = (b.y0 + b.y1) / 2;
        let placed = lines.iter_mut().find(|l| {
            let r = &l[0];
            let half = (r.y1 - r.y0) / 2 + 1;
            cy.abs_diff((r.y0 + r.y1) / 2) <= half
        });
        match placed {
            Some(l) => l.push(b),
            None => lines.push(vec![b]),
        }
    }
    lines
        .into_iter()
        .map(|mut l| {
            l.sort_by_key(|b| b.x0);
            l.into_iter().map(|b| b.text).collect::<Vec<_>>().join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// OCR an embedded picture (DOCX/PPTX media). None when OCR is off, for icons, or no text.
pub fn image_text(img: &DynamicImage) -> Option<String> {
    let ocr = engine()?;
    let (w, h) = img.dimensions();
    if w < super::MIN_IMAGE_W || h < super::MIN_IMAGE_H {
        return None;
    }
    ocr.recognize(img).ok().filter(|t| !t.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test --release bench_ocr -- --ignored --nocapture` with DOCASK_TEST_RES / DOCASK_TEST_DIR
    #[test]
    #[ignore]
    fn bench_ocr() {
        let res = std::path::PathBuf::from(std::env::var("DOCASK_TEST_RES").expect("DOCASK_TEST_RES"));
        let dir = std::path::PathBuf::from(std::env::var("DOCASK_TEST_DIR").expect("DOCASK_TEST_DIR"));
        // a dense page: the screenshot stacked 5× (≈45 text lines)
        let shot = image::open(dir.join("screen.png")).unwrap().to_rgb8();
        let mut page = RgbImage::new(shot.width(), shot.height() * 5);
        for k in 0..5 {
            image::imageops::replace(&mut page, &shot, 0, (k * shot.height()) as i64);
        }
        use crate::encoder::Layout;
        for layout in [Layout { intra: 8, workers: 1 }, Layout { intra: 4, workers: 2 }, Layout::auto(), Layout { intra: 1, workers: 7 }] {
            let ocr = Ocr::load_with(&res.join("ocr"), layout).unwrap();
            let mut first = String::new();
            for round in 0..2 {
                let t = std::time::Instant::now();
                let mut boxes = ocr.detect(&page).unwrap();
                let det = t.elapsed();
                let t = std::time::Instant::now();
                ocr.read_boxes(&page, &mut boxes).unwrap();
                if round == 0 {
                    first = boxes.iter().map(|b| b.text.clone()).collect::<Vec<_>>().join("|");
                }
                println!("{layout:?} round {round}: detect {det:?}, recognise {} lines {:?}", boxes.len(), t.elapsed());
            }
            assert!(first.contains("Job Data"));
        }
    }

    #[test]
    fn ctc_collapses_repeats_and_blanks() {
        let dict: Vec<String> = ["ก", "ข", "A"].iter().map(|s| s.to_string()).collect();
        // classes: 0 blank, 1 ก, 2 ข, 3 A, 4 space
        let steps = [1, 1, 0, 1, 2, 4, 3, 3];
        let c = 5;
        let mut data = vec![0.0f32; steps.len() * c];
        for (t, &k) in steps.iter().enumerate() {
            data[t * c + k] = 0.9;
        }
        assert_eq!(ctc_decode(&data, steps.len(), c, &dict), "กกข A");
    }

    #[test]
    fn groups_boxes_into_lines() {
        let b = |x0, y0, t: &str| TextBox { x0, y0, x1: x0 + 50, y1: y0 + 20, text: t.into() };
        let s = group_lines(vec![b(200, 102, "world"), b(10, 40, "line1"), b(10, 100, "hello")]);
        assert_eq!(s, "line1\nhello world");
    }

    /// End-to-end on a real image when the bundled models are present:
    /// `cargo test ocr_real -- --ignored` with DOCASK_OCR_DIR and DOCASK_OCR_IMAGE set.
    #[test]
    #[ignore]
    fn ocr_real() {
        let (Ok(dir), Ok(img)) = (std::env::var("DOCASK_OCR_DIR"), std::env::var("DOCASK_OCR_IMAGE")) else {
            eprintln!("skipped: set DOCASK_OCR_DIR and DOCASK_OCR_IMAGE");
            return;
        };
        let dir = PathBuf::from(dir);
        let img = image::open(img).unwrap();
        let ocr = Ocr::load(&dir).unwrap();
        let t = std::time::Instant::now();
        let text = ocr.recognize(&img).unwrap();
        println!("rec_height={} took={:?}\n{text}", ocr.rec_height, t.elapsed());
        assert!(!text.is_empty());
    }
}
