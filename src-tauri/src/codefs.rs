//! Safe, project-scoped filesystem for the "เขียนโค้ด" (coding) mode. Every path the model gives is
//! resolved against the chosen project root and refused if it escapes it (`..`, absolute paths, or a
//! symlink pointing outside). Reads/writes are limited to UTF-8 text under a size cap; binary and
//! oversized files are refused. Each write/edit returns the content *before* it, so a whole turn can
//! be undone — restoring originals and deleting files that were newly created. Never logs file
//! contents — only relative paths, counts, and error codes.

use crate::errors::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Refuse to read or write a text file larger than this (keeps the model's context bounded and
/// avoids pulling in big generated/binary blobs).
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// Most entries returned from one `list`.
const MAX_LIST: usize = 400;
/// Most hits returned from one `grep`, and the most files it will scan.
const MAX_GREP_HITS: usize = 80;
const MAX_GREP_FILES: usize = 4000;
/// Directories never walked or listed (noise / huge / not source).
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".next", ".venv", "__pycache__", ".idea", ".vscode"];

/// One file changed during a coding turn. `before` is the content before the turn first touched the
/// file (`None` when the file was created); `after` is the content when the turn ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodeChange {
    /// path relative to the project root, forward-slashed
    pub path: String,
    /// "create" (new file) or "modify"
    pub kind: String,
    pub before: Option<String>,
    pub after: String,
}

/// A turn's changes plus the root they apply to — saved with the assistant message and used by undo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Changeset {
    pub root: String,
    pub items: Vec<CodeChange>,
}

/// One entry from `list`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DirEntry {
    pub name: String,
    pub dir: bool,
}

/// One `grep` hit.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// The content before and after a write/edit (before is `None` when the file was created).
#[derive(Debug, Clone, PartialEq)]
pub struct WriteOutcome {
    pub before: Option<String>,
    pub after: String,
}

/// How many files an undo restored and deleted.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoResult {
    pub restored: usize,
    pub deleted: usize,
}

/// A coding session bound to one project root. The root is canonicalized once; all operations stay
/// inside it.
pub struct Workspace {
    root: PathBuf,
}

fn bad_path() -> AppError {
    AppError::new("code_path", "เส้นทางไฟล์ไม่ถูกต้องหรืออยู่นอกโฟลเดอร์โปรเจค")
}

impl Workspace {
    pub fn new(root: &str) -> AppResult<Self> {
        let p = PathBuf::from(root);
        let real = p.canonicalize().map_err(|_| AppError::new("code_root", "ไม่พบโฟลเดอร์โปรเจค"))?;
        if !real.is_dir() {
            return Err(AppError::new("code_root", "เส้นทางที่เลือกไม่ใช่โฟลเดอร์"));
        }
        Ok(Self { root: real })
    }

    pub fn root_display(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    /// Join `rel` onto the root, allowing only plain path segments (no `..`, no absolute/prefix), and
    /// verify the nearest existing ancestor still canonicalizes inside the root (catches symlinks).
    fn resolve(&self, rel: &str) -> AppResult<PathBuf> {
        let rel = rel.trim().replace('\\', "/");
        let rel = rel.trim_start_matches('/');
        let mut out = self.root.clone();
        for comp in Path::new(rel).components() {
            match comp {
                Component::Normal(c) => out.push(c),
                Component::CurDir => {}
                _ => return Err(bad_path()), // ParentDir, RootDir, Prefix
            }
        }
        self.ensure_within(&out)?;
        Ok(out)
    }

    fn ensure_within(&self, full: &Path) -> AppResult<()> {
        let mut p = full;
        loop {
            if let Ok(real) = p.canonicalize() {
                return real.starts_with(&self.root).then_some(()).ok_or_else(bad_path);
            }
            match p.parent() {
                Some(parent) => p = parent,
                None => return Err(bad_path()),
            }
        }
    }

    /// Relative, forward-slashed path for display/recording.
    fn rel_of(&self, full: &Path) -> String {
        full.strip_prefix(&self.root).unwrap_or(full).to_string_lossy().replace('\\', "/")
    }

    pub fn list(&self, rel: &str) -> AppResult<Vec<DirEntry>> {
        let dir = self.resolve(rel)?;
        if !dir.is_dir() {
            return Err(AppError::new("code_notdir", "ไม่ใช่โฟลเดอร์"));
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|_| AppError::new("code_io", "อ่านโฟลเดอร์ไม่ได้"))?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if name.starts_with('.') || (is_dir && SKIP_DIRS.contains(&name.as_str())) {
                continue;
            }
            out.push(DirEntry { name, dir: is_dir });
            if out.len() >= MAX_LIST {
                break;
            }
        }
        out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(out)
    }

