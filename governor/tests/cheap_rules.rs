//! Review item 3 (2026-10-31): GST head vs states, same document under a new number, credit
//! notes, checked arithmetic.
use invoiceshield_governor::engine::{content_key, state_code, Evidence, Governor, Request};
use invoiceshield_governor::master::{Master, Policy, PoLine, PurchaseOrder, Vendor};
use invoiceshield_governor::Verdict;
use serde_json::json;

const CLEAN: &str = r#"{"signals":[]}"#;
const GJ: &str = "24ZZACS0001A1Z5"; // vendor in Gujarat (24)
const MH: &str = "27ZZACM0002B1Z3"; // vendor in Maharashtra (27)
const BUYER: &str = "24ZZACB7319B1Z5";

fn vendor(id: &str, gstin: &str) -> Vendor {
    Vendor { vendor_id: id.into(), legal_name: id.into(), gstin: Some(gstin.into()), pan: None, beneficiary_id: format!("BEN_{id}"),
        bank_account: "77770000000101".into(), ifsc: "BARB0MAKARP".into(), bank_name: "Bank".into(), email_domains: vec![], category: "x".into() }
}

fn master(buyer: Option<&str>) -> Master {
    let mut m = Master { policy: Policy { buyer_gstin: buyer.map(Into::into), ..Policy::default() }, ..Default::default() };
    for (id, g, po) in [("V1", GJ, "PO-1"), ("V2", MH, "PO-2")] {
        m.vendors.insert(id.into(), vendor(id, g));
        m.pos.insert(po.into(), PurchaseOrder { po_ref: po.into(), owner_vendor_id: id.into(), category: "x".into(), currency: "INR".into(),
            open_balance_paise: 1_50_000_00, lines: vec![PoLine { item: "Goods".into(), aliases: vec![], qty: 1, unit_price_minor: 1_50_000_00, currency: None, unit: "lot".into() }] });
    }
    m
}

fn ev(gstin: &str, heads: &[&str]) -> Evidence {
    Evidence { text_layer_gstin: Some(gstin.into()), has_text_layer: true, tax_kinds: heads.iter().map(|s| s.to_string()).collect(), ..Default::default() }
}

fn eval(g: &mut Governor, v: &str, inv: &str, amount: i64, po: &str, e: Evidence, now: i64) -> invoiceshield_governor::Decision {
    let p = json!({"vendor_id": v, "invoice_no": inv, "amount_paise": amount, "po_ref": po});
    g.evaluate(&Request { proposal: p, evidence: e, checker_raw: Some(CLEAN.into()), ..Default::default() }, now)
}

fn rules(d: &invoiceshield_governor::Decision) -> Vec<String> {
    d.reasons.iter().map(|r| r.rule.clone()).collect()
}

#[test]
fn state_codes() {
    assert_eq!(state_code(GJ).as_deref(), Some("24"));
    assert_eq!(state_code("x").as_deref(), None);
}

#[test]
fn intra_state_with_igst_reviews() {
    let mut g = Governor::new(master(Some(BUYER)));
    let d = eval(&mut g, "V1", "A-1", 50_000_00, "PO-1", ev(GJ, &["IGST"]), 1_000);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(rules(&d).contains(&"tax_type_mismatch".into()));
}

