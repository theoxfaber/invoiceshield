//! Review items 1 and 2 (2026-10-31): fail closed on unreadable item lines; model-proposed lines
//! are used only when they verify verbatim against the signed text layer.
use invoiceshield_governor::engine::{Evidence, Governor, ItemLine, Request};
use invoiceshield_governor::master::{Master, PoLine, Policy, PurchaseOrder, Vendor};
use invoiceshield_governor::Verdict;
use serde_json::json;

const CLEAN: &str = r#"{"signals":[]}"#;
const GSTIN: &str = "24ZZACS0001A1Z5";

const TEXT: &str = "TAX INVOICE\nSharma Office Supplies\nGSTIN: 24ZZACS0001A1Z5\nSl\nDescription\nQty\nRate\nTaxable Value\n1\nA4 copier paper 75 gsm\n4802\n100\n250.00\n25,000.00\n2\nStapler heavy duty\n8472\n10\n500.00\n5,000.00\nTaxable Total\n₹ 30,000.00\nCGST\n₹ 2,700.00\nSGST\n₹ 2,700.00\nGRAND TOTAL\n₹ 35,400.00\n";
const TEXT_FEE: &str = "TAX INVOICE\nSl\nDescription\nQty\nRate\nTaxable Value\n1\nA4 copier paper 75 gsm\n4802\n100\n250.00\n25,000.00\n2\nUrgent handling fee\n9985\n1\n5,000.00\n5,000.00\nTaxable Total\n₹ 30,000.00\nCGST\n₹ 2,700.00\nSGST\n₹ 2,700.00\nGRAND TOTAL\n₹ 35,400.00\n";

fn master() -> Master {
    let mut m = Master {
        policy: Policy {
            buyer_gstin: Some("24ZZACB7319B1Z5".into()),
            ..Policy::default()
        },
        ..Default::default()
    };
    m.vendors.insert(
        "V001".into(),
        Vendor {
            vendor_id: "V001".into(),
            legal_name: "Sharma Office Supplies Pvt Ltd".into(),
            gstin: Some(GSTIN.into()),
            pan: None,
            beneficiary_id: "BEN_V001".into(),
            bank_account: "77770000000101".into(),
            ifsc: "BARB0MAKARP".into(),
            bank_name: "Bank".into(),
            email_domains: vec![],
            category: "Stationery".into(),
        },
    );
    m.pos.insert(
        "PO-1".into(),
        PurchaseOrder {
            po_ref: "PO-1".into(),
            owner_vendor_id: "V001".into(),
            category: "Stationery".into(),
            currency: "INR".into(),
            open_balance_paise: 50_000_00,
            lines: vec![
                PoLine {
                    item: "A4 copier paper 75 gsm".into(),
                    aliases: vec![],
                    qty: 200,
                    unit_price_minor: 250_00,
                    currency: None,
                    unit: "ream".into(),
                },
                PoLine {
                    item: "Stapler heavy duty".into(),
                    aliases: vec![],
                    qty: 20,
                    unit_price_minor: 500_00,
                    currency: None,
                    unit: "nos".into(),
                },
            ],
        },
    );
    m
}

fn ev(text: &str, total: Option<i64>) -> Evidence {
    Evidence {
        text_layer_gstin: Some(GSTIN.into()),
        has_text_layer: true,
        text_source: Some("text_layer".into()),
        printed_total_paise: total,
        text_layer: Some(text.into()),
        ..Default::default()
    }
}

fn l(d: &str, a: i64, q: Option<i64>, u: Option<i64>, tax: bool) -> ItemLine {
    ItemLine {
        description: d.into(),
        amount_paise: a,
        qty: q,
        unit_price_minor: u,
        is_tax: tax,
    }
}

fn good_lines() -> Vec<ItemLine> {
    vec![
        l(
            "A4 copier paper 75 gsm",
            25_000_00,
            Some(100),
            Some(250_00),
            false,
        ),
        l(
            "Stapler heavy duty",
            5_000_00,
            Some(10),
            Some(500_00),
            false,
        ),
        l("CGST", 2_700_00, None, None, true),
        l("SGST", 2_700_00, None, None, true),
    ]
}

fn run(evidence: Evidence, proposed: Option<Vec<ItemLine>>) -> invoiceshield_governor::Decision {
    let mut g = Governor::new(master());
    let p =
        json!({"vendor_id":"V001","invoice_no":"SOS/1","amount_paise":35_400_00,"po_ref":"PO-1"});
    g.evaluate(
        &Request {
            proposal: p,
            evidence,
            checker_raw: Some(CLEAN.into()),
            proposed_item_lines: proposed,
        },
        1_000,
    )
}

fn rules(d: &invoiceshield_governor::Decision) -> Vec<String> {
    d.reasons.iter().map(|r| r.rule.clone()).collect()
}

#[test]
fn text_layer_and_total_but_no_lines_reviews_line_items_unreadable() {
    let d = run(ev(TEXT, Some(35_400_00)), None);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(rules(&d).contains(&"line_items_unreadable".into()));
}

