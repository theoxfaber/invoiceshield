//! Phase 4 (security) through the real `governor-server` binary: role enforcement per route,
//! maker-checker over HTTP, the bank-change workflow with two approvers and cooling-off on the
//! eval clock, 429 rate limiting, TLS start, and secrets from *_FILE.
mod common;
use common::*;
use invoiceshield_governor::auth::hash_key_hex;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const AGENT: &str = "agent-key-0123456789abcdef";
const OPER: &str = "operator-key-0123456789abc";
const LEAD: &str = "lead-key-0123456789abcdefg"; // operator + approver
const RAVI: &str = "ravi-key-0123456789abcdefg";
const MEENA: &str = "meena-key-0123456789abcdef";
const NEW_ACCT: &str = "88880000000101";
const NEW_IFSC: &str = "CNRB0001234";

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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn identities_file(dir: &Path) -> PathBuf {
    let j = serde_json::json!([
        {"name": "ap-agent", "roles": ["agent"], "key_sha256": hash_key_hex(AGENT)},
        {"name": "anita", "roles": ["operator"], "key_sha256": hash_key_hex(OPER)},
        {"name": "lead", "roles": ["operator", "approver"], "key_sha256": hash_key_hex(LEAD)},
        {"name": "ravi", "roles": ["approver"], "key_sha256": hash_key_hex(RAVI)},
        {"name": "meena", "roles": ["approver"], "key_sha256": hash_key_hex(MEENA)},
    ]);
    let p = dir.join("identities.json");
    std::fs::write(&p, j.to_string()).unwrap();
    p
}

