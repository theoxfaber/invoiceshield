//! Phase 3: durable state in SQLite. Library-level tests (no HTTP).
mod common;
use common::*;
use invoiceshield_governor::anchor;
use invoiceshield_governor::audit::AuditLog;
use invoiceshield_governor::persist::{self, ActionOutcome, Fault, Store, SCHEMA_VERSION};
use invoiceshield_governor::Verdict;
use rusqlite::Connection;

fn fresh(o: ActionOutcome) -> invoiceshield_governor::Decision {
    match o {
        ActionOutcome::Fresh(d) => d,
        other => panic!("expected a fresh decision, got {other:?}"),
    }
}

fn count(c: &Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn in_memory_store_works_and_is_migrated() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let v: i64 = s.conn().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    let d = fresh(s.act(&body("INV-1", 1_000), 0, None).unwrap());
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    let d2 = fresh(s.act(&body("INV-1", 1_000), 10, None).unwrap());
    assert_eq!(d2.verdict, Verdict::Block);
    assert!(d2.reasons.iter().any(|r| r.rule == "duplicate_invoice"));
}

#[test]
fn file_db_uses_wal() {
    let dir = tmpdir("wal");
    let db = dir.join("g.db");
    let s = Store::open(db.to_str().unwrap(), master()).unwrap();
    let mode: String = s.conn().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn restart_keeps_state_and_duplicate_still_blocked() {
    let dir = tmpdir("restart");
    let db = dir.join("g.db");
    let (paid_id, review_id, head) = {
        let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
        let a = fresh(s.act(&body("SOS/26-27/0231", 1_000), 100, None).unwrap());
        assert_eq!(a.verdict, Verdict::Allow);
        let r = fresh(s.act(r#"{"not": "a request"}"#, 101, None).unwrap());
        assert_eq!(r.verdict, Verdict::Review);
        let head = s.audit_head().unwrap().unwrap();
        (a.decision_id, r.decision_id, head)
    }; // "server" dropped

    let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
    assert_eq!(s.get_decision(&paid_id).unwrap().unwrap().verdict, Verdict::Allow);
    assert_eq!(s.get_decision(&review_id).unwrap().unwrap().verdict, Verdict::Review);
    assert_eq!(s.audit_head().unwrap().unwrap(), head);
    assert_eq!(s.ledger().unwrap().entries.len(), 1);
    // Restored in memory exactly, chain still verifies after a JSON round trip.
    assert_eq!(s.gov.audit.records.len(), 2);
    assert_eq!(s.gov.audit.verify(), None);
    // Same invoice after restart: still a duplicate.
    let d = fresh(s.act(&body("SOS/26-27/0231", 1_000), 200, None).unwrap());
    assert_eq!(d.verdict, Verdict::Block);
    assert!(d.reasons.iter().any(|r| r.rule == "duplicate_invoice"));
    // Normalised key too ("SOS-26-27-0231" is the same invoice).
    let d = fresh(s.act(&body("sos 26 27 0231", 1_000), 201, None).unwrap());
    assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
    // Decision ids keep counting instead of restarting at dec_000001.
    assert_ne!(d.decision_id, paid_id);
    assert_eq!(d.decision_id, "dec_000004");
}

#[test]
fn injected_failure_mid_transaction_leaves_no_partial_row() {
    let mut s = Store::open_in_memory(master()).unwrap();
    s.inject_fault(Some(Fault::ErrorAfterLedgerInsert));
    assert!(s.act(&body("INV-9", 1_000), 0, None).is_err());
    for t in ["ledger", "decisions", "audit", "idempotency"] {
        assert_eq!(count(s.conn(), t), 0, "{t} has a partial row");
    }
    s.inject_fault(None);
    // The rolled-back attempt left nothing behind in memory either: the retry pays.
    let d = fresh(s.act(&body("INV-9", 1_000), 1, None).unwrap());
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert_eq!(d.decision_id, "dec_000001");
    assert_eq!(count(s.conn(), "ledger"), 1);
    assert_eq!(s.gov.audit.verify(), None);
}

#[test]
fn idempotency_key_replays_or_conflicts() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let a = fresh(s.act(&body("INV-7", 1_000), 0, Some("k-1")).unwrap());
    match s.act(&body("INV-7", 1_000), 5, Some("k-1")).unwrap() {
        ActionOutcome::Replay(d) => assert_eq!(d.decision_id, a.decision_id),
        o => panic!("{o:?}"),
    }
    match s.act(&body("INV-7", 2_000), 5, Some("k-1")).unwrap() {
        ActionOutcome::KeyConflict { decision_id } => assert_eq!(decision_id, a.decision_id),
        o => panic!("{o:?}"),
    }
    // A replay writes nothing new.
    assert_eq!(count(s.conn(), "decisions"), 1);
    assert_eq!(count(s.conn(), "audit"), 1);
}

#[test]
fn unique_index_refuses_second_live_entry() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let d = fresh(s.act(&body("INV-U", 1_000), 0, None).unwrap());
    let c = s.conn();
    c.execute("INSERT INTO decisions (decision_id, verdict, body, ts) VALUES ('x1','allow','{}',0)", []).unwrap();
    let dup = c.execute(
        "INSERT INTO ledger (decision_id, vendor_id, invoice_key, amount_paise, irn, po_ref, status, ts, entry)
         SELECT 'x1', vendor_id, invoice_key, amount_paise, NULL, po_ref, 'paid', 1, entry FROM ledger WHERE decision_id = ?1",
        [&d.decision_id],
    );
    assert!(dup.is_err(), "a second live ledger row for the same invoice must be impossible");
    // A paid entry cannot be flipped or edited.
    assert!(c.execute("UPDATE ledger SET status = 'rejected' WHERE decision_id = ?1", [&d.decision_id]).is_err());
    assert!(c.execute("UPDATE audit SET payload = '{}' WHERE seq = 0", []).is_err());
}

#[test]
fn stale_process_cannot_pay_twice() {
    // Two independent connections (as two server processes would hold).
    let dir = tmpdir("stale");
    let db = dir.join("g.db");
    let mut a = Store::open(db.to_str().unwrap(), master()).unwrap();
    let mut b = Store::open(db.to_str().unwrap(), master()).unwrap();
    assert_eq!(fresh(a.act(&body("INV-S", 1_000), 0, None).unwrap()).verdict, Verdict::Allow);
    // b's in-memory ledger is stale; it refreshes inside the write transaction.
    let d = fresh(b.act(&body("INV-S", 1_000), 1, None).unwrap());
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(d.decision_id, "dec_000002");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_identical_requests_pay_once() {
    let dir = tmpdir("conc");
    let db = dir.join("g.db");
    drop(Store::open(db.to_str().unwrap(), master()).unwrap());
    let mut handles = vec![];
    for i in 0..24 {
        let db = db.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            // Each task has its own connection and its own in-memory governor: no shared mutex.
            let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
            s.act(&body("INV-RACE", 1_000), i, None).unwrap()
        }));
    }
    let mut allow = 0;
    for h in handles {
        if let ActionOutcome::Fresh(d) = h.await.unwrap() {
            if d.verdict == Verdict::Allow {
                allow += 1;
            }
        }
    }
    assert_eq!(allow, 1);
    let c = persist::connect(db.to_str().unwrap()).unwrap();
    assert_eq!(count(&c, "ledger"), 1);
    assert_eq!(count(&c, "decisions"), 24);
    assert_eq!(AuditLog { records: persist::load_audit(&c).unwrap() }.verify(), None);
}