    pub fn read(&self, rel: &str) -> AppResult<String> {
        let path = self.resolve(rel)?;
        read_text(&path)
    }

    pub fn grep(&self, pattern: &str, sub: Option<&str>) -> AppResult<Vec<Hit>> {
        let re = regex::Regex::new(pattern).map_err(|_| AppError::new("code_regex", "รูปแบบค้นหา (regex) ไม่ถูกต้อง"))?;
        let start = self.resolve(sub.unwrap_or(""))?;
        let mut hits = Vec::new();
        let mut files = 0usize;
        let mut stack = vec![start];
        while let Some(dir) = stack.pop() {
            if hits.len() >= MAX_GREP_HITS || files >= MAX_GREP_FILES {
                break;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let p = entry.path();
                if is_dir {
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(p);
                    }
                    continue;
                }
                files += 1;
                let Ok(text) = read_text(&p) else { continue };
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        hits.push(Hit { path: self.rel_of(&p), line: i + 1, text: line.chars().take(300).collect() });
                        if hits.len() >= MAX_GREP_HITS {
                            break;
                        }
                    }
                }
                if hits.len() >= MAX_GREP_HITS || files >= MAX_GREP_FILES {
                    break;
                }
            }
        }
        Ok(hits)
    }

    /// Create or overwrite a file with `content`. Returns the content before (None when created).
    pub fn write(&self, rel: &str, content: &str) -> AppResult<WriteOutcome> {
        if content.len() > MAX_FILE_BYTES {
            return Err(AppError::new("code_big", "เนื้อหาที่จะเขียนใหญ่เกินกำหนด"));
        }
        let path = self.resolve(rel)?;
        if path.is_dir() {
            return Err(AppError::new("code_isdir", "เส้นทางนี้เป็นโฟลเดอร์"));
        }
        // refuse to clobber a file we can't read back as text (binary / too large)
        let before = if path.exists() { Some(read_text(&path)?) } else { None };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| AppError::new("code_io", "สร้างโฟลเดอร์ไม่ได้"))?;
        }
        std::fs::write(&path, content).map_err(|_| AppError::new("code_io", "เขียนไฟล์ไม่ได้"))?;
        Ok(WriteOutcome { before, after: content.to_string() })
    }

    /// Replace the single occurrence of `old` with `new` in an existing file. Errors when `old` is
    /// absent or appears more than once, so the model is told to make it unique and retry.
    pub fn edit(&self, rel: &str, old: &str, new: &str) -> AppResult<WriteOutcome> {
        if old.is_empty() {
            return Err(AppError::new("code_edit", "ต้องระบุข้อความเดิมที่จะแก้ (old)"));
        }
        let path = self.resolve(rel)?;
        let current = read_text(&path)?;
        let count = current.matches(old).count();
        if count == 0 {
            return Err(AppError::new("code_edit", "ไม่พบข้อความเดิม (old) ในไฟล์นี้ ลองคัดลอกให้ตรงอักขระ"));
        }
        if count > 1 {
            return Err(AppError::new("code_edit", format!("พบข้อความเดิม (old) {count} ที่ในไฟล์ ให้ใส่บริบทรอบ ๆ ให้ยาวพอจะเจาะจงที่เดียว")));
        }
        let after = current.replacen(old, new, 1);
        if after.len() > MAX_FILE_BYTES {
            return Err(AppError::new("code_big", "ไฟล์หลังแก้ใหญ่เกินกำหนด"));
        }
        std::fs::write(&path, &after).map_err(|_| AppError::new("code_io", "เขียนไฟล์ไม่ได้"))?;
        Ok(WriteOutcome { before: Some(current), after })
    }

    /// Reverse a turn's changes: restore each `before`, delete files that were created.
    pub fn undo(&self, items: &[CodeChange]) -> UndoResult {
        let mut out = UndoResult::default();
        // restore deepest paths last is irrelevant; just apply each independently
        let mut done: HashSet<&str> = HashSet::new();
        for c in items {
            if !done.insert(c.path.as_str()) {
                continue;
            }
            let Ok(path) = self.resolve(&c.path) else { continue };
            match &c.before {
                None => {
                    // file was created this turn → remove it
                    if std::fs::remove_file(&path).is_ok() {
                        out.deleted += 1;
                    }
                }
                Some(original) => {
                    if std::fs::write(&path, original).is_ok() {
                        out.restored += 1;
                    }
                }
            }
        }
        out
    }
}

