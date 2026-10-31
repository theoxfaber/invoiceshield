//! Post-review hardening, durable-state level (docs/REVIEW_FIXES.md):
//! finding 4 (evidence nonces survive a restart) and the human-rejection permanence fix.
mod common;
use base64::Engine as _;
use common::*;
use ed25519_dalek::{Signer, SigningKey};
use invoiceshield_governor::persist::{self, ActionOutcome, ReopenOutcome, ResolveOutcome, Store};
use invoiceshield_governor::trust::key_id;
use invoiceshield_governor::{Decision, TrustConfig, TrustVerifier, Verdict};
use rand_core::OsRng;
use serde_json::{json, Value};

const DOC: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const DAY: i64 = 86_400;

fn fresh(o: ActionOutcome) -> Decision {
    match o {
        ActionOutcome::Fresh(d) => d,
        other => panic!("expected a fresh decision, got {other:?}"),
    }
}

fn rules(d: &Decision) -> Vec<String> {
    d.reasons.iter().map(|r| r.rule.clone()).collect()
}

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn env(sk: &SigningKey, payload: &Value) -> Value {
    let p = payload.to_string();
    json!({"payload": p, "sig": base64::engine::general_purpose::STANDARD.encode(sk.sign(p.as_bytes()).to_bytes())})
}

/// A fully signed body (evidence + checker) for V001, issued now on the real clock.
fn signed_body(ex: &SigningKey, ck: &SigningKey, invoice_no: &str) -> String {
    let mut n = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut OsRng, &mut n);
    let nonce = hex::encode(n);
    let t = wall();
    let ev = env(
        ex,
        &json!({"v": 1, "key_id": key_id(&ex.verifying_key()), "doc_sha256": DOC, "extractor_version": "test",
                "issued_at": t, "nonce": nonce, "status": "ok", "sandbox": {"network_namespace": true},
                "evidence": {"text_layer_gstin": "24ZZACS4821A1ZX", "sender_domain": "sharmaofficesupplies.in",
                             "has_text_layer": true, "text_source": "text_layer"}}),
    );
    let c = env(
        ck,
        &json!({"v": 1, "kind": "checker", "key_id": key_id(&ck.verifying_key()), "doc_sha256": DOC,
                "evidence_nonce": nonce, "issued_at": t, "raw": CLEAN}),
    );
    json!({"proposal": {"vendor_id": "V001", "invoice_no": invoice_no, "amount_paise": 1_000_00,
                        "po_ref": "PO-2026-0101", "document_sha256": DOC},
           "signed_evidence": ev, "signed_checker": c})
    .to_string()
}

fn trusted_store(db: &str, ex: &SigningKey, ck: &SigningKey) -> Store {
    let mut s = Store::open(db, master()).unwrap();
    s.trust = Some(TrustVerifier::new(
        TrustConfig::new(vec![ex.verifying_key()]).with_checker_keys(vec![ck.verifying_key()]),
    ));
    s
}

// ------------------------------------------------------------------ finding 4

#[test]
fn replay_after_restart_is_still_blocked() {
    let dir = tmpdir("nonce-restart");
    let db = dir.join("g.db");
    let db = db.to_str().unwrap();
    let (ex, ck) = (SigningKey::generate(&mut OsRng), SigningKey::generate(&mut OsRng));
    let b = signed_body(&ex, &ck, "INV-N1");
    {
        let mut s = trusted_store(db, &ex, &ck);
        let d = fresh(s.act(&b, 0, None).unwrap());
        assert_eq!(d.verdict, Verdict::Allow, "{:?} {:?}", d.reasons, d.signals);
        assert_eq!(persist::nonces_persisted(s.conn()).unwrap(), 1);
    }
    // "Restart": a new process has an empty in-memory nonce set, but the DB remembers.
    let mut s = trusted_store(db, &ex, &ck);
    let d = fresh(s.act(&b, 1, None).unwrap());
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(rules(&d), vec!["evidence_replayed"]);
}

#[test]
fn second_process_on_same_db_cannot_replay() {
    let dir = tmpdir("nonce-2proc");
    let db = dir.join("g.db");
    let db = db.to_str().unwrap();
    let (ex, ck) = (SigningKey::generate(&mut OsRng), SigningKey::generate(&mut OsRng));
    let b = signed_body(&ex, &ck, "INV-N2");
    let mut a = trusted_store(db, &ex, &ck);
    let mut c = trusted_store(db, &ex, &ck);
    assert_eq!(fresh(a.act(&b, 0, None).unwrap()).verdict, Verdict::Allow);
    let d = fresh(c.act(&b, 1, None).unwrap());
    assert_eq!(rules(&d), vec!["evidence_replayed"]);
}

#[test]
fn expired_nonces_are_pruned_from_the_db() {
    let (ex, ck) = (SigningKey::generate(&mut OsRng), SigningKey::generate(&mut OsRng));
    let mut s = trusted_store(":memory:", &ex, &ck);
    s.conn()
        .execute(
            "INSERT INTO used_nonces (nonce, issued_at) VALUES ('00', ?1)",
            [wall() - 10_000],
        )
        .unwrap();
    fresh(s.act(&signed_body(&ex, &ck, "INV-N3"), 0, None).unwrap());
    assert_eq!(persist::nonces_persisted(s.conn()).unwrap(), 1, "old row pruned, new row kept");
}

