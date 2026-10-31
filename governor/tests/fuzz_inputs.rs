//! CI-sized fuzzing (proptest) of everything that parses untrusted input on the Rust side:
//! normalizers, the raw-request path, and the signed-evidence path. Properties: no panic,
//! idempotent normalizers, and junk can never produce ALLOW.
//! Case count: PROPTEST_CASES (default 256 per property here) — raise it for a longer run.
use ed25519_dalek::SigningKey;
use invoiceshield_governor::{normalize, Governor, Master, TrustConfig, TrustVerifier, Verdict};
use proptest::prelude::*;
use rand_core::OsRng;

fn master() -> Master {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dataset/fixtures/master.json"
    );
    Master::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn cfg() -> ProptestConfig {
    ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256),
        ..ProptestConfig::default()
    }
}

/// JSON-shaped strings biased toward the real request shape, so the fuzzer gets past the parser.
fn json_value() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        "\\PC{0,24}".prop_map(serde_json::Value::from),
        prop::sample::select(vec![
            "V001",
            "PO-2026-0101",
            "24ZZACS4821A1ZX",
            "sharmaofficesupplies.in",
            "ok",
            "failed"
        ])
        .prop_map(serde_json::Value::from),
    ];
    leaf.prop_recursive(3, 24, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
            prop::collection::btree_map(
                prop::sample::select(vec![
                    "proposal",
                    "evidence",
                    "signed_evidence",
                    "checker_raw",
                    "payload",
                    "sig",
                    "vendor_id",
                    "invoice_no",
                    "amount_paise",
                    "po_ref",
                    "document_sha256",
                    "key_id",
                    "nonce",
                    "issued_at",
                    "v",
                    "status",
                    "text_layer_gstin",
                    "printed_bank_account",
                    "x"
                ])
                .prop_map(String::from),
                inner,
                0..6
            )
            .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    fn normalizers_total_and_idempotent(s in "\\PC{0,64}") {
        let a = normalize::invoice_no(&s);
        prop_assert_eq!(normalize::invoice_no(&a), a.clone());
        let g = normalize::gstin(&s);
        prop_assert_eq!(normalize::gstin(&g), g.clone());
        let d = normalize::domain(&s);
        let _ = normalize::domain(&d);
    }

    #[test]
    fn arbitrary_bytes_never_allow_in_strict_mode(s in "\\PC{0,256}") {
        let sk = SigningKey::generate(&mut OsRng);
        let mut g = Governor::new(master());
        let mut t = TrustVerifier::new(TrustConfig::new(vec![sk.verifying_key()]));
        let d = g.evaluate_signed_json(&s, 0, 1_800_000_000, &mut t);
        prop_assert_ne!(d.verdict, Verdict::Allow);
    }

    #[test]
    fn structured_junk_never_allows_without_signature(v in json_value()) {
        let sk = SigningKey::generate(&mut OsRng);
        let mut g = Governor::new(master());
        let mut t = TrustVerifier::new(TrustConfig::new(vec![sk.verifying_key()]));
        let d = g.evaluate_signed_json(&v.to_string(), 0, 1_800_000_000, &mut t);
        prop_assert_ne!(d.verdict, Verdict::Allow);
    }

    #[test]
    fn dev_path_parser_never_panics(v in json_value()) {
        // Old unsigned path (dev mode): may allow well-formed requests, must never panic.
        let mut g = Governor::new(master());
        let _ = g.evaluate_json(&v.to_string(), 0);
    }
}

#[test]
fn invoice_no_composed_and_decomposed_greek_share_a_key() {
    // Regression from the fuzz run: "ΐ" and "Ϊ́" must not be two different duplicate keys.
    assert_eq!(
        normalize::invoice_no("INV-ΐ-1"),
        normalize::invoice_no("INV-\u{3AA}\u{301}-1")
    );
}