#[test]
fn resolve_persists() {
    let dir = tmpdir("resolve");
    let db = dir.join("g.db");
    let id = {
        let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
        // 81,000 > 80,000 open balance but inside the 2% tolerance: REVIEW with a pending entry.
        let d = fresh(s.act(&body("INV-R", 81_000), 0, None).unwrap());
        assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
        d.decision_id
    };
    {
        let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
        assert!(s.resolve(&id, false, "ap.lead", 10).unwrap());
        assert!(!s.resolve(&id, true, "ap.lead", 11).unwrap(), "already resolved");
    }
    let s = Store::open(db.to_str().unwrap(), master()).unwrap();
    let e = s.ledger().unwrap().entries.into_iter().find(|e| e.decision_id == id).unwrap();
    assert_eq!(e.status, invoiceshield_governor::ledger::EntryStatus::Rejected);
    assert_eq!(s.audit_records().unwrap().len(), 2);
}

#[test]
fn newer_schema_is_refused() {
    let dir = tmpdir("schema");
    let db = dir.join("g.db");
    drop(Store::open(db.to_str().unwrap(), master()).unwrap());
    Connection::open(&db).unwrap().execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1)).unwrap();
    assert!(matches!(Store::open(db.to_str().unwrap(), master()), Err(persist::StoreError::SchemaTooNew(_))));
}

