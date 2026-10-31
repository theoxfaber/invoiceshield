//! Phase 1 hardening: currency, round-off rule, structured PO-line matching, IFSC normaliser,
//! bounded decision store, auth on /v1/audit/verify.
use invoiceshield_governor::decisions::DecisionStore;
use invoiceshield_governor::engine::{Decision, Evidence, Governor, ItemLine, Request};
use invoiceshield_governor::master::{Limits, PoLine, PurchaseOrder};
use invoiceshield_governor::{normalize, Master, Verdict};
use serde_json::json;

const CLEAN: &str = r#"{"signals":[]}"#;

fn gov() -> Governor {
    Governor::new(Master::from_json(include_str!("../../dataset/fixtures/master.json")).unwrap())
}

fn ev_for(g: &Governor, vid: &str) -> Evidence {
    let v = &g.master.vendors[vid];
    Evidence {
        text_layer_gstin: v.gstin.clone(),
        sender_domain: Some(v.email_domains[0].clone()),
        printed_bank_account: Some(v.bank_account.clone()),
        has_text_layer: true,
        ..Default::default()
    }
}

fn line(d: &str, a: i64) -> ItemLine {
    ItemLine {
        description: d.into(),
        amount_paise: a,
        is_tax: false,
        qty: None,
        unit_price_minor: None,
    }
}
fn qline(d: &str, qty: i64, unit: i64) -> ItemLine {
    ItemLine {
        description: d.into(),
        amount_paise: qty * unit,
        is_tax: false,
        qty: Some(qty),
        unit_price_minor: Some(unit),
    }
}
fn tax(d: &str, a: i64) -> ItemLine {
    ItemLine {
        description: d.into(),
        amount_paise: a,
        is_tax: true,
        qty: None,
        unit_price_minor: None,
    }
}

fn eval(g: &mut Governor, p: serde_json::Value, ev: Evidence) -> Decision {
    g.evaluate(
        &Request {
            proposal: p,
            evidence: ev,
            checker_raw: Some(CLEAN.into()),
            ..Default::default()
        },
        1_000,
    )
}
fn has(d: &Decision, rule: &str) -> bool {
    d.reasons.iter().any(|r| r.rule == rule)
}

// ---------------------------------------------------------------- currency

