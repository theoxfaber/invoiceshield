//! Phase 7 (product): the REVIEW queue (GET /v1/reviews) and the approver page (/v1/ui/review)
//! through the real `governor-server` binary. The page resolves through the existing
//! POST /v1/decisions/:id/resolve, so maker-checker is the server's, not the page's.
mod common;
use common::*;
use invoiceshield_governor::auth::hash_key_hex;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const AGENT: &str = "agent-key-0123456789abcdef";
const OPER: &str = "operator-key-0123456789abc";
const MEENA: &str = "meena-key-0123456789abcdef";
const RAVI: &str = "ravi-key-0123456789abcdefg";

struct Server {
    child: Child,
    port: u16,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Server {
    let dir = tmpdir("review-ui");
    let ids = serde_json::json!([
        {"name": "ap-agent", "roles": ["agent"], "key_sha256": hash_key_hex(AGENT)},
        {"name": "anita", "roles": ["operator"], "key_sha256": hash_key_hex(OPER)},
        {"name": "meena", "roles": ["approver"], "key_sha256": hash_key_hex(MEENA)},
        {"name": "ravi", "roles": ["approver"], "key_sha256": hash_key_hex(RAVI)},
    ]);
    let p = dir.join("identities.json");
    std::fs::write(&p, ids.to_string()).unwrap();
    for _ in 0..5 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new(env!("CARGO_BIN_EXE_governor-server"))
            .env_clear()
            .env("GOVERNOR_MASTER", master_path())
            .env("GOVERNOR_PORT", port.to_string())
            .env("GOVERNOR_DB", ":memory:")
            .env("GOVERNOR_EVAL_CLOCK", "1")
            .env("GOVERNOR_DEV_UNSIGNED_EVIDENCE", "1")
            .env("GOVERNOR_IDENTITIES_FILE", &p)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn governor-server");
        let mut s = Server { child, port };
        let t0 = Instant::now();
        loop {
            if let Ok(Some(_)) = s.child.try_wait() {
                break;
            }
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return s;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    panic!("server did not start");
}

async fn http(
    port: u16,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: &str,
) -> (u16, String, String) {
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if let Some(k) = key {
        req.push_str(&format!("Authorization: Bearer {k}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = vec![];
    s.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out).to_string();
    let code = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let (head, b) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (code, head.to_ascii_lowercase(), b.to_string())
}

fn json(b: &str) -> serde_json::Value {
    serde_json::from_str(b).unwrap_or(serde_json::Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn approver_page_is_static_with_strict_csp_and_no_secrets() {
    let s = start();
    let (c, head, body) = http(s.port, "GET", "/v1/ui/review", None, "").await;
    assert_eq!(c, 200);
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(
        head.contains("content-security-policy: default-src 'none'; script-src 'self'"),
        "{head}"
    );
    assert!(
        head.contains("frame-ancestors 'none'") && head.contains("x-content-type-options: nosniff"),
        "{head}"
    );
    assert!(body.contains("/v1/ui/review.js") && body.contains("REVIEW queue"));
    assert!(!body.contains("<script>"), "no inline script (CSP)");
    let (c, head, js) = http(s.port, "GET", "/v1/ui/review.js", None, "").await;
    assert_eq!(c, 200);
    assert!(head.contains("text/javascript"));
    assert!(
        js.contains("/v1/reviews") && js.contains("/resolve"),
        "page uses the existing API"
    );
    assert!(
        !js.contains("innerHTML") && !js.contains("localStorage"),
        "no HTML injection sink, key not persisted"
    );
    assert_eq!(
        http(s.port, "GET", "/v1/ui/review.css", None, "").await.0,
        200
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn review_queue_roles_evidence_and_maker_checker() {
    let s = start();
    let p = s.port;
    // queue needs auth and the approver or operator role
    assert_eq!(http(p, "GET", "/v1/reviews", None, "").await.0, 401);
    assert_eq!(http(p, "GET", "/v1/reviews", Some(AGENT), "").await.0, 403);
    let (c, _, b) = http(p, "GET", "/v1/reviews", Some(RAVI), "").await;
    assert_eq!((c, json(&b)["count"].as_u64()), (200, Some(0)), "{b}");

    // an ALLOW never enters the queue; two REVIEWs do
    let (_, _, b) = http(
        p,
        "POST",
        "/v1/actions",
        Some(AGENT),
        &body("INV-UI-A", 1_000),
    )
    .await;
    assert_eq!(json(&b)["verdict"], "allow");
    let (_, _, b) = http(
        p,
        "POST",
        "/v1/actions",
        Some(AGENT),
        &body("INV-UI-R1", 80_000),
    )
    .await;
    let r1 = json(&b);
    assert_eq!(r1["verdict"], "review", "{b}");
    let mut b2: serde_json::Value = serde_json::from_str(&body("INV-UI-R2", 50_000)).unwrap();
    b2["proposal"]["po_ref"] = "PO-2026-0102".into();
    let (_, _, b) = http(p, "POST", "/v1/actions", Some(AGENT), &b2.to_string()).await;
    let r2 = json(&b);
    assert_eq!(r2["verdict"], "review", "{b}");
    let (id1, id2) = (
        r1["decision_id"].as_str().unwrap().to_string(),
        r2["decision_id"].as_str().unwrap().to_string(),
    );

    let (c, _, b) = http(p, "GET", "/v1/reviews", Some(OPER), "").await;
    assert_eq!(c, 200, "operators may look at the queue");
    let q = json(&b);
    assert_eq!(q["identity"], "anita");
    assert_eq!(q["count"], 2, "{b}");
    let items = q["reviews"].as_array().unwrap();
    assert_eq!(items[0]["decision_id"], id1.as_str());
    assert_eq!(items[0]["proposed_by"], "ap-agent");
    assert_eq!(
        (
            items[0]["vendor_id"].as_str(),
            items[0]["amount_minor"].as_i64(),
            items[0]["currency"].as_str()
        ),
        (Some("V001"), Some(8_000_000), Some("INR"))
    );
    let reasons = items[0]["decision"]["reasons"].as_array().unwrap();
    assert!(
        !reasons.is_empty()
            && reasons
                .iter()
                .all(|r| r["rule"].is_string() && r["detail"].is_string()),
        "reason codes + evidence detail: {b}"
    );
    assert_eq!(
        items[0]["decision"]["payee"]["beneficiary_id"], "BENE_V001",
        "payee comes from the vendor master"
    );
    assert_eq!(items[1]["proposed_by"], "ap-agent");
    let (_, _, b) = http(p, "GET", "/v1/reviews?limit=1", Some(RAVI), "").await;
    assert_eq!(json(&b)["count"], 1);
    assert_eq!(
        http(p, "GET", "/v1/reviews?bogus=1", Some(RAVI), "")
            .await
            .0,
        400,
        "unknown query params refused"
    );

    // the operator can see the queue but cannot resolve; the proposing agent can never resolve
    // (an agent identity cannot hold the approver role; the store-level self-approval refusal is
    // covered in src/review_queue.rs and tests/security_process.rs)
    assert_eq!(
        http(
            p,
            "POST",
            &format!("/v1/decisions/{id1}/resolve"),
            Some(OPER),
            r#"{"approve":true}"#
        )
        .await
        .0,
        403
    );
    let (c, _, b) = http(
        p,
        "POST",
        &format!("/v1/decisions/{id1}/resolve"),
        Some(AGENT),
        r#"{"approve":true}"#,
    )
    .await;
    assert_eq!(
        (c, json(&b)["error"].as_str()),
        (403, Some("forbidden")),
        "{b}"
    );
    // the page's body: {"approve": bool} only; the recorded approver is the authenticated identity
    let (c, _, b) = http(
        p,
        "POST",
        &format!("/v1/decisions/{id1}/resolve"),
        Some(MEENA),
        r#"{"approve":false}"#,
    )
    .await;
    assert_eq!(
        (c, json(&b)["approver"].as_str()),
        (200, Some("meena")),
        "{b}"
    );
    let (c, _, b) = http(
        p,
        "POST",
        &format!("/v1/decisions/{id2}/resolve"),
        Some(RAVI),
        r#"{"approve":true}"#,
    )
    .await;
    assert_eq!(
        (c, json(&b)["approver"].as_str()),
        (200, Some("ravi")),
        "{b}"
    );
    // resolved items leave the queue; resolving twice is a conflict
    let (_, _, b) = http(p, "GET", "/v1/reviews", Some(RAVI), "").await;
    assert_eq!(json(&b)["count"], 0, "{b}");
    assert_eq!(
        http(
            p,
            "POST",
            &format!("/v1/decisions/{id2}/resolve"),
            Some(RAVI),
            r#"{"approve":true}"#
        )
        .await
        .0,
        409
    );
}
