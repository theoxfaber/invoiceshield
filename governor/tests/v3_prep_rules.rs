//! Unit tests for the v3-prep rules (commit 75d5863, 2026-10-31): resubmission under a new number
//! (same itemized lines; bumped number +1/+2), the per-PO slow-drip check, credit-note BLOCK →
//! REVIEW, registered secondary accounts, the effective (discounted) unit price, round-off words
//! and the helper functions behind them. The tax-column parsing fix for v2 case_006 lives in the
//! Python assembler and is tested in tests/test_v3_prep_assembler.py.
use invoiceshield_governor::engine::{
    check_lines_against_po, is_round_off, lines_key, Evidence, Governor, ItemLine, Request,
};
use invoiceshield_governor::master::{
    Master, PoLine, Policy, PurchaseOrder, RegisteredAccount, Vendor,
};
use invoiceshield_governor::normalize::numbers_bumped;
use invoiceshield_governor::{Decision, Verdict};
use serde_json::json;

const CLEAN: &str = r#"{"signals":[]}"#;
const GJ: &str = "24ZZACS0001A1Z5";
const DAY: i64 = 86_400;
const UNIT: i64 = 1_000_00; // ₹1,000.00 per widget on the PO

fn vendor() -> Vendor {
    Vendor {
        vendor_id: "V1".into(),
        legal_name: "Vendor One".into(),
        gstin: Some(GJ.into()),
        pan: None,
        beneficiary_id: "BEN_V1".into(),
        bank_account: "77770000000101".into(),
        ifsc: "BARB0MAKARP".into(),
        bank_name: "Bank".into(),
        email_domains: vec![],
        category: "x".into(),
    }
}

fn po_line(item: &str, qty: i64, unit: i64) -> PoLine {
    PoLine {
        item: item.into(),
        aliases: vec![],
        qty,
        unit_price_minor: unit,
        currency: None,
        unit: "nos".into(),
    }
}

fn master() -> Master {
    let mut m = Master {
        policy: Policy::default(),
        ..Default::default()
    }; // threshold ₹1,00,000, window 7 d, resubmission 30 d
    m.vendors.insert("V1".into(), vendor());
    m.pos.insert(
        "PO-1".into(),
        PurchaseOrder {
            po_ref: "PO-1".into(),
            owner_vendor_id: "V1".into(),
            category: "x".into(),
            currency: "INR".into(),
            open_balance_paise: 50_00_000_00,
            lines: vec![
                po_line("Widget", 5_000, UNIT),
                po_line("Bracket", 5_000, 200_00),
                PoLine {
                    item: "Maintenance service".into(),
                    aliases: vec![],
                    qty: 1,
                    unit_price_minor: 2_40_000_00,
                    currency: None,
                    unit: "lot".into(),
                },
            ],
        },
    );
    m
}

fn line(desc: &str, qty: i64, unit: i64) -> ItemLine {
    ItemLine {
        description: desc.into(),
        amount_paise: qty * unit,
        qty: Some(qty),
        unit_price_minor: Some(unit),
        is_tax: false,
    }
}

fn ev_lines(items: Vec<ItemLine>) -> Evidence {
    let total = items.iter().map(|l| l.amount_paise).sum();
    Evidence {
        text_layer_gstin: Some(GJ.into()),
        has_text_layer: true,
        printed_total_paise: Some(total),
        item_lines: items,
        ..Default::default()
    }
}

/// A lump-sum "as per PO" bill: one line with no printed quantity or rate.
fn ev_lump(amount: i64) -> Evidence {
    ev_lines(vec![ItemLine {
        description: "Maintenance service".into(),
        amount_paise: amount,
        qty: None,
        unit_price_minor: None,
        is_tax: false,
    }])
}

fn eval(g: &mut Governor, inv: &str, amount: i64, e: Evidence, now: i64) -> Decision {
    let p = json!({"vendor_id": "V1", "invoice_no": inv, "amount_paise": amount, "po_ref": "PO-1"});
    g.evaluate(
        &Request {
            proposal: p,
            evidence: e,
            checker_raw: Some(CLEAN.into()),
            ..Default::default()
        },
        now,
    )
}

fn rules(d: &Decision) -> Vec<String> {
    d.reasons.iter().map(|r| r.rule.clone()).collect()
}

fn has(d: &Decision, rule: &str) -> bool {
    d.reasons.iter().any(|r| r.rule == rule)
}