// ------------------------------------------- human rejection is permanent until reopened

/// A REVIEW (over the approval threshold) proposed by `agent`, then rejected by `ravi`.
fn rejected(s: &mut Store, inv: &str) -> String {
    let d = fresh(s.act_as(&body(inv, 81_000), 0, None, Some("agent")).unwrap());
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert_eq!(
        s.resolve_as(&d.decision_id, false, "ravi", 10).unwrap(),
        ResolveOutcome::Resolved
    );
    d.decision_id
}

#[test]
fn rejected_invoice_stays_blocked_beyond_every_window() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let id = rejected(&mut s, "INV-REJ-1");
    for t in [20, 31 * DAY, 91 * DAY, 400 * DAY] {
        let d = fresh(s.act_as(&body("INV-REJ-1", 81_000), t, None, Some("agent")).unwrap());
        assert_eq!(d.verdict, Verdict::Block, "at t={t}");
        assert!(rules(&d).contains(&"previously_rejected".to_string()), "{:?}", rules(&d));
        assert!(d.reasons.iter().any(|r| r.detail.contains(&id)));
    }
    // A smaller amount under the same number is the same rejected invoice.
    let d = fresh(s.act_as(&body("INV-REJ-1", 1_000), 500 * DAY, None, Some("agent")).unwrap());
    assert!(rules(&d).contains(&"previously_rejected".to_string()));
    assert_eq!(d.verdict, Verdict::Block);
}

#[test]
fn reopen_requires_another_identity_and_is_audited() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let id = rejected(&mut s, "INV-REJ-2");
    assert_eq!(
        s.reopen_as(&id, "agent", "vendor sent proof", 20).unwrap(),
        ReopenOutcome::SelfReopen,
        "the proposer cannot reopen its own rejected payment"
    );
    let audit_before = s.audit_records().unwrap().len();
    assert_eq!(
        s.reopen_as(&id, "anita", "vendor sent corrected PO", 30).unwrap(),
        ReopenOutcome::Reopened
    );
    let recs = s.audit_records().unwrap();
    assert_eq!(recs.len(), audit_before + 1);
    assert!(recs.last().unwrap().payload.to_string().contains("reopen"));
    assert!(recs.last().unwrap().payload.to_string().contains("anita"));
    assert_eq!(
        s.reopen_as(&id, "anita", "again", 31).unwrap(),
        ReopenOutcome::NotRejected,
        "a reopen happens once"
    );
    let d = fresh(s.act_as(&body("INV-REJ-2", 81_000), 40 * DAY, None, Some("agent")).unwrap());
    assert!(!rules(&d).contains(&"previously_rejected".to_string()), "{:?}", rules(&d));
    assert_ne!(d.verdict, Verdict::Block, "{:?}", d.reasons);
}

#[test]
fn reopen_survives_restart_and_rejection_without_reopen_too() {
    let dir = tmpdir("reopen-restart");
    let db = dir.join("g.db");
    let db = db.to_str().unwrap();
    let (a, b);
    {
        let mut s = Store::open(db, master()).unwrap();
        a = rejected(&mut s, "INV-REJ-A");
        b = rejected(&mut s, "INV-REJ-B");
        assert_eq!(s.reopen_as(&a, "anita", "ok", 20).unwrap(), ReopenOutcome::Reopened);
    }
    let mut s = Store::open(db, master()).unwrap();
    let da = fresh(s.act_as(&body("INV-REJ-A", 81_000), 100 * DAY, None, Some("agent")).unwrap());
    assert!(!rules(&da).contains(&"previously_rejected".to_string()), "{a}");
    let db_ = fresh(s.act_as(&body("INV-REJ-B", 81_000), 100 * DAY, None, Some("agent")).unwrap());
    assert!(rules(&db_).contains(&"previously_rejected".to_string()), "{b}");
    assert!(s.audit_head().unwrap().is_some());
}

#[test]
fn only_human_rejected_decisions_can_be_reopened() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let allowed = fresh(s.act_as(&body("INV-OK-1", 1_000), 0, None, Some("agent")).unwrap());
    assert_eq!(allowed.verdict, Verdict::Allow);
    assert_eq!(
        s.reopen_as(&allowed.decision_id, "anita", "x", 1).unwrap(),
        ReopenOutcome::NotRejected
    );
    assert_eq!(
        s.reopen_as("dec_999999", "anita", "x", 1).unwrap(),
        ReopenOutcome::NotRejected
    );
    // The database refuses it as well, even if the in-process check were bypassed.
    let e = s.conn().execute(
        "INSERT INTO rejection_reopens (decision_id, reopened_by, reason, ts) VALUES (?1, 'anita', 'x', 1)",
        [&allowed.decision_id],
    );
    assert!(e.is_err());
}

#[test]
fn reopen_rows_are_append_only_in_the_db() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let id = rejected(&mut s, "INV-REJ-3");
    assert_eq!(s.reopen_as(&id, "anita", "ok", 20).unwrap(), ReopenOutcome::Reopened);
    assert!(s
        .conn()
        .execute("UPDATE rejection_reopens SET reopened_by = 'agent'", [])
        .is_err());
}
