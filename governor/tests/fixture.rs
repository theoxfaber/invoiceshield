//! The shipped fictional vendor master parses strictly and drives the core cases.
use invoiceshield_governor::engine::{Evidence, Governor, Request};
use invoiceshield_governor::{Master, Verdict};
use serde_json::json;

const CLEAN: &str = r#"{"signals":[]}"#;

fn gov() -> Governor {
    let m = Master::from_json(include_str!("../../dataset/fixtures/master.json")).expect("master.json must parse strictly");
    assert_eq!(m.vendors.len(), 12);
    assert_eq!(m.pos.len(), 16);
    Governor::new(m)
}

fn v001(g: &Governor) -> Evidence {
    let v = &g.master.vendors["V001"];
    Evidence {
        text_layer_gstin: v.gstin.clone(),
        sender_domain: Some(v.email_domains[0].clone()),
        printed_bank_account: Some(v.bank_account.clone()),
        printed_ifsc: Some(v.ifsc.clone()),
        has_text_layer: true,
        ..Default::default()
    }
}

fn b01() -> serde_json::Value {
    json!({"vendor_id":"V001","invoice_no":"SOS/26-27/0231","amount_paise":45_000_00,"po_ref":"PO-2026-0101"})
}

#[test]
fn b01_genuine_invoice_allows_and_pays_master_account() {
    let mut g = gov();
    let ev = v001(&g);
    let d = g.evaluate(&Request { proposal: b01(), evidence: ev, checker_raw: Some(CLEAN.into()) }, 1_000);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    assert_eq!(d.payee.unwrap().beneficiary_id, "BENE_V001");
}

#[test]
fn l01_lookalike_gstin_and_domain_blocks() {
    let mut g = gov();
    let mut ev = v001(&g);
    ev.text_layer_gstin = Some("24ZZACS4891A1ZJ".into());
    ev.sender_domain = Some("sharrnaofficesupplies.in".into());
    let d = g.evaluate(&Request { proposal: b01(), evidence: ev, checker_raw: Some(CLEAN.into()) }, 1_000);
    assert_eq!(d.verdict, Verdict::Block);
    assert!(d.payee.is_none());
}

#[test]
fn nagpur_style_bank_change_never_pays_new_account() {
    let mut g = gov();
    let mut ev = v001(&g);
    ev.printed_bank_account = Some("3269010273503".into());
    ev.printed_ifsc = Some("CNRB0002778".into());
    let d = g.evaluate(&Request { proposal: b01(), evidence: ev, checker_raw: Some(CLEAN.into()) }, 1_000);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(d.tamper_alert);
    assert_eq!(d.payee.unwrap().bank_name, "Bank of Baroda");
}

#[test]
fn agent_supplied_bank_fields_block() {
    let mut g = gov();
    let ev = v001(&g);
    let mut p = b01();
    p["bank_ifsc"] = json!("CNRB0002778");
    let d = g.evaluate(&Request { proposal: p, evidence: ev, checker_raw: Some(CLEAN.into()) }, 1_000);
    assert_eq!(d.verdict, Verdict::Block);
}

#[test]
fn pasted_account_outside_the_bank_box_is_a_tamper_alert() {
    // The bank box matches the master, but the text elsewhere names another account.
    let mut g = gov();
    let mut ev = v001(&g);
    ev.all_printed_accounts = vec![ev.printed_bank_account.clone().unwrap(), "3269010273503".into()];
    ev.all_printed_ifscs = vec![ev.printed_ifsc.clone().unwrap(), "CNRB0002778".into()];
    let d = g.evaluate(&Request { proposal: b01(), evidence: ev, checker_raw: Some(CLEAN.into()) }, 1_000);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(d.tamper_alert);
    assert_eq!(d.payee.unwrap().bank_name, "Bank of Baroda");
}

#[test]
fn matching_values_everywhere_stay_allow() {
    let mut g = gov();
    let mut ev = v001(&g);
    ev.all_printed_accounts = vec![ev.printed_bank_account.clone().unwrap()];
    ev.all_printed_ifscs = vec![ev.printed_ifsc.clone().unwrap()];
    let d = g.evaluate(&Request { proposal: b01(), evidence: ev, checker_raw: Some(CLEAN.into()) }, 1_000);
    assert_eq!(d.verdict, Verdict::Allow);
}