// --- helpers ------------------------------------------------------------------------------------

#[test]
fn numbers_bumped_only_last_digit_run_by_one_or_two() {
    assert!(numbers_bumped("vec/26-27/0233", "vec/26-27/0234"));
    assert!(numbers_bumped("vec/26-27/0233", "vec/26-27/0235"));
    assert!(numbers_bumped("inv-9", "inv-10"));
    assert!(
        !numbers_bumped("vec/26-27/0233", "vec/26-27/0236"),
        "+3 is not a bump"
    );
    assert!(
        !numbers_bumped("vec/26-27/0233", "vec/26-27/0233"),
        "same number is the duplicate rule's job"
    );
    assert!(
        !numbers_bumped("vec/25-26/0233", "vec/26-27/0234"),
        "an earlier digit run differs"
    );
    assert!(!numbers_bumped("a-0233", "b-0234"), "prefix differs");
    assert!(!numbers_bumped("inv-12a", "inv-13b"), "suffix differs");
    assert!(!numbers_bumped("abc", "abd"), "no digits");
    assert!(
        !numbers_bumped("x-1234567890123", "x-1234567890124"),
        "over 12 digits is not parsed"
    );
}

#[test]
fn lines_key_ignores_order_tax_and_needs_qty_and_rate() {
    let a = [line("Widget", 10, UNIT), line("Bracket", 5, 200_00)];
    let b = [line("Bracket", 5, 200_00), line("Widget", 10, UNIT)];
    assert_eq!(lines_key(&a), lines_key(&b), "order does not matter");
    let mut with_tax = a.to_vec();
    with_tax.push(ItemLine {
        description: "CGST".into(),
        amount_paise: 9_00,
        qty: Some(1),
        unit_price_minor: Some(9_00),
        is_tax: true,
    });
    assert_eq!(
        lines_key(&a),
        lines_key(&with_tax),
        "tax lines are not part of the fingerprint"
    );
    assert_ne!(
        lines_key(&a),
        lines_key(&[line("Widget", 11, UNIT), line("Bracket", 5, 200_00)])
    );
    let lump = [ItemLine {
        description: "As per PO".into(),
        amount_paise: 10_000_00,
        qty: None,
        unit_price_minor: None,
        is_tax: false,
    }];
    assert_eq!(
        lines_key(&lump),
        None,
        "lump-sum billing has no fingerprint"
    );
    assert_eq!(lines_key(&[]), None);
}

#[test]
fn round_off_words() {
    let ro = |d: &str, amt: i64| ItemLine {
        description: d.into(),
        amount_paise: amt,
        qty: None,
        unit_price_minor: None,
        is_tax: false,
    };
    for d in [
        "Round off",
        "Rounding",
        "R/O",
        "Round off Amt",
        "Rounded off",
        "Less: Round off",
        "Round Off Adjustment",
    ] {
        assert!(is_round_off(&ro(d, 42)), "{d}");
        assert!(is_round_off(&ro(d, -99)), "{d} negative");
    }
    assert!(
        !is_round_off(&ro("Round off", 100)),
        "one rupee or more is a charge"
    );
    assert!(
        !is_round_off(&ro("Round off charges for freight", 42)),
        "other words make it a charge"
    );
    assert!(!is_round_off(&ro("Less", 42)), "needs a round word");
    assert!(!is_round_off(&ro("Amount", 42)));
    let mut tax = ro("Round off", 42);
    tax.is_tax = true;
    assert!(!is_round_off(&tax));
}

// --- resubmission ------------------------------------------------------------------------------

#[test]
fn resubmission_same_items_under_new_number_reviews() {
    let mut g = Governor::new(master());
    let items = vec![line("Widget", 30, UNIT), line("Bracket", 10, 200_00)];
    let amt = 32_000_00;
    let d1 = eval(&mut g, "VO/26/1101", amt, ev_lines(items.clone()), DAY);
    assert_eq!(d1.verdict, Verdict::Allow, "{:?}", d1.reasons);
    // same lines, unrelated number, 10 days later (outside the 7-day split window)
    let d2 = eval(&mut g, "ZX-77", amt, ev_lines(items), 11 * DAY);
    assert_eq!(d2.verdict, Verdict::Review, "{:?}", d2.reasons);
    assert!(has(&d2, "resubmission_same_items"), "{:?}", rules(&d2));
}

