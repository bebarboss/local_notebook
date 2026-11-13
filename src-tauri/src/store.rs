//! Per-service vector store: `%APPDATA%\DocAsk\services\<id>\index.db` (SQLite).
//! Brute-force vector search over an in-memory cache, plus a trigram FTS5 index of the
//! same chunks for keyword search (works for Thai without word segmentation).
//! `hybrid_search` fuses both rankings. `VectorStore` keeps the door open for `sqlite-vec` later.

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

/// A picture kept from a document. `chunk_ord` = the chunk it sits in (None → whole page/file).
pub struct NewImage {
    pub page: Option<u32>,
    pub chunk_ord: Option<i64>,
    pub w: u32,
    pub h: u32,
    pub jpeg: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub id: i64,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Hit {
    pub chunk_id: i64,
    /// cosine similarity to the question
    pub score: f32,
    /// rank-fusion score (vector + keyword); comparable across services
    pub fused: f32,
    /// found by the keyword index
    pub keyword: bool,
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
    fn replace_chunks(&mut self, doc_id: i64, chunks: &[NewChunk], images: &[NewImage]) -> AppResult<()>;
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
             CREATE TABLE IF NOT EXISTS images (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
               page INTEGER,
               chunk_ord INTEGER,
               w INTEGER NOT NULL,
               h INTEGER NOT NULL,
               data BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS images_doc ON images(doc_id, page);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text, content='chunks', content_rowid='id', tokenize='trigram');
             CREATE TRIGGER IF NOT EXISTS chunks_fts_ins AFTER INSERT ON chunks BEGIN
               INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS chunks_fts_del AFTER DELETE ON chunks BEGIN
               INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
             END;",
        )?;
        let s = Self { conn, path: path.to_path_buf(), cache: None };
        // indexes made before v0.5: build the keyword index from the chunks already there
        if s.meta("fts")?.is_none() {
            s.conn.execute("INSERT INTO chunks_fts(chunks_fts) VALUES ('rebuild')", [])?;
            s.set_meta("fts", "1")?;
        }
        Ok(s)
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

    /// Point documents at new paths (export / import / a moved folder). `f` returns the new
    /// path, or None to keep the old one. Returns how many changed.
    pub fn rewrite_paths(&mut self, f: impl Fn(&str) -> Option<String>) -> AppResult<usize> {
        let rows: Vec<(i64, String)> = {
            let mut st = self.conn.prepare("SELECT id, path FROM documents")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            r
        };
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for (id, p) in rows {
            if let Some(np) = f(&p).filter(|np| *np != p) {
                tx.execute("UPDATE documents SET path=?2 WHERE id=?1", params![id, np])?;
                n += 1;
            }
        }
        tx.commit()?;
        self.cache = None;
        Ok(n)
    }

