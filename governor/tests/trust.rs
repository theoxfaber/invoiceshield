//! Provenance tests: only extractor-signed evidence reaches the rules.
//! Keys are generated at runtime; no private key is ever committed.
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use invoiceshield_governor::trust::key_id;
use invoiceshield_governor::{Governor, Master, TrustConfig, TrustVerifier, Verdict};
use rand_core::OsRng;
use serde_json::{json, Value};

const DOC: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const NOW: i64 = 1_800_000_000;
const CLEAN: &str = r#"{"signals":[]}"#;

fn master() -> Master {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../dataset/fixtures/master.json");
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
fn payload(sk: &SigningKey, doc: &str, issued_at: i64, nonce: &str, status: &str, evidence: Value) -> String {
    serde_json::to_string(&json!({
        "v": 1, "key_id": key_id(&sk.verifying_key()), "doc_sha256": doc, "extractor_version": "test",
        "issued_at": issued_at, "nonce": nonce, "status": status, "evidence": evidence,
    })).unwrap()
}

fn envelope(sk: &SigningKey, payload: &str) -> Value {
    json!({"payload": payload, "sig": base64::engine::general_purpose::STANDARD.encode(sk.sign(payload.as_bytes()).to_bytes())})
}

fn body(doc: Option<&str>, env: Option<Value>, raw_evidence: Option<Value>) -> String {
    let mut p = json!({"vendor_id": "V001", "invoice_no": "SOS/26-27/0231", "amount_paise": 4_500_000, "po_ref": "PO-2026-0101"});
    if let Some(d) = doc { p["document_sha256"] = json!(d); }
    let mut b = json!({"proposal": p, "checker_raw": CLEAN});
    if let Some(e) = env { b["signed_evidence"] = e; }
    if let Some(e) = raw_evidence { b["evidence"] = e; }
    b.to_string()
}

fn setup() -> (Governor, TrustVerifier, SigningKey) {
    let sk = SigningKey::generate(&mut OsRng);
    (Governor::new(master()), TrustVerifier::new(TrustConfig::new(vec![sk.verifying_key()])), sk)
}

fn rules(d: &invoiceshield_governor::Decision) -> Vec<String> { d.reasons.iter().map(|r| r.rule.clone()).collect() }

#[test]
fn valid_signed_evidence_passes() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()));
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
    let p = payload(&sk, DOC, NOW, &nonce(), "ok", json!({"text_layer_gstin": "24ZZACS4821A1ZX", "has_text_layer": true}));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(envelope(&attacker, &p)), None), NOW, NOW, &mut t);
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
    let env = envelope(&other, &payload(&other, DOC, NOW, &nonce(), "ok", good_evidence()));
    for e in [env, json!({"payload": "{}", "sig": "!!"}), json!({"payload": 3}), json!("x"),
              json!({"payload": "not json", "sig": ""})] {
        let d = g.evaluate_signed_json(&body(Some(DOC), Some(e), None), NOW, NOW, &mut t);
        assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
    }
}

#[test]
fn tampered_document_hash_blocks() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()));
    let other = "0".repeat(64);
    let d = g.evaluate_signed_json(&body(Some(&other), Some(env), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(rules(&d), vec!["document_hash_mismatch"]);
}

#[test]
fn missing_document_hash_reviews() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()));
    let d = g.evaluate_signed_json(&body(None, Some(env), None), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Review);
    assert_eq!(rules(&d), vec!["document_hash_missing"]);
}

#[test]
fn replay_blocks() {
    let (mut g, mut t, sk) = setup();
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()));
    let b = body(Some(DOC), Some(env), None);
    assert_eq!(g.evaluate_signed_json(&b, NOW, NOW, &mut t).verdict, Verdict::Allow);
    g.reset_state(); // even after an eval ledger reset the nonce stays spent
    let d = g.evaluate_signed_json(&b, NOW, NOW + 1, &mut t);
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(rules(&d), vec!["evidence_replayed"]);
}

