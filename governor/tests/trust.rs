//! Provenance tests: only extractor-signed evidence reaches the rules.
//! Keys are generated at runtime; no private key is ever committed.
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use invoiceshield_governor::trust::key_id;
use invoiceshield_governor::{Governor, Master, TrustConfig, TrustVerifier, Verdict};
use rand_core::OsRng;
use serde_json::{json, Value};
use std::sync::OnceLock;

const DOC: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const NOW: i64 = 1_800_000_000;
const CLEAN: &str = r#"{"signals":[]}"#;

fn master() -> Master {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dataset/fixtures/master.json"
    );
    Master::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn good_evidence() -> Value {
    json!({"text_layer_gstin": "24ZZACS4821A1ZX", "sender_domain": "sharmaofficesupplies.in",
           "printed_bank_account": "23276810695553", "printed_ifsc": "BARB0MAKVAD",
           "all_printed_accounts": ["23276810695553"], "all_printed_ifscs": ["BARB0MAKVAD"],
           "has_text_layer": true, "text_source": "text_layer"})
}

fn nonce() -> String {
    let mut b = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut OsRng, &mut b);
    hex::encode(b)
}

/// Canonical JSON: serde_json's Value map is a BTreeMap, so keys come out sorted, no whitespace.
fn payload(
    sk: &SigningKey,
    doc: &str,
    issued_at: i64,
    nonce: &str,
    status: &str,
    evidence: Value,
) -> String {
    payload_sandbox(
        sk,
        doc,
        issued_at,
        nonce,
        status,
        evidence,
        json!({"network_namespace": true}),
    )
}

fn payload_sandbox(
    sk: &SigningKey,
    doc: &str,
    issued_at: i64,
    nonce: &str,
    status: &str,
    evidence: Value,
    sandbox: Value,
) -> String {
    serde_json::to_string(&json!({
        "v": 1, "key_id": key_id(&sk.verifying_key()), "doc_sha256": doc, "extractor_version": "test",
        "issued_at": issued_at, "nonce": nonce, "status": status, "evidence": evidence, "sandbox": sandbox,
    })).unwrap()
}

/// The checker's own key (separate from the extractor's), generated once per test binary.
fn checker_key() -> &'static SigningKey {
    static K: OnceLock<SigningKey> = OnceLock::new();
    K.get_or_init(|| SigningKey::generate(&mut OsRng))
}

fn checker_payload(ck: &SigningKey, doc: &str, evidence_nonce: &str, issued_at: i64, raw: &str) -> String {
    serde_json::to_string(&json!({
        "v": 1, "kind": "checker", "key_id": key_id(&ck.verifying_key()), "doc_sha256": doc,
        "evidence_nonce": evidence_nonce, "issued_at": issued_at, "raw": raw,
    }))
    .unwrap()
}

