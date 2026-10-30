//! Red-team findings R4 (suffix re-submission) and R6 (look-alike characters in bank fields).
//! Rules decided in docs/DECISIONS.md (Fri Oct 30, 2026, IST) before any eval re-run.
mod common;
use common::*;
use invoiceshield_governor::engine::{Governor, Request};
use invoiceshield_governor::normalize::{has_non_ascii, near_dup_base};
use invoiceshield_governor::persist::{ActionOutcome, Store};
use invoiceshield_governor::Verdict;

const DAY: i64 = 86_400;

fn fresh(o: ActionOutcome) -> invoiceshield_governor::Decision {
    match o {
        ActionOutcome::Fresh(d) => d,
        other => panic!("expected a fresh decision, got {other:?}"),
    }
}

fn eval(g: &mut Governor, raw: &str, now: i64) -> invoiceshield_governor::Decision {
    g.evaluate(&serde_json::from_str::<Request>(raw).unwrap(), now)
}

fn has(d: &invoiceshield_governor::Decision, rule: &str) -> bool {
    d.reasons.iter().any(|r| r.rule == rule)
}

#[test]
fn near_dup_base_strips_one_resubmission_suffix() {
    assert_eq!(near_dup_base("INV-101A").as_deref(), Some("INV101"));
    assert_eq!(near_dup_base("INV-101-A").as_deref(), Some("INV101"));
    assert_eq!(near_dup_base("INV-101-1").as_deref(), Some("INV101"));
    assert_eq!(near_dup_base("INV-101/02").as_deref(), Some("INV101"));
    assert_eq!(near_dup_base("SOS/26-27/0900-A").as_deref(), Some("SOS26270900"));
    assert_eq!(near_dup_base("inv-101 rev").as_deref(), Some("INV101"));
    // not a suffix: digits with no separator, a plain number, letters only, 4+ letters
    assert_eq!(near_dup_base("INV-1011"), None);
    assert_eq!(near_dup_base("INV-101"), None);
    assert_eq!(near_dup_base("ABC"), None);
    assert_eq!(near_dup_base("INV-101-COPY"), None);
    assert_eq!(near_dup_base("INV-101-123"), None);
}

#[test]
fn r4_suffix_resubmission_same_amount_reviews() {
    for suffix in ["INV-101A", "INV-101-1", "INV-101/A", "INV-101-R"] {
        let mut g = Governor::new(master());
        let a = eval(&mut g, &body("INV-101", 10_000), 0);
        assert_eq!(a.verdict, Verdict::Allow, "{:?}", a.reasons);
        let b = eval(&mut g, &body(suffix, 10_000), DAY);
        assert_eq!(b.verdict, Verdict::Review, "{suffix}: {:?}", b.reasons);
        assert!(has(&b, "near_duplicate_invoice"), "{suffix}: {:?}", b.reasons);
        assert!(!b.tamper_alert);
    }
}

#[test]
fn r4_works_in_both_directions() {
    // the suffixed number was paid first, then the plain one arrives
    let mut g = Governor::new(master());
    assert_eq!(eval(&mut g, &body("INV-101A", 10_000), 0).verdict, Verdict::Allow);
    let b = eval(&mut g, &body("INV-101", 10_000), DAY);
    assert_eq!(b.verdict, Verdict::Review, "{:?}", b.reasons);
    assert!(has(&b, "near_duplicate_invoice"));
}

#[test]
fn r4_not_triggered_by_other_amount_other_number_or_old_entry() {
    let mut g = Governor::new(master());
    assert_eq!(eval(&mut g, &body("INV-101", 10_000), 0).verdict, Verdict::Allow);
    // different amount: a genuine supplementary invoice is not this rule's business
    let d = eval(&mut g, &body("INV-101A", 9_000), DAY);
    assert!(!has(&d, "near_duplicate_invoice"), "{:?}", d.reasons);
    // the next number in sequence, same amount (monthly retainer)
    let d = eval(&mut g, &body("INV-102", 10_000), 2 * DAY);
    assert!(!has(&d, "near_duplicate_invoice"), "{:?}", d.reasons);
    // digits glued on without a separator are a different number
    let d = eval(&mut g, &body("INV-1019", 10_000), 3 * DAY);
    assert!(!has(&d, "near_duplicate_invoice"), "{:?}", d.reasons);
    // outside the 90-day window
    let mut g = Governor::new(master());
    assert_eq!(eval(&mut g, &body("INV-201", 10_000), 0).verdict, Verdict::Allow);
    let d = eval(&mut g, &body("INV-201A", 10_000), 91 * DAY);
    assert!(!has(&d, "near_duplicate_invoice"), "{:?}", d.reasons);
}

