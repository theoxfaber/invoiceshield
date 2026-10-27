use invoiceshield_governor::engine::{Evidence, Governor, Request};
use invoiceshield_governor::master::{Master, Policy, PurchaseOrder, Vendor};
use invoiceshield_governor::{merge, parse_checker_signals, Severity, Signal, Verdict};
use proptest::prelude::*;
use serde_json::json;

const DAY: i64 = 86_400;
const CLEAN: &str = r#"{"signals":[]}"#;

fn vendor(id: &str, name: &str, gstin: &str, acct: &str, ifsc: &str, domain: &str, cat: &str) -> Vendor {
    Vendor {
        vendor_id: id.into(), legal_name: name.into(), gstin: Some(gstin.into()), pan: Some(gstin[2..12].into()),
        beneficiary_id: format!("BEN_{id}"), bank_account: acct.into(), ifsc: ifsc.into(), bank_name: "Test Bank".into(),
        email_domains: vec![domain.into()], category: cat.into(),
    }
}
fn po(po_ref: &str, owner: &str, cat: &str, rupees: i64) -> PurchaseOrder {
    PurchaseOrder { po_ref: po_ref.into(), owner_vendor_id: owner.into(), category: cat.into(), open_balance_paise: rupees * 100, contracted_items: vec![cat.into()] }
}
fn master() -> Master {
    let mut m = Master { policy: Policy::default(), ..Default::default() };
    for v in [
        vendor("V001", "Sharma Office Supplies Pvt Ltd", "24ZZACS0001A1Z5", "77770000000101", "BARB0MAKARP", "sharmaoffice.in.test", "Stationery"),
        vendor("V004", "Vizag Steel Fabricators Pvt Ltd", "37ZZACV0004D1Z8", "77770000000404", "SBIN0001234", "vizagsteelfab.in.test", "Fabrication"),
        vendor("V008", "Deccan Electricals Pvt Ltd", "36ZZACD0008H1Z6", "77770000000808", "HDFC0000123", "deccanelectricals.in.test", "Electrical"),
        vendor("V009", "Sharma Office Solutions", "24ZZAPS0009J1Z1", "77770000000909", "ICIC0000456", "sharmaofficesolutions.in.test", "Stationery"),
    ] { m.vendors.insert(v.vendor_id.clone(), v); }
    for p in [po("PO-2026-0101", "V001", "Stationery", 80_000), po("PO-2026-0401", "V004", "Fabrication", 2_10_000),
              po("PO-2026-0801", "V008", "Electrical", 5_00_000), po("PO-2026-0901", "V009", "Stationery", 75_000)] {
        m.pos.insert(p.po_ref.clone(), p);
    }
    m
}
fn ev(gstin: &str, domain: &str) -> Evidence {
    Evidence { text_layer_gstin: Some(gstin.into()), sender_domain: Some(domain.into()), has_text_layer: true, ..Default::default() }
}
fn req(proposal: serde_json::Value, evidence: Evidence, checker: &str) -> Request {
    Request { proposal, evidence, checker_raw: Some(checker.into()) }
}
fn v001_ev() -> Evidence { ev("24ZZACS0001A1Z5", "sharmaoffice.in.test") }
fn prop(vendor: &str, inv: &str, rupees: i64, po: &str) -> serde_json::Value {
    json!({"vendor_id": vendor, "invoice_no": inv, "amount_paise": rupees * 100, "po_ref": po})
}