/// A checker envelope bound to the evidence envelope `env` (its document and nonce).
fn signed_checker_for(env: &Value, raw: &str) -> Value {
    let p: Value = env
        .get("payload")
        .and_then(|p| p.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let doc = p.get("doc_sha256").and_then(|v| v.as_str()).unwrap_or(DOC);
    let n = p.get("nonce").and_then(|v| v.as_str()).unwrap_or("");
    let issued = p.get("issued_at").and_then(|v| v.as_i64()).unwrap_or(NOW);
    envelope(checker_key(), &checker_payload(checker_key(), doc, n, issued, raw))
}

fn trust_cfg(sk: &SigningKey) -> TrustConfig {
    TrustConfig::new(vec![sk.verifying_key()]).with_checker_keys(vec![checker_key().verifying_key()])
}

fn envelope(sk: &SigningKey, payload: &str) -> Value {
    json!({"payload": payload, "sig": base64::engine::general_purpose::STANDARD.encode(sk.sign(payload.as_bytes()).to_bytes())})
}

fn body(doc: Option<&str>, env: Option<Value>, raw_evidence: Option<Value>) -> String {
    let mut p = json!({"vendor_id": "V001", "invoice_no": "SOS/26-27/0231", "amount_paise": 4_500_000, "po_ref": "PO-2026-0101"});
    if let Some(d) = doc {
        p["document_sha256"] = json!(d);
    }
    let mut b = json!({"proposal": p});
    if let Some(e) = env {
        b["signed_checker"] = signed_checker_for(&e, CLEAN);
        b["signed_evidence"] = e;
    } else {
        b["checker_raw"] = json!(CLEAN); // dev unsigned path only
    }
    if let Some(e) = raw_evidence {
        b["evidence"] = e;
    }
    b.to_string()
}

fn setup() -> (Governor, TrustVerifier, SigningKey) {
    let sk = SigningKey::generate(&mut OsRng);
    (
        Governor::new(master()),
        TrustVerifier::new(trust_cfg(&sk)),
        sk,
    )
}

fn rules(d: &invoiceshield_governor::Decision) -> Vec<String> {
    d.reasons.iter().map(|r| r.rule.clone()).collect()
}

#[test]
fn valid_signed_evidence_passes() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(
        &sk,
        &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()),
    );
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn signed_evidence_still_reaches_rules() {
    // A signed tampered bank box is still a tamper alert: signing proves origin, not honesty of the vendor.
    let (mut g, mut t, sk) = setup();
    let mut e = good_evidence();
    e["printed_bank_account"] = json!("50200012345678");
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", e));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW, &mut t);
    assert!(d.tamper_alert);
    assert_ne!(d.verdict, Verdict::Allow);
}

#[test]
fn unsigned_forged_evidence_is_review_never_allow() {
    // The attack: a compromised agent sends its own evidence (empty bank box, right GSTIN).
    let (mut g, mut t, _) = setup();
    let forged = json!({"text_layer_gstin": "24ZZACS4821A1ZX", "sender_domain": "sharmaofficesupplies.in", "has_text_layer": true});
    let d = g.evaluate_signed_json(&body(Some(DOC), None, Some(forged)), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(rules(&d), vec!["evidence_unsigned"]);
    assert!(d.payee.is_none());
}

#[test]
fn forged_signature_blocks() {
    let (mut g, mut t, sk) = setup();
    let attacker = SigningKey::generate(&mut OsRng);
    // attacker signs with its own key but claims the real key id
    let p = payload(
        &sk,
        DOC,
        NOW,
        &nonce(),
        "ok",
        json!({"text_layer_gstin": "24ZZACS4821A1ZX", "has_text_layer": true}),
    );
    let d = g.evaluate_signed_json(
        &body(Some(DOC), Some(envelope(&attacker, &p)), None),
        NOW,
        NOW,
        &mut t,
    );
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(rules(&d), vec!["evidence_signature_invalid"]);
}

#[test]
fn edited_payload_blocks() {
    let (mut g, mut t, sk) = setup();
    let p = payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence());
    let mut env = envelope(&sk, &p);
    env["payload"] = json!(p.replace("23276810695553", "00000000000000"));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(rules(&d), vec!["evidence_signature_invalid"]);
}

#[test]
fn unknown_key_and_garbage_envelopes_block() {
    let (mut g, mut t, _) = setup();
    let other = SigningKey::generate(&mut OsRng);
    let env = envelope(
        &other,
        &payload(&other, DOC, NOW, &nonce(), "ok", good_evidence()),
    );
    for e in [
        env,
        json!({"payload": "{}", "sig": "!!"}),
        json!({"payload": 3}),
        json!("x"),
        json!({"payload": "not json", "sig": ""}),
    ] {
        let d = g.evaluate_signed_json(&body(Some(DOC), Some(e), None), NOW, NOW, &mut t);
        assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
    }
}