#[test]
fn r4_persists_across_restart_in_sqlite_store() {
    let dir = tmpdir("r4");
    let db = dir.join("g.db");
    {
        let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
        let a = fresh(s.act(&body("SOS/26-27/0900", 10_000), 100, None).unwrap());
        assert_eq!(a.verdict, Verdict::Allow, "{:?}", a.reasons);
    }
    let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
    let b = fresh(s.act(&body("SOS/26-27/0900-A", 10_000), 200, None).unwrap());
    assert_eq!(b.verdict, Verdict::Review, "{:?}", b.reasons);
    assert!(has(&b, "near_duplicate_invoice"));
    // and the reverse direction after a second restart: the stored suffixed entry's base
    drop(s);
    let mut s = Store::open(db.to_str().unwrap(), master()).unwrap();
    let e = s.ledger().unwrap().entries.iter().find(|e| e.decision_id == b.decision_id).cloned().unwrap();
    assert_eq!(e.near_key.as_deref(), Some("SOS26270900"));
    let c = fresh(s.act(&body("SOS/26-27/0900-B", 10_000), 300, None).unwrap());
    assert!(has(&c, "near_duplicate_invoice"), "{:?}", c.reasons);
}

fn with_bank(invoice_no: &str, acct: Option<&str>, ifsc: Option<&str>) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&body(invoice_no, 10_000)).unwrap();
    let ev = v["evidence"].as_object_mut().unwrap();
    if let Some(a) = acct {
        ev.insert("all_printed_accounts".into(), serde_json::json!([a]));
    }
    if let Some(i) = ifsc {
        ev.insert("all_printed_ifscs".into(), serde_json::json!([i]));
    }
    v.to_string()
}

fn master_bank() -> (String, String) {
    let v = master().vendors.get("V001").cloned().unwrap();
    (v.bank_account, v.ifsc)
}

#[test]
fn has_non_ascii_flags_lookalikes() {
    assert!(has_non_ascii("SВIN0001234")); // Cyrillic В
    assert!(has_non_ascii("１２３４")); // fullwidth
    assert!(has_non_ascii("1234\u{200B}5678"));
    assert!(!has_non_ascii("SBIN0001234"));
    assert!(!has_non_ascii("1234 5678"));
}

#[test]
fn r6_cyrillic_ifsc_raises_tamper_review() {
    let (acct, ifsc) = master_bank();
    let mut g = Governor::new(master());
    let clean = eval(&mut g, &with_bank("INV-501", Some(&acct), Some(&ifsc)), 0);
    assert_eq!(clean.verdict, Verdict::Allow, "{:?}", clean.reasons);
    // replace the first ASCII letter of the master IFSC with its Cyrillic twin
    let homo: String = ifsc.chars().enumerate().map(|(i, c)| if i == 0 { cyr(c) } else { c }).collect();
    assert_ne!(homo, ifsc);
    let d = eval(&mut g, &with_bank("INV-502", Some(&acct), Some(&homo)), DAY);
    assert!(d.tamper_alert, "{:?}", d.reasons);
    assert!(d.verdict >= Verdict::Review, "{:?}", d.reasons);
    assert!(d.reasons.iter().any(|r| r.detail.contains("non-ASCII")), "{:?}", d.reasons);
}

#[test]
fn r6_cyrillic_letter_inside_matching_account_digits_is_tamper() {
    // ASCII digits equal the master account; a Cyrillic О is slipped in. digits() would drop it
    // and call it a match, so the non-ASCII rule is what catches it.
    let (acct, ifsc) = master_bank();
    let mut forged = acct.clone();
    forged.insert(3, 'О');
    let mut g = Governor::new(master());
    let d = eval(&mut g, &with_bank("INV-601", Some(&forged), Some(&ifsc)), 0);
    assert!(d.tamper_alert, "{:?}", d.reasons);
    assert_eq!(d.verdict, Verdict::Review, "{:?}", d.reasons);
    assert!(has(&d, "bank_details_non_ascii"), "{:?}", d.reasons);
}

#[test]
fn r6_tamper_plus_another_problem_blocks_as_before() {
    let (acct, ifsc) = master_bank();
    let homo: String = ifsc.chars().enumerate().map(|(i, c)| if i == 0 { cyr(c) } else { c }).collect();
    let mut v: serde_json::Value = serde_json::from_str(&with_bank("INV-701", Some(&acct), Some(&homo))).unwrap();
    v["evidence"]["text_source"] = serde_json::json!("ocr"); // a second REVIEW reason
    let mut g = Governor::new(master());
    let d = eval(&mut g, &v.to_string(), 0);
    assert!(d.tamper_alert);
    assert_eq!(d.verdict, Verdict::Block, "{:?}", d.reasons);
}

fn cyr(c: char) -> char {
    match c {
        'A' => 'А', 'B' => 'В', 'C' => 'С', 'E' => 'Е', 'H' => 'Н', 'K' => 'К', 'M' => 'М', 'O' => 'О',
        'P' => 'Р', 'T' => 'Т', 'X' => 'Х', other => other,
    }
}
