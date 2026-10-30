//! Operational surface of the governor server: structured JSON logs, Prometheus
//! metrics, liveness/readiness probes. Kept in one module so the HTTP wiring in
//! `bin/server.rs` is three lines (`init_logging`, `.merge(routes())`, `.layer(from_fn(track))`).
//!
//! Nothing here can change a verdict. The middleware only *reads* the decision the
//! handler already produced (it re-parses the small JSON response body of
//! `POST /v1/actions`) to count verdicts and reasons. It never logs request bodies,
//! invoice fields or credentials: only method, route template, status, latency,
//! verdict and rule names.
//!
//! Metric names (all prefixed `governor_`):
//! - `decisions_total{verdict}`
//! - `decision_reasons_total{verdict,reason}` (one count per reason rule on a decision, and
//!   `signal_<kind>` per escalate-only checker signal;
//!   `reason="none"` for a clean ALLOW)
//! - `parse_failures_total{kind}` (`request_unparseable`, `checker_unparseable`)
//! - `http_requests_total{method,route,status}`
//! - `http_request_duration_seconds{route}` histogram
//! - `ready` gauge, `build_info{version}` gauge
use axum::{
    body::{to_bytes, Body},
    extract::{MatchedPath, Request},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use subtle::ConstantTimeEq;

/// Upper bounds (seconds) of the latency histogram buckets.
pub const BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
];
/// Responses larger than this are passed through without verdict accounting.
const MAX_INSPECT_BYTES: usize = 256 * 1024;

#[derive(Default)]
struct Histogram {
    counts: Vec<u64>, // one per bucket, non-cumulative
    sum: f64,
    count: u64,
}

impl Histogram {
    fn observe(&mut self, secs: f64) {
        if self.counts.is_empty() {
            self.counts = vec![0; BUCKETS.len()];
        }
        if let Some(i) = BUCKETS.iter().position(|b| secs <= *b) {
            self.counts[i] += 1;
        }
        self.sum += secs;
        self.count += 1;
    }
}

#[derive(Default)]
struct Inner {
    decisions: BTreeMap<String, u64>,
    reasons: BTreeMap<(String, String), u64>,
    parse_failures: BTreeMap<String, u64>,
    requests: BTreeMap<(String, String, u16), u64>,
    latency: BTreeMap<String, Histogram>,
}

/// Process-wide metrics registry.
pub struct Metrics {
    inner: Mutex<Inner>,
    ready: AtomicBool,
}

static METRICS: OnceLock<Metrics> = OnceLock::new();

/// The global registry (created on first use).
pub fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| Metrics {
        inner: Mutex::new(Inner::default()),
        ready: AtomicBool::new(false),
    })
}

/// Mark the server ready (call after the listener is bound and state is loaded).
pub fn set_ready(ready: bool) {
    metrics().ready.store(ready, Ordering::SeqCst);
}