#[test]
fn eval_reset_clears_db() {
    let mut s = Store::open_in_memory(master()).unwrap();
    fresh(s.act(&body("INV-E", 1_000), 0, None).unwrap());
    s.reset().unwrap();
    let d = fresh(s.act(&body("INV-E", 1_000), 0, None).unwrap());
    assert_eq!(d.verdict, Verdict::Allow);
    assert_eq!(d.decision_id, "dec_000001");
}

// ---------- anchoring ----------

fn chain_with(n: usize) -> (std::path::PathBuf, Store) {
    let dir = tmpdir("anchor");
    let db = dir.join("g.db");
    let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
    for i in 0..n {
        fresh(s.act(&body(&format!("INV-A{i}"), 1_000), i as i64, None).unwrap());
    }
    (db, s)
}

#[test]
fn anchors_verify_and_detect_whole_chain_rewrite() {
    let key = anchor::generate_key(); // generated at runtime; never stored
    let pk = key.verifying_key();
    let (db, s) = chain_with(4);
    let afile = db.with_extension("anchors.jsonl");
    let (seq, hash, _) = s.audit_head().unwrap().unwrap();
    anchor::append(&afile, &anchor::sign(&key, seq, &hash, 1)).unwrap();
    let anchors = anchor::read_all(&afile).unwrap();
    let rep = anchor::verify_chain(&s.audit_records().unwrap(), &anchors, &pk);
    assert!(rep.ok, "{rep:?}");
    drop(s);

    // Attacker with the DB file: drop the guard trigger, lower an amount in record 1 and
    // recompute every hash so the chain is internally consistent again.
    let c = Connection::open(&db).unwrap();
    c.execute_batch("DROP TRIGGER audit_append_only").unwrap();
    let mut recs = persist::load_audit(&c).unwrap();
    recs[1].payload["proposal"]["amount_paise"] = serde_json::json!(1);
    let mut forged = AuditLog::default();
    for r in &recs {
        forged.append(r.payload.clone());
    }
    for r in &forged.records {
        c.execute("UPDATE audit SET prev_hash = ?1, payload = ?2, hash = ?3 WHERE seq = ?4",
            rusqlite::params![r.prev_hash, r.payload.to_string(), r.hash, r.seq as i64]).unwrap();
    }
    let stored = persist::load_audit(&c).unwrap();
    // The hash chain alone cannot see it...
    assert_eq!(AuditLog { records: stored.clone() }.verify(), None);
    // ...the anchor can.
    let rep = anchor::verify_chain(&stored, &anchors, &pk);
    assert!(!rep.ok);
    assert!(rep.anchor_errors[0].contains("rewritten"), "{rep:?}");
}

