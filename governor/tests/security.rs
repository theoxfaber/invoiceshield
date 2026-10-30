//! Phase 4 (security), library level: identities and roles, maker-checker, rate limiting,
//! secrets from files, the persisted vendor bank-change workflow and the v1 -> v2 migration.
mod common;
use common::*;
use invoiceshield_governor::audit::AuditLog;
use invoiceshield_governor::auth::{hash_key_hex, KeyRing, Role};
use invoiceshield_governor::master_store::{CallbackReport, McError, NewDetails};
use invoiceshield_governor::persist::{self, ActionOutcome, ResolveOutcome, Store, SCHEMA_VERSION};
use invoiceshield_governor::ratelimit::{Check, LimitsConfig, Limiter, Rate};
use invoiceshield_governor::secrets::secret_from;
use invoiceshield_governor::security::SecurityConfig;
use invoiceshield_governor::Verdict;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

const H: i64 = 3600;
const OLD_ACCT: &str = "23276810695553";
const NEW_ACCT: &str = "88880000000101";
const NEW_IFSC: &str = "CNRB0001234";

fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |k: &str| m.get(k).cloned()
}

fn idents(list: &[(&str, &[&str], &str)]) -> String {
    serde_json::Value::Array(
        list.iter()
            .map(|(n, roles, key)| serde_json::json!({"name": n, "roles": roles, "key_sha256": hash_key_hex(key)}))
            .collect(),
    )
    .to_string()
}

fn fresh(o: ActionOutcome) -> invoiceshield_governor::Decision {
    match o {
        ActionOutcome::Fresh(d) => d,
        other => panic!("expected a fresh decision, got {other:?}"),
    }
}

// ------------------------------------------------------------------ identities

#[test]
fn identities_authenticate_by_hashed_key_and_carry_roles() {
    let j = idents(&[("ap-agent", &["agent"], "agent-key-0123456789"), ("anita", &["operator"], "op-key-0123456789ab"), ("ravi", &["approver"], "apr-key-0123456789ab")]);
    let ring = KeyRing::from_lookup(&lookup(&[("GOVERNOR_IDENTITIES", &j)])).unwrap();
    assert_eq!(ring.len(), 3);
    let a = ring.authenticate("agent-key-0123456789").unwrap();
    assert_eq!(a.name, "ap-agent");
    assert!(a.has(Role::Agent) && !a.has(Role::Approver) && !a.has(Role::Operator));
    assert!(ring.authenticate("apr-key-0123456789ab").unwrap().has(Role::Approver));
    assert!(ring.authenticate("agent-key-012345678").is_none(), "prefix of a key is not the key");
    assert!(ring.authenticate("").is_none());
    assert!(ring.authenticate(&hash_key_hex("agent-key-0123456789")).is_none(), "the stored hash is not a credential");
    assert!(!format!("{ring:?}").contains("agent-key"), "Debug never shows keys");
}

#[test]
fn agent_role_cannot_be_combined_with_approver_or_operator() {
    for roles in [&["agent", "approver"][..], &["agent", "operator"][..]] {
        let j = idents(&[("bot", roles, "agent-key-0123456789")]);
        let e = KeyRing::from_lookup(&lookup(&[("GOVERNOR_IDENTITIES", &j)])).unwrap_err();
        assert!(e.iter().any(|m| m.contains("agent role cannot be combined")), "{e:?}");
    }
}