impl Metrics {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock must not take metrics (or the server) down.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Count one decision from its JSON form (`verdict`, `reasons[].rule`, `signals[].kind`).
    pub fn record_decision_json(&self, d: &serde_json::Value) {
        let Some(verdict) = d.get("verdict").and_then(|v| v.as_str()) else {
            return;
        };
        let names = |field: &str, key: &str, prefix: &str| -> Vec<String> {
            d.get(field)
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r.get(key).and_then(|x| x.as_str()))
                        .map(|x| format!("{prefix}{}", sanitize(x)))
                        .collect()
                })
                .unwrap_or_default()
        };
        // Deterministic reason rules as-is; escalate-only model signals as `signal_<kind>`.
        let mut rules = names("reasons", "rule", "");
        rules.extend(names("signals", "kind", "signal_"));
        rules.sort();
        rules.dedup();
        let mut m = self.lock();
        *m.decisions.entry(sanitize(verdict)).or_default() += 1;
        if rules.is_empty() {
            *m.reasons
                .entry((sanitize(verdict), "none".into()))
                .or_default() += 1;
        }
        for r in rules {
            let base = r.strip_prefix("signal_").unwrap_or(&r);
            if base.ends_with("_unparseable") {
                *m.parse_failures.entry(base.to_string()).or_default() += 1;
            }
            *m.reasons.entry((sanitize(verdict), r)).or_default() += 1;
        }
    }

    fn record_request(&self, method: &str, route: &str, status: u16, secs: f64) {
        let mut m = self.lock();
        *m.requests
            .entry((method.to_string(), route.to_string(), status))
            .or_default() += 1;
        m.latency
            .entry(route.to_string())
            .or_default()
            .observe(secs);
    }

    /// Prometheus text exposition format 0.0.4.
    pub fn render(&self) -> String {
        let m = self.lock();
        let mut o = String::new();
        let _ = writeln!(
            o,
            "# HELP governor_build_info Build information.\n# TYPE governor_build_info gauge"
        );
        let _ = writeln!(
            o,
            "governor_build_info{{version=\"{}\"}} 1",
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(o, "# HELP governor_ready 1 when the server is ready to take decisions.\n# TYPE governor_ready gauge");
        let _ = writeln!(
            o,
            "governor_ready {}",
            u8::from(self.ready.load(Ordering::SeqCst))
        );
        let _ = writeln!(o, "# HELP governor_decisions_total Decisions returned by POST /v1/actions, by final verdict.");
        let _ = writeln!(o, "# TYPE governor_decisions_total counter");
        for v in ["allow", "review", "block"] {
            let n = m.decisions.get(v).copied().unwrap_or(0);
            let _ = writeln!(o, "governor_decisions_total{{verdict=\"{v}\"}} {n}");
        }
        for (v, n) in m
            .decisions
            .iter()
            .filter(|(v, _)| !["allow", "review", "block"].contains(&v.as_str()))
        {
            let _ = writeln!(o, "governor_decisions_total{{verdict=\"{v}\"}} {n}");
        }
        let _ = writeln!(
            o,
            "# HELP governor_decision_reasons_total Reason rules on decisions, by final verdict."
        );
        let _ = writeln!(o, "# TYPE governor_decision_reasons_total counter");
        for ((v, r), n) in &m.reasons {
            let _ = writeln!(
                o,
                "governor_decision_reasons_total{{verdict=\"{v}\",reason=\"{r}\"}} {n}"
            );
        }
        let _ = writeln!(o, "# HELP governor_parse_failures_total Requests or checker outputs that failed strict parsing (fail closed to REVIEW).");
        let _ = writeln!(o, "# TYPE governor_parse_failures_total counter");
        for k in ["request_unparseable", "checker_unparseable"] {
            let n = m.parse_failures.get(k).copied().unwrap_or(0);
            let _ = writeln!(o, "governor_parse_failures_total{{kind=\"{k}\"}} {n}");
        }
        let _ = writeln!(o, "# HELP governor_http_requests_total HTTP requests by method, route template and status.");
        let _ = writeln!(o, "# TYPE governor_http_requests_total counter");
        for ((meth, route, status), n) in &m.requests {
            let _ = writeln!(o, "governor_http_requests_total{{method=\"{meth}\",route=\"{route}\",status=\"{status}\"}} {n}");
        }
        let _ = writeln!(o, "# HELP governor_http_request_duration_seconds Server-side request latency by route template.");
        let _ = writeln!(o, "# TYPE governor_http_request_duration_seconds histogram");
        for (route, h) in &m.latency {
            let mut cum = 0;
            for (i, b) in BUCKETS.iter().enumerate() {
                cum += h.counts.get(i).copied().unwrap_or(0);
                let _ = writeln!(o, "governor_http_request_duration_seconds_bucket{{route=\"{route}\",le=\"{b}\"}} {cum}");
            }
            let _ = writeln!(
                o,
                "governor_http_request_duration_seconds_bucket{{route=\"{route}\",le=\"+Inf\"}} {}",
                h.count
            );
            let _ = writeln!(
                o,
                "governor_http_request_duration_seconds_sum{{route=\"{route}\"}} {}",
                h.sum
            );
            let _ = writeln!(
                o,
                "governor_http_request_duration_seconds_count{{route=\"{route}\"}} {}",
                h.count
            );
        }
        o
    }
}

/// Label values come from our own rule names, but never trust them into the exposition format.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}

/// Structured JSON logs to stderr. `RUST_LOG` sets the filter (default `info`);
/// `GOVERNOR_LOG_FORMAT=pretty` switches to human-readable lines for local work.
/// Safe to call more than once (later calls are no-ops).
pub fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let pretty = std::env::var("GOVERNOR_LOG_FORMAT").as_deref() == Ok("pretty");
    let builder = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false);
    let _ = if pretty {
        builder.try_init()
    } else {
        builder
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .try_init()
    };
}

