"""DocAsk embedding server: OpenAI-style /v1/embeddings for one sentence-transformers model,
plus (when DATABASE_URL is set) the team's shared document index and chat history in
PostgreSQL + pgvector (see db.py).

Runs at home (Mac mini) behind a tunnel. Every route except /health needs
`Authorization: Bearer <EMBED_TOKEN>`. Texts are never logged — only counts and timings.
"""

from __future__ import annotations

import base64
import hashlib
import logging
import os
import secrets
import threading
import time
from typing import Any, Literal, Optional, Union

from fastapi import Depends, FastAPI, Header, HTTPException, Query, Request
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel, Field

MODEL_NAME = os.environ.get("EMBED_MODEL", "BAAI/bge-m3")
# what clients send as "model" (the default is the last part of the repo name: "bge-m3")
MODEL_ID = os.environ.get("EMBED_MODEL_ID", MODEL_NAME.rsplit("/", 1)[-1])
MODEL_REVISION = os.environ.get("EMBED_MODEL_REVISION") or None
# e5 models want "query: " / "passage: " — bge-m3 needs nothing
QUERY_PREFIX = os.environ.get("EMBED_QUERY_PREFIX", "")
PASSAGE_PREFIX = os.environ.get("EMBED_PASSAGE_PREFIX", "")
MAX_TOKENS = int(os.environ.get("EMBED_MAX_TOKENS", "2048"))
BATCH = int(os.environ.get("EMBED_BATCH", "16"))
MAX_INPUTS = 64
MAX_CHARS = 32_000
MAX_BODY = 16 * 1024 * 1024
MAX_CHUNKS = 64
# original files of shared documents: FILES_DIR/<sha256[:2]>/<sha256>
FILES_DIR = os.environ.get("FILES_DIR") or os.path.join(os.path.expanduser("~"), ".docask-embed", "files")
MAX_FILE = int(os.environ.get("EMBED_MAX_FILE_MB", "50")) * 1024 * 1024
MAX_IMAGES = 32
MAX_QUERIES = 4
DATABASE_URL = os.environ.get("DATABASE_URL", "").strip()

log = logging.getLogger("docask-embed")
logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")


def _token() -> str:
    t = os.environ.get("EMBED_TOKEN", "").strip()
    path = os.environ.get("EMBED_TOKEN_FILE", "/run/secrets/embed_token")
    if not t and os.path.isfile(path):
        with open(path, encoding="utf-8") as f:
            t = f.read().strip()
    if len(t) < 16 or any(c.isspace() for c in t):
        raise SystemExit("EMBED_TOKEN is missing or shorter than 16 characters (see README)")
    return t


class Encoder:
    """The model, loaded once; one encode at a time (the CPU is the limit anyway)."""

    def __init__(self) -> None:
        from sentence_transformers import SentenceTransformer

        t = time.time()
        self.model = SentenceTransformer(MODEL_NAME, revision=MODEL_REVISION, device=os.environ.get("EMBED_DEVICE") or None)
        self.model.max_seq_length = min(MAX_TOKENS, self.model.max_seq_length or MAX_TOKENS)
        self.dim = self.model.get_sentence_embedding_dimension()
        self.device = str(self.model.device)
        self.lock = threading.Lock()
        log.info("loaded %s (%d dims) on %s in %.0fs", MODEL_NAME, self.dim, self.device, time.time() - t)

    def encode(self, texts: list[str]) -> list[list[float]]:
        with self.lock:
            v = self.model.encode(texts, batch_size=BATCH, normalize_embeddings=True, convert_to_numpy=True)
        return v.tolist()


class EmbeddingRequest(BaseModel):
    input: Union[str, list[str]]
    model: str | None = None
    encoding_format: Literal["float"] | None = "float"
    # not in the OpenAI API: lets the server add the e5-style prefixes
    input_type: Literal["query", "passage"] | None = None
    user: str | None = None


class ServiceIn(BaseModel):
    data: dict[str, Any]


class DocumentIn(BaseModel):
    path: str = Field(min_length=1, max_length=4000)
    hash: str = ""
    mtime: int = 0
    bytes: int = 0
    status: str = Field("queued", max_length=40)
    message: Optional[str] = None
    indexedBy: str = Field("", max_length=200)


class StatusIn(BaseModel):
    status: str = Field(max_length=40)
    message: Optional[str] = None


class ChunkIn(BaseModel):
    ord: int
    page: Optional[int] = None
    text: str = Field(max_length=MAX_CHARS)
    # vectors made earlier with the same model (moving an index up); else the server embeds
    embedding: Optional[list[float]] = None


class ChunksIn(BaseModel):
    replace: bool = False
    chunks: list[ChunkIn]


