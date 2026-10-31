# syntax=docker/dockerfile:1
# Public sandbox image (docs/SANDBOX.md): the same governor + Python shield as ./Dockerfile, plus the
# Postgres driver for the durable spend counter, running `python -m sandbox.app` on $PORT.
#   docker build -f sandbox/Dockerfile -t invoiceshield-sandbox .
# No BuildKit cache mounts here, so any builder (Render, Koyeb, Fly) can build it.

# ---- stage 1: build the governor ----
FROM rust:1-slim-bookworm AS governor-build
WORKDIR /src/governor
COPY governor/ ./
RUN cargo build --release --locked --bins \
 && mkdir -p /out && cp target/release/governor-server target/release/governor /out/ \
 && strip /out/governor-server /out/governor

# ---- stage 2: Python runtime ----
FROM python:3.12-slim-bookworm AS runtime
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 PIP_NO_CACHE_DIR=1 PIP_DISABLE_PIP_VERSION_CHECK=1
RUN apt-get update \
 && apt-get install -y --no-install-recommends tesseract-ocr poppler-utils \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY requirements.txt ./
COPY sandbox/requirements.txt ./sandbox-requirements.txt
RUN pip install -r requirements.txt -r sandbox-requirements.txt
COPY . .
COPY --from=governor-build /out/governor-server /out/governor /app/governor/target/release/
RUN groupadd --system --gid 10001 app \
 && useradd --system --uid 10001 --gid app --home-dir /app --shell /usr/sbin/nologin app
USER app:app
# Defaults for a public host; the platform sets PORT. Secrets (BASETEN_API_KEY, SANDBOX_DATABASE_URL)
# come only from the platform's environment, never from the image.
ENV PORT=8000 \
    SANDBOX_STATE=/tmp/sandbox-counter.json \
    SANDBOX_REQUIRE_DURABLE=1
EXPOSE 8000
CMD ["python", "-m", "sandbox.app"]
