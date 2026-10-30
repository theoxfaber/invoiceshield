//! HTTP front for the governor. Fail closed: bad auth → 401, bad body → REVIEW decision,
//! state store unavailable → 503 (nothing is paid).
//!
//! Env: GOVERNOR_API_KEY (required), GOVERNOR_MASTER (path to master.json),
//! GOVERNOR_PORT (default 8080), GOVERNOR_EVAL_CLOCK=1 to honour the `x-eval-now`
//! header (unix seconds) for split-payment eval cases. Off by default.
//!
//! Durable state (Phase 3, see persist.rs / anchor.rs):
//!   GOVERNOR_DB                SQLite file (WAL). Default `governor.db`; `:memory:` in eval mode.
//!   GOVERNOR_ANCHOR_KEY[_FILE] Ed25519 seed (hex) that signs audit-chain anchors.
//!   GOVERNOR_ANCHOR_FILE       append-only anchor file (default `<db>.anchors.jsonl`).
//!   GOVERNOR_ANCHOR_INTERVAL_SECS  default 60.
//!   GOVERNOR_ANCHOR_URL        optional http:// URL each anchor is POSTed to.
//!
//! Evidence provenance (Phase 2, see trust.rs and docs/DECISIONS.md):
//!   GOVERNOR_EXTRACTOR_PUBKEY  (required) the extraction service's Ed25519 public key(s),
//!                              64 hex chars or base64, comma-separated for rotation. Evidence is
//!                              read only from a `signed_evidence` envelope that verifies.
//!   GOVERNOR_EVIDENCE_MAX_AGE_SECS (default 300), GOVERNOR_EVIDENCE_MAX_SKEW_SECS (default 30).
//!   GOVERNOR_DEV_UNSIGNED_EVIDENCE=1  DEV/TEST ONLY: also accept the old caller-supplied
//!                              `evidence` field. Refused unless GOVERNOR_EVAL_CLOCK=1; off by default.
//!
//! Ops (Phase 6): GOVERNOR_BIND (default 127.0.0.1). Startup validation of every variable
//! above: src/config.rs (all errors listed, exit 2). JSON logs, /healthz, /readyz, /metrics:
//! src/observability.rs. API schema: docs/openapi.yaml.
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use invoiceshield_governor::anchor;
use invoiceshield_governor::persist::{ActionOutcome, Fault, Store};
use invoiceshield_governor::TrustVerifier;
use invoiceshield_governor::{config::Config, observability};
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

#[derive(Clone)]
struct Shared {
    /// In-process lock kept as defence in depth; the DB write lock + UNIQUE indexes are the
    /// guarantee across processes (DECISIONS.md).
    store: Arc<Mutex<Store>>,
    key: Arc<String>,
    eval_clock: bool,
    anchors: Option<Arc<AnchorCfg>>,
}

struct AnchorCfg {
    key: ed25519_dalek::SigningKey,
    file: PathBuf,
    url: Option<String>,
}

fn authorized(h: &HeaderMap, key: &str) -> bool {
    let got = h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    !key.is_empty() && got.as_bytes().ct_eq(key.as_bytes()).into()
}

fn now(h: &HeaderMap, eval_clock: bool) -> i64 {
    if eval_clock {
        if let Some(t) = h.get("x-eval-now").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()) {
            return t;
        }
    }
    unix_now()
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn err(code: StatusCode, msg: &str) -> axum::response::Response {
    (code, Json(serde_json::json!({"error": msg}))).into_response()
}

/// Run blocking SQLite work off the async workers, under the in-process lock.
async fn with_store<T: Send + 'static>(s: &Shared, f: impl FnOnce(&mut Store) -> T + Send + 'static) -> Option<T> {
    let st = s.store.clone();
    tokio::task::spawn_blocking(move || {
        let mut g = st.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    })
    .await
    .ok()
}

