"""Shared index + chats against a real PostgreSQL with pgvector.

    docker run -d --name docask-test-db -e POSTGRES_PASSWORD=test -p 127.0.0.1:55432:5432 pgvector/pgvector:0.8.0-pg16
    TEST_DATABASE_URL=postgresql://postgres:test@127.0.0.1:55432/postgres EMBED_NO_AUTOLOAD=1 python -m pytest -q

Skipped when TEST_DATABASE_URL is not set. Every test starts from empty tables.
"""

import os

os.environ["EMBED_NO_AUTOLOAD"] = "1"

import math

import pytest
from fastapi.testclient import TestClient

import app as server

URL = os.environ.get("TEST_DATABASE_URL")
pytestmark = pytest.mark.skipif(not URL, reason="TEST_DATABASE_URL not set")
TOKEN = "test-token-0123456789"
H = {"Authorization": f"Bearer {TOKEN}"}
WORDS = ["ลา", "ot", "เงินเดือน", "อนุมัติ"]


class TopicEncoder:
    """4-dim vectors: one axis per topic word, so similarity is predictable."""

    dim = 4
    device = "cpu"

    def encode(self, texts):
        out = []
        for t in texts:
            t = t.lower()
            v = [float(t.count(w)) for w in WORDS]
            n = math.sqrt(sum(x * x for x in v)) or 1.0
            out.append([x / n if any(v) else 0.5 for x in v])
        return out


@pytest.fixture()
def client():
    import psycopg

    from db import Store

    with psycopg.connect(URL, autocommit=True) as c:
        c.execute("DROP TABLE IF EXISTS messages, chats, chunks, documents, services, meta CASCADE")
    store = Store(URL, TopicEncoder.dim, "topic-test")
    yield TestClient(server.create_app(TopicEncoder(), TOKEN, store)), store
    store.close()


def doc(c, sid, path, chunks):
    d = c.put(f"/v1/services/{sid}/documents", json={"path": path, "hash": "h", "status": "indexing"}, headers=H).json()
    r = c.post(f"/v1/documents/{d['id']}/chunks", json={"replace": True, "chunks": chunks}, headers=H)
    assert r.status_code == 200, r.text
    c.patch(f"/v1/documents/{d['id']}", json={"status": "ready"}, headers=H)
    return d["id"]


def test_needs_the_token(client):
    c, _ = client
    assert c.get("/v1/store").status_code == 401
    assert c.post("/v1/search", json={"serviceIds": ["hr"], "queries": ["x"]}).status_code == 401
    assert c.get("/v1/chats", headers={"Authorization": "Bearer nope"}).status_code == 401
    assert c.get("/health").json()["database"] is True


def test_index_and_search(client):
    c, _ = client
    doc(c, "hr", "C:/docs/leave.pdf", [
        {"ord": 0, "page": 1, "text": "การลาพักร้อน ลา 10 วัน"},
        {"ord": 1, "page": 2, "text": "ขั้นตอนอนุมัติใบลา อนุมัติโดยหัวหน้า"},
    ])
    doc(c, "hr", "C:/docs/ot.xlsx", [{"ord": 0, "text": "OT คิดค่าล่วงเวลา OT"}])
    doc(c, "pos", "C:/pos/sales.pdf", [{"ord": 0, "text": "ลา ลา ลา (อีก service)"}])

    hits = c.post("/v1/search", json={"serviceIds": ["hr"], "queries": ["ลาได้กี่วัน"], "k": 3, "threshold": 0.2}, headers=H).json()["hits"]
    assert hits and hits[0]["docPath"] == "C:/docs/leave.pdf"
    assert all(h["serviceId"] == "hr" for h in hits), "other services stay out"
    assert {"chunkId", "page", "ord", "text", "score", "fused", "keyword"} <= set(hits[0])

    # exact code found by keyword even when vectors disagree
    hits = c.post("/v1/search", json={"serviceIds": ["hr"], "queries": ["ค่าล่วงเว"], "k": 3, "threshold": 0.99}, headers=H).json()["hits"]
    assert [h["docPath"] for h in hits] == ["C:/docs/ot.xlsx"] and hits[0]["keyword"]

    # several queries: merged, no duplicates
    hits = c.post("/v1/search", json={"serviceIds": ["hr", "pos"], "queries": ["ลา", "อนุมัติ"], "k": 10}, headers=H).json()["hits"]
    ids = [h["chunkId"] for h in hits]
    assert len(ids) == len(set(ids))


def test_documents_replace_and_delete(client):
    c, store = client
    did = doc(c, "hr", "a.pdf", [{"ord": i, "text": f"ลา {i}"} for i in range(3)])
    r = c.post(f"/v1/documents/{did}/chunks", json={"replace": True, "chunks": [{"ord": 0, "text": "ใหม่"}]}, headers=H).json()
    assert r["chunkCount"] == 1
    r = c.post(f"/v1/documents/{did}/chunks", json={"chunks": [{"ord": 1, "text": "ต่อ", "embedding": [1, 0, 0, 0]}]}, headers=H).json()
    assert r["chunkCount"] == 2, "appended, own vector kept"
    assert c.post(f"/v1/documents/{did}/chunks", json={"chunks": [{"ord": 2, "text": "x", "embedding": [1, 0]}]}, headers=H).status_code == 400
    assert c.post(f"/v1/documents/{did}/chunks", json={"chunks": [{"ord": i, "text": "x"} for i in range(65)]}, headers=H).status_code == 413
    docs = c.get("/v1/services/hr/documents", headers=H).json()
    assert docs[0]["path"] == "a.pdf" and docs[0]["status"] == "ready" and docs[0]["chunkCount"] == 2
    assert c.delete("/v1/services/hr/documents", params={"path": "a.pdf"}, headers=H).json()["deleted"] == 1
    assert store.info()["chunks"] == 0, "chunks go with their document"
    assert c.post("/v1/documents/999999/chunks", json={"chunks": [{"ord": 0, "text": "x"}]}, headers=H).status_code == 404