/// A V001 (rupee) PO with a USD invoice. Before Phase 1 the governor compared the cents on the
/// invoice with the PO's paise and ALLOWed it (USD 450.00 = 45,000 "paise" = ₹450 looked tiny).
#[test]
fn usd_invoice_against_inr_po_is_review_not_pay() {
    let mut g = gov();
    let mut ev = ev_for(&g, "V001");
    ev.currency = Some("USD".into());
    ev.all_currencies = vec!["USD".into()];
    ev.printed_total_paise = Some(45_000);
    // the agent proposes the printed number and says nothing about currency (defaults to INR)
    let d = eval(
        &mut g,
        json!({"vendor_id":"V001","invoice_no":"SOS/26-27/0400","amount_paise":45_000,"po_ref":"PO-2026-0101"}),
        ev.clone(),
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(has(&d, "currency_mismatch_invoice"));
    // the agent says USD: now the PO (INR) disagrees
    let d = eval(
        &mut g,
        json!({"vendor_id":"V001","invoice_no":"SOS/26-27/0401","amount_paise":45_000,"currency":"USD","po_ref":"PO-2026-0101"}),
        ev,
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(has(&d, "currency_mismatch_po"));
}

#[test]
fn genuine_usd_invoice_on_usd_po_allows_only_when_proposed_in_usd() {
    let mut g = gov();
    let mut ev = ev_for(&g, "V003");
    ev.currency = Some("USD".into());
    ev.all_currencies = vec!["USD".into()];
    ev.printed_total_paise = Some(840_00);
    ev.item_lines = vec![line("SaaS - Team plan, 25 seats, monthly", 840_00)];
    let p = json!({"vendor_id":"V003","invoice_no":"BC-INV-10090","amount_paise":840_00,"currency":"USD","po_ref":"PO-2026-0301"});
    let d = eval(&mut g, p, ev.clone());
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    g.reset_state();
    let p = json!({"vendor_id":"V003","invoice_no":"BC-INV-10090","amount_paise":840_00,"po_ref":"PO-2026-0301"});
    let d = eval(&mut g, p, ev);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
}

#[test]
fn usd_amounts_use_usd_limits_and_usd_po_balance() {
    let mut g = gov();
    let mut ev = ev_for(&g, "V003");
    ev.currency = Some("USD".into());
    // $1,000 open on the USD PO: $1,500 is over it (and over the $1,190 approval threshold)
    let d = eval(
        &mut g,
        json!({"vendor_id":"V003","invoice_no":"BC-1","amount_paise":1_500_00,"currency":"USD","po_ref":"PO-2026-0301"}),
        ev.clone(),
    );
    assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
    assert!(has(&d, "over_po_balance"));
    // threshold on its own: a larger USD PO, $1,200 > $1,190 → REVIEW (120000 cents is far below ₹1,00,000 in paise)
    g.reset_state();
    g.master
        .pos
        .get_mut("PO-2026-0301")
        .unwrap()
        .open_balance_paise = 10_000_00;
    let d = eval(
        &mut g,
        json!({"vendor_id":"V003","invoice_no":"BC-2","amount_paise":1_200_00,"currency":"USD","po_ref":"PO-2026-0301"}),
        ev.clone(),
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(has(&d, "over_approval_threshold"));
    // cap: $6,000 > $5,952 → BLOCK
    g.reset_state();
    let d = eval(
        &mut g,
        json!({"vendor_id":"V003","invoice_no":"BC-3","amount_paise":6_000_00,"currency":"USD","po_ref":"PO-2026-0301"}),
        ev,
    );
    assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
    assert!(has(&d, "over_payout_cap"));
}

#[test]
fn unknown_or_unconfigured_currency_is_review() {
    for cur in ["XYZ", "JPY", "rupees!", ""] {
        let mut g = gov();
        let ev = ev_for(&g, "V001");
        let d = eval(
            &mut g,
            json!({"vendor_id":"V001","invoice_no":format!("U-{cur}"),"amount_paise":10_000_00,"currency":cur,"po_ref":"PO-2026-0101"}),
            ev,
        );
        assert_ne!(d.verdict, Verdict::Allow, "{cur}: {:?}", d.reasons);
        assert!(has(&d, "unknown_currency"), "{cur}: {:?}", d.reasons);
    }
}

#[test]
fn configured_eur_limits_make_eur_known() {
    let mut g = gov();
    g.master.policy.currency_limits.insert(
        "EUR".into(),
        Limits {
            approval_threshold_minor: 1_000_00,
            payout_cap_minor: 5_000_00,
        },
    );
    assert_eq!(
        g.master
            .policy
            .limits("EUR")
            .unwrap()
            .approval_threshold_minor,
        1_000_00
    );
    assert_eq!(
        g.master
            .policy
            .limits("INR")
            .unwrap()
            .approval_threshold_minor,
        1_00_000_00
    );
    assert!(g.master.policy.limits("GBP").is_none());
}

#[test]
fn document_with_two_currencies_is_review() {
    let mut g = gov();
    let mut ev = ev_for(&g, "V001");
    ev.currency = Some("INR".into());
    ev.all_currencies = vec!["INR".into(), "USD".into()];
    let d = eval(
        &mut g,
        json!({"vendor_id":"V001","invoice_no":"SOS/26-27/0402","amount_paise":10_000_00,"po_ref":"PO-2026-0101"}),
        ev,
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(has(&d, "currency_ambiguous"));
}

#[test]
fn inr_invoice_with_no_currency_marker_still_allows() {
    let mut g = gov();
    let ev = ev_for(&g, "V001");
    let d = eval(
        &mut g,
        json!({"vendor_id":"V001","invoice_no":"SOS/26-27/0403","amount_paise":10_000_00,"po_ref":"PO-2026-0101"}),
        ev,
    );
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn split_window_never_adds_dollars_to_rupees() {
    let mut g = gov();
    g.master
        .pos
        .get_mut("PO-2026-0301")
        .unwrap()
        .open_balance_paise = 100_000_00;
    let mut ev = ev_for(&g, "V003");
    ev.currency = Some("USD".into());
    // $900 then $900 within 7 days: 1,800 > 1,190 → second is REVIEW (window), in USD terms
    let d1 = g.evaluate(&Request { proposal: json!({"vendor_id":"V003","invoice_no":"W-1","amount_paise":900_00,"currency":"USD","po_ref":"PO-2026-0301"}), evidence: ev.clone(), checker_raw: Some(CLEAN.into()), ..Default::default() }, 1_000);
    assert_eq!(d1.verdict, Verdict::Allow, "{:?}", d1.reasons);
    let d2 = g.evaluate(&Request { proposal: json!({"vendor_id":"V003","invoice_no":"W-2","amount_paise":900_00,"currency":"USD","po_ref":"PO-2026-0301"}), evidence: ev, checker_raw: Some(CLEAN.into()), ..Default::default() }, 2_000);
    assert!(has(&d2, "vendor_window_exceeded"), "{:?}", d2.reasons);
}

// ---------------------------------------------------------------- round-off rule

fn v001_with_lines(g: &Governor, items: Vec<ItemLine>) -> (Evidence, i64) {
    let mut ev = ev_for(g, "V001");
    let total: i64 = items.iter().map(|l| l.amount_paise).sum();
    ev.printed_total_paise = Some(total);
    ev.item_lines = items;
    (ev, total)
}

#[test]
fn round_off_under_one_rupee_is_not_a_charge() {
    for (desc, amt) in [
        ("Round off", 36),
        ("Round off", -13),
        ("Rounding", 99),
        ("Round-off", -99),
        ("ROUND OFF:", 50),
    ] {
        let mut g = gov();
        let (ev, total) = v001_with_lines(
            &g,
            vec![
                line("Stationery - A4 copier paper 75 gsm", 10_000_00),
                tax("CGST @ 9%", 900_00),
                tax("SGST @ 9%", 900_00),
                line(desc, amt),
            ],
        );
        let d = eval(
            &mut g,
            json!({"vendor_id":"V001","invoice_no":"RO-1","amount_paise":total,"po_ref":"PO-2026-0101"}),
            ev,
        );
        assert_eq!(d.verdict, Verdict::Allow, "{desc} {amt}: {:?}", d.reasons);
    }
}

#[test]
fn round_off_of_one_rupee_or_more_or_other_wording_still_reviews() {
    for (desc, amt) in [
        ("Round off", 100),
        ("Round off", -150),
        ("Round off adjustment fee", 40),
        ("Rounding charges", 50),
    ] {
        let mut g = gov();
        let (ev, total) = v001_with_lines(
            &g,
            vec![
                line("Stationery - A4 copier paper 75 gsm", 10_000_00),
                line(desc, amt),
            ],
        );
        let d = eval(
            &mut g,
            json!({"vendor_id":"V001","invoice_no":"RO-2","amount_paise":total,"po_ref":"PO-2026-0101"}),
            ev,
        );
        assert_eq!(d.verdict, Verdict::Review, "{desc} {amt}: {:?}", d.reasons);
        assert!(has(&d, "charge_not_on_po"));
    }
}

// ---------------------------------------------------------------- structured PO lines

fn po_with(lines: Vec<PoLine>) -> PurchaseOrder {
    PurchaseOrder {
        po_ref: "PO-T".into(),
        owner_vendor_id: "V001".into(),
        category: "Facilities".into(),
        currency: "INR".into(),
        open_balance_paise: 90_000_00,
        lines,
    }
}
fn rate(item: &str, qty: i64, unit: i64) -> PoLine {
    PoLine {
        item: item.into(),
        aliases: vec![],
        qty,
        unit_price_minor: unit,
        currency: None,
        unit: "hour".into(),
    }
}
fn lump(item: &str, cap: i64) -> PoLine {
    PoLine {
        item: item.into(),
        aliases: vec![],
        qty: 1,
        unit_price_minor: cap,
        currency: None,
        unit: "lot".into(),
    }
}
fn gov_with(po: PurchaseOrder) -> Governor {
    let mut g = gov();
    g.master.pos.insert(po.po_ref.clone(), po);
    g
}
fn run_po(g: &mut Governor, items: Vec<ItemLine>) -> Decision {
    let (ev, total) = v001_with_lines(g, items);
    eval(
        g,
        json!({"vendor_id":"V001","invoice_no":"PF-1","amount_paise":total,"po_ref":"PO-T"}),
        ev,
    )
}

#[test]
fn padded_fee_naming_a_contracted_word_mid_description_is_review() {
    // The reviewer's bypass: the old substring test passed this because "cleaning" appears in it.
    let mut g = gov_with(po_with(vec![lump("Cleaning", 50_000_00)]));
    let d = run_po(
        &mut g,
        vec![
            line("Cleaning - office deep clean", 20_000_00),
            line("Consulting fee — totally not cleaning", 15_000_00),
        ],
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(has(&d, "charge_not_on_po"));
    assert!(d
        .reasons
        .iter()
        .any(|r| r.detail.contains("Consulting fee")));
}

#[test]
fn padded_fee_worded_with_the_item_name_is_caught_by_rate_and_quantity() {
    // "IT services expedite" names the item, so only qty x unit price can stop it.
    let po = po_with(vec![rate("IT services", 10, 4_000_00)]);
    // billed as one unit at ₹5,000 against a ₹4,000 rate → over the unit price
    let mut g = gov_with(po.clone());
    let d = run_po(
        &mut g,
        vec![
            qline("IT services - support visits", 8, 4_000_00),
            line("IT services expedite", 5_000_00),
        ],
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(d
        .reasons
        .iter()
        .any(|r| r.rule == "charge_not_on_po" && r.detail.contains("expedite")));
    // billed at the contracted rate but past the contracted quantity → over the quantity
    let mut g = gov_with(po.clone());
    let d = run_po(
        &mut g,
        vec![
            qline("IT services - support visits", 10, 4_000_00),
            qline("IT services expedite", 1, 4_000_00),
        ],
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    // within rate and quantity → allowed
    let mut g = gov_with(po);
    let d = run_po(
        &mut g,
        vec![
            qline("IT services - support visits", 6, 4_000_00),
            qline("Server patching (IT services)", 2, 3_500_00),
        ],
    );
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn line_whose_quantity_times_rate_is_not_its_amount_is_review() {
    let mut g = gov_with(po_with(vec![rate("IT services", 10, 4_000_00)]));
    let mut l = qline("IT services - support visits", 2, 4_000_00);
    l.amount_paise = 12_000_00; // prints 2 x 4,000.00 = 12,000.00
    let d = run_po(&mut g, vec![l]);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
}

#[test]
fn discounted_rate_line_bills_less_and_fits_but_rate_and_quantity_still_bind() {
    // 2026-10-31: a printed line discount (amount below qty x rate) is not over-billing.
    let mut g = gov_with(po_with(vec![rate("IT services", 10, 4_000_00)]));
    let mut l = qline("IT services - support visits", 2, 4_000_00);
    l.amount_paise = 7_840_00; // 2 x 4,000.00 less 2% discount
    let d = run_po(&mut g, vec![l]);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    // discounted from a list rate above the PO rate: since 75d5863 the effective rate (amount / qty,
    // rounded up) is what is compared with the PO rate. 7,900 / 2 = 3,950 <= 4,000 -> fits.
    let mut g = gov_with(po_with(vec![rate("IT services", 10, 4_000_00)]));
    let mut l = qline("IT services - support visits", 2, 4_500_00);
    l.amount_paise = 7_900_00;
    let d = run_po(&mut g, vec![l]);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    // discounted, but the effective rate is still above the PO rate (8,400 / 2 = 4,200) -> REVIEW
    let mut g = gov_with(po_with(vec![rate("IT services", 10, 4_000_00)]));
    let mut l = qline("IT services - support visits", 2, 4_500_00);
    l.amount_paise = 8_400_00;
    let d = run_po(&mut g, vec![l]);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    // discounted, but more units than the PO line holds -> still REVIEW
    let mut g = gov_with(po_with(vec![rate("IT services", 10, 4_000_00)]));
    let mut l = qline("IT services - support visits", 11, 4_000_00);
    l.amount_paise = 40_000_00;
    let d = run_po(&mut g, vec![l]);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
}

#[test]
fn lump_sum_line_caps_the_total_billed_against_it() {
    let mut g = gov_with(po_with(vec![lump("Cleaning", 30_000_00)]));
    let d = run_po(
        &mut g,
        vec![
            line("Cleaning - floors", 20_000_00),
            line("Cleaning: windows", 9_000_00),
        ],
    );
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    let mut g = gov_with(po_with(vec![lump("Cleaning", 30_000_00)]));
    let d = run_po(
        &mut g,
        vec![
            line("Cleaning - floors", 20_000_00),
            line("Cleaning: windows", 11_000_00),
        ],
    );
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
}

#[test]
fn price_tolerance_is_configurable_and_zero_by_default() {
    let po = po_with(vec![rate("IT services", 10, 4_000_00)]);
    let mut g = gov_with(po.clone());
    let d = run_po(&mut g, vec![qline("IT services - visits", 2, 4_040_00)]); // +1%
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    let mut g = gov_with(po);
    g.master.policy.line_price_tolerance_bp = 100;
    let d = run_po(&mut g, vec![qline("IT services - visits", 2, 4_040_00)]);
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
}

#[test]
fn item_matching_rules() {
    let mut g = gov_with(po_with(vec![
        lump("Packaging (shrink wrap)", 50_000_00),
        PoLine {
            aliases: vec!["Stretch film".into()],
            ..lump("Wrap film", 50_000_00)
        },
    ]));
    for ok in [
        "Packaging (shrink wrap)",
        "Packaging (shrink wrap) - LDPE 25 mic",
        "Packaging (shrink wrap): LDPE",
        "stretch film rolls 23 mic (Packaging (shrink wrap))",
        "Stretch film - 23 mic",
        "PACKAGING (SHRINK WRAP)",
    ] {
        let d = run_po(&mut g, vec![line(ok, 1_000_00)]);
        assert_eq!(d.verdict, Verdict::Allow, "{ok}: {:?}", d.reasons);
        g.reset_state();
    }
    for bad in [
        "Packagingx",
        "Freight for Packaging (shrink wrap)",
        "Expedite fee",
        "shrink wrap",
        "Pallet (Packaging) shrink wrap",
    ] {
        let d = run_po(&mut g, vec![line(bad, 1_000_00)]);
        assert_eq!(d.verdict, Verdict::Review, "{bad}: {:?}", d.reasons);
        g.reset_state();
    }
}

#[test]
fn po_line_in_another_currency_never_matches() {
    let mut po = po_with(vec![PoLine {
        currency: Some("USD".into()),
        ..lump("Cleaning", 50_000_00)
    }]);
    po.currency = "INR".into();
    let mut g = gov_with(po);
    let d = run_po(&mut g, vec![line("Cleaning - floors", 1_000_00)]);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
}

#[test]
fn shipped_master_has_lines_for_every_po() {
    let g = gov();
    for po in g.master.pos.values() {
        assert!(!po.lines.is_empty(), "{}", po.po_ref);
        assert_eq!(
            po.currency,
            if po.owner_vendor_id == "V003" {
                "USD"
            } else {
                "INR"
            }
        );
    }
}

// ---------------------------------------------------------------- IFSC normaliser

#[test]
fn ifsc_normaliser() {
    assert_eq!(
        normalize::ifsc("barb0makarp").as_deref(),
        Some("BARB0MAKARP")
    );
    assert_eq!(
        normalize::ifsc(" BARB 0MAK ARP ").as_deref(),
        Some("BARB0MAKARP")
    );
    assert_eq!(
        normalize::ifsc("HDFC0001234").as_deref(),
        Some("HDFC0001234")
    );
    for bad in [
        "BARB1MAKARP",
        "BAR0MAKARP",
        "BARB0MAKARPX",
        "B4RB0MAKARP",
        "BARB-0MAKARP",
        "",
        "CHASUS33XXX",
    ] {
        assert_eq!(normalize::ifsc(bad), None, "{bad}");
    }
    assert!(normalize::same_bank_code("barb0makarp", "BARB0MAKARP"));
    assert!(!normalize::same_bank_code("BARB0MAKARQ", "BARB0MAKARP"));
    // a malformed code is never equal to a valid one, and two malformed codes compare as tokens
    assert!(!normalize::same_bank_code("BARB-0MAKARP", "BARB0MAKARP"));
    assert!(normalize::same_bank_code("chasus33xxx", "CHASUS33XXX"));
    assert!(!normalize::same_bank_code("", ""));
}

#[test]
fn lowercase_spaced_ifsc_on_the_invoice_is_not_a_tamper_alert_but_a_malformed_one_is() {
    let mut g = gov();
    let mut ev = ev_for(&g, "V001");
    let master_ifsc = g.master.vendors["V001"].ifsc.clone();
    ev.printed_ifsc = Some(format!(" {} ", master_ifsc.to_lowercase()));
    let d = eval(
        &mut g,
        json!({"vendor_id":"V001","invoice_no":"IF-1","amount_paise":10_000_00,"po_ref":"PO-2026-0101"}),
        ev.clone(),
    );
    assert_eq!(d.verdict, Verdict::Allow, "{:?}", d.reasons);
    ev.printed_ifsc = Some(master_ifsc.replacen('0', "O", 1)); // letter O in the fixed-zero slot
    let d = eval(
        &mut g,
        json!({"vendor_id":"V001","invoice_no":"IF-2","amount_paise":10_000_00,"po_ref":"PO-2026-0101"}),
        ev,
    );
    assert!(d.tamper_alert, "{:?}", d.reasons);
}

// ---------------------------------------------------------------- bounded decision store

fn decision(id: &str, verdict: Verdict) -> Decision {
    Decision {
        decision_id: id.into(),
        verdict,
        deterministic_verdict: verdict,
        reasons: vec![],
        signals: vec![],
        payee: None,
        tamper_alert: false,
        audit_hash: String::new(),
    }
}

#[test]
fn decision_store_is_bounded_and_keeps_pending_reviews() {
    let mut s = DecisionStore::new(3);
    s.insert(decision("d1", Verdict::Review));
    s.insert(decision("d2", Verdict::Allow));
    s.insert(decision("d3", Verdict::Block));
    s.insert(decision("d4", Verdict::Allow));
    assert_eq!(s.len(), 3);
    assert!(s.get("d1").is_some(), "pending REVIEW kept");
    assert!(s.get("d2").is_none(), "oldest non-review evicted");
    s.insert(decision("d5", Verdict::Review));
    s.insert(decision("d6", Verdict::Review));
    assert_eq!(s.len(), 3);
    assert!(s.get("d1").is_some() && s.get("d5").is_some() && s.get("d6").is_some());
    s.insert(decision("d7", Verdict::Review)); // all pending: oldest pending goes
    assert_eq!(s.len(), 3);
    assert!(s.get("d1").is_none() && s.get("d7").is_some());
    for i in 0..10_000 {
        s.insert(decision(&format!("x{i}"), Verdict::Allow));
    }
    assert_eq!(s.len(), 3);
    s.clear();
    assert!(s.is_empty());
}

// ---------------------------------------------------------------- HTTP: auth on /v1/audit/verify

fn http_get(port: u16, path: &str, key: Option<&str>) -> u16 {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = key
        .map(|k| format!("Authorization: Bearer {k}\r\n"))
        .unwrap_or_default();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn audit_verify_requires_the_api_key() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let key = "phase1-test-key-0123456789";
    let master = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dataset/fixtures/master.json"
    );
    // Server stderr goes to a file so a failed start says why.
    let errlog = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("phase1-audit-verify-{port}.log"));
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_governor-server"))
        .env("GOVERNOR_API_KEY", key)
        .env(
            "GOVERNOR_EXTRACTOR_PUBKEY",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        )
        .env("GOVERNOR_MASTER", master)
        .env("GOVERNOR_PORT", port.to_string())
        .env("GOVERNOR_DB", ":memory:") // Phase 3: keep this test from creating governor.db in the crate dir
        .env_remove("GOVERNOR_EVAL_CLOCK")
        .stderr(std::fs::File::create(&errlog).unwrap())
        .spawn()
        .unwrap();
    let mut up = false;
    // Up to ~20 s: the 5 s budget was flaky on a loaded shared runner.
    for _ in 0..400 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let result = std::panic::catch_unwind(|| {
        assert!(
            up,
            "server did not start within ~20 s; stderr: {}",
            std::fs::read_to_string(&errlog).unwrap_or_default()
        );
        assert_eq!(http_get(port, "/v1/audit/verify", None), 401);
        assert_eq!(
            http_get(port, "/v1/audit/verify", Some("wrong-key-wrong-key-00")),
            401
        );
        assert_eq!(http_get(port, "/v1/audit/verify", Some(key)), 200);
        assert_eq!(http_get(port, "/v1/decisions/dec_000001", None), 401);
        assert_eq!(http_get(port, "/health", None), 200);
    });
    let _ = child.kill();
    let _ = child.wait();
    result.unwrap();
}
