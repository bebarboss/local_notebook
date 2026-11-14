"""PostgreSQL (pgvector) storage for DocAsk: the shared document index and chat history.

Optional: without DATABASE_URL the server only does embeddings, as before.
Texts are never logged — only counts and timings.
"""

from __future__ import annotations

import logging
import time
from typing import Any, Iterable

from psycopg.rows import dict_row
from psycopg.types.json import Jsonb
from psycopg_pool import ConnectionPool
from pgvector.psycopg import register_vector

log = logging.getLogger("docask-embed")

SCHEMA_VERSION = 1
RRF_K = 60.0
MIN_KEYWORD_COVERAGE = 0.35
MAX_QUERY_TRIGRAMS = 64
_SPLIT = set("?!,;:\"'()[]{}<>“”‘’…|*`~^=+")


def now_ms() -> int:
    return int(time.time() * 1000)


def query_trigrams(text: str) -> list[str]:
    """Distinct lower-case 3-character windows of each word (same as the app's local search)."""
    out: list[str] = []
    word = []
    for c in text.lower() + " ":
        if c.isspace() or c in _SPLIT:
            w = "".join(word).strip(".-/")
            for i in range(len(w) - 2):
                g = w[i:i + 3]
                if g not in out:
                    out.append(g)
            word = []
        else:
            word.append(c)
    return out[:MAX_QUERY_TRIGRAMS]


def rrf(rank: int) -> float:
    return 1.0 / (RRF_K + rank + 1.0)


def _schema(dim: int) -> str:
    return f"""
    CREATE EXTENSION IF NOT EXISTS vector;
    CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS services (
      id TEXT PRIMARY KEY,
      data JSONB NOT NULL,
      updated_at BIGINT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS documents (
      id BIGSERIAL PRIMARY KEY,
      service_id TEXT NOT NULL,
      path TEXT NOT NULL,
      hash TEXT NOT NULL DEFAULT '',
      mtime BIGINT NOT NULL DEFAULT 0,
      bytes BIGINT NOT NULL DEFAULT 0,
      status TEXT NOT NULL DEFAULT 'queued',
      message TEXT,
      chunk_count INTEGER NOT NULL DEFAULT 0,
      indexed_by TEXT NOT NULL DEFAULT '',
      updated_at BIGINT NOT NULL,
      UNIQUE (service_id, path)
    );
    CREATE TABLE IF NOT EXISTS chunks (
      id BIGSERIAL PRIMARY KEY,
      document_id BIGINT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
      ord INTEGER NOT NULL,
      page INTEGER,
      text TEXT NOT NULL,
      embedding vector({dim}) NOT NULL
    );
    CREATE INDEX IF NOT EXISTS chunks_doc ON chunks(document_id);
    CREATE INDEX IF NOT EXISTS chunks_vec ON chunks USING hnsw (embedding vector_cosine_ops);
    CREATE TABLE IF NOT EXISTS chats (
      id TEXT PRIMARY KEY,
      service_id TEXT NOT NULL,
      title TEXT NOT NULL,
      created_at BIGINT NOT NULL,
      updated_at BIGINT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS chats_service ON chats(service_id, updated_at DESC);
    CREATE TABLE IF NOT EXISTS messages (
      id BIGSERIAL PRIMARY KEY,
      chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
      role TEXT NOT NULL,
      content TEXT NOT NULL,
      model TEXT,
      reasoning_effort TEXT,
      sources JSONB NOT NULL DEFAULT '[]',
      elapsed_ms BIGINT,
      error TEXT,
      search_query TEXT,
      created_at BIGINT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS messages_chat ON messages(chat_id, id);
    """


class StoreError(Exception):
    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


