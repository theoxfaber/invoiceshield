//! HTTP front for the governor. Fail closed: bad auth → 401, wrong role → 403, over the rate
//! limit → 429, bad body → REVIEW decision, state store unavailable → 503 (nothing is paid).
//!
//! Identities and roles (Phase 4, see auth.rs / security.rs / docs/THREAT_MODEL.md):
//!   agent    POST /v1/actions (proposes; can never approve)
//!   operator POST /v1/master-changes (maker), .../callback
//!   approver POST /v1/decisions/:id/resolve, /v1/master-changes/:id/approve (checker; never the
//!            identity that proposed / entered the item)
//!   any      GET /v1/decisions/:id, GET /v1/audit/verify
//!   approver|operator GET /v1/reviews (pending REVIEW queue). GET /v1/ui/review is the static
//!            approver page (no auth to load; it calls the API with the approver's key).
//!   GOVERNOR_IDENTITIES[_FILE] (hashed keys), or the legacy GOVERNOR_API_KEY[_FILE] (agent) and
//!   GOVERNOR_OPERATOR_KEY[_FILE] (operator+approver). TLS: GOVERNOR_TLS_CERT_FILE/KEY_FILE.
//!   Rate limits: GOVERNOR_RATE_LIMIT_* (security.rs).
//!
//! Env: GOVERNOR_MASTER (path to master.json),
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
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::IntoResponse,
    routing::{get, post},
    Extension, Json, Router,
};
use invoiceshield_governor::anchor;
use invoiceshield_governor::auth::{bearer, Identity, KeyRing, Role};
use invoiceshield_governor::master_store::{CallbackReport, McError, McResult, NewDetails};
use invoiceshield_governor::persist::{ActionOutcome, Fault, ResolveOutcome, Store};
use invoiceshield_governor::ratelimit::{Check, Limits};
use invoiceshield_governor::review_queue;
use invoiceshield_governor::TrustVerifier;
use invoiceshield_governor::{config::Config, observability};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
struct Shared {
    /// In-process lock kept as defence in depth; the DB write lock + UNIQUE indexes are the
    /// guarantee across processes (DECISIONS.md).
    store: Arc<Mutex<Store>>,
    keys: Arc<KeyRing>,
    limits: Arc<Limits>,
    eval_clock: bool,
    anchors: Option<Arc<AnchorCfg>>,
}

struct AnchorCfg {
    key: ed25519_dalek::SigningKey,
    file: PathBuf,
    url: Option<String>,
}

fn too_many(retry_after_secs: u64, scope: &str) -> axum::response::Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after", retry_after_secs.to_string())],
        Json(serde_json::json!({"error": "rate_limited", "scope": scope, "retry_after_secs": retry_after_secs})),
    )
        .into_response()
}

/// Rate limit by client IP, authenticate, rate limit by identity, then hand the route its
/// `Identity`. Failed authentications draw from a stricter per-IP bucket.
async fn guard(State(s): State<Shared>, mut req: Request, next: Next) -> axum::response::Response {
    let ip = invoiceshield_governor::tls::peer(req.extensions())
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".into());
    if let Some(l) = &s.limits.auth_fail_per_ip {
        if let Check::Limited { retry_after_secs } = l.is_limited(&ip) {
            return too_many(retry_after_secs, "auth_failures");
        }
    }
    if let Some(l) = &s.limits.per_ip {
        if let Check::Limited { retry_after_secs } = l.check(&ip) {
            return too_many(retry_after_secs, "ip");
        }
    }
    let token = bearer(
        req.headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
    );
    let Some(id) = s.keys.authenticate(token) else {
        if let Some(l) = &s.limits.auth_fail_per_ip {
            let _ = l.check(&ip);
        }
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    if let Some(l) = &s.limits.per_identity {
        if let Check::Limited { retry_after_secs } = l.check(&id.name) {
            return too_many(retry_after_secs, "identity");
        }
    }
    req.extensions_mut().insert(id);
    next.run(req).await
}

/// 403 unless the identity holds one of `roles`.
fn require(id: &Identity, roles: &[Role]) -> Option<axum::response::Response> {
    if id.has_any(roles) {
        return None;
    }
    let need: Vec<&str> = roles.iter().map(|r| r.as_str()).collect();
    Some(
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "forbidden", "identity": id.name, "need_role": need})),
        )
            .into_response(),
    )
}