#[test]
fn empty_model_proposal_is_the_same_as_none() {
    let d = run(ev(TEXT, Some(35_400_00)), Some(vec![]));
    assert_eq!(d.verdict, Verdict::Review);
    assert!(rules(&d).contains(&"line_items_unreadable".into()));
}

#[test]
fn no_printed_total_does_not_trigger_the_line_rule() {
    // Nothing to check lines against: other rules decide (unchanged behaviour).
    let d = run(ev(TEXT, None), None);
    assert!(
        !rules(&d).contains(&"line_items_unreadable".into()),
        "{:?}",
        d.reasons
    );
}

#[test]
fn no_text_layer_does_not_trigger_the_rule() {
    let mut e = ev(TEXT, Some(35_400_00));
    e.has_text_layer = false;
    let d = run(e, None);
    assert!(!rules(&d).contains(&"line_items_unreadable".into()));
}

#[test]
fn parsed_lines_are_used_and_model_lines_ignored() {
    let mut e = ev(TEXT, Some(35_400_00));
    e.item_lines = good_lines();
    let bogus = vec![l("Nothing", 1, None, None, false)];
    let d = run(e, Some(bogus));
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert!(!rules(&d).contains(&"line_items_model_verified".into()));
}

#[test]
fn verified_model_lines_on_the_po_allow() {
    let d = run(ev(TEXT, Some(35_400_00)), Some(good_lines()));
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert!(rules(&d).contains(&"line_items_model_verified".into()));
}

#[test]
fn model_lines_that_fail_verification_review() {
    let mut pl = good_lines();
    pl[0].amount_paise = 26_000_00; // not printed
    pl[0].unit_price_minor = Some(260_00);
    let d = run(ev(TEXT, Some(35_400_00)), Some(pl));
    assert_eq!(d.verdict, Verdict::Review);
    assert!(rules(&d).contains(&"line_items_unreadable".into()));
    assert!(!rules(&d).contains(&"line_items_model_verified".into()));
}

#[test]
fn model_lines_without_signed_text_review() {
    let mut e = ev(TEXT, Some(35_400_00));
    e.text_layer = None;
    let d = run(e, Some(good_lines()));
    assert_eq!(d.verdict, Verdict::Review);
    assert!(rules(&d).contains(&"line_items_unreadable".into()));
}

#[test]
fn verified_model_lines_still_face_the_po_rules() {
    // A padded fee read honestly by the model is verified, then caught by the PO-line rule.
    let pl = vec![
        l(
            "A4 copier paper 75 gsm",
            25_000_00,
            Some(100),
            Some(250_00),
            false,
        ),
        l(
            "Urgent handling fee",
            5_000_00,
            Some(1),
            Some(5_000_00),
            false,
        ),
        l("CGST", 2_700_00, None, None, true),
        l("SGST", 2_700_00, None, None, true),
    ];
    let d = run(ev(TEXT_FEE, Some(35_400_00)), Some(pl));
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(rules(&d).contains(&"charge_not_on_po".into()));
}

#[test]
fn model_hiding_the_fee_inside_a_po_line_is_not_verified() {
    // The hijacked/fooled model folds the fee into the paper line: 30,000 is printed only as the
    // taxable total, so the line does not verify and the invoice goes to REVIEW.
    let pl = vec![
        l("A4 copier paper 75 gsm", 30_000_00, None, None, false),
        l("CGST", 2_700_00, None, None, true),
        l("SGST", 2_700_00, None, None, true),
    ];
    let d = run(ev(TEXT_FEE, Some(35_400_00)), Some(pl));
    assert_eq!(d.verdict, Verdict::Review);
    assert!(
        rules(&d).contains(&"line_items_unreadable".into()),
        "{:?}",
        d.reasons
    );
}

#[test]
fn model_lines_cannot_clear_another_review() {
    // Verified lines, but the proposal differs from the printed total: still REVIEW.
    let mut g = Governor::new(master());
    let p =
        json!({"vendor_id":"V001","invoice_no":"SOS/1","amount_paise":35_000_00,"po_ref":"PO-1"});
    let d = g.evaluate(
        &Request {
            proposal: p,
            evidence: ev(TEXT, Some(35_400_00)),
            checker_raw: Some(CLEAN.into()),
            proposed_item_lines: Some(good_lines()),
        },
        1_000,
    );
    assert_eq!(d.verdict, Verdict::Review);
    assert!(rules(&d).contains(&"amount_differs_from_printed_total".into()));
}

#[test]
fn wire_json_accepts_proposed_lines() {
    let mut g = Governor::new(master());
    let body = json!({
        "proposal": {"vendor_id":"V001","invoice_no":"SOS/1","amount_paise":35_400_00,"po_ref":"PO-1"},
        "evidence": ev(TEXT, Some(35_400_00)),
        "checker_raw": CLEAN,
        "proposed_item_lines": good_lines(),
    });
    let d = g.evaluate_json(&body.to_string(), 1_000);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn signed_text_without_a_readable_total_reviews() {
    let d = run(ev(TEXT, None), None);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(rules(&d).contains(&"total_unreadable".into()));
}
