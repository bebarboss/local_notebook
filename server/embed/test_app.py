"""Run: EMBED_NO_AUTOLOAD=1 python -m pytest server/embed -q  (no model download needed)."""

import os

os.environ["EMBED_NO_AUTOLOAD"] = "1"

import math

from fastapi.testclient import TestClient

import app as server

TOKEN = "test-token-0123456789"


class FakeEncoder:
    dim = 2
    device = "cpu"

    def __init__(self):
        self.seen = []

    def encode(self, texts):
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