class ImageIn(BaseModel):
    page: Optional[int] = None
    chunkOrd: Optional[int] = None
    w: int = Field(ge=1, le=20000)
    h: int = Field(ge=1, le=20000)
    data: str = Field(max_length=4_000_000)


class ImagesIn(BaseModel):
    replace: bool = False
    images: list[ImageIn]


class SearchIn(BaseModel):
    serviceIds: list[str] = Field(min_length=1)
    queries: list[str] = Field(min_length=1, max_length=MAX_QUERIES)
    k: int = Field(5, ge=1, le=60)
    threshold: float = Field(0.0, ge=0.0, le=1.0)
    hybrid: bool = True


class ChatIn(BaseModel):
    serviceId: str
    title: str = Field(max_length=500)
    createdAt: Optional[int] = None


class MessageIn(BaseModel):
    role: Literal["user", "assistant"]
    content: str = ""
    model: Optional[str] = None
    reasoningEffort: Optional[str] = None
    sources: list[dict[str, Any]] = []
    elapsedMs: Optional[int] = None
    error: Optional[str] = None
    searchQuery: Optional[str] = None
    createdAt: Optional[int] = None


def create_app(encoder: Encoder | None = None, token: str | None = None, store: Any = None) -> FastAPI:
    """`encoder`/`token`/`store` are injected by tests; the container builds them from env."""
    app = FastAPI(title="DocAsk embedding server", docs_url=None, redoc_url=None, openapi_url=None)
    token = token or _token()
    enc = encoder or Encoder()
    if store is None and DATABASE_URL:
        from db import Store

        store = Store(DATABASE_URL, enc.dim, MODEL_ID)

    def bearer(authorization: str | None) -> str:
        return (authorization or "").removeprefix("Bearer ").strip()

    def is_admin(given: str) -> bool:
        return secrets.compare_digest(given.encode(), token.encode())

    def auth(authorization: str | None = Header(default=None)) -> None:
        """The server token (the owner)."""
        if not is_admin(bearer(authorization)):
            raise HTTPException(401, "invalid token")

    def caller(authorization: str | None = Header(default=None)) -> Optional[str]:
        """None for the owner, else the one service whose share key was sent."""
        given = bearer(authorization)
        if is_admin(given):
            return None
        sid = store.service_of_key(key_hash(given)) if store is not None and len(given) >= 16 else None
        if sid is None:
            raise HTTPException(401, "invalid token")
        return sid

    @app.middleware("http")
    async def limit_body(request: Request, call_next):
        size = request.headers.get("content-length")
        limit = MAX_FILE if request.url.path.endswith("/file") and request.method == "PUT" else MAX_BODY
        if size and size.isdigit() and int(size) > limit:
            return JSONResponse({"error": {"message": "request too large"}}, status_code=413)
        return await call_next(request)

    @app.exception_handler(HTTPException)
    async def openai_error(_: Request, e: HTTPException):
        return JSONResponse({"error": {"message": e.detail, "code": e.status_code}}, status_code=e.status_code)

    if store is not None:
        from db import StoreError

        @app.exception_handler(StoreError)
        async def store_error(_: Request, e: StoreError):
            return JSONResponse({"error": {"message": e.message, "code": e.status}}, status_code=e.status)

    @app.get("/health")
    def health():
        return {"status": "ok", "model": MODEL_ID, "dim": enc.dim, "device": enc.device, "database": store is not None}

    # a member with any service's share key may use the model (indexing and questions need it)
    @app.get("/v1/models", dependencies=[Depends(caller)])
    def models():
        return {"object": "list", "data": [{"id": MODEL_ID, "object": "model", "owned_by": "docask"}]}

    @app.post("/v1/embeddings", dependencies=[Depends(caller)])
    def embeddings(req: EmbeddingRequest):
        texts = [req.input] if isinstance(req.input, str) else req.input
        if not texts:
            raise HTTPException(400, "input is empty")
        if len(texts) > MAX_INPUTS:
            raise HTTPException(413, f"at most {MAX_INPUTS} inputs per request")
        if req.model and req.model not in (MODEL_ID, MODEL_NAME):
            raise HTTPException(404, f"model {req.model!r} is not served here (this server has {MODEL_ID!r})")
        prefix = QUERY_PREFIX if req.input_type == "query" else PASSAGE_PREFIX
        t = time.time()
        vectors = enc.encode([prefix + x[:MAX_CHARS] for x in texts])
        # counts only — never the text
        log.info("embedded %d %s in %.2fs", len(texts), req.input_type or "inputs", time.time() - t)
        return {
            "object": "list",
            "model": MODEL_ID,
            "data": [{"object": "embedding", "index": i, "embedding": v} for i, v in enumerate(vectors)],
            "usage": {"prompt_tokens": 0, "total_tokens": 0},
        }

    if store is not None:
        _store_routes(app, enc, store, auth, caller)
    return app