#[test]
fn identity_config_errors_are_all_reported() {
    // no agent at all
    let j = idents(&[("ravi", &["approver"], "apr-key-0123456789ab")]);
    assert!(KeyRing::from_lookup(&lookup(&[("GOVERNOR_IDENTITIES", &j)])).unwrap_err().iter().any(|m| m.contains("no agent identity")));
    // same key twice, duplicate name, bad hash, empty roles, bad name
    let mut v: serde_json::Value = serde_json::from_str(&idents(&[
        ("a1", &["agent"], "same-key-0123456789"),
        ("r1", &["approver"], "same-key-0123456789"),
        ("a1", &["approver"], "other-key-0123456789"),
        ("r2", &[], "third-key-0123456789"),
        ("bad name", &["approver"], "fourth-key-0123456789"),
    ]))
    .unwrap();
    v.as_array_mut().unwrap().push(serde_json::json!({"name": "r3", "roles": ["approver"], "key_sha256": "abc"}));
    let e = KeyRing::from_lookup(&lookup(&[("GOVERNOR_IDENTITIES", &v.to_string())])).unwrap_err();
    for want in ["same key as another identity", "duplicate identity name", "at least one role", "must be 1-64 chars", "64 hex chars"] {
        assert!(e.iter().any(|m| m.contains(want)), "missing {want:?} in {e:?}");
    }
    // unknown role is a parse error
    let e = KeyRing::from_lookup(&lookup(&[("GOVERNOR_IDENTITIES", r#"[{"name":"x","roles":["admin"],"key_sha256":"00"}]"#)])).unwrap_err();
    assert!(e[0].contains("not a JSON array"), "{e:?}");
}

#[test]
fn legacy_keys_map_to_agent_and_operator() {
    let ring = KeyRing::from_lookup(&lookup(&[("GOVERNOR_API_KEY", "legacy-agent-key-0123"), ("GOVERNOR_OPERATOR_KEY", "legacy-operator-key-01")])).unwrap();
    let a = ring.authenticate("legacy-agent-key-0123").unwrap();
    assert_eq!((a.name.as_str(), a.has(Role::Approver)), ("agent", false));
    let o = ring.authenticate("legacy-operator-key-01").unwrap();
    assert!(o.has(Role::Operator) && o.has(Role::Approver));
    assert!(ring.warnings.iter().any(|w| w.contains("legacy GOVERNOR_OPERATOR_KEY")));
    // same key for both is refused; short keys are refused
    let e = KeyRing::from_lookup(&lookup(&[("GOVERNOR_API_KEY", "legacy-agent-key-0123"), ("GOVERNOR_OPERATOR_KEY", "legacy-agent-key-0123")])).unwrap_err();
    assert!(e.iter().any(|m| m.contains("same key")), "{e:?}");
    assert!(KeyRing::from_lookup(&lookup(&[("GOVERNOR_API_KEY", "short")])).is_err());
}

#[test]
fn secrets_come_from_env_or_file_not_both() {
    let dir = tmpdir("secrets");
    let f = dir.join("api_key");
    std::fs::write(&f, "file-agent-key-0123456789\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let fp = f.to_str().unwrap();
    let s = secret_from(&lookup(&[("GOVERNOR_API_KEY_FILE", fp)]), "GOVERNOR_API_KEY").unwrap().unwrap();
    assert_eq!(s.value, "file-agent-key-0123456789", "trailing newline stripped");
    assert!(!s.loose_permissions);
    let ring = KeyRing::from_lookup(&lookup(&[("GOVERNOR_API_KEY_FILE", fp)])).unwrap();
    assert!(ring.authenticate("file-agent-key-0123456789").is_some());
    let e = secret_from(&lookup(&[("GOVERNOR_API_KEY_FILE", fp), ("GOVERNOR_API_KEY", "x")]), "GOVERNOR_API_KEY").unwrap_err();
    assert!(e.contains("not both"));
    assert!(secret_from(&lookup(&[("GOVERNOR_API_KEY_FILE", "/nonexistent/k")]), "GOVERNOR_API_KEY").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(secret_from(&lookup(&[("GOVERNOR_API_KEY_FILE", fp)]), "GOVERNOR_API_KEY").unwrap().unwrap().loose_permissions);
    }
}

// ------------------------------------------------------------------ rate limiting

#[test]
fn token_bucket_limits_bursts_and_refills() {
    let l = Limiter::new(Rate::new(2.0, 3.0).unwrap(), 100);
    let t0 = Instant::now();
    for _ in 0..3 {
        assert_eq!(l.check_at("ip-a", t0), Check::Allowed);
    }
    assert!(matches!(l.check_at("ip-a", t0), Check::Limited { retry_after_secs: 1 }));
    assert_eq!(l.check_at("ip-b", t0), Check::Allowed, "buckets are per key");
    assert_eq!(l.check_at("ip-a", t0 + Duration::from_millis(600)), Check::Allowed, "refilled ~1.2 tokens");
    assert!(matches!(l.check_at("ip-a", t0 + Duration::from_millis(600)), Check::Limited { .. }));
}

#[test]
fn limiter_map_is_bounded_and_fails_closed() {
    let l = Limiter::new(Rate::new(1.0, 1.0).unwrap(), 2);
    let t0 = Instant::now();
    assert_eq!(l.check_at("a", t0), Check::Allowed);
    assert_eq!(l.check_at("b", t0), Check::Allowed);
    assert!(matches!(l.check_at("c", t0), Check::Limited { .. }), "full of busy buckets: refuse");
    assert_eq!(l.check_at("c", t0 + Duration::from_secs(5)), Check::Allowed, "idle buckets are evicted");
}

#[test]
fn limits_config_defaults_on_in_production_off_in_eval() {
    let prod = LimitsConfig::from_lookup(&lookup(&[]), false).unwrap();
    assert_eq!(prod.identity, Rate::new(100.0, 200.0));
    assert_eq!(prod.ip, Rate::new(200.0, 400.0));
    assert!(prod.auth_fail.is_some());
    let eval = LimitsConfig::from_lookup(&lookup(&[]), true).unwrap();
    assert_eq!((eval.identity, eval.ip, eval.auth_fail), (None, None, None));
    let explicit = LimitsConfig::from_lookup(&lookup(&[("GOVERNOR_RATE_LIMIT_RPS", "5"), ("GOVERNOR_RATE_LIMIT_BURST", "7")]), true).unwrap();
    assert_eq!(explicit.identity, Rate::new(5.0, 7.0));
    let off = LimitsConfig::from_lookup(&lookup(&[("GOVERNOR_RATE_LIMIT_RPS", "0")]), false).unwrap();
    assert_eq!(off.identity, None);
    assert!(LimitsConfig::from_lookup(&lookup(&[("GOVERNOR_RATE_LIMIT_RPS", "-1")]), false).is_err());
    assert!(LimitsConfig::from_lookup(&lookup(&[("GOVERNOR_RATE_LIMIT_IP_RPS", "fast")]), false).is_err());
}

#[test]
fn security_config_validates_tls_pair_phones_and_cooling_off() {
    let base = [("GOVERNOR_API_KEY", "legacy-agent-key-0123")];
    let ok = SecurityConfig::from_lookup(&lookup(&base), false).unwrap();
    assert_eq!(ok.cooling_off_secs, 72 * H);
    assert!(ok.tls.is_none());
    let e = SecurityConfig::from_lookup(&lookup(&[base[0], ("GOVERNOR_TLS_CERT_FILE", "/x.pem")]), false).unwrap_err();
    assert!(e.iter().any(|m| m.contains("both")), "{e:?}");
    let e = SecurityConfig::from_lookup(&lookup(&[base[0], ("GOVERNOR_TLS_CERT_FILE", "/nope.pem"), ("GOVERNOR_TLS_KEY_FILE", "/nope.key")]), false).unwrap_err();
    assert!(e.iter().any(|m| m.contains("GOVERNOR_TLS_CERT_FILE")), "{e:?}");
    let e = SecurityConfig::from_lookup(&lookup(&[base[0], ("GOVERNOR_MASTER_CHANGE_COOLING_OFF_SECS", "-5")]), false).unwrap_err();
    assert!(e.iter().any(|m| m.contains("COOLING_OFF")));
    let dir = tmpdir("phones");
    let p = dir.join("phones.json");
    std::fs::write(&p, r#"{"V001": "+91 265 000 0101"}"#).unwrap();
    let c = SecurityConfig::from_lookup(&lookup(&[base[0], ("GOVERNOR_VENDOR_PHONES", p.to_str().unwrap())]), false).unwrap();
    assert_eq!(c.phones["V001"], "+91 265 000 0101");
    std::fs::write(&p, "[1,2]").unwrap();
    assert!(SecurityConfig::from_lookup(&lookup(&[base[0], ("GOVERNOR_VENDOR_PHONES", p.to_str().unwrap())]), false).is_err());
}

// ------------------------------------------------------------------ maker-checker on REVIEW

#[test]
fn proposer_cannot_resolve_its_own_review() {
    let mut s = Store::open_in_memory(master()).unwrap();
    // 81,000 > 80,000 open balance but inside the 2% tolerance: REVIEW with a pending entry.
    let d = fresh(s.act_as(&body("INV-MC1", 81_000), 0, None, Some("ap-agent")).unwrap());
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert_eq!(s.resolve_as(&d.decision_id, true, "ap-agent", 5).unwrap(), ResolveOutcome::SelfApproval);
    // the database refuses it too, even if code were bypassed
    let e = s.conn().execute(
        "INSERT INTO review_resolutions (decision_id, approver, approve, ts) VALUES (?1, 'ap-agent', 1, 6)",
        [&d.decision_id],
    );
    assert!(e.unwrap_err().to_string().contains("maker-checker"));
    assert_eq!(s.resolve_as(&d.decision_id, true, "ravi", 7).unwrap(), ResolveOutcome::Resolved);
    assert_eq!(s.resolve_as(&d.decision_id, true, "meena", 8).unwrap(), ResolveOutcome::NotPending);
    let by: String = s.conn().query_row("SELECT approver FROM review_resolutions WHERE decision_id = ?1", [&d.decision_id], |r| r.get(0)).unwrap();
    assert_eq!(by, "ravi");
    assert_eq!(AuditLog { records: s.audit_records().unwrap() }.verify(), None);
}

#[test]
fn idempotency_key_is_bound_to_the_proposing_identity() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let b = body("INV-IDEM-ID", 1_000);
    let d = fresh(s.act_as(&b, 0, Some("k-1"), Some("agent-a")).unwrap());
    assert!(matches!(s.act_as(&b, 1, Some("k-1"), Some("agent-a")).unwrap(), ActionOutcome::Replay(r) if r.decision_id == d.decision_id));
    assert!(matches!(s.act_as(&b, 2, Some("k-1"), Some("agent-b")).unwrap(), ActionOutcome::KeyConflict { .. }));
}

// ------------------------------------------------------------------ vendor bank-change workflow

fn phones() -> BTreeMap<String, String> {
    BTreeMap::from([("V001".to_string(), "+91 265 000 0101".to_string())])
}

fn details() -> NewDetails {
    NewDetails { new_account: NEW_ACCT.into(), new_ifsc: NEW_IFSC.into(), new_bank_name: "Canara Bank".into(), new_beneficiary_id: "BENE_V001_2".into() }
}

fn good_cb() -> CallbackReport {
    CallbackReport { number_dialled: "+912650000101".into(), confirmed_account: NEW_ACCT.into(), confirmed_ifsc: NEW_IFSC.to_lowercase() }
}

/// A V001 invoice that prints `acct`/`ifsc` as its bank details.
fn body_printing(inv: &str, rupees: i64, acct: &str, ifsc: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&body(inv, rupees)).unwrap();
    v["evidence"]["printed_bank_account"] = acct.into();
    v["evidence"]["printed_ifsc"] = ifsc.into();
    v.to_string()
}

fn store_at(path: &str) -> Store {
    let mut s = Store::open(path, master()).unwrap();
    s.phones = phones();
    s.cooling_off_secs = 24 * H;
    s
}

#[test]
fn bank_change_needs_callback_two_checkers_and_cooling_off() {
    let dir = tmpdir("mc-flow");
    let db = dir.join("g.db");
    let dbs = db.to_str().unwrap();
    let mut s = store_at(dbs);
    let t0 = 1_000_000;
    let v = s.mc_request("anita", "V001", details(), t0).unwrap().unwrap();
    assert_eq!(v.status, "awaiting_callback");
    let id = v.change_id.clone();
    assert!(matches!(s.mc_request("anita", "V001", details(), t0).unwrap(), Err(McError::OpenChangeExists(_))), "one open change per vendor");
    assert!(matches!(s.mc_request("anita", "V999", details(), t0).unwrap(), Err(McError::UnknownVendor)));
    // approvals before the call-back are refused
    assert!(matches!(s.mc_approve(&id, "ravi", t0 + 1).unwrap(), Err(McError::WrongState(_))));
    // the maker cannot confirm its own call-back
    assert_eq!(s.mc_callback(&id, "anita", good_cb(), t0 + 2).unwrap(), Err(McError::MakerCannot));
    // a call-back to a number from the request (not the one on file) is refused; change stays open
    let mut wrong = good_cb();
    wrong.number_dialled = "+91 98765 43210".into();
    assert!(matches!(s.mc_callback(&id, "ravi", wrong, t0 + 3).unwrap(), Err(McError::Refused(_))));
    assert_eq!(s.mc_get(&id).unwrap().unwrap().status, "awaiting_callback");
    let v = s.mc_callback(&id, "ravi", good_cb(), t0 + 4).unwrap().unwrap();
    assert_eq!(v.status, "awaiting_approvals");
    // maker cannot approve; one approver twice counts once
    assert_eq!(s.mc_approve(&id, "anita", t0 + 5).unwrap(), Err(McError::MakerCannot));
    assert_eq!(s.mc_approve(&id, "ravi", t0 + 6).unwrap().unwrap().status, "awaiting_approvals");
    assert_eq!(s.mc_approve(&id, "ravi", t0 + 7).unwrap(), Err(McError::AlreadyApproved));
    // DB trigger: maker can never be inserted as an approver
    let e = s.conn().execute("INSERT INTO master_change_approvals (change_id, approver, ts) VALUES (?1, 'anita', 1)", [&id]);
    assert!(e.unwrap_err().to_string().contains("maker-checker"));
    let t2 = t0 + 10;
    let v = s.mc_approve(&id, "meena", t2).unwrap().unwrap();
    assert_eq!(v.status, "cooling_off");
    assert_eq!(v.effective_at, Some(t2 + 24 * H));
    assert_eq!(v.approvals, vec!["ravi", "meena"]);

    // During cooling-off the master still has the old account: an invoice printing the new one
    // is a tamper REVIEW, one printing the old one pays the old beneficiary.
    let d = fresh(s.act(&body_printing("INV-CO-1", 1_000, NEW_ACCT, NEW_IFSC), t2 + 24 * H - 1, None).unwrap());
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(d.tamper_alert);
    let d = fresh(s.act(&body_printing("INV-CO-2", 1_000, OLD_ACCT, "BARB0MAKVAD"), t2 + 24 * H - 1, None).unwrap());
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert_eq!(d.payee.unwrap().beneficiary_id, "BENE_V001");
    assert_eq!(s.gov.master.vendors["V001"].bank_account, OLD_ACCT);

    // At the end of the cooling-off the change applies inside the payment transaction.
    let d = fresh(s.act(&body_printing("INV-CO-3", 1_000, NEW_ACCT, NEW_IFSC), t2 + 24 * H, None).unwrap());
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    let p = d.payee.unwrap();
    assert_eq!((p.beneficiary_id.as_str(), p.account_last4.as_str()), ("BENE_V001_2", "0101"));
    let v = s.mc_get(&id).unwrap().unwrap();
    assert_eq!((v.status.as_str(), v.applied_at), ("applied", Some(t2 + 24 * H)));
    // ...and now the old account is the mismatch
    let d = fresh(s.act(&body_printing("INV-CO-4", 1_000, OLD_ACCT, "BARB0MAKVAD"), t2 + 24 * H + 1, None).unwrap());
    assert_eq!(d.verdict, Verdict::Review);
    let recs = s.audit_records().unwrap();
    assert_eq!(AuditLog { records: recs.clone() }.verify(), None);
    let events: Vec<String> = recs.iter().filter_map(|r| r.payload["event"].as_str().map(String::from)).collect();
    for e in ["vendor_bank_change_requested", "vendor_bank_change_callback_refused", "vendor_bank_change_callback_verified",
              "vendor_bank_change_approved", "vendor_bank_change_cooling_off", "vendor_bank_change", "vendor_bank_change_applied"] {
        assert!(events.iter().any(|x| x == e), "missing audit event {e}: {events:?}");
    }
    drop(s);
    // Restart: the applied change is rebuilt from the DB on top of the master file.
    let s = store_at(dbs);
    let mut s = s;
    s.refresh().unwrap();
    assert_eq!(s.gov.master.vendors["V001"].bank_account, NEW_ACCT);
    assert_eq!(s.gov.master.vendors["V001"].beneficiary_id, "BENE_V001_2");
    // Applied changes are final.
    assert!(matches!(s.mc_reject(&id, "ravi", "late", t2 + 30 * H).unwrap(), Err(McError::WrongState(_))));
    assert!(s.conn().execute("UPDATE master_changes SET status = 'rejected' WHERE change_id = ?1", [&id]).is_err());
}

#[test]
fn one_approver_never_applies_and_cooling_off_can_be_stopped() {
    let mut s = Store::open_in_memory(master()).unwrap();
    s.phones = phones();
    s.cooling_off_secs = 24 * H;
    let id = s.mc_request("anita", "V001", details(), 0).unwrap().unwrap().change_id;
    s.mc_callback(&id, "ravi", good_cb(), 1).unwrap().unwrap();
    s.mc_approve(&id, "ravi", 2).unwrap().unwrap();
    s.mc_tick(1_000 * H).unwrap();
    assert_eq!(s.mc_get(&id).unwrap().unwrap().status, "awaiting_approvals");
    assert_eq!(s.gov.master.vendors["V001"].bank_account, OLD_ACCT);
    s.mc_approve(&id, "meena", 3).unwrap().unwrap();
    // stopped during cooling-off: never applied
    let v = s.mc_reject(&id, "ravi", "vendor's old contact says they did not ask", 4).unwrap().unwrap();
    assert_eq!(v.status, "rejected");
    s.mc_tick(1_000 * H).unwrap();
    assert_eq!(s.gov.master.vendors["V001"].bank_account, OLD_ACCT);
    // a new change for the vendor may now be entered
    assert!(s.mc_request("anita", "V001", details(), 5).unwrap().is_ok());
}

#[test]
fn callback_confirming_other_details_rejects_the_change() {
    let mut s = Store::open_in_memory(master()).unwrap();
    s.phones = phones();
    let id = s.mc_request("anita", "V001", details(), 0).unwrap().unwrap().change_id;
    let mut cb = good_cb();
    cb.confirmed_account = "11112222333344".into();
    assert!(matches!(s.mc_callback(&id, "ravi", cb, 1).unwrap(), Err(McError::Refused(_))));
    let v = s.mc_get(&id).unwrap().unwrap();
    assert_eq!((v.status.as_str(), v.rejected_by.as_deref()), ("rejected", Some("ravi")));
}

#[test]
fn callback_without_number_on_file_is_refused() {
    let mut s = Store::open_in_memory(master()).unwrap(); // no phones configured
    let id = s.mc_request("anita", "V001", details(), 0).unwrap().unwrap().change_id;
    assert!(matches!(s.mc_callback(&id, "ravi", good_cb(), 1).unwrap(), Err(McError::Refused(_))));
    assert_eq!(s.mc_get(&id).unwrap().unwrap().status, "awaiting_callback");
}

#[test]
fn invalid_new_details_are_refused() {
    let mut s = Store::open_in_memory(master()).unwrap();
    let mut d = details();
    d.new_account = "12AB".into();
    assert!(matches!(s.mc_request("anita", "V001", d, 0).unwrap(), Err(McError::InvalidDetails(_))));
    let mut d = details();
    d.new_ifsc = "CNRB".into();
    assert!(matches!(s.mc_request("anita", "V001", d, 0).unwrap(), Err(McError::InvalidDetails(_))));
}

// ------------------------------------------------------------------ migration

#[test]
fn v1_database_migrates_to_v2_and_keeps_its_data() {
    let dir = tmpdir("mig");
    let db = dir.join("v1.db");
    // Build a v1 database exactly as Phase 3 wrote it: create a v2 store, then strip it back.
    {
        let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
        fresh(s.act(&body("INV-OLD", 1_000), 0, None).unwrap());
    }
    {
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "DROP TRIGGER review_checker_not_proposer; DROP TABLE review_resolutions;
             DROP TRIGGER master_change_checker_not_maker; DROP TABLE master_change_approvals;
             DROP TRIGGER master_change_final; DROP INDEX master_changes_one_open; DROP TABLE master_changes;
             ALTER TABLE decisions DROP COLUMN proposed_by; PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
    let v: i64 = s.conn().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!((v, SCHEMA_VERSION), (2, 2));
    let d = fresh(s.act(&body("INV-OLD", 1_000), 10, None).unwrap());
    assert_eq!(d.verdict, Verdict::Block, "old ledger survived the migration");
    let n: i64 = s.conn().query_row("SELECT COUNT(*) FROM decisions WHERE proposed_by IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
    assert_eq!(AuditLog { records: persist::load_audit(s.conn()).unwrap() }.verify(), None);
}

// ------------------------------------------------------------------ state key (DB-only attacker)

#[test]
fn hmac_matches_rfc4231() {
    let m = invoiceshield_governor::master_store::hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(hex::encode(m), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
}

#[test]
fn forged_applied_change_in_db_blocks_the_vendor_instead_of_redirecting() {
    let dir = tmpdir("mc-mac");
    let db = dir.join("g.db");
    let dbs = db.to_str().unwrap();
    let key = [7u8; 32];
    {
        // A legitimate change applied with the key survives a restart.
        let mut s = store_at(dbs);
        s.set_state_key(Some(key)).unwrap();
        let id = s.mc_request("anita", "V001", details(), 0).unwrap().unwrap().change_id;
        s.mc_callback(&id, "ravi", good_cb(), 1).unwrap().unwrap();
        s.mc_approve(&id, "ravi", 2).unwrap().unwrap();
        s.mc_approve(&id, "meena", 3).unwrap().unwrap();
        s.mc_tick(3 + 24 * H).unwrap();
        assert_eq!(s.gov.master.vendors["V001"].bank_account, NEW_ACCT);
    }
    {
        let mut s = store_at(dbs);
        s.set_state_key(Some(key)).unwrap();
        assert_eq!(s.gov.master.vendors["V001"].bank_account, NEW_ACCT, "valid MAC: applied");
    }
    // DB-only attacker inserts an 'applied' change for V002 pointing at their own account.
    Connection::open(&db).unwrap().execute(
        "INSERT INTO master_changes (change_id, vendor_id, new_details, maker, requested_at, status, applied_at)
         VALUES ('MC-EVIL', 'V002', ?1, 'x', 0, 'applied', 5)",
        [serde_json::to_string(&details()).unwrap()],
    ).unwrap();
    let mut s = store_at(dbs);
    s.set_state_key(Some(key)).unwrap();
    assert!(!s.gov.master.vendors.contains_key("V002"), "forged row: vendor blocked, not redirected");
    assert_eq!(s.gov.master.vendors["V001"].bank_account, NEW_ACCT);
    let mut b: serde_json::Value = serde_json::from_str(&body("INV-EVIL", 1_000)).unwrap();
    b["proposal"]["vendor_id"] = "V002".into();
    let d = fresh(s.act(&b.to_string(), 10 * 24 * H, None).unwrap());
    assert_eq!(d.verdict, Verdict::Block);
    assert!(d.reasons.iter().any(|r| r.rule == "unknown_vendor"), "{:?}", d.reasons);
    // Without a key (documented weakness) the forged row would apply.
    s.set_state_key(None).unwrap();
    assert_eq!(s.gov.master.vendors["V002"].bank_account, NEW_ACCT);
}
