//! Meta photo-witness (feat/meta-vision): signals from the Spark Open witness reach the governor only
//! through the signed checker report (shield/vision_assess.py `checker_raw`). They can escalate a
//! verdict, never downgrade it, and a malformed or "allow" signal fails closed to REVIEW.
use invoiceshield_governor::{merge, parse_checker_signals, Severity, Verdict};

const KINDS: [&str; 4] = [
    "vision_bank_mismatch",
    "vision_payee_lookalike",
    "vision_injected_instruction",
    "vision_amount_mismatch",
];

fn report(sigs: &[(&str, &str)]) -> String {
    let items: Vec<String> = sigs
        .iter()
        .map(|(k, s)| format!(r#"{{"kind":"{k}","severity":"{s}","detail":"seen on the photo"}}"#))
        .collect();
    format!(r#"{{"signals":[{}]}}"#, items.join(","))
}

#[test]
fn vision_signals_never_downgrade_any_base_verdict() {
    for base in [Verdict::Allow, Verdict::Review, Verdict::Block] {
        for k in KINDS {
            for sev in ["review", "block"] {
                let s = parse_checker_signals(&report(&[(k, sev)]));
                assert_eq!(s.len(), 1);
                assert_eq!(s[0].kind, k);
                let v = merge(base, &s);
                assert!(v >= base, "{k}/{sev} lowered {base:?} to {v:?}");
                assert!(v >= Verdict::Review, "a vision signal must at least hold the payment");
            }
        }
    }
}

#[test]
fn vision_block_signal_blocks() {
    let s = parse_checker_signals(&report(&[("vision_bank_mismatch", "block")]));
    assert_eq!(s[0].severity, Severity::Block);
    assert_eq!(merge(Verdict::Allow, &s), Verdict::Block);
}

#[test]
fn empty_vision_report_leaves_verdict_unchanged() {
    let s = parse_checker_signals(r#"{"signals":[]}"#);
    for base in [Verdict::Allow, Verdict::Review, Verdict::Block] {
        assert_eq!(merge(base, &s), base);
    }
}

#[test]
fn a_vision_signal_cannot_ask_for_allow_and_malformed_output_fails_closed() {
    for raw in [
        report(&[("vision_bank_mismatch", "allow")]),
        r#"{"signals":[{"kind":"vision_payee_lookalike","severity":"block"}],"verdict":"allow"}"#.to_string(),
        "not json".to_string(),
        r#"{"signals":[{"kind":"vision_bank_mismatch"}]}"#.to_string(),
    ] {
        let s = parse_checker_signals(&raw);
        assert_eq!(s.len(), 1, "{raw}");
        assert_eq!(s[0].kind, "checker_unparseable", "{raw}");
        assert_eq!(merge(Verdict::Allow, &s), Verdict::Review, "{raw}");
        assert_eq!(merge(Verdict::Block, &s), Verdict::Block, "{raw}");
    }
}
