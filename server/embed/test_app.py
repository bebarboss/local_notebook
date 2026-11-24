"""Run: EMBED_NO_AUTOLOAD=1 python -m pytest server/embed -q  (no model download needed)."""

import os

os.environ["EMBED_NO_AUTOLOAD"] = "1"

import math
import threading

import httpx
import pytest
from fastapi.testclient import TestClient

import app as server

TOKEN = "test-token-0123456789"


class FakeEncoder:
    dim = 2
    device = "cpu"

    def __init__(self):
        self.seen = []

    def encode(self, texts, query=False):
        self.seen.append(texts)
        out = []
        for t in texts:
            v = [float(len(t)), 1.0]
            n = math.hypot(*v)
            out.append([x / n for x in v])
        return out


class FakeReranker:
    name = "BAAI/bge-reranker-v2-m3"

    def score(self, query, docs):
        # the more words shared with the query, the higher
        q = set(query.split())
        return [len(q & set(d.split())) / max(len(q), 1) for d in docs]


def client(enc=None, reranker=None):
    enc = enc or FakeEncoder()
    return TestClient(server.create_app(enc, TOKEN, reranker=reranker or FakeReranker())), enc


H = {"Authorization": f"Bearer {TOKEN}"}


def test_health_needs_no_token():
    c, _ = client()
    r = c.get("/health")
    assert r.status_code == 200 and r.json()["dim"] == 2


def test_wrong_or_missing_token_is_401():
    c, _ = client()
    assert c.post("/v1/embeddings", json={"input": ["x"]}).status_code == 401
    assert c.post("/v1/embeddings", json={"input": ["x"]}, headers={"Authorization": "Bearer nope"}).status_code == 401
    assert c.get("/v1/models").status_code == 401


def test_vectors_in_input_order_and_normalised():
    c, enc = client()
    r = c.post("/v1/embeddings", json={"model": server.MODEL_ID, "input": ["ก", "ลาพักร้อน"], "encoding_format": "float"}, headers=H)
    assert r.status_code == 200
    data = r.json()["data"]
    assert [d["index"] for d in data] == [0, 1]
    for d in data:
        assert abs(math.hypot(*d["embedding"]) - 1) < 1e-6
    assert enc.seen == [["ก", "ลาพักร้อน"]]


def test_single_string_input():
    c, _ = client()
    r = c.post("/v1/embeddings", json={"input": "คำถาม", "input_type": "query"}, headers=H)
    assert r.status_code == 200 and len(r.json()["data"]) == 1


def test_limits():
    c, _ = client()
    assert c.post("/v1/embeddings", json={"input": ["x"] * 65}, headers=H).status_code == 413
    assert c.post("/v1/embeddings", json={"input": []}, headers=H).status_code == 400
    r = c.post("/v1/embeddings", json={"model": "text-embedding-3-small", "input": ["x"]}, headers=H)
    assert r.status_code == 404 and "error" in r.json()


def test_prefixes(monkeypatch):
    monkeypatch.setattr(server, "QUERY_PREFIX", "query: ")
    monkeypatch.setattr(server, "PASSAGE_PREFIX", "passage: ")
    c, enc = client()
    c.post("/v1/embeddings", json={"input": ["a"], "input_type": "query"}, headers=H)
    c.post("/v1/embeddings", json={"input": ["b"], "input_type": "passage"}, headers=H)
    assert enc.seen == [["query: a"], ["passage: b"]]


def test_rerank():
    c, _ = client()
    assert c.get("/health").json()["rerank"] == "bge-reranker-v2-m3"
    assert c.post("/v1/rerank", json={"query": "ลา พักร้อน", "documents": ["x"]}).status_code == 401
    r = c.post("/v1/rerank", json={"query": "ลา พักร้อน", "documents": ["เงินเดือน", "ลา พักร้อน 10 วัน", "ลา กิจ"]}, headers=H)
    assert r.status_code == 200
    scores = [x["score"] for x in r.json()["results"]]
    assert [x["index"] for x in r.json()["results"]] == [0, 1, 2]
    assert scores[1] > scores[2] > scores[0]
    assert c.post("/v1/rerank", json={"query": "q", "documents": ["d"] * 65}, headers=H).status_code == 422


# --- Encoder: bge-m3 through Ollama's POST /api/embed (no real Ollama needed) ---


class CharCut:
    """Stand-in tokenizer: one character = one token."""

    def cut(self, text, max_tokens):
        return text[: max_tokens - 2]


def ollama(handler):
    calls = []

    def wrapped(request):
        calls.append(request)
        return handler(request)

    client = httpx.Client(base_url="http://ollama.test", transport=httpx.MockTransport(wrapped))
    return server.Encoder("http://ollama.test", client=client, truncator=CharCut()), calls


def vectors(request, dim=1024):
    import json

    body = json.loads(request.content)
    return httpx.Response(200, json={"model": body["model"], "embeddings": [[float(len(t))] + [1.0] * (dim - 1) for t in body["input"]]})