/// `/healthz` (process up), `/readyz` (ready flag set), `/metrics` (Prometheus text).
/// `/metrics` requires `Authorization: Bearer $GOVERNOR_METRICS_TOKEN` when that variable is set.
pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
}

async fn healthz() -> impl IntoResponse {
    axum::Json(serde_json::json!({"status": "ok"}))
}

async fn readyz() -> impl IntoResponse {
    if metrics().ready.load(Ordering::SeqCst) {
        (
            StatusCode::OK,
            axum::Json(serde_json::json!({"status": "ready"})),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({"status": "starting"})),
        )
    }
}

async fn metrics_handler(h: HeaderMap) -> Response {
    if let Ok(tok) = std::env::var("GOVERNOR_METRICS_TOKEN") {
        let got = h
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        let ok: bool = got.as_bytes().ct_eq(tok.as_bytes()).into();
        if tok.is_empty() || !ok {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        metrics().render(),
    )
        .into_response()
}

/// Access log + latency + verdict accounting. Wire with
/// `.layer(axum::middleware::from_fn(observability::track))` on the whole router.
pub async fn track(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().as_str().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let is_action = route == "/v1/actions" && method == "POST";
    let resp = next.run(req).await;
    let (resp, verdict) = if is_action && resp.status().is_success() {
        let (parts, body) = resp.into_parts();
        match to_bytes(body, MAX_INSPECT_BYTES).await {
            Ok(bytes) => {
                let verdict = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|d| {
                        metrics().record_decision_json(&d);
                        d.get("verdict").and_then(|v| v.as_str()).map(sanitize)
                    });
                (Response::from_parts(parts, Body::from(bytes)), verdict)
            }
            // Body too large or unreadable: fail closed for the client (it gets a 500, never a
            // half-read decision), and the request is still counted below.
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR.into_response(), None),
        }
    } else {
        (resp, None)
    };

    let secs = start.elapsed().as_secs_f64();
    let status = resp.status().as_u16();
    metrics().record_request(&method, &route, status, secs);
    tracing::info!(
        event = "http_request",
        method = %method,
        route = %route,
        status = status,
        latency_ms = secs * 1000.0,
        verdict = verdict.as_deref().unwrap_or(""),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_verdicts_reasons_and_parse_failures() {
        let m = Metrics {
            inner: Mutex::new(Inner::default()),
            ready: AtomicBool::new(true),
        };
        m.record_decision_json(&serde_json::json!({"verdict": "allow", "reasons": []}));
        m.record_decision_json(&serde_json::json!({"verdict": "review", "reasons": [],
            "signals": [{"kind": "checker_unparseable", "severity": "review"}]}));
        m.record_decision_json(&serde_json::json!({"verdict": "review", "reasons": [
            {"rule": "request_unparseable"}, {"rule": "request_unparseable"}]}));
        m.record_decision_json(&serde_json::json!({"verdict": "block", "reasons": [{"rule": "bank_details_differ_from_master"}]}));
        m.record_request("POST", "/v1/actions", 200, 0.002);
        let t = m.render();
        assert!(
            t.contains("governor_decisions_total{verdict=\"allow\"} 1"),
            "{t}"
        );
        assert!(t.contains("governor_decisions_total{verdict=\"review\"} 2"));
        assert!(t.contains("governor_decision_reasons_total{verdict=\"allow\",reason=\"none\"} 1"));
        assert!(t.contains("governor_decision_reasons_total{verdict=\"block\",reason=\"bank_details_differ_from_master\"} 1"));
        assert!(t.contains("governor_parse_failures_total{kind=\"request_unparseable\"} 1"));
        assert!(t.contains("governor_parse_failures_total{kind=\"checker_unparseable\"} 1"));
        assert!(t.contains(
            "governor_decision_reasons_total{verdict=\"review\",reason=\"signal_checker_unparseable\"} 1"
        ));
        assert!(t.contains(
            "governor_http_request_duration_seconds_bucket{route=\"/v1/actions\",le=\"0.0025\"} 1"
        ));
        assert!(t.contains(
            "governor_http_request_duration_seconds_bucket{route=\"/v1/actions\",le=\"0.001\"} 0"
        ));
        assert!(t.contains("governor_ready 1"));
    }

    #[test]
    fn label_values_are_sanitized() {
        assert_eq!(sanitize("a\"b}\nc"), "abc");
    }
}