#[test]
fn tampered_document_hash_blocks() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(
        &sk,
        &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()),
    );
    let other = "0".repeat(64);
    let d = g.evaluate_signed_json(&body(Some(&other), Some(env), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(rules(&d), vec!["document_hash_mismatch"]);
}

#[test]
fn missing_document_hash_reviews() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(
        &sk,
        &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()),
    );
    let d = g.evaluate_signed_json(&body(None, Some(env), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(rules(&d), vec!["document_hash_missing"]);
}

#[test]
fn replay_blocks() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(
        &sk,
        &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()),
    );
    let b = body(Some(DOC), Some(env), None);
    assert_eq!(
        g.evaluate_signed_json(&b, NOW, NOW, &mut t).verdict,
        Verdict::Allow
    );
    g.reset_state(); // even after an eval ledger reset the nonce stays spent
    let d = g.evaluate_signed_json(&b, NOW, NOW + 1, &mut t);
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(rules(&d), vec!["evidence_replayed"]);
}

#[test]
fn stale_and_future_evidence_review() {
    let (mut g, mut t, sk) = setup();
    let old = envelope(
        &sk,
        &payload(&sk, DOC, NOW - 301, &nonce(), "ok", good_evidence()),
    );
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(old), None), NOW, NOW, &mut t);
    assert_eq!(
        (d.verdict, rules(&d)),
        (Verdict::Review, vec!["evidence_stale".to_string()])
    );
    let fut = envelope(
        &sk,
        &payload(&sk, DOC, NOW + 31, &nonce(), "ok", good_evidence()),
    );
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(fut), None), NOW, NOW, &mut t);
    assert_eq!(
        (d.verdict, rules(&d)),
        (Verdict::Review, vec!["evidence_from_future".to_string()])
    );
}

#[test]
fn extraction_failure_reviews() {
    let (mut g, mut t, sk) = setup();
    let p = serde_json::to_string(
        &json!({"v": 1, "key_id": key_id(&sk.verifying_key()), "doc_sha256": DOC,
        "extractor_version": "test", "issued_at": NOW, "nonce": nonce(), "status": "failed",
        "failure": "wall_clock_timeout"}),
    )
    .unwrap();
    let d = g.evaluate_signed_json(
        &body(Some(DOC), Some(envelope(&sk, &p)), None),
        NOW,
        NOW,
        &mut t,
    );
    assert_eq!(
        (d.verdict, rules(&d)),
        (Verdict::Review, vec!["extraction_failed".to_string()])
    );
}

#[test]
fn signed_payload_with_unknown_evidence_field_reviews() {
    let (mut g, mut t, sk) = setup();
    let mut e = good_evidence();
    e["surprise"] = json!(1);
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", e));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW, &mut t);
    assert_eq!(
        (d.verdict, rules(&d)),
        (
            Verdict::Review,
            vec!["evidence_payload_invalid".to_string()]
        )
    );
}

#[test]
fn raw_evidence_next_to_envelope_is_ignored() {
    let (mut g, mut t, sk) = setup();
    let mut e = good_evidence();
    e["printed_bank_account"] = json!("50200012345678"); // the document really shows another account
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", e));
    let d = g.evaluate_signed_json(
        &body(Some(DOC), Some(env), Some(good_evidence())),
        NOW,
        NOW,
        &mut t,
    );
    assert!(
        d.tamper_alert,
        "caller's clean raw evidence must not override the signed values"
    );
}

