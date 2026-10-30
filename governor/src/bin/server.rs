//! HTTP front for the governor. Fail closed: bad auth → 401, bad body → REVIEW decision.
//!
//! Env: GOVERNOR_API_KEY (required), GOVERNOR_MASTER (path to master.json),
//! GOVERNOR_PORT (default 8080), GOVERNOR_EVAL_CLOCK=1 to honour the `x-eval-now`
//! header (unix seconds) for split-payment eval cases. Off by default.
//! GOVERNOR_DECISIONS_CAP: how many recent decisions GET /v1/decisions/:id keeps (default 10000).
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use invoiceshield_governor::decisions::DecisionStore;
use invoiceshield_governor::{Governor, Master};
use serde::Deserialize;
use std::{sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

struct App {
    gov: Governor,
    decisions: DecisionStore,
}

#[derive(Clone)]
struct Shared {
    app: Arc<Mutex<App>>,
    key: Arc<String>,
    eval_clock: bool,
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
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

async fn actions(State(s): State<Shared>, h: HeaderMap, body: String) -> impl IntoResponse {
    if !authorized(&h, &s.key) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"}))).into_response();
    }
    let t = now(&h, s.eval_clock);
    // One mutex around evaluate + ledger write: two copies of the same invoice
    // racing each other cannot both pass the duplicate check.
    let mut app = s.app.lock().await;
    let d = app.gov.evaluate_json(&body, t);
    app.decisions.insert(d.clone());
    (StatusCode::OK, Json(d)).into_response()
}

async fn get_decision(State(s): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> impl IntoResponse {
    if !authorized(&h, &s.key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match s.app.lock().await.decisions.get(&id) {
        Some(d) => Json(d.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
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
    let mut app = s.app.lock().await;
    let pending = app.decisions.get(&id).map(|d| d.verdict == invoiceshield_governor::Verdict::Review).unwrap_or(false);
    if !pending || !app.gov.resolve_review(&id, r.approve, &r.approver, t) {
        return StatusCode::CONFLICT.into_response();
    }
    Json(serde_json::json!({"decision_id": id, "approved": r.approve})).into_response()
}

/// Eval-only reset; 404 unless the server was started with GOVERNOR_EVAL_CLOCK=1.
async fn eval_reset(State(s): State<Shared>, h: HeaderMap) -> impl IntoResponse {
    if !s.eval_clock {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !authorized(&h, &s.key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut app = s.app.lock().await;
    app.gov.reset_state();
    app.decisions.clear();
    Json(serde_json::json!({"reset": true})).into_response()
}

async fn audit_verify(State(s): State<Shared>, h: HeaderMap) -> impl IntoResponse {
    if !authorized(&h, &s.key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let app = s.app.lock().await;
    let n = app.gov.audit.records.len();
    Json(serde_json::json!({"records": n, "first_broken": app.gov.audit.verify()})).into_response()
}

#[tokio::main]
async fn main() {
    let key = std::env::var("GOVERNOR_API_KEY").unwrap_or_default();
    if key.len() < 16 {
        eprintln!("GOVERNOR_API_KEY must be set (>= 16 chars)");
        std::process::exit(2);
    }
    let path = std::env::var("GOVERNOR_MASTER").unwrap_or_else(|_| "../dataset/fixtures/master.json".into());
    let master = Master::from_json(&std::fs::read_to_string(&path).expect("read master")).expect("parse master");
    let cap = match std::env::var("GOVERNOR_DECISIONS_CAP") {
        Err(_) => 10_000,
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => {
                eprintln!("GOVERNOR_DECISIONS_CAP must be a positive integer");
                std::process::exit(2);
            }
        },
    };
    let shared = Shared {
        app: Arc::new(Mutex::new(App { gov: Governor::new(master), decisions: DecisionStore::new(cap) })),
        key: Arc::new(key),
        eval_clock: std::env::var("GOVERNOR_EVAL_CLOCK").as_deref() == Ok("1"),
    };
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/actions", post(actions))
        .route("/v1/decisions/:id", get(get_decision))
        .route("/v1/decisions/:id/resolve", post(resolve))
        .route("/v1/audit/verify", get(audit_verify))
        .route("/v1/eval/reset", post(eval_reset))
        .with_state(shared);
    let port = std::env::var("GOVERNOR_PORT").unwrap_or_else(|_| "8080".into());
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await.expect("bind");
    eprintln!("governor listening on 127.0.0.1:{port}");
    axum::serve(listener, app).await.expect("serve");
}