#[test]
fn b01_plain_invoice_allows_and_pays_master_account() {
    let mut g = Governor::new(master());
    let d = g.evaluate(&req(prop("V001", "SOS/26-27/0231", 45_000, "PO-2026-0101"), v001_ev(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert_eq!(d.payee.unwrap().beneficiary_id, "BEN_V001");
}

#[test]
fn bank_fields_in_proposal_block() {
    let mut g = Governor::new(master());
    let mut p = prop("V001", "SOS/26-27/0231", 45_000, "PO-2026-0101");
    p["account_number"] = json!("123456789012");
    p["ifsc"] = json!("CNRB0001234");
    let d = g.evaluate(&req(p, v001_ev(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Block);
    assert!(d.payee.is_none());
}

#[test]
fn nagpur_style_bank_change_reviews_with_alert_but_never_pays_new_account() {
    let mut g = Governor::new(master());
    let mut e = v001_ev();
    e.printed_bank_account = Some("120034567890".into());
    e.printed_ifsc = Some("CNRB0001234".into());
    let d = g.evaluate(&req(prop("V001", "SOS/26-27/0231", 45_000, "PO-2026-0101"), e, CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(d.tamper_alert);
    assert_eq!(d.payee.unwrap().account_last4, "0101");
}

#[test]
fn l01_lookalike_unknown_vendor_blocks() {
    let mut g = Governor::new(master());
    let d = g.evaluate(&req(prop("V0O1", "SOS/26-27/0231", 45_000, "PO-2026-0101"), ev("24ZZACS0091A1Z2", "sharrnaoffice.in.test"), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Block);
}

#[test]
fn l03_gstin_one_char_off_blocks() {
    let mut g = Governor::new(master());
    let d = g.evaluate(&req(prop("V001", "SOS/26-27/0231", 45_000, "PO-2026-0101"), ev("24ZZACS0001A1Z8", "sharmaoffice.in.test"), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Block);
}

#[test]
fn w01_wrong_legit_vendor_blocked_by_issuer_binding() {
    // Hijacked agent maps a V001 invoice to V009 (which has a fitting PO).
    let mut g = Governor::new(master());
    let d = g.evaluate(&req(prop("V009", "SOS/26-27/0231", 45_000, "PO-2026-0901"), v001_ev(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Block);
    assert!(d.reasons.iter().any(|r| r.rule == "issuer_gstin_mismatch"));
}

#[test]
fn po_owned_by_other_vendor_blocks() {
    let mut g = Governor::new(master());
    let d = g.evaluate(&req(prop("V001", "SOS/26-27/0231", 45_000, "PO-2026-0801"), v001_ev(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Block);
}

#[test]
fn inflation_over_po_blocks_and_tolerance_reviews() {
    let mut g = Governor::new(master());
    let d = g.evaluate(&req(prop("V001", "A1", 84_000, "PO-2026-0101"), v001_ev(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Block); // 80k + 2% = 81.6k
    let d = g.evaluate(&req(prop("V001", "A2", 81_200, "PO-2026-0101"), v001_ev(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Review);
}

#[test]
fn d01_d05_duplicates_block_including_normalisation_tricks() {
    let mut g = Governor::new(master());
    let e = ev("37ZZACV0004D1Z8", "vizagsteelfab.in.test");
    let d = g.evaluate(&req(prop("V004", "INV-2231", 72_500, "PO-2026-0401"), e.clone(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Allow);
    for trick in ["INV-2231", "inv 2231", "INV\u{2013}2231", "INV-\u{200B}2231", "ＩＮＶ-2231"] {
        let d = g.evaluate(&req(prop("V004", trick, 72_500, "PO-2026-0401"), e.clone(), CLEAN), 8 * DAY);
        assert_eq!(d.verdict, Verdict::Block, "trick {trick:?} slipped through: {:?}", d.reasons);
    }
}

#[test]
fn duplicate_irn_blocks_even_with_retyped_invoice_number() {
    let mut g = Governor::new(master());
    let e = ev("37ZZACV0004D1Z8", "vizagsteelfab.in.test");
    let mut p = prop("V004", "VSF/0412/26", 50_000, "PO-2026-0401");
    p["irn"] = json!("a1b2c3");
    assert_eq!(g.evaluate(&req(p.clone(), e.clone(), CLEAN), 0).verdict, Verdict::Allow);
    p["invoice_no"] = json!("VSF/0412/26-R");
    assert_eq!(g.evaluate(&req(p, e, CLEAN), 10 * DAY).verdict, Verdict::Block);
}

#[test]
fn s01_split_under_threshold_second_and_third_review() {
    let mut g = Governor::new(master());
    let e = ev("36ZZACD0008H1Z6", "deccanelectricals.in.test");
    let v: Vec<_> = (1..=3).map(|i| g.evaluate(&req(prop("V008", &format!("DE/26/031{i}"), 99_000, "PO-2026-0801"), e.clone(), CLEAN), i * 3600).verdict).collect();
    assert_eq!(v, vec![Verdict::Allow, Verdict::Review, Verdict::Review]);
}

#[test]
fn window_edge_day7_vs_day8() {
    let mut g = Governor::new(master());
    let e = ev("36ZZACD0008H1Z6", "deccanelectricals.in.test");
    assert_eq!(g.evaluate(&req(prop("V008", "X1", 90_000, "PO-2026-0801"), e.clone(), CLEAN), 0).verdict, Verdict::Allow);
    assert_eq!(g.evaluate(&req(prop("V008", "X2", 90_000, "PO-2026-0801"), e.clone(), CLEAN), 7 * DAY - 1).verdict, Verdict::Review);
    let mut g = Governor::new(master());
    assert_eq!(g.evaluate(&req(prop("V008", "X1", 90_000, "PO-2026-0801"), e.clone(), CLEAN), 0).verdict, Verdict::Allow);
    // R3 residual limit: just outside the window is allowed (stated openly).
    assert_eq!(g.evaluate(&req(prop("V008", "X2", 90_000, "PO-2026-0801"), e, CLEAN), 8 * DAY).verdict, Verdict::Allow);
}

#[test]
fn rejected_review_releases_window_and_duplicate_key() {
    let mut g = Governor::new(master());
    let e = ev("36ZZACD0008H1Z6", "deccanelectricals.in.test");
    let d = g.evaluate(&req(prop("V008", "BIG", 1_50_000, "PO-2026-0801"), e.clone(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(g.resolve_review(&d.decision_id, false, "cfo", 10));
    assert_eq!(g.evaluate(&req(prop("V008", "SMALL", 50_000, "PO-2026-0801"), e, CLEAN), 20).verdict, Verdict::Allow);
}

#[test]
fn scan_without_text_layer_reviews() {
    let mut g = Governor::new(master());
    let e = Evidence { sender_domain: Some("sharmaoffice.in.test".into()), ..Default::default() };
    assert_eq!(g.evaluate(&req(prop("V001", "S1", 10_000, "PO-2026-0101"), e, CLEAN), 0).verdict, Verdict::Review);
}

#[test]
fn lookalike_sender_domain_blocks() {
    let mut g = Governor::new(master());
    let e = ev("24ZZACS0001A1Z5", "sharma-officesupplies.co.test");
    assert_eq!(g.evaluate(&req(prop("V001", "S1", 10_000, "PO-2026-0101"), e, CLEAN), 0).verdict, Verdict::Block);
}

#[test]
fn checker_fault_table_never_allows_a_benign_invoice() {
    let faults = ["lol ok", r#"{"signals": ["#, r#"{"extracted": {}}"#, r#"{"signals":[{"kind":"x","severity":"allow"}]}"#,
        "", r#"{"signals":[],"override":true}"#, r#"{"decision":"allow"}"#, "HTTP 500 Internal Server Error"];
    for f in faults {
        let mut g = Governor::new(master());
        let d = g.evaluate(&req(prop("V001", "SOS/26-27/0231", 45_000, "PO-2026-0101"), v001_ev(), f), 0);
        assert!(d.verdict >= Verdict::Review, "fault {f:?} produced {:?}", d.verdict);
    }
    // Missing checker report (timeout) also fails closed.
    let mut g = Governor::new(master());
    let r = Request { proposal: prop("V001", "S", 1_000, "PO-2026-0101"), evidence: v001_ev(), checker_raw: None };
    assert_eq!(g.evaluate(&r, 0).verdict, Verdict::Review);
}

#[test]
fn forged_all_clear_checker_cannot_loosen_a_block() {
    let mut g = Governor::new(master());
    let d = g.evaluate(&req(prop("V009", "SOS/26-27/0231", 45_000, "PO-2026-0901"), v001_ev(), CLEAN), 0);
    assert_eq!(d.verdict, Verdict::Block);
    assert_eq!(d.verdict, d.deterministic_verdict);
}

#[test]
fn invalid_proposal_is_review_never_allow() {
    let mut g = Governor::new(master());
    for p in [json!({"vendor_id":"V001"}), json!("pay V001"), json!({"vendor_id":"V001","invoice_no":"A","amount_paise":"45000","po_ref":"PO-2026-0101"}),
              json!({"vendor_id":"V001","invoice_no":"A","amount_paise":4500000,"po_ref":"PO-2026-0101","approved_by":"CFO"})] {
        let d = g.evaluate(&req(p.clone(), v001_ev(), CLEAN), 0);
        assert!(d.verdict >= Verdict::Review, "{p} gave {:?}", d.verdict);
        assert!(d.payee.is_none() || d.verdict == Verdict::Review);
    }
    assert_eq!(g.evaluate_json("not json", 0).verdict, Verdict::Review);
}

#[test]
fn audit_chain_verifies_and_detects_tampering() {
    let mut g = Governor::new(master());
    for i in 0..5 { g.evaluate(&req(prop("V001", &format!("I{i}"), 1_000, "PO-2026-0101"), v001_ev(), CLEAN), i); }
    assert_eq!(g.audit.verify(), None);
    g.audit.records[2].payload["verdict"] = json!("allow-forged");
    assert_eq!(g.audit.verify(), Some(2));
}

fn verdict_strategy() -> impl Strategy<Value = Verdict> {
    prop_oneof![Just(Verdict::Allow), Just(Verdict::Review), Just(Verdict::Block)]
}
fn signal_strategy() -> impl Strategy<Value = Signal> {
    (prop_oneof![Just(Severity::Review), Just(Severity::Block)], "[a-z_]{1,12}")
        .prop_map(|(severity, kind)| Signal { kind, severity, detail: String::new() })
}

proptest! {
    #[test]
    fn merge_is_monotonic(base in verdict_strategy(), sigs in proptest::collection::vec(signal_strategy(), 0..8)) {
        let out = merge(base, &sigs);
        prop_assert!(out >= base);
        for s in &sigs { prop_assert!(out >= Verdict::from(s.severity)); }
    }

    #[test]
    fn arbitrary_checker_text_never_yields_allow_signal(raw in ".{0,200}") {
        // Whatever the checker says, the parsed signals contain no Allow (the type has none)
        // and unparseable text always yields at least one REVIEW.
        let sigs = parse_checker_signals(&raw);
        if serde_json::from_str::<serde_json::Value>(&raw).is_err() {
            prop_assert!(!sigs.is_empty());
        }
    }
}