def test_services_are_shared_settings(client):
    c, _ = client
    c.put("/v1/services/hr", json={"data": {"name": "HR", "topK": 8}}, headers=H)
    c.put("/v1/services/hr", json={"data": {"name": "Human Resource", "topK": 8}}, headers=H)
    s = c.get("/v1/services", headers=H).json()
    assert len(s) == 1 and s[0]["data"]["name"] == "Human Resource"


def test_chats_and_messages(client):
    c, _ = client
    c.put("/v1/chats/c1", json={"serviceId": "hr", "title": "ใบลาไปผิดคน"}, headers=H)
    c.put("/v1/chats/c2", json={"serviceId": "pos", "title": "ยอดขาย"}, headers=H)
    c.post("/v1/chats/c1/messages", json={"role": "user", "content": "แล้วตรวจยังไง"}, headers=H)
    m = c.post("/v1/chats/c1/messages", json={"role": "assistant", "content": "ตาม [1]", "model": "godev1",
                                               "sources": [{"n": 1, "fileName": "a.pdf"}], "searchQuery": "วิธีตรวจใบลา", "elapsedMs": 900}, headers=H).json()
    assert m["searchQuery"] == "วิธีตรวจใบลา" and m["sources"][0]["fileName"] == "a.pdf"
    msgs = c.get("/v1/chats/c1/messages", headers=H).json()
    assert [x["role"] for x in msgs] == ["user", "assistant"] and "searchQuery" not in msgs[0]
    assert [x["id"] for x in c.get("/v1/chats", params={"serviceId": "hr"}, headers=H).json()] == ["c1"]
    assert [x["id"] for x in c.get("/v1/chats", params={"q": "ยอด"}, headers=H).json()] == ["c2"]
    assert c.get("/v1/chats", headers=H).json()[0]["id"] == "c1", "newest first"
    assert c.post("/v1/chats/nope/messages", json={"role": "user", "content": "x"}, headers=H).status_code == 404
    c.delete("/v1/chats/c1", headers=H)
    assert c.get("/v1/chats/c1/messages", headers=H).json() == []
    assert c.get("/v1/chats/c1", headers=H).status_code == 404


def test_share_keys_open_one_service_only(client):
    c, _ = client
    c.put("/v1/services/hr", json={"data": {"name": "HR"}}, headers=H)
    c.put("/v1/services/pos", json={"data": {"name": "POS"}}, headers=H)
    doc(c, "pos", "p.pdf", [{"ord": 0, "text": "ลา ยอดขาย"}])
    key = c.post("/v1/services/hr/key", headers=H).json()["key"]
    K = {"Authorization": f"Bearer {key}"}

    assert c.post("/v1/services/hr/key", headers=K).status_code == 401, "members cannot make keys"
    assert c.get("/v1/services/hr", headers=K).json()["data"]["name"] == "HR"
    assert c.get("/v1/services/pos", headers=K).status_code == 403
    assert c.get("/v1/services", headers=K).status_code == 401, "the list is the owner's"
    assert c.get("/v1/store", headers=K).status_code == 401

    # a member adds a document and searches its own service
    d = c.put("/v1/services/hr/documents", json={"path": "m.pdf", "hash": "h"}, headers=K).json()
    assert c.post(f"/v1/documents/{d['id']}/chunks", json={"replace": True, "chunks": [{"ord": 0, "text": "การลาพักร้อน"}]}, headers=K).status_code == 200
    assert c.patch(f"/v1/documents/{d['id']}", json={"status": "ready"}, headers=K).status_code == 200
    hits = c.post("/v1/search", json={"serviceIds": ["hr"], "queries": ["ลา"]}, headers=K).json()["hits"]
    assert [h["docPath"] for h in hits] == ["m.pdf"]
    assert c.post("/v1/search", json={"serviceIds": ["hr", "pos"], "queries": ["ลา"]}, headers=K).status_code == 403
    pos_doc = c.get("/v1/services/pos/documents", headers=H).json()[0]["id"]
    assert c.post(f"/v1/documents/{pos_doc}/chunks", json={"chunks": [{"ord": 1, "text": "x"}]}, headers=K).status_code == 403
    assert c.delete("/v1/services/hr/documents", headers=K).status_code == 403, "no clearing a whole service"
    assert c.delete("/v1/services/hr/documents", params={"path": "m.pdf"}, headers=K).json()["deleted"] == 1
    assert c.post("/v1/embeddings", json={"input": ["ลา"]}, headers=K).status_code == 200
    assert c.get("/v1/chats", headers=K).status_code == 401

    # a new key: the old one stops working
    new = c.post("/v1/services/hr/key", headers=H).json()["key"]
    assert new != key
    assert c.get("/v1/services/hr", headers=K).status_code == 401
    assert c.get("/v1/services/hr", headers={"Authorization": f"Bearer {new}"}).status_code == 200


def test_model_mismatch_refuses_to_start(client):
    from db import Store

    with pytest.raises(SystemExit):
        Store(URL, TopicEncoder.dim, "another-model")
