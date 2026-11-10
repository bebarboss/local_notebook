//! Per-service vector store: `%APPDATA%\DocAsk\services\<id>\index.db` (SQLite).
//! Brute-force search over an in-memory cache. `VectorStore` keeps the door open
//! for `sqlite-vec` later.

use crate::errors::AppResult;
use crate::search::{from_blob, to_blob, top_k};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub chunk_count: i64,
    pub bytes: i64,
    pub progress: f32,
    pub message: Option<String>,
    #[serde(skip)]
    pub hash: String,
    #[serde(skip)]
    pub mtime: i64,
}

pub struct NewChunk {
    pub page: Option<u32>,
    pub ord: i64,
    pub text: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: i64,
    pub score: f32,
    pub doc_path: String,
    pub page: Option<u32>,
    pub ord: i64,
    pub text: String,
}

pub trait VectorStore {
    fn list_documents(&self) -> AppResult<Vec<DocRow>>;
    fn get_document(&self, path: &str) -> AppResult<Option<DocRow>>;
    fn upsert_document(&mut self, path: &str, hash: &str, mtime: i64, bytes: i64, status: &str, message: Option<&str>) -> AppResult<i64>;
    fn set_status(&mut self, doc_id: i64, status: &str, message: Option<&str>) -> AppResult<()>;
    fn replace_chunks(&mut self, doc_id: i64, chunks: &[NewChunk]) -> AppResult<()>;
    fn delete_document(&mut self, path: &str) -> AppResult<()>;
    fn clear(&mut self) -> AppResult<()>;
    fn search(&mut self, query: &[f32], k: usize, threshold: f32) -> AppResult<Vec<Hit>>;
    fn stats(&self) -> AppResult<(i64, i64, i64, i64, i64)>;
}

pub struct SqliteStore {
    conn: Connection,
    path: PathBuf,
    cache: Option<Vec<(i64, Vec<f32>)>>,
}

impl SqliteStore {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS documents (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               path TEXT NOT NULL UNIQUE,
               hash TEXT NOT NULL DEFAULT '',
               mtime INTEGER NOT NULL DEFAULT 0,
               bytes INTEGER NOT NULL DEFAULT 0,
               status TEXT NOT NULL DEFAULT 'queued',
               message TEXT,
               chunk_count INTEGER NOT NULL DEFAULT 0,
               indexed_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS chunks (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
               page INTEGER,
               ord INTEGER NOT NULL,
               text TEXT NOT NULL,
               embedding BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS chunks_doc ON chunks(doc_id);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Self { conn, path: path.to_path_buf(), cache: None })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn meta(&self, key: &str) -> AppResult<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> AppResult<()> {
        self.conn.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [key, value])?;
        Ok(())
    }

    /// Consistent snapshot of the whole index into another file.
    pub fn export_to(&self, dest: &Path) -> AppResult<()> {
        if dest.exists() {
            std::fs::remove_file(dest)?;
        }
        self.conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
        Ok(())
    }

    pub fn get_chunk(&self, chunk_id: i64) -> AppResult<Option<Hit>> {
        Ok(self
            .conn
            .query_row(
                "SELECT c.id, d.path, c.page, c.ord, c.text FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE c.id=?1",
                [chunk_id],
                |r| Ok(Hit { chunk_id: r.get(0)?, score: 0.0, doc_path: r.get(1)?, page: r.get::<_, Option<u32>>(2)?, ord: r.get(3)?, text: r.get(4)? }),
            )
            .optional()?)
    }

    fn load_cache(&mut self) -> AppResult<()> {
        if self.cache.is_some() {
            return Ok(());
        }
        let mut st = self.conn.prepare("SELECT id, embedding FROM chunks")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, from_blob(&r.get::<_, Vec<u8>>(1)?))))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        self.cache = Some(v);
        Ok(())
    }

    fn doc_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DocRow> {
        let path: String = r.get(1)?;
        let p = Path::new(&path);
        let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        let kind = p.extension().map(|s| s.to_string_lossy().to_uppercase()).unwrap_or_default();
        let status: String = r.get(5)?;
        Ok(DocRow {
            id: r.get(0)?,
            name,
            kind,
            progress: if status == "ready" { 1.0 } else { 0.0 },
            path,
            hash: r.get(2)?,
            mtime: r.get(3)?,
            bytes: r.get(4)?,
            status,
            message: r.get(6)?,
            chunk_count: r.get(7)?,
        })
    }
}

const DOC_COLS: &str = "id, path, hash, mtime, bytes, status, message, chunk_count";

impl VectorStore for SqliteStore {
    fn list_documents(&self) -> AppResult<Vec<DocRow>> {
        let mut st = self.conn.prepare(&format!("SELECT {DOC_COLS} FROM documents ORDER BY path COLLATE NOCASE"))?;
        let rows = st.query_map([], Self::doc_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn get_document(&self, path: &str) -> AppResult<Option<DocRow>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {DOC_COLS} FROM documents WHERE path=?1"), [path], Self::doc_row)
            .optional()?)
    }

    fn upsert_document(&mut self, path: &str, hash: &str, mtime: i64, bytes: i64, status: &str, message: Option<&str>) -> AppResult<i64> {
        self.conn.execute(
            "INSERT INTO documents(path,hash,mtime,bytes,status,message) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(path) DO UPDATE SET hash=excluded.hash, mtime=excluded.mtime, bytes=excluded.bytes,
               status=excluded.status, message=excluded.message",
            params![path, hash, mtime, bytes, status, message],
        )?;
        Ok(self.conn.query_row("SELECT id FROM documents WHERE path=?1", [path], |r| r.get(0))?)
    }