fn now(h: &HeaderMap, eval_clock: bool) -> i64 {
    if eval_clock {
        if let Some(t) = h
            .get("x-eval-now")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
        {
            return t;
        }
    }
    unix_now()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn err(code: StatusCode, msg: &str) -> axum::response::Response {
    (code, Json(serde_json::json!({"error": msg}))).into_response()
}

/// Run blocking SQLite work off the async workers, under the in-process lock.
async fn with_store<T: Send + 'static>(
    s: &Shared,
    f: impl FnOnce(&mut Store) -> T + Send + 'static,
) -> Option<T> {
    let st = s.store.clone();
    tokio::task::spawn_blocking(move || {
        let mut g = st.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    })
    .await
    .ok()
}

async fn actions(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    h: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if let Some(r) = require(&id, &[Role::Agent]) {
        return r;
    }
    let idem = match h.get("idempotency-key") {
        None => None,
        Some(v) => match v.to_str() {
            Ok(k) if !k.is_empty() && k.len() <= 200 && k.bytes().all(|b| b.is_ascii_graphic()) => {
                Some(k.to_string())
            }
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "Idempotency-Key must be 1-200 printable ASCII characters",
                )
            }
        },
    };
    let t = now(&h, s.eval_clock);
    let who = id.name.clone();
    let out = with_store(&s, move |st| {
        st.act_as(&body, t, idem.as_deref(), Some(&who))
    })
    .await;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// Phase 7: pending REVIEW decisions, oldest first, for the approver page (read-only).