#[test]
fn resubmission_bumped_number_plus_one_and_plus_two_reviews() {
    for bump in ["VO/26/1102", "VO/26/1103"] {
        let mut g = Governor::new(master());
        let amt = 30_000_00;
        eval(
            &mut g,
            "VO/26/1101",
            amt,
            ev_lines(vec![line("Widget", 30, UNIT)]),
            DAY,
        );
        // itemized, same PO and amount, different line split (so not content-equal), number +1/+2
        let d = eval(
            &mut g,
            bump,
            amt,
            ev_lines(vec![line("Widget", 20, UNIT), line("Bracket", 50, 200_00)]),
            11 * DAY,
        );
        assert_eq!(d.verdict, Verdict::Review, "{bump}: {:?}", d.reasons);
        assert!(
            has(&d, "resubmission_bumped_number"),
            "{bump}: {:?}",
            rules(&d)
        );
    }
}

#[test]
fn bumped_number_plus_three_or_other_amount_passes() {
    let mut g = Governor::new(master());
    eval(
        &mut g,
        "VO/26/1101",
        30_000_00,
        ev_lines(vec![line("Widget", 30, UNIT)]),
        DAY,
    );
    let d = eval(
        &mut g,
        "VO/26/1104",
        30_000_00,
        ev_lines(vec![line("Widget", 20, UNIT), line("Bracket", 50, 200_00)]),
        11 * DAY,
    );
    assert_eq!(d.verdict, Verdict::Allow, "+3: {:?}", d.reasons);
    let mut g = Governor::new(master());
    eval(
        &mut g,
        "VO/26/1101",
        30_000_00,
        ev_lines(vec![line("Widget", 30, UNIT)]),
        DAY,
    );
    let d = eval(
        &mut g,
        "VO/26/1102",
        31_000_00,
        ev_lines(vec![line("Widget", 31, UNIT)]),
        11 * DAY,
    );
    assert_eq!(d.verdict, Verdict::Allow, "other amount: {:?}", d.reasons);
}

#[test]
fn lump_sum_monthly_bill_of_same_amount_stays_pay() {
    // recurring "as per PO" service bills of one amount, consecutive numbers, a month apart
    let mut g = Governor::new(master());
    let d1 = eval(&mut g, "SVC/101", 20_000_00, ev_lump(20_000_00), DAY);
    assert_eq!(d1.verdict, Verdict::Allow, "{:?}", d1.reasons);
    let d2 = eval(&mut g, "SVC/102", 20_000_00, ev_lump(20_000_00), 20 * DAY);
    assert_eq!(d2.verdict, Verdict::Allow, "{:?}", d2.reasons);
}