    pub fn vacuum(&self) -> AppResult<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// (page, ord, text, vector) of one document, in order — what the team sync uploads.
    pub fn export_chunks(&self, doc_id: i64) -> AppResult<Vec<(Option<u32>, i64, String, Vec<f32>)>> {
        let mut st = self.conn.prepare("SELECT page, ord, text, embedding FROM chunks WHERE doc_id=?1 ORDER BY ord, id")?;
        let rows = st.query_map([doc_id], |r| Ok((r.get::<_, Option<u32>>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, from_blob(&r.get::<_, Vec<u8>>(3)?))))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_chunk(&self, chunk_id: i64) -> AppResult<Option<Hit>> {
        Ok(self
            .conn
            .query_row(
                "SELECT c.id, d.path, c.page, c.ord, c.text FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE c.id=?1",
                [chunk_id],
                |r| Ok(Hit { chunk_id: r.get(0)?, doc_path: r.get(1)?, page: r.get::<_, Option<u32>>(2)?, ord: r.get(3)?, text: r.get(4)?, ..Default::default() }),
            )
            .optional()?)
    }

    /// Pictures to show next to a cited chunk: everything on its page (PDF/PPTX),
    /// or — for documents without pages — the pictures inside that chunk plus whole-file ones.
    pub fn images_for(&self, doc_path: &str, page: Option<u32>, ord: i64) -> AppResult<Vec<ImageInfo>> {
        let mut st = self.conn.prepare(
            "SELECT i.id, i.w, i.h FROM images i JOIN documents d ON d.id=i.doc_id
             WHERE d.path=?1 AND ((?2 IS NOT NULL AND i.page=?2)
                               OR (?2 IS NULL AND i.page IS NULL AND (i.chunk_ord=?3 OR i.chunk_ord IS NULL)))
             ORDER BY i.id LIMIT 12",
        )?;
        let rows = st.query_map(params![doc_path, page, ord], |r| Ok(ImageInfo { id: r.get(0)?, w: r.get(1)?, h: r.get(2)? }))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Text → vector of a document's current chunks (to reuse when it is re-indexed).
    pub fn chunk_vectors(&self, doc_id: i64) -> AppResult<std::collections::HashMap<String, Vec<f32>>> {
        let mut st = self.conn.prepare("SELECT text, embedding FROM chunks WHERE doc_id=?1")?;
        let rows = st.query_map([doc_id], |r| Ok((r.get::<_, String>(0)?, from_blob(&r.get::<_, Vec<u8>>(1)?))))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Make the next scan re-extract every file (chunks stay until each file is redone).
    pub fn requeue_all(&mut self) -> AppResult<()> {
        self.conn.execute("UPDATE documents SET mtime=0, hash=''", [])?;
        Ok(())
    }

    pub fn image_data(&self, id: i64) -> AppResult<Option<Vec<u8>>> {
        Ok(self.conn.query_row("SELECT data FROM images WHERE id=?1", [id], |r| r.get(0)).optional()?)
    }

    /// Chunk ids best matching the words of `text` (trigram BM25), best first.
    pub fn keyword_search(&self, text: &str, n: usize) -> AppResult<Vec<i64>> {
        let grams = query_trigrams(text);
        if grams.is_empty() || n == 0 {
            return Ok(Vec::new());
        }
        let q = grams.iter().map(|g| format!("\"{}\"", g.replace('"', "\"\""))).collect::<Vec<_>>().join(" OR ");
        let mut st = self.conn.prepare("SELECT rowid, text FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2")?;
        let rows = st.query_map(params![q, (n * 3) as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            let (id, body) = r?;
            // OR over trigrams matches almost anything; keep chunks that share enough of the question
            if coverage(&grams, &body) >= MIN_KEYWORD_COVERAGE {
                out.push(id);
                if out.len() == n {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Vector search fused with keyword search (reciprocal rank fusion).
    /// Vector hits obey `threshold`; keyword hits do not (exact codes/names can have a low cosine).
    pub fn hybrid_search(&mut self, query: &[f32], text: &str, k: usize, threshold: f32) -> AppResult<Vec<Hit>> {
        self.load_cache()?;
        let kw = self.keyword_search(text, k)?;
        let cache = self.cache.as_ref().expect("cache loaded");
        let vec_hits = top_k(query, cache.iter().map(|(id, v)| (*id, v.as_slice())), k, threshold);

        // id -> (fused, cosine, keyword)
        let mut fused: std::collections::HashMap<i64, (f32, Option<f32>, bool)> = std::collections::HashMap::new();
        for (rank, (id, cos)) in vec_hits.iter().enumerate() {
            fused.entry(*id).or_insert((0.0, Some(*cos), false)).0 += rrf(rank);
        }
        for (rank, id) in kw.iter().enumerate() {
            let e = fused.entry(*id).or_insert((0.0, None, false));
            e.0 += rrf(rank);
            e.2 = true;
        }
        // cosine for keyword-only hits, so every source shows a comparable relevance
        let need: std::collections::HashSet<i64> = fused.iter().filter(|(_, v)| v.1.is_none()).map(|(id, _)| *id).collect();
        if !need.is_empty() {
            for (id, v) in cache.iter().filter(|(id, _)| need.contains(id)) {
                if let Some(e) = fused.get_mut(id) {
                    e.1 = Some(crate::search::dot(query, v));
                }
            }
        }
        let mut order: Vec<(i64, (f32, Option<f32>, bool))> = fused.into_iter().collect();
        order.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0).then(b.1 .1.unwrap_or(0.0).total_cmp(&a.1 .1.unwrap_or(0.0))));
        order.truncate(k);
        let mut hits = Vec::with_capacity(order.len());
        for (id, (f, cos, keyword)) in order {
            if let Some(mut h) = self.get_chunk(id)? {
                h.score = cos.unwrap_or(0.0);
                h.fused = f;
                h.keyword = keyword;
                hits.push(h);
            }
        }
        Ok(hits)
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

/// Reciprocal rank fusion constant (the usual 60).
const RRF_K: f32 = 60.0;
fn rrf(rank: usize) -> f32 {
    1.0 / (RRF_K + rank as f32 + 1.0)
}

/// Share of the question's trigrams a chunk must contain to count as a keyword hit.
const MIN_KEYWORD_COVERAGE: f32 = 0.35;
const MAX_QUERY_TRIGRAMS: usize = 64;

/// Distinct lower-case 3-character windows of each word of the question
/// (words = runs between spaces/punctuation; Thai runs are long, which is fine).
pub fn query_trigrams(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for word in lower.split(|c: char| c.is_whitespace() || "?!,;:\"'()[]{}<>“”‘’…|*`~^=+".contains(c)) {
        let chars: Vec<char> = word.trim_matches(|c: char| c == '.' || c == '-' || c == '/').chars().collect();
        for w in chars.windows(3) {
            let g: String = w.iter().collect();
            if !out.contains(&g) {
                out.push(g);
            }
        }
    }
    out.truncate(MAX_QUERY_TRIGRAMS);
    out
}

fn coverage(grams: &[String], body: &str) -> f32 {
    if grams.is_empty() {
        return 0.0;
    }
    let body = body.to_lowercase();
    grams.iter().filter(|g| body.contains(g.as_str())).count() as f32 / grams.len() as f32
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

    fn replace_chunks(&mut self, doc_id: i64, chunks: &[NewChunk], images: &[NewImage]) -> AppResult<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM chunks WHERE doc_id=?1", [doc_id])?;
        tx.execute("DELETE FROM images WHERE doc_id=?1", [doc_id])?;
        {
            let mut ins = tx.prepare("INSERT INTO chunks(doc_id,page,ord,text,embedding) VALUES(?1,?2,?3,?4,?5)")?;
            for c in chunks {
                ins.execute(params![doc_id, c.page, c.ord, c.text, to_blob(&c.embedding)])?;
            }
            let mut img = tx.prepare("INSERT INTO images(doc_id,page,chunk_ord,w,h,data) VALUES(?1,?2,?3,?4,?5,?6)")?;
            for i in images {
                img.execute(params![doc_id, i.page, i.chunk_ord, i.w, i.h, i.jpeg])?;
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
        self.conn.execute_batch("DELETE FROM images; DELETE FROM chunks; DELETE FROM documents;")?;
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
                h.fused = score;
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
        ], &[]).unwrap();
        let hits = s.search(&v(&[0.9, 0.1]), 1, 0.0).unwrap();
        assert_eq!(hits[0].text, "ลาพักร้อน");
        assert_eq!(s.stats().unwrap().1, 2);
        s.delete_document("C:\\a.txt").unwrap();
        assert!(s.search(&v(&[1.0, 0.0]), 3, 0.0).unwrap().is_empty());
    }

    #[test]
    fn images_by_page_and_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = SqliteStore::open(&dir.path().join("index.db")).unwrap();
        let img = |page, chunk_ord, b: u8| NewImage { page, chunk_ord, w: 10, h: 5, jpeg: vec![b] };
        let c = |page, ord| NewChunk { page, ord, text: "x".into(), embedding: v(&[1.0, 0.0]) };
        let pdf = s.upsert_document("C:\\a.pdf", "h", 1, 1, "indexing", None).unwrap();
        s.replace_chunks(pdf, &[c(Some(1), 0), c(Some(2), 1)], &[img(Some(1), Some(0), 1), img(Some(2), None, 2), img(Some(2), Some(1), 3)]).unwrap();
        let docx = s.upsert_document("C:\\b.docx", "h", 1, 1, "indexing", None).unwrap();
        s.replace_chunks(docx, &[c(None, 0), c(None, 1)], &[img(None, Some(0), 4), img(None, Some(1), 5)]).unwrap();

        let ids = |s: &SqliteStore, p: &str, page, ord| s.images_for(p, page, ord).unwrap().iter().map(|i| s.image_data(i.id).unwrap().unwrap()[0]).collect::<Vec<_>>();
        assert_eq!(ids(&s, "C:\\a.pdf", Some(2), 1), vec![2, 3]); // whole page
        assert_eq!(ids(&s, "C:\\a.pdf", Some(1), 0), vec![1]);
        assert_eq!(ids(&s, "C:\\b.docx", None, 1), vec![5]); // only the chunk's own picture
        // re-index replaces, delete cascades
        s.replace_chunks(docx, &[c(None, 0)], &[]).unwrap();
        assert!(ids(&s, "C:\\b.docx", None, 0).is_empty());
        s.delete_document("C:\\a.pdf").unwrap();
        assert!(ids(&s, "C:\\a.pdf", Some(2), 1).is_empty());
    }

    #[test]
    fn vectors_survive_requeue() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = SqliteStore::open(&dir.path().join("index.db")).unwrap();
        let d = s.upsert_document("C:\\a.txt", "h", 5, 1, "indexing", None).unwrap();
        s.replace_chunks(d, &[NewChunk { page: None, ord: 0, text: "เดิม".into(), embedding: v(&[1.0, 0.0]) }], &[]).unwrap();
        s.requeue_all().unwrap();
        let doc = s.get_document("C:\\a.txt").unwrap().unwrap();
        assert_eq!((doc.mtime, doc.hash.as_str()), (0, ""), "file must be picked up again");
        let known = s.chunk_vectors(d).unwrap();
        assert_eq!(known.get("เดิม").map(|x| x.len()), Some(2), "vector kept for reuse");
    }

    #[test]
    fn keyword_finds_exact_codes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = SqliteStore::open(&dir.path().join("index.db")).unwrap();
        let d = s.upsert_document("C:\\c.txt", "h", 1, 1, "indexing", None).unwrap();
        // the vectors say nothing useful: every chunk points the same way
        let same = v(&[1.0, 0.0]);
        let texts = ["สัญญาเลขที่ 2567/044 ผู้รับจ้าง บริษัท ก จำกัด สิ้นสุด 31 มีนาคม 2568",
                     "สัญญาเลขที่ 2567/045 ผู้รับจ้าง บริษัท ข จำกัด สิ้นสุด 30 กันยายน 2568",
                     "สัญญาเลขที่ 2567/046 ผู้รับจ้าง บริษัท ค จำกัด สิ้นสุด 31 ธันวาคม 2568",
                     "Invoice INV-7781 paid by transfer"];
        let chunks: Vec<NewChunk> = texts.iter().enumerate().map(|(i, t)| NewChunk { page: None, ord: i as i64, text: t.to_string(), embedding: same.clone() }).collect();
        s.replace_chunks(d, &chunks, &[]).unwrap();

        let kw = s.keyword_search("สัญญา 2567/045 หมดอายุเมื่อไร", 3).unwrap();
        assert_eq!(s.get_chunk(kw[0]).unwrap().unwrap().ord, 1);
        let kw = s.keyword_search("inv-7781", 3).unwrap();
        assert_eq!(kw.len(), 1, "case-insensitive, unrelated chunks filtered");
        assert!(s.keyword_search("ab", 3).unwrap().is_empty(), "too short for trigrams");

        // the vector points away from the contract: only the keyword index can bring it in
        let away = v(&[0.0, 1.0]);
        let h = s.hybrid_search(&away, "2567/045", 3, 0.5).unwrap();
        let hit = h.iter().find(|h| h.ord == 1).expect("keyword hit kept below the vector threshold");
        assert!(hit.keyword && hit.score.abs() < 0.01);
        assert!(s.search(&away, 3, 0.5).unwrap().is_empty(), "vector search alone finds nothing");

        // keyword index follows re-index and delete
        assert_eq!(s.keyword_search("กันยายน", 3).unwrap().len(), 1);
        s.replace_chunks(d, &chunks[..1], &[]).unwrap();
        assert!(s.keyword_search("กันยายน", 3).unwrap().is_empty());
        s.delete_document("C:\\c.txt").unwrap();
        assert!(s.keyword_search("2567/044", 3).unwrap().is_empty());
    }

    /// An index made before the keyword table existed gets it filled on open.
    #[test]
    fn keyword_index_built_for_old_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index.db");
        {
            let mut s = SqliteStore::open(&p).unwrap();
            let d = s.upsert_document("C:\\a.txt", "h", 1, 1, "indexing", None).unwrap();
            s.replace_chunks(d, &[NewChunk { page: None, ord: 0, text: "รหัสสินค้า SKU-55012".into(), embedding: v(&[1.0, 0.0]) }], &[]).unwrap();
            // simulate a v0.4 index: no keyword table
            s.conn.execute_batch("DROP TRIGGER chunks_fts_ins; DROP TRIGGER chunks_fts_del; DROP TABLE chunks_fts; DELETE FROM meta WHERE key='fts';").unwrap();
        }
        let s = SqliteStore::open(&p).unwrap();
        assert_eq!(s.keyword_search("sku-55012", 3).unwrap().len(), 1);
    }

    /// Vector-only vs hybrid on Thai policy text plus look-alike records (codes, names).
    /// `cargo test --release --lib bench_hybrid -- --ignored --nocapture`
    /// with DOCASK_TEST_EMBED (e5-small folder) and optional DOCASK_TEST_RERANK (folder of model folders, uses `mm`).
    #[test]
    #[ignore]
    fn bench_hybrid() {
        use crate::encoder::{Encoder, Layout, Pooling};
        let mut docs: Vec<String> = vec![
            "การลาพักผ่อนประจำปี พนักงานที่ทำงานครบ 1 ปีมีสิทธิ์ลาพักผ่อนได้ 10 วันทำการต่อปี ต้องยื่นใบลาผ่านระบบล่วงหน้าอย่างน้อย 3 วันทำการ".into(),
            "การลาป่วย พนักงานลาป่วยได้ตามจริงไม่เกิน 30 วันทำการต่อปีโดยได้รับค่าจ้าง หากลาป่วยติดต่อกันตั้งแต่ 3 วันขึ้นไปต้องแนบใบรับรองแพทย์".into(),
            "การลาออก พนักงานต้องยื่นใบลาออกเป็นลายลักษณ์อักษรล่วงหน้าไม่น้อยกว่า 30 วันก่อนวันที่มีผล และคืนทรัพย์สินของบริษัท".into(),
            "การเบิกค่าเดินทาง การเดินทางไปปฏิบัติงานต่างจังหวัด เบิกค่าเบี้ยเลี้ยงวันละ 300 บาท ค่าที่พักไม่เกินคืนละ 1,200 บาท".into(),
            "การทำงานล่วงเวลา ต้องได้รับอนุมัติจากหัวหน้างานก่อน ค่าล่วงเวลาวันทำงานปกติ 1.5 เท่า วันหยุด 3 เท่าของค่าจ้างต่อชั่วโมง".into(),
            "การรีเซ็ตรหัสผ่าน หากลืมรหัสผ่านเข้าระบบ ให้กดลืมรหัสผ่านที่หน้า login ระบบจะส่งลิงก์ไปที่อีเมลบริษัท".into(),
            "การคืนสินค้า ลูกค้าคืนสินค้าได้ภายใน 7 วันพร้อมใบเสร็จ ยกเว้นสินค้าสดและสินค้าลดราคา ไม่รับคืน".into(),
            "การจัดซื้อ การซื้อสินค้าหรือบริการมูลค่าเกิน 100,000 บาท ต้องเปรียบเทียบราคาจากผู้ขายอย่างน้อย 3 ราย".into(),
            "เวลาทำงาน บริษัทกำหนดเวลาทำงานวันจันทร์ถึงศุกร์ 08.30 ถึง 17.30 น. มาสายเกิน 15 นาทีถือว่าสาย".into(),
            "สวัสดิการกองทุนสำรองเลี้ยงชีพ พนักงานสมัครเข้ากองทุนได้หลังผ่านทดลองงาน บริษัทสมทบร้อยละ 5".into(),
        ];
        let firms = ["สยามบริการ", "ไทยเทคโซลูชั่น", "รุ่งเรืองขนส่ง", "อินโนเวทซอฟต์", "เอเชียคลีนนิ่ง", "บางกอกซีเคียว", "พรีเมียมฟู้ด", "กรีนเอนเนอร์ยี่"];
        let months = ["มกราคม", "มีนาคม", "พฤษภาคม", "กรกฎาคม", "กันยายน", "พฤศจิกายน"];
        // look-alike records inside longer chunks, as in real contracts and reports
        let filler = "ทั้งนี้ผู้รับจ้างต้องปฏิบัติตามเงื่อนไขและข้อกำหนดของบริษัท รายงานผลการดำเนินงานทุกเดือน และรักษาความลับของข้อมูลลูกค้าตามนโยบายความปลอดภัยสารสนเทศ หากผิดสัญญาบริษัทมีสิทธิ์บอกเลิกสัญญาและเรียกค่าเสียหาย";
        for i in 0..200 {
            docs.push(format!(
                "{filler} สัญญาเลขที่ 2567/{:03} ผู้รับจ้าง บริษัท {} จำกัด งานบริการบำรุงรักษาระบบ มูลค่า {},000 บาท สิ้นสุดสัญญาวันที่ {} {} 2568 {filler}",
                i, firms[i % firms.len()], 120 + (i * 37) % 900, 1 + i % 28, months[i % months.len()]
            ));
        }
        let people = [("สมชาย ใจดี", "ผู้จัดการสาขาลาดพร้าว"), ("วิภาวรรณ ศรีสุข", "หัวหน้าฝ่ายบัญชี"), ("ธนพล แก้วมณี", "ผู้ดูแลระบบ POS"),
                      ("กมลชนก วงศ์ทอง", "เจ้าหน้าที่ฝ่ายบุคคล"), ("ประเสริฐ มั่นคง", "ผู้จัดการฝ่ายจัดซื้อ"), ("ณัฐวุฒิ พรหมมา", "หัวหน้าคลังสินค้า")];
        for (n, r) in people {
            docs.push(format!("ผู้ติดต่อ: คุณ{n} ตำแหน่ง {r} โทรภายใน ต่อ {} รับผิดชอบงานประสานงานกับฝ่ายต่างๆ", 1000 + n.len()));
        }
        let first = ["สมศักดิ์", "สุดารัตน์", "อนุชา", "พิมพ์ชนก", "วีระพงษ์", "จิราพร", "ชัยวัฒน์", "นภัสสร", "ปิยะ", "รัตนา"];
        let last = ["สุขสวัสดิ์", "ทองดี", "แสงทอง", "บุญมา", "ศรีวงศ์", "จันทร์เพ็ญ"];
        for i in 0..60 {
            docs.push(format!("ผู้ติดต่อ: คุณ{} {} ตำแหน่ง {} โทรภายใน ต่อ {} รับผิดชอบงานประสานงานกับฝ่ายต่างๆ",
                first[i % first.len()], last[i % last.len()], people[i % people.len()].1, 2000 + i));
        }
        for i in 0..120 {
            docs.push(format!("สินค้ารหัส SKU-{} ราคาขายปลีก {} บาท จัดเก็บที่ชั้นวาง {} ในคลังสินค้ากลาง", 50000 + i * 97 % 9000, 45 + i, i % 9 + 1));
        }
        for (i, code) in ["SKU-55012", "SKU-55021", "SKU-55102", "SKU-51205"].iter().enumerate() {
            docs.push(format!("สินค้ารหัส {code} ราคาขายปลีก {} บาท จัดเก็บที่ชั้นวาง {} ในคลังสินค้ากลาง", 45 + i * 10, i + 3));
        }
        let at = |needle: &str| docs.iter().position(|d| d.contains(needle)).unwrap();
        let qs: Vec<(String, usize)> = vec![
            ("ลาป่วยกี่วันต้องมีใบแพทย์".into(), 1), ("ถ้าจะลาออกต้องแจ้งล่วงหน้ากี่วัน".into(), 2),
            ("ไปทำงานต่างจังหวัดเบิกค่าโรงแรมได้เท่าไร".into(), 3), ("ทำโอทีวันเสาร์อาทิตย์ได้กี่เท่า".into(), 4),
            ("ลืม password ทำยังไง".into(), 5), ("ลูกค้าเอาผักมาคืนได้ไหม".into(), 6),
            ("ซื้อของราคาสองแสนต้องทำอะไรบ้าง".into(), 7), ("เข้างานกี่โมง".into(), 8), ("บริษัทสมทบ provident fund กี่เปอร์เซ็นต์".into(), 9),
            ("สัญญาเลขที่ 2567/045 หมดอายุเมื่อไร".into(), at("2567/045")), ("สัญญา 2567/058 มูลค่าเท่าไร".into(), at("2567/058")),
            ("2567/051 ผู้รับจ้างคือใคร".into(), at("2567/051")),
            ("คุณวิภาวรรณ ศรีสุข ตำแหน่งอะไร".into(), at("วิภาวรรณ")), ("เบอร์ภายในของธนพล".into(), at("ธนพล")),
            ("ณัฐวุฒิ พรหมมา ดูแลเรื่องอะไร".into(), at("ณัฐวุฒิ")),
            ("SKU-55021 ราคาเท่าไร".into(), at("SKU-55021")), ("sku-51205 เก็บไว้ที่ไหน".into(), at("SKU-51205")),
        ];

        let emb_dir = std::path::PathBuf::from(std::env::var("DOCASK_TEST_EMBED").expect("DOCASK_TEST_EMBED"));
        let emb = Encoder::load(&emb_dir.join("onnx").join("model_quantized.onnx"), &emb_dir, 512, false, Layout::auto()).unwrap();
        let pv = emb.embed(&docs.iter().map(|p| format!("passage: {p}")).collect::<Vec<_>>(), Pooling::Mean, 8).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut st = SqliteStore::open(&dir.path().join("index.db")).unwrap();
        let d = st.upsert_document("C:\\bench.txt", "h", 1, 1, "indexing", None).unwrap();
        let chunks: Vec<NewChunk> = docs.iter().zip(&pv).enumerate().map(|(i, (t, e))| NewChunk { page: None, ord: i as i64, text: t.clone(), embedding: e.clone() }).collect();
        st.replace_chunks(d, &chunks, &[]).unwrap();

        let rr = std::env::var("DOCASK_TEST_RERANK").ok().map(|r| std::path::PathBuf::from(r).join("mm")).and_then(|dir| {
            Encoder::load(&dir.join("onnx").join("model_quantized.onnx"), &dir, 512, true, Layout { intra: Layout::auto().intra * Layout::auto().workers, workers: 1 }).ok()
        });
        let k = 5;
        let pos = |hits: &[Hit], gold: usize| hits.iter().position(|h| h.ord == gold as i64).map(|p| p + 1).unwrap_or(99);
        let mut table: Vec<(String, Vec<usize>)> = Vec::new();
        for mode in ["vector", "hybrid", "vector+rerank", "hybrid+rerank"] {
            if mode.ends_with("rerank") && rr.is_none() {
                continue;
            }
            let want = if mode.ends_with("rerank") { 15 } else { k };
            let mut ranks = Vec::new();
            for (q, gold) in &qs {
                let qv = emb.embed(&[format!("query: {q}")], Pooling::Mean, 1).unwrap().pop().unwrap();
                let mut hits = if mode.starts_with("hybrid") { st.hybrid_search(&qv, q, want, 0.3).unwrap() } else { st.search(&qv, want, 0.3).unwrap() };
                if let Some(rr) = rr.as_ref().filter(|_| mode.ends_with("rerank")) {
                    let s = rr.score_pairs(q, &hits.iter().map(|h| h.text.clone()).collect::<Vec<_>>(), 4).unwrap();
                    let mut idx: Vec<usize> = (0..hits.len()).collect();
                    idx.sort_by(|a, b| s[*b].total_cmp(&s[*a]));
                    hits = idx.into_iter().map(|i| hits[i].clone()).collect();
                }
                hits.truncate(k);
                ranks.push(pos(&hits, *gold));
            }
            table.push((mode.to_string(), ranks));
        }
        for (mode, ranks) in &table {
            let (sem, exact) = ranks.split_at(9);
            let top1 = |r: &[usize]| r.iter().filter(|&&x| x == 1).count();
            let found = |r: &[usize]| r.iter().filter(|&&x| x <= k).count();
            println!("{mode:<15} meaning: top-1 {}/{} in top-{k} {}/{}   exact words: top-1 {}/{} in top-{k} {}/{}   ranks {ranks:?}",
                top1(sem), sem.len(), found(sem), sem.len(), top1(exact), exact.len(), found(exact), exact.len());
        }
    }

    /// Two services = two files; neither sees the other's chunks.
    #[test]
    fn services_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let mut hr = SqliteStore::open(&dir.path().join("hr").join("index.db")).unwrap();
        let mut pos = SqliteStore::open(&dir.path().join("pos").join("index.db")).unwrap();
        let a = hr.upsert_document("C:\\same.pdf", "h", 1, 1, "indexing", None).unwrap();
        hr.replace_chunks(a, &[NewChunk { page: Some(1), ord: 0, text: "HR only".into(), embedding: v(&[1.0, 0.0]) }], &[]).unwrap();
        // same file may live in both services with separate embeddings
        let b = pos.upsert_document("C:\\same.pdf", "h", 1, 1, "indexing", None).unwrap();
        pos.replace_chunks(b, &[NewChunk { page: Some(1), ord: 0, text: "POS copy".into(), embedding: v(&[0.0, 1.0]) }], &[]).unwrap();
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