#[test]
fn reset_state_forgets_the_ledger() {
    let mut g = gov();
    let ev = v001(&g);
    let req = Request { proposal: b01(), evidence: ev, checker_raw: Some(CLEAN.into()) };
    assert_eq!(g.evaluate(&req, 1_000).verdict, Verdict::Allow);
    assert_eq!(g.evaluate(&req, 1_001).verdict, Verdict::Block); // duplicate
    g.reset_state();
    assert_eq!(g.evaluate(&req, 1_002).verdict, Verdict::Allow);
    assert_eq!(g.audit.verify(), None);
}

// ---- amount rules (EXTRACTION): printed total, arithmetic, unlisted charges ----
use invoiceshield_governor::engine::ItemLine;

fn lines(items: &[(&str, i64, bool)]) -> Vec<ItemLine> {
    items.iter().map(|(d, a, t)| ItemLine { description: (*d).into(), amount_paise: *a, is_tax: *t, qty: None, unit_price_minor: None }).collect()
}

fn run(g: &mut Governor, amount: i64, ev: Evidence) -> invoiceshield_governor::engine::Decision {
    let p = json!({"vendor_id":"V001","invoice_no":"SOS/26-27/0299","amount_paise":amount,"po_ref":"PO-2026-0101"});
    g.evaluate(&Request { proposal: p, evidence: ev, checker_raw: Some(CLEAN.into()) }, 1_000)
}

#[test]
fn clean_item_table_that_adds_up_still_allows() {
    let mut g = gov();
    let mut ev = v001(&g);
    ev.printed_total_paise = Some(59_000_00);
    ev.item_lines = lines(&[("Stationery & consumables as per PO-2026-0101", 50_000_00, false), ("CGST 9% + SGST 9%", 9_000_00, true)]);
    let d = run(&mut g, 59_000_00, ev);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn fee_padding_inside_po_balance_is_review_not_pay() {
    // I02 shape: a fee the PO does not buy, total still under balance and threshold.
    let mut g = gov();
    let mut ev = v001(&g);
    ev.printed_total_paise = Some(70_400_00);
    ev.item_lines = lines(&[("Stationery & consumables as per PO-2026-0101", 30_000_00, false), ("IGST @ 18%", 5_400_00, true),
                            ("Expedite / priority handling fee", 35_000_00, false)]);
    let d = run(&mut g, 70_400_00, ev);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(d.reasons.iter().any(|r| r.rule == "charge_not_on_po"));
    assert!(!d.tamper_alert);
}

#[test]
fn proposal_that_differs_from_printed_total_is_review() {
    let mut g = gov();
    let mut ev = v001(&g);
    ev.printed_total_paise = Some(59_000_00);
    let d = run(&mut g, 49_000_00, ev);
    assert_eq!(d.verdict, Verdict::Review);
    assert!(d.reasons.iter().any(|r| r.rule == "amount_differs_from_printed_total"));
}

#[test]
fn total_that_does_not_equal_its_lines_is_review() {
    // I01 shape: 40,677.97 + 7,322.03 = 48,000.00 but the document prints 84,000.00.
    let mut g = gov();
    let mut ev = v001(&g);
    ev.printed_total_paise = Some(84_000_00);
    ev.item_lines = lines(&[("Stationery & consumables as per PO-2026-0101", 40_677_97, false), ("CGST 9% + SGST 9%", 7_322_03, true)]);
    let d = run(&mut g, 84_000_00, ev);
    assert_ne!(d.verdict, Verdict::Allow); // also over this PO's open balance, so BLOCK
    assert!(d.reasons.iter().any(|r| r.rule == "invoice_does_not_add_up"));
}

#[test]
fn absent_extraction_adds_no_amount_reason() {
    // A scan has no text layer: the amount rules stay silent and the GSTIN rule asks a human.
    let mut g = gov();
    let ev = Evidence { has_text_layer: false, ..Default::default() };
    let d = run(&mut g, 59_000_00, ev);
    assert!(!d.reasons.iter().any(|r| r.rule.starts_with("amount_") || r.rule == "charge_not_on_po"));
}