#[test]
fn stale_and_future_evidence_review() {
    let (mut g, mut t, sk) = setup();
    let old = envelope(&sk, &payload(&sk, DOC, NOW - 301, &nonce(), "ok", good_evidence()));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(old), None), NOW, NOW, &mut t);
    assert_eq!((d.verdict, rules(&d)), (Verdict::Review, vec!["evidence_stale".to_string()]));
    let fut = envelope(&sk, &payload(&sk, DOC, NOW + 31, &nonce(), "ok", good_evidence()));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(fut), None), NOW, NOW, &mut t);
    assert_eq!((d.verdict, rules(&d)), (Verdict::Review, vec!["evidence_from_future".to_string()]));
}

#[test]
fn extraction_failure_reviews() {
    let (mut g, mut t, sk) = setup();
    let p = serde_json::to_string(&json!({"v": 1, "key_id": key_id(&sk.verifying_key()), "doc_sha256": DOC,
        "extractor_version": "test", "issued_at": NOW, "nonce": nonce(), "status": "failed",
        "failure": "wall_clock_timeout"})).unwrap();
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(envelope(&sk, &p)), None), NOW, NOW, &mut t);
    assert_eq!((d.verdict, rules(&d)), (Verdict::Review, vec!["extraction_failed".to_string()]));
}

#[test]
fn signed_payload_with_unknown_evidence_field_reviews() {
    let (mut g, mut t, sk) = setup();
    let mut e = good_evidence();
    e["surprise"] = json!(1);
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", e));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW, &mut t);
    assert_eq!((d.verdict, rules(&d)), (Verdict::Review, vec!["evidence_payload_invalid".to_string()]));
}

#[test]
fn raw_evidence_next_to_envelope_is_ignored() {
    let (mut g, mut t, sk) = setup();
    let mut e = good_evidence();
    e["printed_bank_account"] = json!("50200012345678"); // the document really shows another account
    let env = envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", e));
    let d = g.evaluate_signed_json(&body(Some(DOC), Some(env), Some(good_evidence())), NOW, NOW, &mut t);
    assert!(d.tamper_alert, "caller's clean raw evidence must not override the signed values");
}

#[test]
fn dev_mode_accepts_unsigned_only_when_enabled() {
    let (mut g, _, sk) = setup();
    let mut cfg = TrustConfig::new(vec![sk.verifying_key()]);
    cfg.dev_unsigned = true;
    let mut t = TrustVerifier::new(cfg);
    let d = g.evaluate_signed_json(&body(None, None, Some(good_evidence())), NOW, NOW, &mut t);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn nonce_store_prunes_expired_entries() {
    let (mut g, mut t, sk) = setup();
    for i in 0..5 {
        let env = envelope(&sk, &payload(&sk, DOC, NOW + i * 100, &nonce(), "ok", good_evidence()));
        g.reset_state();
        g.evaluate_signed_json(&body(Some(DOC), Some(env), None), NOW, NOW + i * 100, &mut t);
    }
    assert!(t.nonces_held() <= 4, "held {}", t.nonces_held());
}

#[test]
fn nonce_store_full_fails_closed() {
    let (mut g, _, sk) = setup();
    let mut cfg = TrustConfig::new(vec![sk.verifying_key()]);
    cfg.max_nonces = 1;
    let mut t = TrustVerifier::new(cfg);
    let mk = || body(Some(DOC), Some(envelope(&sk, &payload(&sk, DOC, NOW, &nonce(), "ok", good_evidence()))), None);
    assert_eq!(g.evaluate_signed_json(&mk(), NOW, NOW, &mut t).verdict, Verdict::Allow);
    g.reset_state();
    let d = g.evaluate_signed_json(&mk(), NOW, NOW, &mut t);
    assert_eq!((d.verdict, rules(&d)), (Verdict::Review, vec!["evidence_nonce_store_full".to_string()]));
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
