# syntax=docker/dockerfile:1
# InvoiceShield: Rust governor + Python shield in one image.
#   docker build -t invoiceshield:local .
#   docker run --rm -e GOVERNOR_API_KEY=$(openssl rand -hex 16) -e GOVERNOR_EXTRACTOR_PUBKEY=<extractor pubkey hex> \
#     -p 127.0.0.1:8080:8080 invoiceshield:local      (docker-compose.yml generates the extractor key for you)
# Default command runs the governor HTTP server (fails fast on bad config, see governor/src/config.rs),
# with durable state in the /data volume (docker/governor.sh). The operator CLI is `governor`.
# The same image runs the offline demo (`python -m eval.demo`) and the extraction service.

# ---- stage 1: build the governor ----
FROM rust:1-slim-bookworm AS governor-build
WORKDIR /src/governor
COPY governor/ ./
# BuildKit cache mounts keep the crate registry and target dir between builds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/governor/target \
    cargo build --release --locked --bins \
 && mkdir -p /out && cp target/release/governor-server target/release/governor /out/ \
 && strip /out/governor-server /out/governor

# ---- stage 2: Python runtime ----
FROM python:3.12-slim-bookworm AS runtime
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 PIP_NO_CACHE_DIR=1 PIP_DISABLE_PIP_VERSION_CHECK=1
# tesseract + poppler: scans/photos are OCR'd locally (assembler/scan.py).
RUN apt-get update \
 && apt-get install -y --no-install-recommends tesseract-ocr poppler-utils \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY requirements.txt ./
RUN pip install -r requirements.txt
COPY . .
# The offline demo regenerates eval/riskset (byte-identical), so only that directory is app-writable.
# The Python shield starts governor/target/release/governor-server (shield/client.py).
COPY --from=governor-build /out/governor-server /out/governor /app/governor/target/release/
RUN groupadd --system --gid 10001 app \
 && useradd --system --uid 10001 --gid app --home-dir /app --shell /usr/sbin/nologin app \
 && mkdir -p /data /keys /pub && chown app:app /data /keys /pub && chmod 700 /keys \
 && chown -R app:app /app/eval/riskset
USER app:app
ENV GOVERNOR_BIND=0.0.0.0 \
    GOVERNOR_PORT=8080 \
    GOVERNOR_MASTER=/app/dataset/fixtures/master.json
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
  CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz', timeout=2)" || exit 1
CMD ["/app/docker/governor.sh"]