#[test]
fn intra_state_with_cgst_sgst_passes() {
    let mut g = Governor::new(master(Some(BUYER)));
    let d = eval(&mut g, "V1", "A-1", 50_000_00, "PO-1", ev(GJ, &["CGST", "SGST"]), 1_000);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn inter_state_with_cgst_sgst_reviews_and_with_igst_passes() {
    let mut g = Governor::new(master(Some(BUYER)));
    let d = eval(&mut g, "V2", "B-1", 50_000_00, "PO-2", ev(MH, &["CGST", "SGST"]), 1_000);
    assert!(rules(&d).contains(&"tax_type_mismatch".into()));
    let mut g = Governor::new(master(Some(BUYER)));
    let d = eval(&mut g, "V2", "B-1", 50_000_00, "PO-2", ev(MH, &["IGST"]), 1_000);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn no_buyer_gstin_no_tax_rule() {
    let mut g = Governor::new(master(None));
    let d = eval(&mut g, "V1", "A-1", 50_000_00, "PO-1", ev(GJ, &["IGST"]), 1_000);
    assert_eq!(d.verdict, Verdict::Allow);
}

fn doc(inv: &str, date: &str) -> String {
    format!("TAX INVOICE\nVendor One Pvt Ltd\nGSTIN {GJ}\nInvoice No: {inv}\nDate: {date}\nDescription Goods supplied as per PO-1, batch 7, 120 cartons of corrugated boxes, delivered to Vadodara warehouse.\nTaxable 42,372.88\nCGST 3,813.56\nSGST 3,813.56\nGrand Total 50,000.00\n")
}

#[test]
fn same_document_under_a_bumped_number_reviews() {
    let mut g = Governor::new(master(Some(BUYER)));
    let mut e1 = ev(GJ, &["CGST", "SGST"]);
    e1.text_layer = Some(doc("VO/26/1101", "2026-10-12"));
    assert_eq!(eval(&mut g, "V1", "VO/26/1101", 50_000_00, "PO-1", e1, 1_000).verdict, Verdict::Allow);
    let mut e2 = ev(GJ, &["CGST", "SGST"]);
    e2.text_layer = Some(doc("VO/26/1102", "2026-10-12"));
    let d = eval(&mut g, "V1", "VO/26/1102", 50_000_00, "PO-1", e2, 2_000);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(rules(&d).contains(&"near_duplicate_content".into()));
}

#[test]
fn next_month_same_amount_is_not_a_content_duplicate() {
    let mut g = Governor::new(master(Some(BUYER)));
    let mut e1 = ev(GJ, &["CGST", "SGST"]);
    e1.text_layer = Some(doc("VO/26/1101", "2026-09-12"));
    eval(&mut g, "V1", "VO/26/1101", 50_000_00, "PO-1", e1, 1_000);
    let mut e2 = ev(GJ, &["CGST", "SGST"]);
    e2.text_layer = Some(doc("VO/26/1102", "2026-10-12"));
    let d = eval(&mut g, "V1", "VO/26/1102", 30_000_00, "PO-1", e2, 40 * 86_400);
    assert!(!rules(&d).contains(&"near_duplicate_content".into()), "{:?}", d.reasons);
}

#[test]
fn content_key_ignores_the_number_and_needs_text() {
    assert_eq!(content_key(Some(&doc("X/1", "d")), "X/1"), content_key(Some(&doc("X/2", "d")), "X/2"));
    assert_ne!(content_key(Some(&doc("X/1", "d1")), "X/1"), content_key(Some(&doc("X/1", "d2")), "X/1"));
    assert_eq!(content_key(Some("short"), "X/1"), None);
    assert_eq!(content_key(None, "X/1"), None);
}

#[test]
fn credit_note_with_negative_amount_reviews_not_blocks_and_writes_no_ledger() {
    let mut g = Governor::new(master(Some(BUYER)));
    let mut e = ev(GJ, &[]);
    e.document_kind = Some("credit_note".into());
    let d = eval(&mut g, "V1", "CN-19", -2_006_00, "PO-1", e, 1_000);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert_eq!(rules(&d), vec!["credit_note_not_payable".to_string()]);
    assert!(g.ledger.entries.is_empty());
    assert!(!g.resolve_review(&d.decision_id, true, "approver", 2_000), "nothing to approve into a payment");
}

#[test]
fn credit_note_with_positive_amount_is_still_not_payable() {
    let mut g = Governor::new(master(Some(BUYER)));
    let mut e = ev(GJ, &["CGST", "SGST"]);
    e.document_kind = Some("credit_note".into());
    let d = eval(&mut g, "V1", "CN-20", 2_006_00, "PO-1", e, 1_000);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(g.ledger.entries.is_empty());
}

#[test]
fn negative_amount_without_credit_note_still_blocks() {
    let mut g = Governor::new(master(Some(BUYER)));
    let d = eval(&mut g, "V1", "X-1", -2_006_00, "PO-1", ev(GJ, &[]), 1_000);
    assert_eq!(d.verdict, Verdict::Block);
}

#[test]
fn ledger_overflow_fails_closed() {
    let mut m = master(Some(BUYER));
    m.pos.get_mut("PO-1").unwrap().open_balance_paise = i64::MAX;
    m.policy.approval_threshold_paise = i64::MAX;
    m.policy.payout_cap_paise = i64::MAX;
    let mut g = Governor::new(m);
    // first payment near i64::MAX, then another: window and PO sums must not wrap
    let big = i64::MAX - 10;
    let d1 = eval(&mut g, "V1", "BIG-1", big, "PO-1", ev(GJ, &[]), 1_000);
    assert_ne!(d1.verdict, Verdict::Block, "{:?}", d1.reasons);
    let d2 = eval(&mut g, "V1", "BIG-2", 100, "PO-1", ev(GJ, &[]), 2_000);
    assert_ne!(d2.verdict, Verdict::Allow, "{:?}", d2.reasons);
    assert!(rules(&d2).contains(&"arithmetic_overflow".into()), "{:?}", d2.reasons);
}