    fn set_status(&mut self, doc_id: i64, status: &str, message: Option<&str>) -> AppResult<()> {
        self.conn.execute("UPDATE documents SET status=?2, message=?3 WHERE id=?1", params![doc_id, status, message])?;
        Ok(())
    }

    fn replace_chunks(&mut self, doc_id: i64, chunks: &[NewChunk]) -> AppResult<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM chunks WHERE doc_id=?1", [doc_id])?;
        {
            let mut ins = tx.prepare("INSERT INTO chunks(doc_id,page,ord,text,embedding) VALUES(?1,?2,?3,?4,?5)")?;
            for c in chunks {
                ins.execute(params![doc_id, c.page, c.ord, c.text, to_blob(&c.embedding)])?;
            }
        }
        tx.execute(
            "UPDATE documents SET chunk_count=?2, status='ready', message=NULL, indexed_at=strftime('%s','now') WHERE id=?1",
            params![doc_id, chunks.len() as i64],
        )?;
        tx.commit()?;
        self.cache = None;
        Ok(())
    }

    fn delete_document(&mut self, path: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM documents WHERE path=?1", [path])?;
        self.cache = None;
        Ok(())
    }

    fn clear(&mut self) -> AppResult<()> {
        self.conn.execute_batch("DELETE FROM chunks; DELETE FROM documents;")?;
        self.cache = None;
        Ok(())
    }

    fn search(&mut self, query: &[f32], k: usize, threshold: f32) -> AppResult<Vec<Hit>> {
        self.load_cache()?;
        let cache = self.cache.as_ref().expect("cache loaded");
        let best = top_k(query, cache.iter().map(|(id, v)| (*id, v.as_slice())), k, threshold);
        let mut hits = Vec::with_capacity(best.len());
        for (id, score) in best {
            if let Some(mut h) = self.get_chunk(id)? {
                h.score = score;
                hits.push(h);
            }
        }
        Ok(hits)
    }

    /// (documents, chunks, bytes, skipped, pending)
    fn stats(&self) -> AppResult<(i64, i64, i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(chunk_count),0), COALESCE(SUM(bytes),0),
                    COALESCE(SUM(status='skipped'),0), COALESCE(SUM(status IN ('queued','indexing')),0) FROM documents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?)
    }
}

/// One open store per service, shared between search and the indexer.
#[derive(Default)]
pub struct Stores {
    map: parking_lot::Mutex<std::collections::HashMap<String, std::sync::Arc<parking_lot::Mutex<SqliteStore>>>>,
}

impl Stores {
    pub fn get(&self, service_id: &str) -> AppResult<std::sync::Arc<parking_lot::Mutex<SqliteStore>>> {
        let mut m = self.map.lock();
        if let Some(s) = m.get(service_id) {
            return Ok(s.clone());
        }
        let s = std::sync::Arc::new(parking_lot::Mutex::new(SqliteStore::open(&crate::services::index_path(service_id))?));
        m.insert(service_id.to_string(), s.clone());
        Ok(s)
    }

    /// Close (drop) a service's store so its folder can be deleted.
    pub fn close(&self, service_id: &str) {
        self.map.lock().remove(service_id);
    }

    pub fn close_all(&self) {
        self.map.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::normalize;

    fn v(x: &[f32]) -> Vec<f32> {
        let mut v = x.to_vec();
        normalize(&mut v);
        v
    }

    #[test]
    fn insert_search_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = SqliteStore::open(&dir.path().join("index.db")).unwrap();
        let d = s.upsert_document("C:\\a.txt", "h1", 1, 10, "indexing", None).unwrap();
        s.replace_chunks(d, &[
            NewChunk { page: None, ord: 0, text: "ลาพักร้อน".into(), embedding: v(&[1.0, 0.0]) },
            NewChunk { page: Some(2), ord: 1, text: "เงินเดือน".into(), embedding: v(&[0.0, 1.0]) },
        ]).unwrap();
        let hits = s.search(&v(&[0.9, 0.1]), 1, 0.0).unwrap();
        assert_eq!(hits[0].text, "ลาพักร้อน");
        assert_eq!(s.stats().unwrap().1, 2);
        s.delete_document("C:\\a.txt").unwrap();
        assert!(s.search(&v(&[1.0, 0.0]), 3, 0.0).unwrap().is_empty());
    }

    /// Two services = two files; neither sees the other's chunks.
    #[test]
    fn services_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let mut hr = SqliteStore::open(&dir.path().join("hr").join("index.db")).unwrap();
        let mut pos = SqliteStore::open(&dir.path().join("pos").join("index.db")).unwrap();
        let a = hr.upsert_document("C:\\same.pdf", "h", 1, 1, "indexing", None).unwrap();
        hr.replace_chunks(a, &[NewChunk { page: Some(1), ord: 0, text: "HR only".into(), embedding: v(&[1.0, 0.0]) }]).unwrap();
        // same file may live in both services with separate embeddings
        let b = pos.upsert_document("C:\\same.pdf", "h", 1, 1, "indexing", None).unwrap();
        pos.replace_chunks(b, &[NewChunk { page: Some(1), ord: 0, text: "POS copy".into(), embedding: v(&[0.0, 1.0]) }]).unwrap();
        let h = hr.search(&v(&[1.0, 0.0]), 5, -1.0).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].text, "HR only");
        let p = pos.search(&v(&[1.0, 0.0]), 5, -1.0).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].text, "POS copy");
        // deleting from one leaves the other intact
        hr.clear().unwrap();
        assert_eq!(pos.stats().unwrap().0, 1);
    }
}