async fn actions(State(s): State<Shared>, h: HeaderMap, body: String) -> impl IntoResponse {
    if !authorized(&h, &s.key) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let idem = match h.get("idempotency-key") {
        None => None,
        Some(v) => match v.to_str() {
            Ok(k) if !k.is_empty() && k.len() <= 200 && k.bytes().all(|b| b.is_ascii_graphic()) => Some(k.to_string()),
            _ => return err(StatusCode::BAD_REQUEST, "Idempotency-Key must be 1-200 printable ASCII characters"),
        },
    };
    let t = now(&h, s.eval_clock);
    let out = with_store(&s, move |st| st.act(&body, t, idem.as_deref())).await;
    match out {
        Some(Ok(ActionOutcome::Fresh(d))) => (StatusCode::OK, Json(d)).into_response(),
        Some(Ok(ActionOutcome::Replay(d))) => (StatusCode::OK, [("idempotent-replayed", "true")], Json(d)).into_response(),
        Some(Ok(ActionOutcome::KeyConflict { decision_id })) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "idempotency_key_reused_with_different_body", "decision_id": decision_id})),
        )
            .into_response(),
        Some(Ok(ActionOutcome::DbRejected(m))) => {
            tracing::error!(event = "db_rejected_write", error = %m, "duplicate live ledger entry");
            (StatusCode::CONFLICT, Json(serde_json::json!({"error": "duplicate_rejected_by_database", "paid": false}))).into_response()
        }
        Some(Err(e)) => {
            tracing::error!(event = "state_store_error", error = %e, "failing closed");
            err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable")
        }
        None => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

