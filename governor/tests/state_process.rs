//! Phase 3: durable state through the real `governor-server` binary (separate OS processes).
//! Restart, a hard crash in the middle of a write transaction, two servers on one DB file,
//! idempotency over HTTP, periodic anchoring and the backup/restore CLI.
mod common;
use common::*;
use invoiceshield_governor::persist::{self, Store};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const KEY: &str = "test-api-key-0123456789";

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
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start(db: &Path, extra: &[(&str, &str)]) -> Server {
    let port = free_port();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_governor-server"));
    cmd.env_clear()
        .env("GOVERNOR_API_KEY", KEY)
        .env("GOVERNOR_MASTER", master_path())
        .env("GOVERNOR_PORT", port.to_string())
        .env("GOVERNOR_DB", db)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn governor-server");
    let s = Server { child, port };
    let t0 = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(20), "server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    s
}

/// Minimal HTTP/1.1 client (no extra dependency). Returns (status, headers+body text).
async fn http(port: u16, method: &str, path: &str, body: &str, headers: &[(&str, &str)]) -> Option<(u16, String)> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.ok()?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.ok()?;
    let mut out = vec![];
    s.read_to_end(&mut out).await.ok()?;
    let text = String::from_utf8_lossy(&out).to_string();
    let code = text.split_whitespace().nth(1)?.parse().ok()?;
    Some((code, text))
}

fn json_body(resp: &str) -> serde_json::Value {
    let b = resp.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(b).unwrap_or(serde_json::Value::Null)
}

fn verdict(resp: &str) -> String {
    json_body(resp)["verdict"].as_str().unwrap_or("").to_string()
}

