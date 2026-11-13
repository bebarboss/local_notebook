//! Hand a service to someone else: one .zip with the index and the documents.
//!
//! ```text
//! DocAsk-HR-นโยบาย.zip
//!   docask-service.json   manifest: service settings, embedding model, index signature
//!   index.db              chunks + vectors + keyword index + pictures, paths relative ("docs/…")
//!   docs/…                the documents, in their sub-folders
//! ```
//!
//! Import unpacks `docs/` into a folder the receiver picks, points the index at it and
//! creates a normal service on that folder. The indexer then sees every file with the
//! same content hash and keeps the vectors — nothing is embedded again.

use crate::errors::{AppError, AppResult};
use crate::services::Service;
use crate::store::{SqliteStore, VectorStore};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "docask-service.json";
const INDEX: &str = "index.db";
const DOCS: &str = "docs/";
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    pub app_version: String,
    pub name: String,
    pub code: String,
    pub color: String,
    pub model: String,
    pub effort: String,
    pub top_k: u32,
    #[serde(default)]
    pub system_prompt: String,
    /// `Source::id()` of the embedding that made the vectors
    pub embedding: String,
    pub documents: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub documents: usize,
    pub bytes: u64,
    /// documents in the index whose file no longer exists (left out)
    pub missing: usize,
}

/// Case-insensitive "is `path` inside `dir`" → the rest with `/` separators.
pub fn relative_to(path: &str, dir: &str) -> Option<String> {
    let dir = dir.trim_end_matches(['\\', '/']);
    if path.len() <= dir.len() || !path.is_char_boundary(dir.len()) {
        return None;
    }
    let (head, rest) = path.split_at(dir.len());
    if !head.eq_ignore_ascii_case(dir) && head.to_lowercase() != dir.to_lowercase() {
        return None;
    }
    let rest = rest.strip_prefix(['\\', '/'])?;
    Some(rest.replace('\\', "/"))
}

fn safe_part(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let t = cleaned.trim().trim_end_matches('.').to_string();
    if t.is_empty() { "service".into() } else { t }
}

/// Where each document goes inside the zip (`docs/…`), keyed by its absolute path.
fn zip_names(svc: &Service, paths: &[String]) -> Vec<(String, String)> {
    let multi = svc.folders.len() > 1;
    let mut used: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let found = svc.folders.iter().enumerate().find_map(|(i, f)| {
            relative_to(p, f).map(|rel| {
                if multi {
                    let name = Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    format!("{}{}_{}/{rel}", DOCS, i + 1, safe_part(&name))
                } else {
                    format!("{DOCS}{rel}")
                }
            })
        });
        let mut name = found.unwrap_or_else(|| {
            let file = Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            format!("{DOCS}_files/{file}")
        });
        // two single files with the same name
        let mut k = 2;
        while !used.insert(name.to_lowercase()) {
            let path = Path::new(&name);
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let parent = name.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default();
            name = format!("{parent}/{stem} ({k}){ext}");
            k += 1;
        }
        out.push((p.clone(), name));
    }
    out
}

/// Write `<dest_dir>/DocAsk-<code>-<name>.zip`. `progress(done, total)` after each file.
pub fn export_zip(
    svc: &Service,
    store: &parking_lot::Mutex<SqliteStore>,
    embedding: &str,
    dest_dir: &Path,
    mut progress: impl FnMut(usize, usize),
) -> AppResult<ExportResult> {
    let tmp = tempdir_in(dest_dir)?;
    let db_copy = tmp.join(INDEX);
    store.lock().export_to(&db_copy)?;

    // the copy's documents get relative paths; files that are gone are left out
    let mut copy = SqliteStore::open(&db_copy)?;
    let docs = copy.list_documents()?;
    let present: Vec<String> = docs.iter().map(|d| d.path.clone()).filter(|p| Path::new(p).is_file()).collect();
    let missing = docs.len() - present.len();
    for d in &docs {
        if !Path::new(&d.path).is_file() {
            copy.delete_document(&d.path)?;
        }
    }
    let names = zip_names(svc, &present);
    copy.rewrite_paths(|p| names.iter().find(|(abs, _)| abs == p).map(|(_, rel)| rel.clone()))?;
    copy.vacuum()?;
    drop(copy);

    let manifest = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").into(),
        name: svc.name.clone(),
        code: svc.code.clone(),
        color: svc.color.clone(),
        model: svc.model.clone(),
        effort: svc.effort.clone(),
        top_k: svc.top_k,
        system_prompt: svc.system_prompt.clone(),
        embedding: embedding.to_string(),
        documents: names.len(),
    };

    let out_path = dest_dir.join(format!("DocAsk-{}-{}.zip", safe_part(&svc.code), safe_part(&svc.name)));
    let part = out_path.with_extension("zip.part");
    let result = (|| -> AppResult<()> {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&part)?);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(MANIFEST, opts).map_err(zip_err)?;
        zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        zip.start_file(INDEX, opts.large_file(true)).map_err(zip_err)?;
        std::io::copy(&mut std::fs::File::open(&db_copy)?, &mut zip)?;
        for (i, (abs, rel)) in names.iter().enumerate() {
            let len = std::fs::metadata(abs).map(|m| m.len()).unwrap_or(0);
            zip.start_file(rel.as_str(), opts.large_file(len > u32::MAX as u64 / 2)).map_err(zip_err)?;
            std::io::copy(&mut std::fs::File::open(abs)?, &mut zip)?;
            progress(i + 1, names.len());
        }
        zip.finish().map_err(zip_err)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    if out_path.exists() {
        std::fs::remove_file(&out_path)?;
    }
    std::fs::rename(&part, &out_path)?;
    let bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    Ok(ExportResult { path: out_path.to_string_lossy().into_owned(), documents: names.len(), bytes, missing })
}