#[test]
fn resubmission_outside_window_passes() {
    let mut g = Governor::new(master());
    let items = vec![line("Widget", 30, UNIT)];
    eval(
        &mut g,
        "VO/26/1101",
        30_000_00,
        ev_lines(items.clone()),
        DAY,
    );
    let d = eval(&mut g, "VO/26/1102", 30_000_00, ev_lines(items), 40 * DAY);
    assert!(
        !has(&d, "resubmission_same_items") && !has(&d, "resubmission_bumped_number"),
        "{:?}",
        rules(&d)
    );
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

// --- slow-drip on one PO ------------------------------------------------------------------------

#[test]
fn third_invoice_on_po_over_threshold_in_window_reviews() {
    let mut g = Governor::new(master());
    // ₹40,000 each, 8 days apart: the 7-day vendor window never sees two; the 30-day PO window does
    let a = eval(
        &mut g,
        "D-1",
        40_000_00,
        ev_lines(vec![line("Widget", 40, UNIT)]),
        DAY,
    );
    let b = eval(
        &mut g,
        "D-50",
        40_000_00,
        ev_lines(vec![line("Bracket", 200, 200_00)]),
        9 * DAY,
    );
    assert_eq!(a.verdict, Verdict::Allow, "{:?}", a.reasons);
    assert_eq!(
        b.verdict,
        Verdict::Allow,
        "two invoices are monthly-style billing: {:?}",
        b.reasons
    );
    let c = eval(
        &mut g,
        "D-99",
        40_000_00,
        ev_lines(vec![line("Widget", 10, UNIT), line("Bracket", 150, 200_00)]),
        17 * DAY,
    );
    assert_eq!(c.verdict, Verdict::Review, "{:?}", c.reasons);
    assert!(has(&c, "po_drip_over_threshold"), "{:?}", rules(&c));
}

#[test]
fn drip_under_threshold_total_or_outside_window_passes() {
    let mut g = Governor::new(master());
    eval(
        &mut g,
        "D-1",
        20_000_00,
        ev_lines(vec![line("Widget", 20, UNIT)]),
        DAY,
    );
    eval(
        &mut g,
        "D-50",
        20_000_00,
        ev_lines(vec![line("Bracket", 100, 200_00)]),
        9 * DAY,
    );
    let c = eval(
        &mut g,
        "D-99",
        20_000_00,
        ev_lines(vec![line("Widget", 10, UNIT), line("Bracket", 50, 200_00)]),
        17 * DAY,
    );
    assert!(
        !has(&c, "po_drip_over_threshold"),
        "total ₹60,000 is under the threshold: {:?}",
        rules(&c)
    );
    let mut g = Governor::new(master());
    eval(
        &mut g,
        "D-1",
        40_000_00,
        ev_lines(vec![line("Widget", 40, UNIT)]),
        DAY,
    );
    eval(
        &mut g,
        "D-50",
        40_000_00,
        ev_lines(vec![line("Bracket", 200, 200_00)]),
        12 * DAY,
    );
    let c = eval(
        &mut g,
        "D-99",
        40_000_00,
        ev_lines(vec![line("Widget", 10, UNIT), line("Bracket", 150, 200_00)]),
        33 * DAY,
    );
    assert!(
        !has(&c, "po_drip_over_threshold"),
        "first invoice is out of the 30-day window: {:?}",
        rules(&c)
    );
}

// --- credit notes -------------------------------------------------------------------------------

#[test]
fn credit_note_over_po_balance_is_review_not_block() {
    let mut m = master();
    m.pos.get_mut("PO-1").unwrap().open_balance_paise = 10_000_00;
    let mut g = Governor::new(m);
    let mut e = ev_lines(vec![line("Widget", 20, UNIT)]);
    e.document_kind = Some("credit_note".into());
    let d = eval(&mut g, "CN-7", 20_000_00, e, DAY);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    let ob = d
        .reasons
        .iter()
        .find(|r| r.rule == "over_po_balance")
        .expect("over_po_balance still shown");
    assert_eq!(ob.verdict, Verdict::Review);
    assert!(ob.detail.contains("credit note"));
    assert!(
        g.ledger.entries.is_empty(),
        "credit notes write no ledger entry"
    );
}

#[test]
fn same_amount_over_po_balance_on_an_invoice_still_blocks() {
    let mut m = master();
    m.pos.get_mut("PO-1").unwrap().open_balance_paise = 10_000_00;
    let mut g = Governor::new(m);
    let d = eval(
        &mut g,
        "INV-7",
        20_000_00,
        ev_lines(vec![line("Widget", 20, UNIT)]),
        DAY,
    );
    assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
}

#[test]
fn credit_note_identity_block_stays_block() {
    let mut g = Governor::new(master());
    let mut e = ev_lines(vec![line("Widget", 20, UNIT)]);
    e.document_kind = Some("credit_note".into());
    e.text_layer_gstin = Some("27ZZACM0002B1Z3".into()); // someone else's GSTIN
    let d = eval(&mut g, "CN-8", 20_000_00, e, DAY);
    assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
}

// --- registered secondary accounts --------------------------------------------------------------

fn with_secondary() -> Master {
    let mut m = master();
    m.registered_accounts.insert(
        "V1".into(),
        vec![RegisteredAccount {
            vendor_id: "V1".into(),
            bank_account: "55550000000202".into(),
            ifsc: "HDFC0001234".into(),
        }],
    );
    m
}

#[test]
fn printed_registered_secondary_account_is_not_tamper_and_payee_stays_primary() {
    let mut g = Governor::new(with_secondary());
    let mut e = ev_lines(vec![line("Widget", 10, UNIT)]);
    e.printed_bank_account = Some("5555 0000 0002 02".into());
    e.printed_ifsc = Some("HDFC0001234".into());
    let d = eval(&mut g, "S-1", 10_000_00, e, DAY);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert!(has(&d, "printed_registered_secondary_account"));
    assert!(!has(&d, "bank_details_differ_from_master"));
    assert_eq!(
        d.payee.as_ref().map(|p| p.beneficiary_id.as_str()),
        Some("BEN_V1")
    );
    assert_eq!(
        d.payee.as_ref().map(|p| p.account_last4.as_str()),
        Some("0101"),
        "payment goes to the primary account"
    );
}

#[test]
fn unregistered_account_is_still_tamper_even_with_a_secondary_on_file() {
    let mut g = Governor::new(with_secondary());
    let mut e = ev_lines(vec![line("Widget", 10, UNIT)]);
    e.all_printed_accounts = vec!["55550000000202".into(), "99990000000303".into()];
    let d = eval(&mut g, "S-2", 10_000_00, e, DAY);
    assert_ne!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert!(has(&d, "bank_details_differ_from_master"));
    assert!(!has(&d, "printed_registered_secondary_account"));
}

#[test]
fn secondary_of_another_vendor_is_tamper() {
    let mut m = master();
    m.registered_accounts.insert(
        "V9".into(),
        vec![RegisteredAccount {
            vendor_id: "V9".into(),
            bank_account: "55550000000202".into(),
            ifsc: "HDFC0001234".into(),
        }],
    );
    let mut g = Governor::new(m);
    let mut e = ev_lines(vec![line("Widget", 10, UNIT)]);
    e.printed_bank_account = Some("55550000000202".into());
    let d = eval(&mut g, "S-3", 10_000_00, e, DAY);
    assert!(
        has(&d, "bank_details_differ_from_master"),
        "{:?}",
        rules(&d)
    );
}

#[test]
fn master_json_loads_registered_accounts() {
    let s = r#"{"vendors": [], "pos": [], "registered_accounts": [
        {"vendor_id": "V1", "bank_account": "1", "ifsc": "HDFC0001234"},
        {"vendor_id": "V1", "bank_account": "2", "ifsc": "HDFC0001234"}]}"#;
    let m = Master::from_json(s).unwrap();
    assert_eq!(m.registered_accounts.get("V1").map(Vec::len), Some(2));
    let bad = r#"{"vendors": [], "pos": [], "registered_accounts": [{"vendor_id": "V1", "bank_account": "1", "ifsc": "X", "payee": true}]}"#;
    assert!(
        Master::from_json(bad).is_err(),
        "unknown fields are rejected"
    );
}