class Store:
    """All SQL lives here; routes stay thin. `dim` / `model` come from the embedding model."""

    def __init__(self, url: str, dim: int, model: str):
        self.dim = dim
        self.model = model
        self.pool = ConnectionPool(url, min_size=1, max_size=8, kwargs={"autocommit": True}, configure=self._configure, open=True)
        self.migrate()

    @staticmethod
    def _configure(conn) -> None:
        # the extension must exist before its type can be registered
        conn.execute("CREATE EXTENSION IF NOT EXISTS vector")
        register_vector(conn)

    def close(self) -> None:
        self.pool.close()

    def migrate(self) -> None:
        with self.pool.connection() as c:
            c.execute(_schema(self.dim))
            # v2: a share key per service (SHA-256 of it; the key itself is never stored)
            c.execute("ALTER TABLE services ADD COLUMN IF NOT EXISTS key_hash TEXT")
            c.execute("CREATE UNIQUE INDEX IF NOT EXISTS services_key ON services(key_hash) WHERE key_hash IS NOT NULL")
            # v3: the original file (stored on disk by its SHA-256, see app.py) and the document's pictures
            c.execute("ALTER TABLE documents ADD COLUMN IF NOT EXISTS file_hash TEXT")
            c.execute("ALTER TABLE documents ADD COLUMN IF NOT EXISTS file_name TEXT")
            c.execute("ALTER TABLE documents ADD COLUMN IF NOT EXISTS file_size BIGINT")
            c.execute("""CREATE TABLE IF NOT EXISTS images (
                 id BIGSERIAL PRIMARY KEY,
                 document_id BIGINT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                 page INTEGER, chunk_ord INTEGER, w INTEGER NOT NULL, h INTEGER NOT NULL,
                 data BYTEA NOT NULL)""")
            c.execute("CREATE INDEX IF NOT EXISTS images_doc ON images(document_id, page)")
            # pg_trgm only speeds up keyword search (LIKE works without it)
            try:
                with c.transaction():
                    c.execute("CREATE EXTENSION IF NOT EXISTS pg_trgm")
                    c.execute("CREATE INDEX IF NOT EXISTS chunks_trgm ON chunks USING gin (lower(text) gin_trgm_ops)")
            except Exception as e:  # noqa: BLE001 — a missing extension is not fatal
                log.warning("pg_trgm unavailable, keyword search without an index: %s", type(e).__name__)
            row = c.execute("SELECT value FROM meta WHERE key='model'").fetchone()
            if row is None:
                c.execute("INSERT INTO meta VALUES ('model', %s), ('dim', %s), ('schema', %s)", (self.model, str(self.dim), str(SCHEMA_VERSION)))
            elif row[0] != self.model:
                # vectors of another model cannot be searched with this one
                raise SystemExit(f"database was built with model {row[0]!r}, this server runs {self.model!r} — "
                                 "use the same EMBED_MODEL or a new database")
        log.info("database ready (schema %d, %d dims)", SCHEMA_VERSION, self.dim)

    # ------------------------------------------------------------------ info

    def info(self) -> dict:
        with self.pool.connection() as c:
            d, ch, chats = c.execute(
                "SELECT (SELECT count(*) FROM documents), (SELECT count(*) FROM chunks), (SELECT count(*) FROM chats)"
            ).fetchone()
        return {"model": self.model, "dim": self.dim, "documents": d, "chunks": ch, "chats": chats}

    # -------------------------------------------------------------- services

    def services(self) -> list[dict]:
        with self.pool.connection() as c:
            rows = c.execute("SELECT id, data, updated_at FROM services ORDER BY updated_at").fetchall()
        return [{"id": r[0], "data": r[1], "updatedAt": r[2]} for r in rows]

    def put_service(self, sid: str, data: dict) -> dict:
        t = now_ms()
        with self.pool.connection() as c:
            c.execute(
                "INSERT INTO services VALUES (%s, %s, %s) ON CONFLICT (id) DO UPDATE SET data=EXCLUDED.data, updated_at=EXCLUDED.updated_at",
                (sid, Jsonb(data), t),
            )
        return {"id": sid, "data": data, "updatedAt": t}

    def get_service(self, sid: str) -> dict:
        with self.pool.connection() as c:
            r = c.execute("SELECT id, data, updated_at FROM services WHERE id=%s", (sid,)).fetchone()
        if r is None:
            raise StoreError(404, "no such service")
        return {"id": r[0], "data": r[1], "updatedAt": r[2]}

    def set_service_key(self, sid: str, key_hash: str) -> None:
        with self.pool.connection() as c:
            n = c.execute("UPDATE services SET key_hash=%s WHERE id=%s", (key_hash, sid)).rowcount
        if not n:
            raise StoreError(404, "no such service")

    def service_of_key(self, key_hash: str) -> str | None:
        with self.pool.connection() as c:
            r = c.execute("SELECT id FROM services WHERE key_hash=%s", (key_hash,)).fetchone()
        return r[0] if r else None

    def service_of_document(self, doc_id: int) -> str:
        with self.pool.connection() as c:
            r = c.execute("SELECT service_id FROM documents WHERE id=%s", (doc_id,)).fetchone()
        if r is None:
            raise StoreError(404, "no such document")
        return r[0]

    def delete_service(self, sid: str, with_data: bool) -> None:
        with self.pool.connection() as c, c.transaction():
            c.execute("DELETE FROM services WHERE id=%s", (sid,))
            if with_data:
                c.execute("DELETE FROM documents WHERE service_id=%s", (sid,))
                c.execute("DELETE FROM chats WHERE service_id=%s", (sid,))

    # ------------------------------------------------------------- documents

    _DOC = "id, path, hash, mtime, bytes, status, message, chunk_count, indexed_by, updated_at, file_hash, file_size"

    @staticmethod
    def _doc(r) -> dict:
        return {"id": r[0], "path": r[1], "hash": r[2], "mtime": r[3], "bytes": r[4], "status": r[5],
                "message": r[6], "chunkCount": r[7], "indexedBy": r[8], "updatedAt": r[9],
                "fileHash": r[10], "fileSize": r[11]}

    def documents(self, sid: str) -> list[dict]:
        with self.pool.connection() as c:
            rows = c.execute(f"SELECT {self._DOC} FROM documents WHERE service_id=%s ORDER BY lower(path)", (sid,)).fetchall()
        return [self._doc(r) for r in rows]

    def put_document(self, sid: str, d: dict) -> dict:
        with self.pool.connection() as c:
            r = c.execute(
                f"""INSERT INTO documents(service_id, path, hash, mtime, bytes, status, message, indexed_by, updated_at)
                    VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s)
                    ON CONFLICT (service_id, path) DO UPDATE SET hash=EXCLUDED.hash, mtime=EXCLUDED.mtime, bytes=EXCLUDED.bytes,
                      status=EXCLUDED.status, message=EXCLUDED.message, indexed_by=EXCLUDED.indexed_by, updated_at=EXCLUDED.updated_at
                    RETURNING {self._DOC}""",
                (sid, d["path"], d.get("hash", ""), d.get("mtime", 0), d.get("bytes", 0), d.get("status", "queued"),
                 d.get("message"), d.get("indexedBy", ""), now_ms()),
            ).fetchone()
        return self._doc(r)

    def set_status(self, doc_id: int, status: str, message: str | None) -> None:
        with self.pool.connection() as c:
            n = c.execute("UPDATE documents SET status=%s, message=%s, updated_at=%s WHERE id=%s", (status, message, now_ms(), doc_id)).rowcount
        if not n:
            raise StoreError(404, "no such document")

    def add_chunks(self, doc_id: int, chunks: list[dict], vectors: list[list[float]], replace: bool) -> int:
        """Store chunks (with their vectors). `replace` drops the document's old chunks first."""
        import numpy as np

        with self.pool.connection() as c, c.transaction():
            if c.execute("SELECT 1 FROM documents WHERE id=%s FOR UPDATE", (doc_id,)).fetchone() is None:
                raise StoreError(404, "no such document")
            if replace:
                c.execute("DELETE FROM chunks WHERE document_id=%s", (doc_id,))
            with c.cursor() as cur:
                cur.executemany(
                    "INSERT INTO chunks(document_id, ord, page, text, embedding) VALUES (%s, %s, %s, %s, %s)",
                    [(doc_id, ch["ord"], ch.get("page"), ch["text"], np.asarray(v, dtype=np.float32)) for ch, v in zip(chunks, vectors)],
                )
            n = c.execute(
                "UPDATE documents SET chunk_count=(SELECT count(*) FROM chunks WHERE document_id=%s), updated_at=%s WHERE id=%s RETURNING chunk_count",
                (doc_id, now_ms(), doc_id),
            ).fetchone()[0]
        return n

    def delete_document(self, sid: str, path: str | None) -> tuple[int, list[str]]:
        """Delete documents; returns (count, file hashes no document uses any more)."""
        with self.pool.connection() as c, c.transaction():
            if path is None:
                rows = c.execute("DELETE FROM documents WHERE service_id=%s RETURNING file_hash", (sid,)).fetchall()
            else:
                rows = c.execute("DELETE FROM documents WHERE service_id=%s AND path=%s RETURNING file_hash", (sid, path)).fetchall()
            hashes = {r[0] for r in rows if r[0]}
            unused = [h for h in hashes if c.execute("SELECT 1 FROM documents WHERE file_hash=%s LIMIT 1", (h,)).fetchone() is None]
        return len(rows), unused

    def set_file(self, doc_id: int, file_hash: str, name: str, size: int) -> str | None:
        """Record the document's file; returns the previous hash when no document uses it any more."""
        with self.pool.connection() as c, c.transaction():
            r = c.execute("SELECT file_hash FROM documents WHERE id=%s FOR UPDATE", (doc_id,)).fetchone()
            if r is None:
                raise StoreError(404, "no such document")
            c.execute("UPDATE documents SET file_hash=%s, file_name=%s, file_size=%s WHERE id=%s", (file_hash, name, size, doc_id))
            old = r[0]
            if old and old != file_hash and c.execute("SELECT 1 FROM documents WHERE file_hash=%s LIMIT 1", (old,)).fetchone() is None:
                return old
        return None

    def file_of(self, sid: str, path: str) -> tuple[str, str]:
        with self.pool.connection() as c:
            r = c.execute("SELECT file_hash, file_name FROM documents WHERE service_id=%s AND path=%s", (sid, path)).fetchone()
        if r is None or not r[0]:
            raise StoreError(404, "the file of this document is not on the server")
        return r[0], r[1] or "document"

    def files(self, sid: str) -> list[dict]:
        with self.pool.connection() as c:
            rows = c.execute("SELECT path, file_hash, file_name, file_size FROM documents WHERE service_id=%s AND file_hash IS NOT NULL ORDER BY lower(path)", (sid,)).fetchall()
        return [{"path": r[0], "fileHash": r[1], "fileName": r[2], "fileSize": r[3]} for r in rows]

    def add_images(self, doc_id: int, images: list[dict], replace: bool) -> int:
        with self.pool.connection() as c, c.transaction():
            if c.execute("SELECT 1 FROM documents WHERE id=%s", (doc_id,)).fetchone() is None:
                raise StoreError(404, "no such document")
            if replace:
                c.execute("DELETE FROM images WHERE document_id=%s", (doc_id,))
            with c.cursor() as cur:
                cur.executemany(
                    "INSERT INTO images(document_id, page, chunk_ord, w, h, data) VALUES (%s, %s, %s, %s, %s, %s)",
                    [(doc_id, i.get("page"), i.get("chunkOrd"), i["w"], i["h"], i["data"]) for i in images],
                )
            return c.execute("SELECT count(*) FROM images WHERE document_id=%s", (doc_id,)).fetchone()[0]

    def images_for(self, sid: str, path: str, page: int | None, ord_: int) -> list[dict]:
        """The same choice as the app's local index: the page's pictures, else the chunk's own."""
        with self.pool.connection() as c:
            rows = c.execute(
                """SELECT i.id, i.w, i.h FROM images i JOIN documents d ON d.id = i.document_id
                   WHERE d.service_id=%s AND d.path=%s AND ((%s::int IS NOT NULL AND i.page=%s::int)
                     OR (%s::int IS NULL AND i.page IS NULL AND (i.chunk_ord=%s OR i.chunk_ord IS NULL)))
                   ORDER BY i.id LIMIT 12""",
                (sid, path, page, page, page, ord_),
            ).fetchall()
        return [{"id": r[0], "w": r[1], "h": r[2]} for r in rows]

    def image(self, image_id: int) -> tuple[str, bytes]:
        with self.pool.connection() as c:
            r = c.execute("SELECT d.service_id, i.data FROM images i JOIN documents d ON d.id = i.document_id WHERE i.id=%s", (image_id,)).fetchone()
        if r is None:
            raise StoreError(404, "no such picture")
        return r[0], bytes(r[1])

    # ---------------------------------------------------------------- search

    def search(self, service_ids: list[str], query: str, qv: list[float], k: int, threshold: float, hybrid: bool) -> list[dict]:
        """Vector search, fused with trigram keyword search (reciprocal rank fusion) when `hybrid`.
        Vector hits obey `threshold`; keyword hits do not (exact codes can have a low cosine)."""
        import numpy as np

        v = np.asarray(qv, dtype=np.float32)
        with self.pool.connection() as c:
            c.execute("SET hnsw.ef_search = 100")
            vec = c.execute(
                """SELECT ch.id, 1 - (ch.embedding <=> %s) AS cos
                   FROM chunks ch JOIN documents d ON d.id = ch.document_id
                   WHERE d.service_id = ANY(%s)
                   ORDER BY ch.embedding <=> %s LIMIT %s""",
                (v, service_ids, v, k),
            ).fetchall()
            vec = [(i, float(cos)) for i, cos in vec if cos >= threshold]
            kw: list[int] = []
            grams = query_trigrams(query) if hybrid else []
            if grams:
                pats = ["%" + g.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%" for g in grams]
                rows = c.execute(
                    """SELECT ch.id, (SELECT count(*) FROM unnest(%s::text[]) p WHERE lower(ch.text) LIKE p) AS hit
                       FROM chunks ch JOIN documents d ON d.id = ch.document_id
                       WHERE d.service_id = ANY(%s) AND lower(ch.text) LIKE ANY(%s::text[])
                       ORDER BY hit DESC LIMIT %s""",
                    (pats, service_ids, pats, k * 3),
                ).fetchall()
                kw = [i for i, hit in rows if hit / len(grams) >= MIN_KEYWORD_COVERAGE][:k]

            fused: dict[int, list] = {}  # id -> [fused, cos | None, keyword]
            for rank, (i, cos) in enumerate(vec):
                fused.setdefault(i, [0.0, cos, False])[0] += rrf(rank)
            for rank, i in enumerate(kw):
                e = fused.setdefault(i, [0.0, None, False])
                e[0] += rrf(rank)
                e[2] = True
            if not fused:
                return []
            rows = c.execute(
                """SELECT ch.id, d.service_id, d.path, ch.page, ch.ord, ch.text, 1 - (ch.embedding <=> %s)
                   FROM chunks ch JOIN documents d ON d.id = ch.document_id WHERE ch.id = ANY(%s)""",
                (v, list(fused)),
            ).fetchall()
        hits = []
        for i, sid, path, page, ord_, text, cos in rows:
            f, vcos, keyword = fused[i]
            hits.append({"chunkId": i, "serviceId": sid, "docPath": path, "page": page, "ord": ord_, "text": text,
                         "score": float(vcos if vcos is not None else cos), "fused": f, "keyword": keyword})
        hits.sort(key=lambda h: (-h["fused"], -h["score"]))
        return hits[:k]

    # ----------------------------------------------------------------- chats

    @staticmethod
    def _chat(r) -> dict:
        return {"id": r[0], "serviceId": r[1], "title": r[2], "createdAt": r[3], "updatedAt": r[4]}

    def chats(self, service_id: str | None, q: str | None, limit: int) -> list[dict]:
        sql = "SELECT id, service_id, title, created_at, updated_at FROM chats WHERE true"
        args: list[Any] = []
        if service_id:
            sql += " AND service_id=%s"
            args.append(service_id)
        if q:
            sql += " AND lower(title) LIKE %s"
            args.append("%" + q.lower().replace("%", "\\%").replace("_", "\\_") + "%")
        sql += " ORDER BY updated_at DESC LIMIT %s"
        args.append(limit)
        with self.pool.connection() as c:
            return [self._chat(r) for r in c.execute(sql, args).fetchall()]

    def get_chat(self, cid: str) -> dict:
        with self.pool.connection() as c:
            r = c.execute("SELECT id, service_id, title, created_at, updated_at FROM chats WHERE id=%s", (cid,)).fetchone()
        if r is None:
            raise StoreError(404, "no such chat")
        return self._chat(r)

    def put_chat(self, cid: str, service_id: str, title: str, created_at: int | None) -> dict:
        t = now_ms()
        with self.pool.connection() as c:
            r = c.execute(
                """INSERT INTO chats VALUES (%s, %s, %s, %s, %s)
                   ON CONFLICT (id) DO UPDATE SET title=EXCLUDED.title, updated_at=EXCLUDED.updated_at
                   RETURNING id, service_id, title, created_at, updated_at""",
                (cid, service_id, title, created_at or t, t),
            ).fetchone()
        return self._chat(r)

    def delete_chats(self, ids: Iterable[str] | None, service_id: str | None) -> int:
        with self.pool.connection() as c:
            if ids is not None:
                return c.execute("DELETE FROM chats WHERE id = ANY(%s)", (list(ids),)).rowcount
            if service_id:
                return c.execute("DELETE FROM chats WHERE service_id=%s", (service_id,)).rowcount
            return c.execute("DELETE FROM chats").rowcount

    _MSG = "id, chat_id, role, content, model, reasoning_effort, sources, elapsed_ms, error, created_at, search_query"

    @staticmethod
    def _msg(r) -> dict:
        m = {"id": r[0], "chatId": r[1], "role": r[2], "content": r[3], "model": r[4], "reasoningEffort": r[5],
             "sources": r[6], "elapsedMs": r[7], "error": r[8], "createdAt": r[9]}
        if r[10] is not None:
            m["searchQuery"] = r[10]
        return m

    def messages(self, cid: str) -> list[dict]:
        with self.pool.connection() as c:
            rows = c.execute(f"SELECT {self._MSG} FROM messages WHERE chat_id=%s ORDER BY id", (cid,)).fetchall()
        return [self._msg(r) for r in rows]

    def add_message(self, cid: str, m: dict) -> dict:
        t = m.get("createdAt") or now_ms()
        with self.pool.connection() as c, c.transaction():
            if c.execute("SELECT 1 FROM chats WHERE id=%s", (cid,)).fetchone() is None:
                raise StoreError(404, "no such chat")
            r = c.execute(
                f"""INSERT INTO messages(chat_id, role, content, model, reasoning_effort, sources, elapsed_ms, error, search_query, created_at)
                    VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s) RETURNING {self._MSG}""",
                (cid, m["role"], m.get("content", ""), m.get("model"), m.get("reasoningEffort"), Jsonb(m.get("sources") or []),
                 m.get("elapsedMs"), m.get("error"), m.get("searchQuery"), t),
            ).fetchone()
            c.execute("UPDATE chats SET updated_at=%s WHERE id=%s", (max(t, now_ms()), cid))
        return self._msg(r)

