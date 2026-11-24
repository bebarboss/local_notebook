# DocAsk embedding server — the API; bge-m3 runs in Ollama on the host (OLLAMA_URL).
FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    HF_HOME=/models \
    HF_HUB_DISABLE_TELEMETRY=1 \
    FILES_DIR=/data/files

RUN useradd --create-home --uid 10001 app && mkdir -p /models /data/files && chown app /models /data/files
WORKDIR /srv

COPY requirements.txt .
RUN pip install --no-cache-dir torch==2.5.1 --index-url https://download.pytorch.org/whl/cpu \
 && pip install --no-cache-dir -r requirements.txt

COPY app.py db.py ./
USER app
EXPOSE 8080

# the first rerank downloads bge-reranker-v2-m3 (≈ 2.3 GB) into the /models volume; bge-m3 is Ollama's
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
  CMD python -c "import json,urllib.request,sys; sys.exit(json.load(urllib.request.urlopen('http://127.0.0.1:8080/health', timeout=5))['status'] != 'ok')"

CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080", "--workers", "1", "--no-server-header"]