// --- effective unit price -----------------------------------------------------------------------

#[test]
fn discounted_line_at_list_rate_above_po_rate_bills_po_rate() {
    let po = master().pos.remove("PO-1").unwrap();
    // list rate ₹1,111.12, 10% off → ₹1,000.00 a unit effective; 10 units bill ₹10,000.00
    let l = ItemLine {
        description: "Widget".into(),
        amount_paise: 10_000_00,
        qty: Some(10),
        unit_price_minor: Some(1_111_12),
        is_tax: false,
    };
    assert!(check_lines_against_po(&po, &[l], "INR", 0).is_empty());
}

#[test]
fn discount_that_still_bills_above_po_rate_is_flagged() {
    let po = master().pos.remove("PO-1").unwrap();
    // list ₹1,200, 5% off → ₹1,140 a unit > ₹1,000
    let l = ItemLine {
        description: "Widget".into(),
        amount_paise: 11_400_00,
        qty: Some(10),
        unit_price_minor: Some(1_200_00),
        is_tax: false,
    };
    assert_eq!(check_lines_against_po(&po, &[l], "INR", 0).len(), 1);
}

#[test]
fn rate_above_po_without_discount_is_flagged_and_rounding_is_not_a_discount() {
    let po = master().pos.remove("PO-1").unwrap();
    let l = line("Widget", 10, 1_050_00);
    assert_eq!(check_lines_against_po(&po, &[l], "INR", 0).len(), 1);
    // a per-unit rounding of one paisa does not switch to the effective price
    let l = ItemLine {
        description: "Widget".into(),
        amount_paise: 10_000_00 - 5,
        qty: Some(10),
        unit_price_minor: Some(UNIT),
        is_tax: false,
    };
    assert!(check_lines_against_po(&po, &[l], "INR", 0).is_empty());
}

#[test]
fn discounted_line_end_to_end_pays() {
    let mut g = Governor::new(master());
    let l = ItemLine {
        description: "Widget".into(),
        amount_paise: 10_000_00,
        qty: Some(10),
        unit_price_minor: Some(1_111_12),
        is_tax: false,
    };
    let d = eval(&mut g, "DS-1", 10_000_00, ev_lines(vec![l]), DAY);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}
