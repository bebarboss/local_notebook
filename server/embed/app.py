"""DocAsk embedding server: OpenAI-style /v1/embeddings for one sentence-transformers model.

Runs at home (Docker on a Mac mini) and is reached over Tailscale. Every route except
/health needs `Authorization: Bearer <EMBED_TOKEN>`. Texts are never logged — only counts
and timings.
"""

from __future__ import annotations

import logging
import os
import secrets
import threading
import time
from typing import Literal, Union

from fastapi import Depends, FastAPI, Header, HTTPException, Request
from fastapi.responses import JSONResponse
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
MAX_BODY = 4 * 1024 * 1024

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


def create_app(encoder: Encoder | None = None, token: str | None = None) -> FastAPI:
    """`encoder`/`token` are injected by tests; the container builds them from env."""
    app = FastAPI(title="DocAsk embedding server", docs_url=None, redoc_url=None, openapi_url=None)
    token = token or _token()
    enc = encoder or Encoder()

    def auth(authorization: str | None = Header(default=None)) -> None:
        given = (authorization or "").removeprefix("Bearer ").strip()
        if not secrets.compare_digest(given.encode(), token.encode()):
            raise HTTPException(401, "invalid token")

    @app.middleware("http")
    async def limit_body(request: Request, call_next):
        size = request.headers.get("content-length")
        if size and size.isdigit() and int(size) > MAX_BODY:
            return JSONResponse({"error": {"message": "request too large"}}, status_code=413)
        return await call_next(request)

    @app.exception_handler(HTTPException)
    async def openai_error(_: Request, e: HTTPException):
        return JSONResponse({"error": {"message": e.detail, "code": e.status_code}}, status_code=e.status_code)

    @app.get("/health")
    def health():
        return {"status": "ok", "model": MODEL_ID, "dim": enc.dim, "device": enc.device}

    @app.get("/v1/models", dependencies=[Depends(auth)])
    def models():
        return {"object": "list", "data": [{"id": MODEL_ID, "object": "model", "owned_by": "docask"}]}

    @app.post("/v1/embeddings", dependencies=[Depends(auth)])
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

    return app


if os.environ.get("EMBED_NO_AUTOLOAD") != "1":
    app = create_app()