#[test]
fn dev_mode_accepts_unsigned_only_when_enabled() {
    let (mut g, _, sk) = setup();
    let mut cfg = trust_cfg(&sk);
    cfg.dev_unsigned = true;
    let mut t = TrustVerifier::new(cfg);
    let d = g.evaluate_signed_json(&body(None, None, Some(good_evidence())), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn nonce_store_prunes_expired_entries() {
    let (mut g, mut t, sk) = setup();
    for i in 0..5 {
        let env = envelope(
            &sk,
            &payload(&sk, DOC, NOW + i * 100, &nonce(), "ok", good_evidence()),
        );
        g.reset_state();
        g.evaluate_signed_json(
            &body(Some(DOC), Some(env), None),
            NOW,
            NOW + i * 100,
            &mut t,
        );
    }
    assert!(t.nonces_held() <= 4, "held {}", t.nonces_held());
}

#[test]
fn nonce_store_full_fails_closed() {
    let (mut g, _, sk) = setup();
    let mut cfg = trust_cfg(&sk);
    cfg.max_nonces = 1;
    let mut t = TrustVerifier::new(cfg);
    let mk = || {
        body(
            Some(DOC),
            Some(envelope(
                &sk,
                &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()),
            )),
            None,
        )
    };
    assert_eq!(
        g.evaluate_signed_json(&mk(), NOW, NOW, &mut t).verdict,
        Verdict::Allow
    );
    g.reset_state();
    let d = g.evaluate_signed_json(&mk(), NOW, NOW, &mut t);
    assert_eq!(
        (d.verdict, rules(&d)),
        (
            Verdict::Review,
            vec!["evidence_nonce_store_full".to_string()]
        )
    );
}

#[test]
fn key_parsing_accepts_hex_and_base64() {
    let sk = SigningKey::generate(&mut OsRng);
    let vk = sk.verifying_key();
    let hex_k = hex::encode(vk.as_bytes());
    let b64_k = base64::engine::general_purpose::STANDARD.encode(vk.as_bytes());
    let keys = TrustConfig::parse_keys(&format!("{hex_k}, {b64_k}")).unwrap();
    assert_eq!(keys.len(), 2);
    assert!(TrustConfig::parse_keys("abcd").is_err());
}

// ------------------------------------------------- post-review hardening (docs/REVIEW_FIXES.md)

fn signals(d: &invoiceshield_governor::Decision) -> Vec<String> {
    d.signals.iter().map(|s| s.kind.clone()).collect()
}

/// Signed evidence + an arbitrary checker field, for the checker-binding tests.
fn body_with_checker(env: Value, checker: Option<Value>, checker_raw: Option<&str>) -> String {
    let p = json!({"vendor_id": "V001", "invoice_no": "SOS/26-27/0231", "amount_paise": 4_500_000,
                   "po_ref": "PO-2026-0101", "document_sha256": DOC});
    let mut b = json!({"proposal": p, "signed_evidence": env});
    if let Some(c) = checker {
        b["signed_checker"] = c;
    }
    if let Some(r) = checker_raw {
        b["checker_raw"] = json!(r);
    }
    b.to_string()
}

fn fresh_env(sk: &SigningKey) -> Value {
    envelope(sk, &payload(sk, DOC, NOW, &nonce(), "ok", good_evidence()))
}

#[test]
fn checker_dropped_by_agent_reviews() {
    // Finding 1: a compromised agent omits the checker entirely.
    let (mut g, mut t, sk) = setup();
    let d = g.evaluate_signed_json(&body_with_checker(fresh_env(&sk), None, None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(signals(&d), vec!["checker_missing"]);
}

#[test]
fn unsigned_empty_checker_from_agent_reviews_never_allow() {
    // The exact attack in the review: {"signals":[]} in the agent's body.
    let (mut g, mut t, sk) = setup();
    let d = g.evaluate_signed_json(
        &body_with_checker(fresh_env(&sk), None, Some(CLEAN)),
        NOW,
        NOW,
        &mut t,
    );
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(signals(&d), vec!["checker_unsigned"]);
}

#[test]
fn unsigned_checker_raw_next_to_signed_checker_is_ignored() {
    // A signed BLOCK signal cannot be overridden by an unsigned clean checker_raw.
    let (mut g, mut t, sk) = setup();
    let env = fresh_env(&sk);
    let block = r#"{"signals":[{"kind":"injection_suspected","severity":"block","detail":"x"}]}"#;
    let c = signed_checker_for(&env, block);
    let d = g.evaluate_signed_json(&body_with_checker(env, Some(c), Some(CLEAN)), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(signals(&d), vec!["injection_suspected"]);
}

#[test]
fn forged_checker_signature_reviews() {
    let (mut g, mut t, sk) = setup();
    let env = fresh_env(&sk);
    let n = serde_json::from_str::<Value>(env["payload"].as_str().unwrap()).unwrap()["nonce"]
        .as_str()
        .unwrap()
        .to_string();
    let attacker = SigningKey::generate(&mut OsRng);
    // claims the real checker key id, signed by the attacker
    let forged = envelope(&attacker, &checker_payload(checker_key(), DOC, &n, NOW, CLEAN));
    let d = g.evaluate_signed_json(&body_with_checker(env, Some(forged), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(signals(&d), vec!["checker_signature_invalid"]);
}

#[test]
fn checker_signed_by_the_extractor_key_is_not_a_checker() {
    let (mut g, mut t, sk) = setup();
    let env = fresh_env(&sk);
    let n = serde_json::from_str::<Value>(env["payload"].as_str().unwrap()).unwrap()["nonce"]
        .as_str()
        .unwrap()
        .to_string();
    let c = envelope(&sk, &checker_payload(&sk, DOC, &n, NOW, CLEAN));
    let d = g.evaluate_signed_json(&body_with_checker(env, Some(c), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(signals(&d), vec!["checker_signature_invalid"]);
}

#[test]
fn edited_checker_payload_reviews() {
    // The agent strips a BLOCK signal out of a genuinely signed checker report.
    let (mut g, mut t, sk) = setup();
    let env = fresh_env(&sk);
    let block = r#"{"signals":[{"kind":"injection_suspected","severity":"block","detail":"x"}]}"#;
    let mut c = signed_checker_for(&env, block);
    let edited = c["payload"]
        .as_str()
        .unwrap()
        .replace(r#"{\"kind\":\"injection_suspected\",\"severity\":\"block\",\"detail\":\"x\"}"#, "");
    assert_ne!(edited, c["payload"].as_str().unwrap(), "the edit must change the payload");
    c["payload"] = json!(edited);
    let d = g.evaluate_signed_json(&body_with_checker(env, Some(c), None), NOW, NOW, &mut t);
    assert_ne!(d.verdict, Verdict::Allow);
    assert_eq!(signals(&d), vec!["checker_signature_invalid"]);
}

#[test]
fn replayed_checker_payload_reviews() {
    // A clean checker report for request A is re-attached to fresh evidence B (same document).
    let (mut g, mut t, sk) = setup();
    let env_a = fresh_env(&sk);
    let c_a = signed_checker_for(&env_a, CLEAN);
    let d = g.evaluate_signed_json(&body_with_checker(env_a, Some(c_a.clone()), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    g.reset_state();
    let d = g.evaluate_signed_json(&body_with_checker(fresh_env(&sk), Some(c_a), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(signals(&d), vec!["checker_nonce_mismatch"]);
}

#[test]
fn checker_for_another_document_reviews() {
    let (mut g, mut t, sk) = setup();
    let env = fresh_env(&sk);
    let n = serde_json::from_str::<Value>(env["payload"].as_str().unwrap()).unwrap()["nonce"]
        .as_str()
        .unwrap()
        .to_string();
    let other = "1".repeat(64);
    let c = envelope(checker_key(), &checker_payload(checker_key(), &other, &n, NOW, CLEAN));
    let d = g.evaluate_signed_json(&body_with_checker(env, Some(c), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(signals(&d), vec!["checker_document_mismatch"]);
}

#[test]
fn stale_checker_and_bad_checker_schema_review() {
    let (mut g, mut t, sk) = setup();
    for (issued, kind, want) in [
        (NOW - 301, "checker", "checker_stale"),
        (NOW + 31, "checker", "checker_stale"),
        (NOW, "extractor", "checker_payload_invalid"),
    ] {
        let env = fresh_env(&sk);
        let n = serde_json::from_str::<Value>(env["payload"].as_str().unwrap()).unwrap()["nonce"]
            .as_str()
            .unwrap()
            .to_string();
        let p = serde_json::to_string(&json!({
            "v": 1, "kind": kind, "key_id": key_id(&checker_key().verifying_key()), "doc_sha256": DOC,
            "evidence_nonce": n, "issued_at": issued, "raw": CLEAN,
        }))
        .unwrap();
        let c = envelope(checker_key(), &p);
        g.reset_state();
        let d = g.evaluate_signed_json(&body_with_checker(env, Some(c), None), NOW, NOW, &mut t);
        assert_eq!(d.verdict, Verdict::Review, "{want}");
        assert_eq!(signals(&d), vec![want.to_string()]);
    }
    for c in [json!("x"), json!({"payload": "{}", "sig": "!!"}), json!({"payload": 3})] {
        g.reset_state();
        let d = g.evaluate_signed_json(&body_with_checker(fresh_env(&sk), Some(c), None), NOW, NOW, &mut t);
        assert_eq!(d.verdict, Verdict::Review);
        assert_eq!(signals(&d), vec!["checker_signature_invalid"]);
    }
}

#[test]
fn legacy_unsigned_checker_only_with_explicit_flag() {
    let (mut g, _, sk) = setup();
    let mut cfg = trust_cfg(&sk);
    cfg.checker_unsigned_ok = true;
    let mut t = TrustVerifier::new(cfg);
    let d = g.evaluate_signed_json(
        &body_with_checker(fresh_env(&sk), None, Some(CLEAN)),
        NOW,
        NOW,
        &mut t,
    );
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.signals);
}

#[test]
fn no_checker_key_configured_fails_closed() {
    let (mut g, _, sk) = setup();
    let mut t = TrustVerifier::new(TrustConfig::new(vec![sk.verifying_key()]));
    let env = fresh_env(&sk);
    let c = signed_checker_for(&env, CLEAN);
    let d = g.evaluate_signed_json(&body_with_checker(env, Some(c), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(signals(&d), vec!["checker_signature_invalid"]);
}

#[test]
fn sandbox_without_network_namespace_reviews_by_default() {
    // Finding 2: the extractor signed network_namespace=false (the Python socket-patch fallback).
    let (mut g, mut t, sk) = setup();
    for sb in [json!({"network_namespace": false}), json!({}), Value::Null, json!({"network_namespace": "true"})] {
        let env = envelope(
            &sk,
            &payload_sandbox(&sk, DOC, NOW, &nonce(), "ok", good_evidence(), sb.clone()),
        );
        g.reset_state();
        let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW, &mut t);
        assert_eq!(d.verdict, Verdict::Review, "{sb}");
        assert_eq!(rules(&d), vec!["sandbox_network_not_isolated"], "{sb}");
    }
}

#[test]
fn sandbox_without_network_namespace_allowed_only_when_configured() {
    let (mut g, _, sk) = setup();
    let mut cfg = trust_cfg(&sk);
    cfg.allow_no_netns = true;
    let mut t = TrustVerifier::new(cfg);
    let env = envelope(
        &sk,
        &payload_sandbox(&sk, DOC, NOW, &nonce(), "ok", good_evidence(), json!({"network_namespace": false})),
    );
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn verify_accept_returns_the_nonce_to_persist() {
    let (_, mut t, sk) = setup();
    let n = nonce();
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &n, "ok", good_evidence()));
    let (_, acc) = t.verify_accept(&body(Some(DOC), Some(env), None), NOW).unwrap();
    let acc = acc.expect("signed path returns the accepted nonce");
    assert_eq!((acc.nonce, acc.issued_at), (n, NOW));
}
