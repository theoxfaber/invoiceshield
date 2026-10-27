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