async fn get_decision(State(s): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
    if !authorized(&h, &s.key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match with_store(&s, move |st| st.get_decision(&id)).await {
        Some(Ok(Some(d))) => Json(d).into_response(),
        Some(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    approve: bool,
    approver: String,
}

/// Human decision on a REVIEW. Reachable only with the operator key, never from the agent's tool.
async fn resolve(State(s): State<Shared>, h: HeaderMap, Path(id): Path<String>, Json(r): Json<Resolve>) -> impl IntoResponse {
    let op_key = std::env::var("GOVERNOR_OPERATOR_KEY").unwrap_or_default();
    if !authorized(&h, &op_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let t = now(&h, s.eval_clock);
    let id2 = id.clone();
    match with_store(&s, move |st| st.resolve(&id2, r.approve, &r.approver, t)).await {
        Some(Ok(true)) => Json(serde_json::json!({"decision_id": id, "approved": r.approve})).into_response(),
        Some(Ok(false)) => StatusCode::CONFLICT.into_response(),
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

/// Eval-only reset; 404 unless the server was started with GOVERNOR_EVAL_CLOCK=1.
async fn eval_reset(State(s): State<Shared>, h: HeaderMap) -> impl IntoResponse {
    if !s.eval_clock {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !authorized(&h, &s.key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match with_store(&s, |st| st.reset()).await {
        Some(Ok(())) => Json(serde_json::json!({"reset": true})).into_response(),
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

async fn audit_verify(State(s): State<Shared>, h: HeaderMap) -> impl IntoResponse {
    if !authorized(&h, &s.key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let anchors = s.anchors.clone();
    let r = with_store(&s, move |st| -> Result<serde_json::Value, String> {
        let records = st.audit_records().map_err(|e| e.to_string())?;
        let first_broken = invoiceshield_governor::audit::AuditLog { records: records.clone() }.verify();
        let mut out = serde_json::json!({"records": records.len(), "first_broken": first_broken});
        if let Some(a) = anchors {
            let list = anchor::read_all(&a.file).map_err(|e| e.to_string())?;
            let rep = anchor::verify_chain(&records, &list, &a.key.verifying_key());
            out["anchors"] = serde_json::json!({"checked": rep.anchors_checked, "errors": rep.anchor_errors, "ok": rep.ok});
        }
        Ok(out)
    })
    .await;
    match r {
        Some(Ok(v)) => Json(v).into_response(),
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

/// Write a signed anchor for the current chain head if it moved since the last one.
async fn anchor_once(s: &Shared, last: &mut Option<u64>) {
    let Some(cfg) = s.anchors.clone() else { return };
    let head = with_store(s, |st| st.audit_head()).await;
    let Some(Ok(Some((seq, hash, _)))) = head else { return };
    if *last == Some(seq) {
        return;
    }
    let a = anchor::sign(&cfg.key, seq, &hash, unix_now());
    let file = cfg.file.clone();
    let a2 = a.clone();
    match tokio::task::spawn_blocking(move || anchor::append(&file, &a2)).await {
        Ok(Ok(())) => *last = Some(seq),
        other => {
            tracing::error!(event = "anchor_write_failed", error = ?other);
            return;
        }
    }
    if let Some(url) = &cfg.url {
        if let Err(e) = anchor::post(url, &a).await {
            tracing::warn!(event = "anchor_post_failed", url = %url, error = %e, "file anchor kept");
        }
    }
}

#[tokio::main]
async fn main() {
    observability::init_logging();
    // Fail fast on bad configuration (all errors reported together, exit 2). See config.rs.
    let cfg = Config::from_env_or_exit();
    let addr = cfg.addr();
    let (key, eval_clock, master_path, tcfg) = (cfg.api_key, cfg.eval_clock, cfg.master_path, cfg.trust);
    if tcfg.dev_unsigned {
        tracing::warn!(event = "dev_unsigned_evidence", "DEV MODE: unsigned caller-supplied evidence is accepted. Never run this in production.");
    }
    let db = std::env::var("GOVERNOR_DB").unwrap_or_else(|_| if eval_clock { ":memory:".into() } else { "governor.db".into() });
    let mut store = match Store::open(&db, cfg.master) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(event = "state_db_open_failed", db = %db, error = %e);
            eprintln!("cannot open state DB {db}: {e}");
            std::process::exit(2);
        }
    };
    store.trust = Some(TrustVerifier::new(tcfg));
    if db == ":memory:" {
        tracing::warn!(event = "state_in_memory", "GOVERNOR_DB=:memory: — ledger and audit are lost on exit (eval mode only)");
    }
    if eval_clock {
        if let Ok(f) = std::env::var("GOVERNOR_TEST_FAULT") {
            store.inject_fault(match f.as_str() {
                "abort_after_ledger_insert" => Some(Fault::AbortAfterLedgerInsert),
                "error_after_ledger_insert" => Some(Fault::ErrorAfterLedgerInsert),
                _ => None,
            });
        }
    }
    let anchors = match anchor::key_from_env() {
        Err(e) => {
            tracing::error!(event = "anchor_key_invalid", error = %e);
            eprintln!("{e}");
            std::process::exit(2);
        }
        Ok(None) => {
            tracing::info!(event = "anchoring_off", "set GOVERNOR_ANCHOR_KEY or GOVERNOR_ANCHOR_KEY_FILE to anchor the audit chain");
            None
        }
        Ok(Some(k)) => {
            let file = std::env::var("GOVERNOR_ANCHOR_FILE").map(PathBuf::from).unwrap_or_else(|_| {
                if db == ":memory:" { PathBuf::from("governor-memory.anchors.jsonl") } else { PathBuf::from(format!("{db}.anchors.jsonl")) }
            });
            tracing::info!(event = "anchoring_on", file = %file.display(), pubkey = %hex::encode(k.verifying_key().to_bytes()));
            Some(Arc::new(AnchorCfg { key: k, file, url: std::env::var("GOVERNOR_ANCHOR_URL").ok().filter(|u| !u.is_empty()) }))
        }
    };
    let shared = Shared { store: Arc::new(Mutex::new(store)), key: Arc::new(key), eval_clock, anchors };
    if shared.anchors.is_some() {
        let secs: u64 = std::env::var("GOVERNOR_ANCHOR_INTERVAL_SECS").ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(60);
        let s2 = shared.clone();
        tokio::spawn(async move {
            let mut last = None;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs));
            loop {
                tick.tick().await;
                anchor_once(&s2, &mut last).await;
            }
        });
    }
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/actions", post(actions))
        .route("/v1/decisions/:id", get(get_decision))
        .route("/v1/decisions/:id/resolve", post(resolve))
        .route("/v1/audit/verify", get(audit_verify))
        .route("/v1/eval/reset", post(eval_reset))
        .merge(observability::routes())
        .layer(axum::middleware::from_fn(observability::track))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
        tracing::error!(event = "bind_failed", addr = %addr, error = %e);
        std::process::exit(2);
    });
    observability::set_ready(true);
    tracing::info!(event = "listening", addr = %addr, state = %db, master = %master_path, eval_clock);
    axum::serve(listener, app).await.expect("serve");
}