def test_encoder_batches_in_order_and_normalises(monkeypatch):
    import json

    monkeypatch.setattr(server, "BATCH", 3)
    enc, calls = ollama(vectors)
    texts = [f"ข้อความ {i}" * (i + 1) for i in range(7)]
    out = enc.encode(texts)
    assert [len(json.loads(c.content)["input"]) for c in calls] == [3, 3, 1]
    assert json.loads(calls[0].content)["model"] == "bge-m3" and calls[0].url.path == "/api/embed"
    assert len(out) == 7 and all(abs(math.sqrt(sum(x * x for x in v)) - 1) < 1e-9 for v in out)
    # first value grows with the text: order is kept across batches
    assert [v[0] for v in out] == sorted(v[0] for v in out)


def test_questions_are_cut_and_cached(monkeypatch):
    import json

    monkeypatch.setattr(server, "QUERY_MAX_TOKENS", 10)
    monkeypatch.setattr(server, "MAX_TOKENS", 100)
    enc, calls = ollama(vectors)
    enc.encode(["x" * 50], query=True)
    assert json.loads(calls[0].content)["input"] == ["x" * 8]
    enc.encode(["y" * 50])  # passages keep EMBED_MAX_TOKENS
    assert json.loads(calls[1].content)["input"] == ["y" * 50]
    a = enc.encode(["ลากี่วัน", "x" * 50], query=True)  # the long one is cached already
    assert json.loads(calls[2].content)["input"] == ["ลากี่วัน"]
    b = enc.encode(["ลากี่วัน"], query=True)
    assert len(calls) == 3 and b[0] == a[0]


def test_cache_is_lru(monkeypatch):
    monkeypatch.setattr(server, "CACHE_SIZE", 2)
    enc, calls = ollama(vectors)
    for q in ["a", "b", "a", "c", "a", "b"]:
        enc.encode([q], query=True)
    # a b (hit a) c → evicts b; a hit; b again
    assert len(calls) == 4 and list(enc.cache) == ["a", "b"]


def test_ollama_errors_are_clear():
    def down(request):
        raise httpx.ConnectError("refused")

    enc, _ = ollama(down)
    with pytest.raises(server.EmbedError) as e:
        enc.encode(["x"])
    assert e.value.status == 503 and "not reachable" in e.value.message

    enc, _ = ollama(lambda r: httpx.Response(404, json={"error": 'model "bge-m3" not found, try pulling it first'}))
    with pytest.raises(server.EmbedError) as e:
        enc.encode(["x"])
    assert e.value.status == 503 and "ollama pull bge-m3" in e.value.message

    enc, _ = ollama(lambda r: httpx.Response(500, json={"error": "out of memory"}))
    with pytest.raises(server.EmbedError) as e:
        enc.encode(["x"])
    assert e.value.status == 502 and "out of memory" in e.value.message

    enc, _ = ollama(lambda r: vectors(r, dim=768))
    with pytest.raises(server.EmbedError) as e:
        enc.encode(["x"])
    assert e.value.status == 502 and "1024" in e.value.message

    def slow(request):
        raise httpx.ReadTimeout("slow")

    enc, _ = ollama(slow)
    with pytest.raises(server.EmbedError) as e:
        enc.encode(["x"])
    assert e.value.status == 504


def test_at_most_n_requests_at_once(monkeypatch):
    monkeypatch.setattr(server, "CONCURRENCY", 2)
    running, peak, lock = [0], [0], threading.Lock()
    gate = threading.Event()

    def handler(request):
        with lock:
            running[0] += 1
            peak[0] = max(peak[0], running[0])
        gate.wait(2)
        with lock:
            running[0] -= 1
        return vectors(request)

    enc, _ = ollama(handler)
    threads = [threading.Thread(target=enc.encode, args=([f"t{i}"],)) for i in range(6)]
    for t in threads:
        t.start()
    threading.Timer(0.3, gate.set).start()
    for t in threads:
        t.join(5)
    assert peak[0] == 2


def test_busy_when_queue_wait_runs_out(monkeypatch):
    monkeypatch.setattr(server, "CONCURRENCY", 1)
    monkeypatch.setattr(server, "QUEUE_TIMEOUT", 0.1)
    enc, _ = ollama(vectors)
    enc.slots.acquire()  # someone else holds the only slot
    with pytest.raises(server.EmbedError) as e:
        enc.encode(["x"])
    assert e.value.status == 503 and "busy" in e.value.message


def test_api_returns_ollama_errors_as_json():
    enc, _ = ollama(lambda r: httpx.Response(500, json={"error": "boom"}))
    enc.health = lambda: "Ollama is not reachable"
    c = TestClient(server.create_app(enc, TOKEN, reranker=FakeReranker()))
    r = c.post("/v1/embeddings", json={"input": ["x"]}, headers=H)
    assert r.status_code == 502 and "boom" in r.json()["error"]["message"]
    h = c.get("/health")
    assert h.status_code == 200 and h.json()["status"] == "degraded" and h.json()["device"] == "ollama"


def test_real_tokenizer_cuts_on_a_token_boundary():
    t = server.Truncator(server.MODEL_NAME)
    if t.tok is None:
        pytest.skip("bge-m3 tokenizer not downloaded")
    text = "การลาพักร้อนของพนักงานประจำ " * 200
    cut = t.cut(text, 512)
    assert text.startswith(cut) and len(cut) < len(text)
    assert len(t.tok(cut, add_special_tokens=True)["input_ids"]) <= 512
    assert t.cut("สั้นๆ", 512) == "สั้นๆ"
