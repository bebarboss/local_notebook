//! A small local log (`%APPDATA%\DocAsk\index.log`) of why indexing ran, so problems on
//! another machine can be diagnosed. File names only — never document text or keys.

use std::io::Write;

const MAX_BYTES: u64 = 512 * 1024;

pub fn log_path() -> std::path::PathBuf {
    crate::config::data_dir().join("index.log")
}

pub fn log(line: impl AsRef<str>) {
    let path = log_path();
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{now} {}", line.as_ref());
    }
}