fn zip_err(e: zip::result::ZipError) -> AppError {
    AppError::new("zip", format!("ไฟล์ zip ผิดพลาด: {e}"))
}

fn tempdir_in(dir: &Path) -> AppResult<PathBuf> {
    let p = dir.join(format!(".docask-export-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

/// Read the manifest of an exported zip.
pub fn read_manifest(zip_path: &Path) -> AppResult<Manifest> {
    let not_ours = || AppError::new("not_docask", "ไฟล์นี้ไม่ใช่ไฟล์ส่งออกของ DocAsk (ไม่พบ docask-service.json)");
    let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path)?).map_err(|_| not_ours())?;
    let mut f = zip.by_name(MANIFEST).map_err(|_| not_ours())?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    let m: Manifest = serde_json::from_str(&s).map_err(|_| not_ours())?;
    if m.format > FORMAT {
        return Err(AppError::new("too_new", "ไฟล์นี้มาจาก DocAsk เวอร์ชันใหม่กว่า — อัปเดตแอปก่อน"));
    }
    Ok(m)
}

/// A folder name that does not exist yet under `parent` ("HR", "HR (2)", …).
pub fn free_folder(parent: &Path, name: &str) -> PathBuf {
    let base = safe_part(name);
    let mut p = parent.join(&base);
    let mut k = 2;
    while p.exists() {
        p = parent.join(format!("{base} ({k})"));
        k += 1;
    }
    p
}

/// Unpack `docs/…` into `target` (zip-slip safe) and, if `index_to` is given, the index
/// there with its paths pointed at `target`. Returns the number of documents.
pub fn unpack(zip_path: &Path, target: &Path, index_to: Option<&Path>, mut progress: impl FnMut(usize, usize)) -> AppResult<usize> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path)?).map_err(zip_err)?;
    let total = (0..zip.len()).filter(|&i| zip.name_for_index(i).is_some_and(|n| n.starts_with(DOCS) && !n.ends_with('/'))).count();
    let mut done = 0;
    std::fs::create_dir_all(target)?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(zip_err)?;
        let Some(rel) = f.enclosed_name() else { continue }; // "../" and absolute names are skipped
        let name = f.name().to_string();
        if !name.starts_with(DOCS) || f.is_dir() {
            continue;
        }
        let dest = target.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut f, &mut out)?;
        done += 1;
        progress(done, total);
    }
    if let Some(db) = index_to {
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut f = zip.by_name(INDEX).map_err(|_| AppError::new("not_docask", "ไม่พบ index.db ในไฟล์ zip"))?;
            let mut out = std::fs::File::create(db)?;
            std::io::copy(&mut f, &mut out)?;
        }
        let mut store = SqliteStore::open(db)?;
        store.rewrite_paths(|p| {
            p.strip_prefix(DOCS).map(|rel| target.join("docs").join(rel.replace('/', "\\")).to_string_lossy().into_owned())
        })?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewChunk;

    fn svc(folders: Vec<String>) -> Service {
        Service {
            id: "s1".into(), name: "HR นโยบาย".into(), code: "HR".into(), color: "#2F6FEB".into(), folders, files: vec![], excluded: vec![],
            model: "godev1".into(), effort: "medium".into(), top_k: 5, system_prompt: String::new(), knowledge_dir: String::new(), shared: false, created_at: 0, updated_at: 0,
        }
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative_to(r"C:\Docs\HR\a\b.pdf", r"C:\docs\hr"), Some("a/b.pdf".into()));
        assert_eq!(relative_to(r"C:\Docs\HR2\b.pdf", r"C:\Docs\HR"), None);
        assert_eq!(relative_to(r"C:\Docs\HR\b.pdf", r"C:\Docs\HR\"), Some("b.pdf".into()));
        assert_eq!(relative_to(r"C:\เอกสาร\ลา.pdf", r"C:\เอกสาร"), Some("ลา.pdf".into()));
    }

    #[test]
    fn names_inside_the_zip() {
        let s = svc(vec![r"C:\A\HR".into(), r"D:\Shared\Policy".into()]);
        let n = zip_names(&s, &[r"C:\A\HR\x\ลา.pdf".into(), r"D:\Shared\Policy\y.docx".into(), r"E:\one\y.docx".into(), r"F:\two\y.docx".into()]);
        assert_eq!(n[0].1, "docs/1_HR/x/ลา.pdf");
        assert_eq!(n[1].1, "docs/2_Policy/y.docx");
        assert_eq!(n[2].1, "docs/_files/y.docx");
        assert_eq!(n[3].1, "docs/_files/y (2).docx");
        let one = svc(vec![r"C:\A\HR".into()]);
        assert_eq!(zip_names(&one, &[r"C:\A\HR\x\ลา.pdf".into()])[0].1, "docs/x/ลา.pdf");
    }

    /// export → import on "another machine": same text, vectors and pictures, paths moved.
    #[test]
    fn export_then_import_keeps_the_index() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("ต้นทาง");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("ลา.txt"), "ลาพักร้อนได้ 10 วัน").unwrap();
        std::fs::write(src.join("gone.txt"), "x").unwrap();
        let s = svc(vec![src.to_string_lossy().into_owned()]);

        let store = parking_lot::Mutex::new(SqliteStore::open(&tmp.path().join("idx").join("index.db")).unwrap());
        {
            let mut st = store.lock();
            for (f, v) in [("sub\\ลา.txt", [1.0, 0.0]), ("gone.txt", [0.0, 1.0])] {
                let p = src.join(f).to_string_lossy().into_owned();
                let id = st.upsert_document(&p, "hash-1", 7, 3, "indexing", None).unwrap();
                st.replace_chunks(id, &[NewChunk { page: None, ord: 0, text: format!("text of {f}"), embedding: v.to_vec() }], &[]).unwrap();
            }
            st.set_meta("signature", "multilingual-e5-small|1000|150|ocr1|img1|chunk2").unwrap();
        }
        std::fs::remove_file(src.join("gone.txt")).unwrap();

        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let r = export_zip(&s, &store, "multilingual-e5-small", &out, |_, _| {}).unwrap();
        assert_eq!((r.documents, r.missing), (1, 1));
        assert!(r.path.ends_with("DocAsk-HR-HR นโยบาย.zip"), "{}", r.path);
        let m = read_manifest(Path::new(&r.path)).unwrap();
        assert_eq!((m.embedding.as_str(), m.documents, m.code.as_str()), ("multilingual-e5-small", 1, "HR"));

        let target = free_folder(&tmp.path().join("ปลายทาง"), &m.name);
        let db = tmp.path().join("new").join("index.db");
        assert_eq!(unpack(Path::new(&r.path), &target, Some(&db), |_, _| {}).unwrap(), 1);
        let file = target.join("docs").join("sub").join("ลา.txt");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ลาพักร้อนได้ 10 วัน");

        let mut imported = SqliteStore::open(&db).unwrap();
        let docs = imported.list_documents().unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].path, file.to_string_lossy());
        assert_eq!(docs[0].hash, "hash-1");
        assert_eq!(imported.meta("signature").unwrap().as_deref(), Some("multilingual-e5-small|1000|150|ocr1|img1|chunk2"));
        let hits = imported.search(&[1.0, 0.0], 5, 0.5).unwrap();
        assert_eq!(hits[0].text, "text of sub\\ลา.txt");
        assert_eq!(hits[0].doc_path, file.to_string_lossy());
        assert!(!out.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".docask-export")));
    }

    #[test]
    fn rejects_other_zips() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        z.start_file("hello.txt", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"hi").unwrap();
        z.finish().unwrap();
        assert_eq!(read_manifest(&p).unwrap_err().code, "not_docask");
    }
}