fn ledger_rows(db: &Path) -> i64 {
    let c = Connection::open(db).unwrap();
    c.query_row("SELECT COUNT(*) FROM ledger", [], |r| r.get(0)).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn server_restart_keeps_state_and_blocks_duplicate() {
    let dir = tmpdir("p-restart");
    let db = dir.join("g.db");
    let id = {
        let s = start(&db, &[]);
        let (code, r) = http(s.port, "POST", "/v1/actions", &body("SOS/26-27/0231", 1_000), &[]).await.unwrap();
        assert_eq!((code, verdict(&r).as_str()), (200, "allow"), "{r}");
        json_body(&r)["decision_id"].as_str().unwrap().to_string()
    }; // process killed (SIGKILL), no graceful shutdown
    let s = start(&db, &[]);
    let (code, r) = http(s.port, "GET", &format!("/v1/decisions/{id}"), "", &[]).await.unwrap();
    assert_eq!(code, 200, "{r}");
    assert_eq!(verdict(&r), "allow");
    let (_, r) = http(s.port, "POST", "/v1/actions", &body("SOS/26-27/0231", 1_000), &[]).await.unwrap();
    assert_eq!(verdict(&r), "block", "{r}");
    assert!(r.contains("duplicate_invoice"));
    let (_, r) = http(s.port, "GET", "/v1/audit/verify", "", &[]).await.unwrap();
    let v = json_body(&r);
    assert_eq!(v["records"], 2);
    assert!(v["first_broken"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_mid_transaction_leaves_no_partial_ledger_row() {
    let dir = tmpdir("p-crash");
    let db = dir.join("g.db");
    {
        // The process aborts after the ledger INSERT and before COMMIT.
        let s = start(&db, &[("GOVERNOR_EVAL_CLOCK", "1"), ("GOVERNOR_TEST_FAULT", "abort_after_ledger_insert")]);
        let r = http(s.port, "POST", "/v1/actions", &body("INV-CRASH", 1_000), &[]).await;
        assert!(r.is_none() || r.as_ref().unwrap().0 != 200, "crashed request must not report success: {r:?}");
    }
    {
        let c = persist::connect(db.to_str().unwrap()).unwrap();
        for t in ["ledger", "decisions", "audit"] {
            let n: i64 = c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0, "partial row left in {t}");
        }
        let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");
    }
    let s = start(&db, &[]);
    let (_, r) = http(s.port, "POST", "/v1/actions", &body("INV-CRASH", 1_000), &[]).await.unwrap();
    assert_eq!(verdict(&r), "allow", "the retry pays exactly once: {r}");
    let (_, r) = http(s.port, "POST", "/v1/actions", &body("INV-CRASH", 1_000), &[]).await.unwrap();
    assert_eq!(verdict(&r), "block");
    assert_eq!(ledger_rows(&db), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn two_server_processes_one_db_concurrent_identical_requests_pay_once() {
    let dir = tmpdir("p-conc");
    let db = dir.join("g.db");
    drop(Store::open(db.to_str().unwrap(), master()).unwrap()); // create + migrate once
    let a = start(&db, &[]);
    let b = start(&db, &[]);
    let mut tasks = vec![];
    for i in 0..40u16 {
        let port = if i % 2 == 0 { a.port } else { b.port };
        tasks.push(tokio::spawn(async move { http(port, "POST", "/v1/actions", &body("INV-RACE-2P", 1_000), &[]).await }));
    }
    let (mut allow, mut block, mut other) = (0, 0, vec![]);
    for t in tasks {
        match t.await.unwrap() {
            Some((200, r)) if verdict(&r) == "allow" => allow += 1,
            Some((200, r)) if verdict(&r) == "block" => block += 1,
            x => other.push(format!("{x:?}")),
        }
    }
    assert_eq!(allow, 1, "exactly one PAY; others: {other:?}");
    assert_eq!(block, 39, "every other copy is a BLOCK, none errored: {other:?}");
    assert_eq!(ledger_rows(&db), 1);
    let c = persist::connect(db.to_str().unwrap()).unwrap();
    let recs = persist::load_audit(&c).unwrap();
    assert_eq!(recs.len(), 40);
    assert_eq!(invoiceshield_governor::audit::AuditLog { records: recs }.verify(), None, "one chain across both processes");
}

#[tokio::test(flavor = "multi_thread")]
async fn idempotency_key_over_http() {
    let dir = tmpdir("p-idem");
    let db = dir.join("g.db");
    let s = start(&db, &[]);
    let b1 = body("INV-IDEM", 1_000);
    let (c1, r1) = http(s.port, "POST", "/v1/actions", &b1, &[("Idempotency-Key", "pay-INV-IDEM-1")]).await.unwrap();
    assert_eq!((c1, verdict(&r1).as_str()), (200, "allow"));
    // Agent retries after a timeout: same decision back, not a duplicate BLOCK.
    let (c2, r2) = http(s.port, "POST", "/v1/actions", &b1, &[("Idempotency-Key", "pay-INV-IDEM-1")]).await.unwrap();
    assert_eq!(c2, 200);
    assert_eq!(json_body(&r2)["decision_id"], json_body(&r1)["decision_id"]);
    assert_eq!(verdict(&r2), "allow");
    assert!(r2.to_lowercase().contains("idempotent-replayed: true"));
    // Same key, different body → 409, nothing written.
    let (c3, r3) = http(s.port, "POST", "/v1/actions", &body("INV-IDEM", 2_000), &[("Idempotency-Key", "pay-INV-IDEM-1")]).await.unwrap();
    assert_eq!(c3, 409, "{r3}");
    // Malformed key → 400.
    let (c4, _) = http(s.port, "POST", "/v1/actions", &b1, &[("Idempotency-Key", "has space")]).await.unwrap();
    assert_eq!(c4, 400);
    assert_eq!(ledger_rows(&db), 1);
    // Survives a restart.
    drop(s);
    let s = start(&db, &[]);
    let (_, r5) = http(s.port, "POST", "/v1/actions", &b1, &[("Idempotency-Key", "pay-INV-IDEM-1")]).await.unwrap();
    assert_eq!(json_body(&r5)["decision_id"], json_body(&r1)["decision_id"]);
}

fn cli(args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_governor"));
    c.args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

#[tokio::test(flavor = "multi_thread")]
async fn periodic_anchor_backup_restore_and_verify_via_cli() {
    let dir = tmpdir("p-anchor");
    let db = dir.join("g.db");
    let anchors = dir.join("anchors.jsonl");
    let key = invoiceshield_governor::anchor::generate_key(); // runtime key, test only
    let seed = hex::encode(key.to_bytes());
    let pubkey = hex::encode(key.verifying_key().to_bytes());
    let s = start(&db, &[
        ("GOVERNOR_ANCHOR_KEY", &seed),
        ("GOVERNOR_ANCHOR_FILE", anchors.to_str().unwrap()),
        ("GOVERNOR_ANCHOR_INTERVAL_SECS", "1"),
    ]);
    for i in 0..3 {
        http(s.port, "POST", "/v1/actions", &body(&format!("INV-AN{i}"), 1_000), &[]).await.unwrap();
    }
    let t0 = Instant::now();
    loop {
        let n = invoiceshield_governor::anchor::read_all(&anchors).map(|v| v.iter().filter(|a| a.seq == 2).count()).unwrap_or(0);
        if n > 0 { break; }
        assert!(t0.elapsed() < Duration::from_secs(10), "server never wrote an anchor for the head");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (_, r) = http(s.port, "GET", "/v1/audit/verify", "", &[]).await.unwrap();
    assert_eq!(json_body(&r)["anchors"]["ok"], true, "{r}");

    // Online backup while the server is still running.
    let bak = dir.join("g.bak");
    let (code, out) = cli(&["backup", "--db", db.to_str().unwrap(), "--out", bak.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{out}");
    drop(s);

    let (code, out) = cli(&["verify", "--db", db.to_str().unwrap(), "--anchors", anchors.to_str().unwrap(), "--pubkey", &pubkey], &[]);
    assert_eq!(code, 0, "{out}");

    // Restore into a fresh path, then the restored DB verifies against the same anchors.
    let restored: PathBuf = dir.join("restored.db");
    let (code, out) = cli(&["restore", "--from", bak.to_str().unwrap(), "--db", restored.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = cli(&["verify", "--db", restored.to_str().unwrap(), "--anchors", anchors.to_str().unwrap(), "--pubkey", &pubkey], &[]);
    assert_eq!(code, 0, "{out}");
    // restore refuses to clobber an existing DB without --force.
    let (code, _) = cli(&["restore", "--from", bak.to_str().unwrap(), "--db", db.to_str().unwrap()], &[]);
    assert_eq!(code, 2);

    // Whole-chain rewrite on the live DB → verify fails (exit 1).
    {
        let c = Connection::open(&db).unwrap();
        c.execute_batch("DROP TRIGGER audit_append_only").unwrap();
        let mut recs = persist::load_audit(&c).unwrap();
        recs[0].payload["verdict"] = serde_json::json!("block");
        let mut forged = invoiceshield_governor::audit::AuditLog::default();
        for r in &recs { forged.append(r.payload.clone()); }
        for r in &forged.records {
            c.execute("UPDATE audit SET prev_hash=?1, payload=?2, hash=?3 WHERE seq=?4",
                rusqlite::params![r.prev_hash, r.payload.to_string(), r.hash, r.seq as i64]).unwrap();
        }
    }
    let (code, out) = cli(&["verify", "--db", db.to_str().unwrap(), "--anchors", anchors.to_str().unwrap(), "--pubkey", &pubkey], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("rewritten"), "{out}");

    // Manual anchor via CLI with the key from env.
    let (code, out) = cli(&["anchor", "--db", restored.to_str().unwrap(), "--anchors", dir.join("a2.jsonl").to_str().unwrap()], &[("GOVERNOR_ANCHOR_KEY", &seed)]);
    assert_eq!(code, 0, "{out}");
}