/// Approver or operator role; the resolve call itself stays approver-only with maker-checker.
async fn list_reviews(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    axum::extract::Query(q): axum::extract::Query<ReviewQuery>,
) -> impl IntoResponse {
    if let Some(r) = require(&who, &[Role::Approver, Role::Operator]) {
        return r;
    }
    let limit = q.limit.unwrap_or(review_queue::MAX_LIST);
    match with_store(&s, move |st| st.pending_reviews(limit)).await {
        Some(Ok(v)) => {
            Json(serde_json::json!({"identity": who.name, "count": v.len(), "reviews": v}))
                .into_response()
        }
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

/// The static approver page (governor/ui/review.*). No secrets in it; it asks for the
/// approver's key and calls the API above. Strict CSP: same-origin script/style/fetch only.
fn ui_asset(body: &'static str, ctype: &'static str) -> axum::response::Response {
    (
        [
            ("content-type", ctype),
            ("content-security-policy", "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'"),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn get_decision(State(s): State<Shared>, Path(id): Path<String>) -> impl IntoResponse {
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
    /// Optional, for older clients; when given it must name the authenticated identity. The
    /// approver recorded is always the identity, never a string from the body.
    #[serde(default)]
    approver: Option<String>,
}

/// Human decision on a REVIEW: approver role only, never the identity that proposed it.
async fn resolve(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(r): Json<Resolve>,
) -> impl IntoResponse {
    if let Some(resp) = require(&who, &[Role::Approver]) {
        return resp;
    }
    if r.approver.as_deref().is_some_and(|a| a != who.name) {
        return err(
            StatusCode::BAD_REQUEST,
            "approver_must_match_authenticated_identity",
        );
    }
    let t = now(&h, s.eval_clock);
    let id2 = id.clone();
    let name = who.name.clone();
    match with_store(&s, move |st| st.resolve_as(&id2, r.approve, &name, t)).await {
        Some(Ok(ResolveOutcome::Resolved)) => Json(
            serde_json::json!({"decision_id": id, "approved": r.approve, "approver": who.name}),
        )
        .into_response(),
        Some(Ok(ResolveOutcome::NotPending)) => err(StatusCode::CONFLICT, "not_a_pending_review"),
        Some(Ok(ResolveOutcome::SelfApproval)) => err(
            StatusCode::FORBIDDEN,
            "maker_checker_proposer_cannot_resolve",
        ),
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

fn mc_response(
    r: Option<invoiceshield_governor::persist::Result<McResult>>,
) -> axum::response::Response {
    match r {
        Some(Ok(Ok(v))) => Json(v).into_response(),
        Some(Ok(Err(e))) => {
            let code = match &e {
                McError::NotFound => StatusCode::NOT_FOUND,
                McError::MakerCannot => StatusCode::FORBIDDEN,
                McError::OpenChangeExists(_)
                | McError::WrongState(_)
                | McError::AlreadyApproved => StatusCode::CONFLICT,
                McError::UnknownVendor | McError::InvalidDetails(_) | McError::Refused(_) => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
            };
            let detail = match &e {
                McError::InvalidDetails(m) => m.clone(),
                McError::OpenChangeExists(id) => format!("open change {id}"),
                McError::WrongState(st) => format!("change is {st}"),
                other => format!("{other:?}"),
            };
            (
                code,
                Json(serde_json::json!({"error": e.code(), "detail": detail})),
            )
                .into_response()
        }
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McRequest {
    vendor_id: String,
    new_account: String,
    new_ifsc: String,
    new_bank_name: String,
    new_beneficiary_id: String,
}

/// Maker: an operator enters new bank details for a vendor.
async fn mc_create(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    h: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if let Some(r) = require(&who, &[Role::Operator]) {
        return r;
    }
    let req: McRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_body", "detail": e.to_string()})),
            )
                .into_response()
        }
    };
    let t = now(&h, s.eval_clock);
    mc_response(
        with_store(&s, move |st| {
            let d = NewDetails {
                new_account: req.new_account,
                new_ifsc: req.new_ifsc,
                new_bank_name: req.new_bank_name,
                new_beneficiary_id: req.new_beneficiary_id,
            };
            st.mc_request(&who.name, &req.vendor_id, d, t)
        })
        .await,
    )
}

async fn mc_get(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(r) = require(&who, &[Role::Operator, Role::Approver]) {
        return r;
    }
    match with_store(&s, move |st| st.mc_get(&id)).await {
        Some(Ok(Some(v))) => Json(v).into_response(),
        Some(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

/// The out-of-band call-back to the vendor's number on file (operator or approver, not the maker).
async fn mc_callback(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> impl IntoResponse {
    if let Some(r) = require(&who, &[Role::Operator, Role::Approver]) {
        return r;
    }
    let cb: CallbackReport = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_body", "detail": e.to_string()})),
            )
                .into_response()
        }
    };
    let t = now(&h, s.eval_clock);
    mc_response(with_store(&s, move |st| st.mc_callback(&id, &who.name, cb, t)).await)
}

/// Checker: approver role, not the maker; two distinct approvers start the cooling-off.
async fn mc_approve(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(r) = require(&who, &[Role::Approver]) {
        return r;
    }
    let t = now(&h, s.eval_clock);
    mc_response(with_store(&s, move |st| st.mc_approve(&id, &who.name, t)).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McReject {
    #[serde(default)]
    reason: String,
}

async fn mc_reject(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: String,
) -> impl IntoResponse {
    if let Some(r) = require(&who, &[Role::Operator, Role::Approver]) {
        return r;
    }
    let rj: McReject = if body.trim().is_empty() {
        McReject {
            reason: String::new(),
        }
    } else {
        match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_body", "detail": e.to_string()})),
                )
                    .into_response()
            }
        }
    };
    let t = now(&h, s.eval_clock);
    mc_response(with_store(&s, move |st| st.mc_reject(&id, &who.name, &rj.reason, t)).await)
}

/// Eval-only reset; 404 unless the server was started with GOVERNOR_EVAL_CLOCK=1.
async fn eval_reset(State(s): State<Shared>) -> impl IntoResponse {
    if !s.eval_clock {
        return StatusCode::NOT_FOUND.into_response();
    }
    match with_store(&s, |st| st.reset()).await {
        Some(Ok(())) => Json(serde_json::json!({"reset": true})).into_response(),
        _ => err(StatusCode::SERVICE_UNAVAILABLE, "state_unavailable"),
    }
}

async fn audit_verify(State(s): State<Shared>) -> impl IntoResponse {
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
    let Some(Ok(Some((seq, hash, _)))) = head else {
        return;
    };
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
    let (sec, eval_clock, master_path, tcfg) =
        (cfg.security, cfg.eval_clock, cfg.master_path, cfg.trust);
    for w in &sec.warnings {
        tracing::warn!(event = "security_config", warning = %w);
    }
    let names: Vec<String> = sec
        .keys
        .identities()
        .map(|i| {
            format!(
                "{}{:?}",
                i.name,
                i.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>()
            )
        })
        .collect();
    tracing::info!(event = "identities", identities = %names.join(", "));
    if tcfg.dev_unsigned {
        tracing::warn!(event = "dev_unsigned_evidence", "DEV MODE: unsigned caller-supplied evidence is accepted. Never run this in production.");
    }
    let db = std::env::var("GOVERNOR_DB").unwrap_or_else(|_| {
        if eval_clock {
            ":memory:".into()
        } else {
            "governor.db".into()
        }
    });
    let mut store = match Store::open(&db, cfg.master) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(event = "state_db_open_failed", db = %db, error = %e);
            eprintln!("cannot open state DB {db}: {e}");
            std::process::exit(2);
        }
    };
    store.trust = Some(TrustVerifier::new(tcfg));
    store.phones = sec.phones.clone();
    store.cooling_off_secs = sec.cooling_off_secs;
    if let Err(e) = store.set_state_key(sec.state_key) {
        tracing::error!(event = "state_db_open_failed", db = %db, error = %e);
        eprintln!("cannot load state DB {db}: {e}");
        std::process::exit(2);
    }
    if db == ":memory:" {
        tracing::warn!(
            event = "state_in_memory",
            "GOVERNOR_DB=:memory: — ledger and audit are lost on exit (eval mode only)"
        );
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
            tracing::info!(
                event = "anchoring_off",
                "set GOVERNOR_ANCHOR_KEY or GOVERNOR_ANCHOR_KEY_FILE to anchor the audit chain"
            );
            None
        }
        Ok(Some(k)) => {
            let file = std::env::var("GOVERNOR_ANCHOR_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    if db == ":memory:" {
                        PathBuf::from("governor-memory.anchors.jsonl")
                    } else {
                        PathBuf::from(format!("{db}.anchors.jsonl"))
                    }
                });
            tracing::info!(event = "anchoring_on", file = %file.display(), pubkey = %hex::encode(k.verifying_key().to_bytes()));
            Some(Arc::new(AnchorCfg {
                key: k,
                file,
                url: std::env::var("GOVERNOR_ANCHOR_URL")
                    .ok()
                    .filter(|u| !u.is_empty()),
            }))
        }
    };
    let tls_cfg = match &sec.tls {
        None => None,
        Some((c, k)) => match invoiceshield_governor::tls::server_config(c, k) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("config error: {e}");
                std::process::exit(2);
            }
        },
    };
    let shared = Shared {
        store: Arc::new(Mutex::new(store)),
        keys: Arc::new(sec.keys),
        limits: Arc::new(sec.limits.build()),
        eval_clock,
        anchors,
    };
    if shared.anchors.is_some() {
        let secs: u64 = std::env::var("GOVERNOR_ANCHOR_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(60);
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
    // Every /v1 route goes through `guard` (rate limits + authentication -> Identity); each
    // handler then checks its role. /health and the ops routes stay outside it.
    let api = Router::new()
        .route("/v1/actions", post(actions))
        .route("/v1/reviews", get(list_reviews))
        .route("/v1/decisions/:id", get(get_decision))
        .route("/v1/decisions/:id/resolve", post(resolve))
        .route("/v1/audit/verify", get(audit_verify))
        .route("/v1/eval/reset", post(eval_reset))
        .route("/v1/master-changes", post(mc_create))
        .route("/v1/master-changes/:id", get(mc_get))
        .route("/v1/master-changes/:id/callback", post(mc_callback))
        .route("/v1/master-changes/:id/approve", post(mc_approve))
        .route("/v1/master-changes/:id/reject", post(mc_reject))
        .route_layer(axum::middleware::from_fn_with_state(shared.clone(), guard));
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/v1/ui/review",
            get(|| async {
                ui_asset(
                    include_str!("../../ui/review.html"),
                    "text/html; charset=utf-8",
                )
            }),
        )
        .route(
            "/v1/ui/review.js",
            get(|| async {
                ui_asset(
                    include_str!("../../ui/review.js"),
                    "text/javascript; charset=utf-8",
                )
            }),
        )
        .route(
            "/v1/ui/review.css",
            get(|| async {
                ui_asset(
                    include_str!("../../ui/review.css"),
                    "text/css; charset=utf-8",
                )
            }),
        )
        .merge(api)
        .merge(observability::routes())
        .layer(axum::middleware::from_fn(observability::track))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(event = "bind_failed", addr = %addr, error = %e);
            std::process::exit(2);
        });
    observability::set_ready(true);
    match tls_cfg {
        Some(cfg) => {
            tracing::info!(event = "listening", addr = %addr, tls = true, state = %db, master = %master_path, eval_clock);
            invoiceshield_governor::tls::serve(listener, cfg, app)
                .await
                .expect("serve");
        }
        None => {
            tracing::info!(event = "listening", addr = %addr, tls = false, state = %db, master = %master_path, eval_clock);
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("serve");
        }
    }
}