#[test]
fn anchors_detect_edit_truncation_and_forged_anchor() {
    let key = anchor::generate_key();
    let pk = key.verifying_key();
    let (_db, s) = chain_with(3);
    let recs = s.audit_records().unwrap();
    let a = anchor::sign(&key, 2, &recs[2].hash, 5);

    // Single-record edit without recomputing: the chain itself breaks.
    let mut edited = recs.clone();
    edited[1].payload["verdict"] = serde_json::json!("allow_forged");
    assert_eq!(anchor::verify_chain(&edited, &[a.clone()], &pk).first_broken, Some(1));

    // Truncation below the anchored head.
    let rep = anchor::verify_chain(&recs[..2], &[a.clone()], &pk);
    assert!(!rep.ok && rep.anchor_errors[0].contains("truncated"), "{rep:?}");

    // An anchor signed by another key (attacker re-anchoring a rewritten chain) is rejected.
    let evil = anchor::generate_key();
    let fake = anchor::sign(&evil, 2, &recs[2].hash, 6);
    let rep = anchor::verify_chain(&recs, &[fake.clone()], &pk);
    assert!(!rep.ok && rep.anchor_errors[0].contains("signature"), "{rep:?}");
    // Swapping in the pinned pubkey field does not help: the signature is checked, not the field.
    let mut spoof = fake;
    spoof.pubkey = hex::encode(pk.to_bytes());
    assert!(!anchor::verify_chain(&recs, &[spoof], &pk).ok);

    // Tampered anchor content (head changed after signing).
    let mut t = a;
    t.head_hash = recs[1].hash.clone();
    assert!(!anchor::verify_chain(&recs, &[t], &pk).ok);
}

// ---------- backup / restore ----------

#[test]
fn backup_and_restore_round_trip() {
    let dir = tmpdir("backup");
    let db = dir.join("g.db");
    let bak = dir.join("g.bak");
    let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
    fresh(s.act(&body("INV-B1", 1_000), 0, None).unwrap());
    fresh(s.act(&body("INV-B2", 1_000), 1, None).unwrap());
    s.backup_to(&bak).unwrap(); // online, while the store is open
    let head_at_backup = s.audit_head().unwrap();
    fresh(s.act(&body("INV-B3", 1_000), 2, None).unwrap());
    drop(s);

    let restored = dir.join("restored.db");
    persist::restore(&bak, restored.to_str().unwrap()).unwrap();
    let mut r = Store::open(restored.to_str().unwrap(), master()).unwrap();
    assert_eq!(r.audit_head().unwrap(), head_at_backup);
    assert_eq!(r.ledger().unwrap().entries.len(), 2);
    let d = fresh(r.act(&body("INV-B1", 1_000), 3, None).unwrap());
    assert_eq!(d.verdict, Verdict::Block, "duplicate must still be blocked after restore");
    // B3 was written after the backup, so the restored DB has never seen it.
    let d = fresh(r.act(&body("INV-B3", 1_000), 4, None).unwrap());
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    // Restore over an existing DB (operator --force path) replaces it.
    persist::restore(&bak, db.to_str().unwrap()).unwrap();
    let s = Store::open(db.to_str().unwrap(), master()).unwrap();
    assert_eq!(s.audit_head().unwrap(), head_at_backup);
}

#[test]
fn restore_refuses_a_backup_with_a_broken_chain() {
    let dir = tmpdir("badbak");
    let db = dir.join("g.db");
    let bak = dir.join("g.bak");
    let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
    fresh(s.act(&body("INV-X1", 1_000), 0, None).unwrap());
    fresh(s.act(&body("INV-X2", 1_000), 1, None).unwrap());
    s.backup_to(&bak).unwrap();
    let c = Connection::open(&bak).unwrap();
    c.execute_batch("DROP TRIGGER audit_append_only; UPDATE audit SET payload = '{\"x\":1}' WHERE seq = 0;").unwrap();
    drop(c);
    assert!(persist::restore(&bak, dir.join("r.db").to_str().unwrap()).is_err());
}