/// Read a file as UTF-8 text, refusing binary content and anything over the size cap.
fn read_text(path: &Path) -> AppResult<String> {
    let meta = std::fs::metadata(path).map_err(|_| AppError::new("code_notfound", "ไม่พบไฟล์"))?;
    if meta.is_dir() {
        return Err(AppError::new("code_isdir", "เส้นทางนี้เป็นโฟลเดอร์"));
    }
    if meta.len() as usize > MAX_FILE_BYTES {
        return Err(AppError::new("code_big", "ไฟล์ใหญ่เกินกำหนด (อ่านได้เฉพาะไฟล์ข้อความ ≤ 256 KB)"));
    }
    let bytes = std::fs::read(path).map_err(|_| AppError::new("code_io", "อ่านไฟล์ไม่ได้"))?;
    if bytes.contains(&0) {
        return Err(AppError::new("code_binary", "ไฟล์นี้ดูเป็นไฟล์ไบนารี อ่านไม่ได้"));
    }
    String::from_utf8(bytes).map_err(|_| AppError::new("code_binary", "ไฟล์นี้ไม่ใช่ข้อความ UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello\nworld\n").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn main() {}\n").unwrap();
        let w = Workspace::new(&dir.path().to_string_lossy()).unwrap();
        (dir, w)
    }

    #[test]
    fn rejects_paths_outside_the_root() {
        let (_d, w) = ws();
        assert!(w.read("../secret.txt").is_err());
        assert!(w.read("..\\secret.txt").is_err());
        assert!(w.write("../evil.txt", "x").is_err());
        // an absolute path is refused too
        #[cfg(windows)]
        assert!(w.read("C:\\Windows\\win.ini").is_err());
        #[cfg(not(windows))]
        assert!(w.read("/etc/passwd").is_err());
    }

    #[test]
    fn reads_and_lists_inside_the_root() {
        let (_d, w) = ws();
        assert_eq!(w.read("a.txt").unwrap(), "hello\nworld\n");
        let entries = w.list("").unwrap();
        // directories first, then files
        assert_eq!(entries[0], DirEntry { name: "src".into(), dir: true });
        assert!(entries.iter().any(|e| e.name == "a.txt" && !e.dir));
    }

    #[test]
    fn write_reports_before_and_undo_deletes_created() {
        let (_d, w) = ws();
        let out = w.write("new/file.py", "print('hi')\n").unwrap();
        assert_eq!(out.before, None, "a created file has no before");
        assert!(std::path::Path::new(&w.root_display()).join("new/file.py").exists());
        let change = CodeChange { path: "new/file.py".into(), kind: "create".into(), before: None, after: out.after };
        let r = w.undo(&[change]);
        assert_eq!(r.deleted, 1);
        assert!(!std::path::Path::new(&w.root_display()).join("new/file.py").exists(), "undo removed the created file");
    }

    #[test]
    fn overwrite_reports_before_and_undo_restores() {
        let (_d, w) = ws();
        let out = w.write("a.txt", "changed\n").unwrap();
        assert_eq!(out.before.as_deref(), Some("hello\nworld\n"));
        let change = CodeChange { path: "a.txt".into(), kind: "modify".into(), before: out.before, after: out.after };
        w.undo(&[change]);
        assert_eq!(w.read("a.txt").unwrap(), "hello\nworld\n", "undo restored the original");
    }

    #[test]
    fn edit_requires_a_unique_match() {
        let (_d, w) = ws();
        fs::write(std::path::Path::new(&w.root_display()).join("dup.txt"), "x\nx\n").unwrap();
        assert!(w.edit("dup.txt", "x", "y").is_err(), "ambiguous match is refused");
        assert!(w.edit("a.txt", "nope", "y").is_err(), "missing match is refused");
        let out = w.edit("a.txt", "world", "earth").unwrap();
        assert_eq!(out.after, "hello\nearth\n");
        assert_eq!(w.read("a.txt").unwrap(), "hello\nearth\n");
    }

    #[test]
    fn grep_finds_matches_with_line_numbers() {
        let (_d, w) = ws();
        let hits = w.grep("main", None).unwrap();
        assert!(hits.iter().any(|h| h.path == "src/lib.rs" && h.line == 1));
    }

    #[test]
    fn read_refuses_binary() {
        let (_d, w) = ws();
        fs::write(std::path::Path::new(&w.root_display()).join("bin"), [0u8, 1, 2, 3]).unwrap();
        assert!(w.read("bin").is_err());
    }
}
