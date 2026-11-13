# DocAsk embedding server — CPU (arm64 on Apple Silicon, amd64 elsewhere).
FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    HF_HOME=/models \
    HF_HUB_DISABLE_TELEMETRY=1

RUN useradd --create-home --uid 10001 app && mkdir /models && chown app /models
WORKDIR /srv

COPY requirements.txt .
RUN pip install --no-cache-dir torch==2.5.1 --index-url https://download.pytorch.org/whl/cpu \
 && pip install --no-cache-dir -r requirements.txt

COPY app.py db.py ./
USER app
EXPOSE 8080

# the first start downloads the model (bge-m3 ≈ 2.3 GB) into the /models volume
HEALTHCHECK --interval=30s --timeout=5s --start-period=15m --retries=3 \
  CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/health', timeout=4)"

CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080", "--workers", "1", "--no-server-header"]