def _blob(h: str) -> str:
    if len(h) != 64 or any(c not in "0123456789abcdef" for c in h):
        raise HTTPException(400, "bad file hash")
    return os.path.join(FILES_DIR, h[:2], h)


def _remove_blob(h: str) -> None:
    try:
        os.remove(_blob(h))
    except (FileNotFoundError, HTTPException):
        pass


def key_hash(key: str) -> str:
    return hashlib.sha256(key.encode()).hexdigest()


def _store_routes(app: FastAPI, enc: Encoder, store: Any, auth, caller) -> None:
    """The shared index and chats. The owner (server token) may do everything; a share key
    opens only its own service: its settings, documents and search."""
    dep = [Depends(auth)]

    def allowed(who: Optional[str], sid: str) -> None:
        if who is not None and who != sid:
            raise HTTPException(403, "this share key is for another service")

    @app.get("/v1/store", dependencies=dep)
    def store_info():
        return store.info()

    @app.get("/v1/services", dependencies=dep)
    def services():
        return store.services()

    @app.put("/v1/services/{sid}", dependencies=dep)
    def put_service(sid: str, body: ServiceIn):
        return store.put_service(sid, body.data)

    @app.get("/v1/services/{sid}")
    def get_service(sid: str, who: Optional[str] = Depends(caller)):
        allowed(who, sid)
        return store.get_service(sid)

    @app.post("/v1/services/{sid}/key", dependencies=dep)
    def new_key(sid: str):
        """A new share key for the service; the old one stops working. Shown once."""
        key = secrets.token_urlsafe(32)
        store.set_service_key(sid, key_hash(key))
        log.info("new share key for a service")
        return {"key": key}

    @app.delete("/v1/services/{sid}", dependencies=dep)
    def delete_service(sid: str, data: bool = False):
        store.delete_service(sid, data)
        return {"ok": True}

    @app.get("/v1/services/{sid}/documents")
    def documents(sid: str, who: Optional[str] = Depends(caller)):
        allowed(who, sid)
        return store.documents(sid)

    @app.put("/v1/services/{sid}/documents")
    def put_document(sid: str, body: DocumentIn, who: Optional[str] = Depends(caller)):
        allowed(who, sid)
        return store.put_document(sid, body.model_dump())

    @app.delete("/v1/services/{sid}/documents")
    def delete_documents(sid: str, path: Optional[str] = None, who: Optional[str] = Depends(caller)):
        allowed(who, sid)
        if path is None and who is not None:
            raise HTTPException(403, "only the owner may clear a whole service")
        n, unused = store.delete_document(sid, path)
        for h in unused:
            _remove_blob(h)
        return {"deleted": n}

    @app.put("/v1/documents/{doc_id}/file")
    async def put_file(doc_id: int, request: Request, name: str = Query(min_length=1, max_length=255), who: Optional[str] = Depends(caller)):
        """The original file of a document (raw body). Stored once per content (SHA-256)."""
        allowed(who, store.service_of_document(doc_id))
        body = await request.body()
        if len(body) > MAX_FILE:
            raise HTTPException(413, "file too large")
        h = hashlib.sha256(body).hexdigest()
        path = _blob(h)
        if not os.path.exists(path):
            os.makedirs(os.path.dirname(path), exist_ok=True)
            tmp = f"{path}.{secrets.token_hex(4)}.tmp"
            with open(tmp, "wb") as f:
                f.write(body)
            os.replace(tmp, path)
        safe = os.path.basename(name.replace("\\", "/")) or "document"
        old = store.set_file(doc_id, h, safe, len(body))
        if old:
            _remove_blob(old)
        log.info("stored a file (%d KB)", len(body) // 1024)
        return {"fileHash": h, "size": len(body)}

    @app.get("/v1/services/{sid}/file")
    def get_file(sid: str, path: str, who: Optional[str] = Depends(caller)):
        allowed(who, sid)
        h, name = store.file_of(sid, path)
        blob = _blob(h)
        if not os.path.exists(blob):
            raise HTTPException(404, "the file of this document is missing on the server")
        return FileResponse(blob, filename=name, media_type="application/octet-stream")

    @app.get("/v1/services/{sid}/files")
    def list_files(sid: str, who: Optional[str] = Depends(caller)):
        allowed(who, sid)
        return store.files(sid)

    @app.post("/v1/documents/{doc_id}/images")
    def add_images(doc_id: int, body: ImagesIn, who: Optional[str] = Depends(caller)):
        allowed(who, store.service_of_document(doc_id))
        if len(body.images) > MAX_IMAGES:
            raise HTTPException(413, f"at most {MAX_IMAGES} pictures per request")
        imgs = []
        for i in body.images:
            try:
                data = base64.b64decode(i.data, validate=True)
            except ValueError:
                raise HTTPException(400, "picture data must be base64") from None
            imgs.append({"page": i.page, "chunkOrd": i.chunkOrd, "w": i.w, "h": i.h, "data": data})
        return {"images": store.add_images(doc_id, imgs, body.replace)}

    @app.get("/v1/services/{sid}/images")
    def images(sid: str, path: str, page: Optional[int] = None, ord: int = 0, who: Optional[str] = Depends(caller)):
        allowed(who, sid)
        return store.images_for(sid, path, page, ord)

    @app.get("/v1/images/{image_id}")
    def image(image_id: int, who: Optional[str] = Depends(caller)):
        sid, data = store.image(image_id)
        allowed(who, sid)
        return {"data": base64.b64encode(data).decode()}

    @app.patch("/v1/documents/{doc_id}")
    def set_status(doc_id: int, body: StatusIn, who: Optional[str] = Depends(caller)):
        allowed(who, store.service_of_document(doc_id))
        store.set_status(doc_id, body.status, body.message)
        return {"ok": True}

    @app.post("/v1/documents/{doc_id}/chunks")
    def add_chunks(doc_id: int, body: ChunksIn, who: Optional[str] = Depends(caller)):
        allowed(who, store.service_of_document(doc_id))
        if len(body.chunks) > MAX_CHUNKS:
            raise HTTPException(413, f"at most {MAX_CHUNKS} chunks per request")
        given = [c.embedding for c in body.chunks]
        if any(v is not None and len(v) != enc.dim for v in given):
            raise HTTPException(400, f"embeddings must have {enc.dim} dimensions")
        todo = [i for i, v in enumerate(given) if v is None]
        t = time.time()
        if todo:
            made = enc.encode([PASSAGE_PREFIX + body.chunks[i].text for i in todo])
            for i, v in zip(todo, made):
                given[i] = v
        n = store.add_chunks(doc_id, [c.model_dump(exclude={"embedding"}) for c in body.chunks], given, body.replace)
        log.info("stored %d chunks (%d embedded) in %.2fs", len(body.chunks), len(todo), time.time() - t)
        return {"chunkCount": n}

    @app.post("/v1/search")
    def search(body: SearchIn, who: Optional[str] = Depends(caller)):
        for sid in body.serviceIds:
            allowed(who, sid)
        t = time.time()
        out: dict[int, dict] = {}
        for q in body.queries:
            qv = enc.encode([QUERY_PREFIX + q[:MAX_CHARS]])[0]
            for h in store.search(body.serviceIds, q, qv, body.k, body.threshold, body.hybrid):
                # the same passage found by several queries keeps its best score
                if h["chunkId"] not in out or h["fused"] > out[h["chunkId"]]["fused"]:
                    out[h["chunkId"]] = h
        hits = sorted(out.values(), key=lambda h: (-h["fused"], -h["score"]))[: body.k]
        log.info("search %d queries over %d services: %d hits in %.2fs", len(body.queries), len(body.serviceIds), len(hits), time.time() - t)
        return {"hits": hits}

    @app.get("/v1/chats", dependencies=dep)
    def chats(serviceId: Optional[str] = None, q: Optional[str] = None, limit: int = Query(200, ge=1, le=1000)):
        return store.chats(serviceId, q, limit)

    @app.get("/v1/chats/{cid}", dependencies=dep)
    def chat(cid: str):
        return store.get_chat(cid)

    @app.put("/v1/chats/{cid}", dependencies=dep)
    def put_chat(cid: str, body: ChatIn):
        return store.put_chat(cid, body.serviceId, body.title, body.createdAt)

    @app.delete("/v1/chats/{cid}", dependencies=dep)
    def delete_chat(cid: str):
        return {"deleted": store.delete_chats([cid], None)}

    @app.delete("/v1/chats", dependencies=dep)
    def delete_chats(serviceId: Optional[str] = None):
        return {"deleted": store.delete_chats(None, serviceId)}

    @app.get("/v1/chats/{cid}/messages", dependencies=dep)
    def messages(cid: str):
        return store.messages(cid)

    @app.post("/v1/chats/{cid}/messages", dependencies=dep)
    def add_message(cid: str, body: MessageIn):
        return store.add_message(cid, body.model_dump())


if os.environ.get("EMBED_NO_AUTOLOAD") != "1":
    app = create_app()