fn base_cmd(port: u16, extra: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_governor-server"));
    cmd.env_clear()
        .env("GOVERNOR_MASTER", master_path())
        .env("GOVERNOR_PORT", port.to_string())
        .env("GOVERNOR_DB", ":memory:")
        .env("GOVERNOR_EVAL_CLOCK", "1")
        .env("GOVERNOR_DEV_UNSIGNED_EVIDENCE", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd
}

/// Run the server to exit (it must refuse to start); returns (exit code, stderr).
fn start_fails(extra: &[(&str, &str)]) -> (i32, String) {
    let out = base_cmd(free_port(), extra).output().expect("spawn");
    (
        out.status.code().unwrap_or(-1),
        format!("{:?} {}", out.status, String::from_utf8_lossy(&out.stderr)),
    )
}

fn start(extra: &[(&str, &str)]) -> Server {
    for _attempt in 0..5 {
        let port = free_port();
        let mut cmd = base_cmd(port, extra);
        cmd.stderr(Stdio::null());
        let mut s = Server {
            child: cmd.spawn().expect("spawn governor-server"),
            port,
        };
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
    panic!("server did not start after 5 attempts");
}

fn raw_request(
    method: &str,
    path: &str,
    key: Option<&str>,
    body: &str,
    headers: &[(&str, &str)],
) -> String {
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if let Some(k) = key {
        req.push_str(&format!("Authorization: Bearer {k}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    req
}

fn parse(text: &str) -> (u16, serde_json::Value, String) {
    let code = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let b = text.split("\r\n\r\n").nth(1).unwrap_or("");
    (
        code,
        serde_json::from_str(b).unwrap_or(serde_json::Value::Null),
        text.to_string(),
    )
}

async fn http(
    port: u16,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: &str,
    headers: &[(&str, &str)],
) -> (u16, serde_json::Value, String) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    s.write_all(raw_request(method, path, key, body, headers).as_bytes())
        .await
        .unwrap();
    let mut out = vec![];
    s.read_to_end(&mut out).await.unwrap();
    parse(&String::from_utf8_lossy(&out))
}

fn printing(inv: &str, rupees: i64, acct: &str, ifsc: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&body(inv, rupees)).unwrap();
    v["evidence"]["printed_bank_account"] = acct.into();
    v["evidence"]["printed_ifsc"] = ifsc.into();
    v.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn roles_are_enforced_per_route() {
    let dir = tmpdir("sec-roles");
    let ids = identities_file(&dir);
    let s = start(&[("GOVERNOR_IDENTITIES_FILE", ids.to_str().unwrap())]);
    let p = s.port;
    let b = body("INV-ROLE-1", 1_000);
    // only the agent proposes
    let (c, v, r) = http(p, "POST", "/v1/actions", Some(AGENT), &b, &[]).await;
    assert_eq!((c, v["verdict"].as_str()), (200, Some("allow")), "{r}");
    for k in [OPER, LEAD, RAVI] {
        let (c, v, _) = http(
            p,
            "POST",
            "/v1/actions",
            Some(k),
            &body("INV-ROLE-2", 1_000),
            &[],
        )
        .await;
        assert_eq!((c, v["error"].as_str()), (403, Some("forbidden")));
    }
    // no key / wrong key
    assert_eq!(http(p, "POST", "/v1/actions", None, &b, &[]).await.0, 401);
    assert_eq!(
        http(
            p,
            "POST",
            "/v1/actions",
            Some("not-a-real-key-000000000"),
            &b,
            &[]
        )
        .await
        .0,
        401
    );
    // the agent can never approve or touch the vendor master
    let review = http(
        p,
        "POST",
        "/v1/actions",
        Some(AGENT),
        &body("INV-ROLE-R", 80_000),
        &[],
    )
    .await;
    assert_eq!(review.1["verdict"], "review", "{}", review.2);
    let did = review.1["decision_id"].as_str().unwrap().to_string();
    let (c, _, _) = http(
        p,
        "POST",
        &format!("/v1/decisions/{did}/resolve"),
        Some(AGENT),
        r#"{"approve":true}"#,
        &[],
    )
    .await;
    assert_eq!(c, 403);
    let mc = format!(
        r#"{{"vendor_id":"V001","new_account":"{NEW_ACCT}","new_ifsc":"{NEW_IFSC}","new_bank_name":"Canara Bank","new_beneficiary_id":"BENE_V001_2"}}"#
    );
    assert_eq!(
        http(p, "POST", "/v1/master-changes", Some(AGENT), &mc, &[])
            .await
            .0,
        403
    );
    // an approver-only identity cannot enter a change (it is not a maker)
    assert_eq!(
        http(p, "POST", "/v1/master-changes", Some(RAVI), &mc, &[])
            .await
            .0,
        403
    );
    // an operator-only identity cannot resolve a REVIEW
    assert_eq!(
        http(
            p,
            "POST",
            &format!("/v1/decisions/{did}/resolve"),
            Some(OPER),
            r#"{"approve":true}"#,
            &[]
        )
        .await
        .0,
        403
    );
    // the body cannot name someone else as the approver
    let (c, v, _) = http(
        p,
        "POST",
        &format!("/v1/decisions/{did}/resolve"),
        Some(RAVI),
        r#"{"approve":true,"approver":"meena"}"#,
        &[],
    )
    .await;
    assert_eq!(
        (c, v["error"].as_str()),
        (400, Some("approver_must_match_authenticated_identity"))
    );
    let (c, v, r) = http(
        p,
        "POST",
        &format!("/v1/decisions/{did}/resolve"),
        Some(RAVI),
        r#"{"approve":false}"#,
        &[],
    )
    .await;
    assert_eq!((c, v["approver"].as_str()), (200, Some("ravi")), "{r}");
    // reads: any identity
    for k in [AGENT, OPER, RAVI] {
        assert_eq!(
            http(p, "GET", &format!("/v1/decisions/{did}"), Some(k), "", &[])
                .await
                .0,
            200
        );
        assert_eq!(
            http(p, "GET", "/v1/audit/verify", Some(k), "", &[]).await.0,
            200
        );
    }
    // health stays unauthenticated
    assert_eq!(http(p, "GET", "/health", None, "", &[]).await.0, 200);
}

#[test]
fn agent_identity_with_approver_role_refuses_to_start() {
    let dir = tmpdir("sec-combo");
    let p = dir.join("ids.json");
    std::fs::write(&p, serde_json::json!([{"name": "bot", "roles": ["agent", "approver"], "key_sha256": hash_key_hex(AGENT)}]).to_string()).unwrap();
    let (code, err) = start_fails(&[("GOVERNOR_IDENTITIES_FILE", p.to_str().unwrap())]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("agent role cannot be combined"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn bank_change_two_approvers_and_cooling_off_over_http() {
    let dir = tmpdir("sec-mc");
    let ids = identities_file(&dir);
    let phones = dir.join("phones.json");
    std::fs::write(&phones, r#"{"V001": "+91 265 000 0101"}"#).unwrap();
    let s = start(&[
        ("GOVERNOR_IDENTITIES_FILE", ids.to_str().unwrap()),
        ("GOVERNOR_VENDOR_PHONES", phones.to_str().unwrap()),
        ("GOVERNOR_MASTER_CHANGE_COOLING_OFF_SECS", "86400"),
    ]);
    let p = s.port;
    let t = |secs: i64| (1_800_000_000 + secs).to_string();
    let mc = format!(
        r#"{{"vendor_id":"V001","new_account":"{NEW_ACCT}","new_ifsc":"{NEW_IFSC}","new_bank_name":"Canara Bank","new_beneficiary_id":"BENE_V001_2"}}"#
    );
    let (c, v, r) = http(
        p,
        "POST",
        "/v1/master-changes",
        Some(LEAD),
        &mc,
        &[("x-eval-now", &t(0))],
    )
    .await;
    assert_eq!(
        (c, v["status"].as_str()),
        (200, Some("awaiting_callback")),
        "{r}"
    );
    let id = v["change_id"].as_str().unwrap().to_string();
    let cb_good = format!(
        r#"{{"number_dialled":"+912650000101","confirmed_account":"{NEW_ACCT}","confirmed_ifsc":"{NEW_IFSC}"}}"#
    );
    let cb_wrong = format!(
        r#"{{"number_dialled":"+91 98765 43210","confirmed_account":"{NEW_ACCT}","confirmed_ifsc":"{NEW_IFSC}"}}"#
    );
    // maker cannot do the call-back; wrong number refused
    let (c, v, _) = http(
        p,
        "POST",
        &format!("/v1/master-changes/{id}/callback"),
        Some(LEAD),
        &cb_good,
        &[("x-eval-now", &t(10))],
    )
    .await;
    assert_eq!(
        (c, v["error"].as_str()),
        (403, Some("maker_cannot_check_own_change"))
    );
    let (c, v, _) = http(
        p,
        "POST",
        &format!("/v1/master-changes/{id}/callback"),
        Some(OPER),
        &cb_wrong,
        &[("x-eval-now", &t(20))],
    )
    .await;
    assert_eq!(
        (c, v["error"].as_str()),
        (422, Some("callback_not_to_number_on_file"))
    );
    let (c, v, r) = http(
        p,
        "POST",
        &format!("/v1/master-changes/{id}/callback"),
        Some(OPER),
        &cb_good,
        &[("x-eval-now", &t(30))],
    )
    .await;
    assert_eq!(
        (c, v["status"].as_str()),
        (200, Some("awaiting_approvals")),
        "{r}"
    );
    // the maker (who also holds the approver role) cannot approve its own change
    let (c, v, _) = http(
        p,
        "POST",
        &format!("/v1/master-changes/{id}/approve"),
        Some(LEAD),
        "",
        &[("x-eval-now", &t(40))],
    )
    .await;
    assert_eq!(
        (c, v["error"].as_str()),
        (403, Some("maker_cannot_check_own_change"))
    );
    // operator-only identity cannot approve
    assert_eq!(
        http(
            p,
            "POST",
            &format!("/v1/master-changes/{id}/approve"),
            Some(OPER),
            "",
            &[("x-eval-now", &t(41))]
        )
        .await
        .0,
        403
    );
    let (c, v, _) = http(
        p,
        "POST",
        &format!("/v1/master-changes/{id}/approve"),
        Some(RAVI),
        "",
        &[("x-eval-now", &t(50))],
    )
    .await;
    assert_eq!((c, v["status"].as_str()), (200, Some("awaiting_approvals")));
    let (c, v, _) = http(
        p,
        "POST",
        &format!("/v1/master-changes/{id}/approve"),
        Some(RAVI),
        "",
        &[("x-eval-now", &t(55))],
    )
    .await;
    assert_eq!(
        (c, v["error"].as_str()),
        (409, Some("already_approved_by_this_identity"))
    );
    let (c, v, _) = http(
        p,
        "POST",
        &format!("/v1/master-changes/{id}/approve"),
        Some(MEENA),
        "",
        &[("x-eval-now", &t(60))],
    )
    .await;
    assert_eq!(
        (c, v["status"].as_str(), v["effective_at"].as_i64()),
        (200, Some("cooling_off"), Some(1_800_000_060 + 86_400))
    );
    // one second before the end of the cooling-off: new details are a tamper REVIEW
    let (_, v, r) = http(
        p,
        "POST",
        "/v1/actions",
        Some(AGENT),
        &printing("INV-HTTP-1", 1_000, NEW_ACCT, NEW_IFSC),
        &[("x-eval-now", &t(60 + 86_399))],
    )
    .await;
    assert_eq!(v["verdict"], "review", "{r}");
    assert_eq!(v["tamper_alert"], true);
    // at the end of it: ALLOW to the new beneficiary
    let (_, v, r) = http(
        p,
        "POST",
        "/v1/actions",
        Some(AGENT),
        &printing("INV-HTTP-2", 1_000, NEW_ACCT, NEW_IFSC),
        &[("x-eval-now", &t(60 + 86_400))],
    )
    .await;
    assert_eq!(v["verdict"], "allow", "{r}");
    assert_eq!(v["payee"]["beneficiary_id"], "BENE_V001_2");
    let (c, v, _) = http(
        p,
        "GET",
        &format!("/v1/master-changes/{id}"),
        Some(RAVI),
        "",
        &[],
    )
    .await;
    assert_eq!((c, v["status"].as_str()), (200, Some("applied")));
    assert_eq!(
        http(
            p,
            "GET",
            &format!("/v1/master-changes/{id}"),
            Some(AGENT),
            "",
            &[]
        )
        .await
        .0,
        403,
        "the agent cannot read the workflow"
    );
    let (_, v, _) = http(p, "GET", "/v1/audit/verify", Some(RAVI), "", &[]).await;
    assert!(v["first_broken"].is_null(), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limits_return_429_per_identity_and_after_failed_auth() {
    let dir = tmpdir("sec-429");
    let ids = identities_file(&dir);
    let s = start(&[
        ("GOVERNOR_IDENTITIES_FILE", ids.to_str().unwrap()),
        ("GOVERNOR_RATE_LIMIT_RPS", "0.5"),
        ("GOVERNOR_RATE_LIMIT_BURST", "3"),
    ]);
    let p = s.port;
    for i in 0..3 {
        assert_eq!(
            http(p, "GET", "/v1/audit/verify", Some(RAVI), "", &[])
                .await
                .0,
            200,
            "request {i}"
        );
    }
    let (c, v, r) = http(p, "GET", "/v1/audit/verify", Some(RAVI), "", &[]).await;
    assert_eq!(
        (c, v["error"].as_str(), v["scope"].as_str()),
        (429, Some("rate_limited"), Some("identity")),
        "{r}"
    );
    assert!(r.to_ascii_lowercase().contains("retry-after: "), "{r}");
    // another identity has its own bucket
    assert_eq!(
        http(p, "GET", "/v1/audit/verify", Some(MEENA), "", &[])
            .await
            .0,
        200
    );
    // a payment request over the limit is refused before evaluation: nothing recorded
    for i in 0..3 {
        http(
            p,
            "POST",
            "/v1/actions",
            Some(AGENT),
            &body(&format!("INV-RL-{i}"), 1_000),
            &[],
        )
        .await;
    }
    let (c, _, _) = http(
        p,
        "POST",
        "/v1/actions",
        Some(AGENT),
        &body("INV-RL-X", 1_000),
        &[],
    )
    .await;
    assert_eq!(c, 429);
    drop(s);

    let s = start(&[
        ("GOVERNOR_IDENTITIES_FILE", ids.to_str().unwrap()),
        ("GOVERNOR_AUTH_FAIL_PER_MIN", "1"),
        ("GOVERNOR_AUTH_FAIL_BURST", "2"),
    ]);
    let p = s.port;
    assert_eq!(
        http(
            p,
            "GET",
            "/v1/audit/verify",
            Some("guess-0000000000000000"),
            "",
            &[]
        )
        .await
        .0,
        401
    );
    assert_eq!(
        http(
            p,
            "GET",
            "/v1/audit/verify",
            Some("guess-0000000000000001"),
            "",
            &[]
        )
        .await
        .0,
        401
    );
    // the IP has used its failed-auth budget: even a valid key is refused for now
    let (c, v, _) = http(p, "GET", "/v1/audit/verify", Some(RAVI), "", &[]).await;
    assert_eq!((c, v["scope"].as_str()), (429, Some("auth_failures")));
}

fn write_cert(dir: &Path) -> (PathBuf, PathBuf, Vec<u8>) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    std::fs::write(&cert, ck.cert.pem()).unwrap();
    std::fs::write(&key, ck.key_pair.serialize_pem()).unwrap();
    (cert, key, ck.cert.der().to_vec())
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_server_starts_and_serves_https_only() {
    use rustls::pki_types::{CertificateDer, ServerName};
    let dir = tmpdir("sec-tls");
    let (cert, key, der) = write_cert(&dir);
    let keyfile = dir.join("agent.key");
    std::fs::write(&keyfile, format!("{AGENT}\n")).unwrap();
    let s = start(&[
        ("GOVERNOR_API_KEY_FILE", keyfile.to_str().unwrap()),
        ("GOVERNOR_TLS_CERT_FILE", cert.to_str().unwrap()),
        ("GOVERNOR_TLS_KEY_FILE", key.to_str().unwrap()),
    ]);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(der)).unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
    let https =
        |method: &'static str, path: &'static str, key: Option<&'static str>, body: String| {
            let connector = connector.clone();
            let port = s.port;
            async move {
                let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                let mut tls = connector
                    .connect(ServerName::try_from("localhost").unwrap(), tcp)
                    .await
                    .expect("TLS handshake");
                tls.write_all(raw_request(method, path, key, &body, &[]).as_bytes())
                    .await
                    .unwrap();
                let mut out = vec![];
                let _ = tls.read_to_end(&mut out).await; // server may close without close_notify
                parse(&String::from_utf8_lossy(&out))
            }
        };
    let (c, _, r) = https("GET", "/health", None, String::new()).await;
    assert_eq!(c, 200, "{r}");
    let (c, v, r) = https("POST", "/v1/actions", Some(AGENT), body("INV-TLS-1", 1_000)).await;
    assert_eq!((c, v["verdict"].as_str()), (200, Some("allow")), "{r}");
    assert_eq!(
        https(
            "POST",
            "/v1/actions",
            Some("wrong-key-000000000000"),
            body("INV-TLS-2", 1_000)
        )
        .await
        .0,
        401
    );
    // plain HTTP on the TLS port gets no HTTP response
    let mut plain = tokio::net::TcpStream::connect(("127.0.0.1", s.port))
        .await
        .unwrap();
    plain
        .write_all(raw_request("GET", "/health", None, "", &[]).as_bytes())
        .await
        .unwrap();
    let mut out = vec![];
    let _ = tokio::time::timeout(Duration::from_secs(15), plain.read_to_end(&mut out)).await;
    assert!(
        !String::from_utf8_lossy(&out).starts_with("HTTP/1.1 200"),
        "plain HTTP must not be served"
    );
}

#[test]
fn bad_tls_or_secret_config_refuses_to_start() {
    let dir = tmpdir("sec-badcfg");
    let (cert, _key, _) = write_cert(&dir);
    let junk = dir.join("junk.pem");
    std::fs::write(&junk, "not a key").unwrap();
    let (code, err) = start_fails(&[
        ("GOVERNOR_API_KEY", AGENT),
        ("GOVERNOR_TLS_CERT_FILE", cert.to_str().unwrap()),
        ("GOVERNOR_TLS_KEY_FILE", junk.to_str().unwrap()),
    ]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("GOVERNOR_TLS_KEY_FILE"), "{err}");
    let (code, err) = start_fails(&[
        ("GOVERNOR_API_KEY", AGENT),
        ("GOVERNOR_TLS_CERT_FILE", cert.to_str().unwrap()),
    ]);
    assert_eq!(code, 2, "{err}");
    let kf = dir.join("k");
    std::fs::write(&kf, AGENT).unwrap();
    let (code, err) = start_fails(&[
        ("GOVERNOR_API_KEY", AGENT),
        ("GOVERNOR_API_KEY_FILE", kf.to_str().unwrap()),
    ]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("not both"), "{err}");
    let (code, err) = start_fails(&[]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("no agent identity"), "{err}");
}
