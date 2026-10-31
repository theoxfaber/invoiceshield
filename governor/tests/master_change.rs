use invoiceshield_governor::audit::AuditLog;
use invoiceshield_governor::master::{Master, Vendor};
use invoiceshield_governor::master_change::{
    apply_bank_change, BankChangeRequest, Callback, ChangeRefused,
};
use std::collections::BTreeMap;

const H: i64 = 3600;
const COOL: i64 = 72 * H;

fn setup() -> (Master, AuditLog, BTreeMap<String, String>) {
    let mut m = Master::default();
    m.vendors.insert(
        "V001".into(),
        Vendor {
            vendor_id: "V001".into(),
            legal_name: "Sharma Office Supplies Pvt Ltd".into(),
            gstin: None,
            pan: None,
            beneficiary_id: "BEN_V001".into(),
            bank_account: "77770000000101".into(),
            ifsc: "BARB0MAKARP".into(),
            bank_name: "Bank of Baroda".into(),
            email_domains: vec!["sharmaoffice.in.test".into()],
            category: "Stationery".into(),
        },
    );
    let phones = BTreeMap::from([("V001".to_string(), "+91 265 000 0101".to_string())]);
    (m, AuditLog::default(), phones)
}

fn req(approvals: &[&str], dialled: &str) -> BankChangeRequest {
    BankChangeRequest {
        vendor_id: "V001".into(),
        new_account: "88880000000101".into(),
        new_ifsc: "CNRB0001234".into(),
        new_bank_name: "Canara Bank".into(),
        new_beneficiary_id: "BEN_V001_2".into(),
        requested_by: "maker.anita".into(),
        requested_at: 0,
        callback: Some(Callback {
            number_dialled: dialled.into(),
            called_by: "maker.anita".into(),
            at: H,
            confirmed_account: "88880000000101".into(),
            confirmed_ifsc: "cnrb0001234".into(),
        }),
        approvals: approvals.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn needs_two_distinct_approvers_not_the_maker() {
    let (mut m, mut a, p) = setup();
    for (apps, have) in [
        (&[][..], 0),
        (&["chk.ravi"][..], 1),
        (&["chk.ravi", "CHK.Ravi "][..], 1),
        (&["chk.ravi", "maker.anita"][..], 1),
    ] {
        let r = apply_bank_change(&mut m, &mut a, &req(apps, "+912650000101"), &p, COOL, COOL);
        assert_eq!(
            r,
            Err(ChangeRefused::NeedTwoDistinctApprovers { have }),
            "{apps:?}"
        );
    }
    assert_eq!(m.vendors["V001"].bank_account, "77770000000101");
    assert!(a.records.is_empty());
}

#[test]
fn callback_must_go_to_number_already_on_file() {
    let (mut m, mut a, p) = setup();
    let r = apply_bank_change(
        &mut m,
        &mut a,
        &req(&["chk.ravi", "chk.meena"], "+91 712 555 0199"),
        &p,
        COOL,
        COOL,
    );
    assert_eq!(r, Err(ChangeRefused::CallbackNotToNumberOnFile));
    let mut no_cb = req(&["chk.ravi", "chk.meena"], "");
    no_cb.callback = None;
    assert_eq!(
        apply_bank_change(&mut m, &mut a, &no_cb, &p, COOL, COOL),
        Err(ChangeRefused::NoCallback)
    );
    assert_eq!(
        apply_bank_change(&mut m, &mut a, &no_cb, &BTreeMap::new(), COOL, COOL),
        Err(ChangeRefused::NoNumberOnFile)
    );
}

#[test]
fn cooling_off_then_applied_and_audited() {
    let (mut m, mut a, p) = setup();
    let r = req(&["chk.ravi", "chk.meena"], "+912650000101");
    assert_eq!(
        apply_bank_change(&mut m, &mut a, &r, &p, COOL - 1, COOL),
        Err(ChangeRefused::CoolingOff { remaining_secs: 1 })
    );
    assert!(apply_bank_change(&mut m, &mut a, &r, &p, COOL, COOL).is_ok());
    let v = &m.vendors["V001"];
    assert_eq!(
        (
            v.bank_account.as_str(),
            v.ifsc.as_str(),
            v.beneficiary_id.as_str()
        ),
        ("88880000000101", "CNRB0001234", "BEN_V001_2")
    );
    assert_eq!(a.records.len(), 1);
    assert_eq!(
        a.records[0].payload["before"]["bank_account"],
        "77770000000101"
    );
    assert_eq!(
        a.records[0].payload["approvers"],
        serde_json::json!(["chk.meena", "chk.ravi"])
    );
    assert_eq!(a.verify(), None);
}

#[test]
fn callback_confirming_other_details_is_refused() {
    let (mut m, mut a, p) = setup();
    let mut r = req(&["chk.ravi", "chk.meena"], "+912650000101");
    r.callback.as_mut().unwrap().confirmed_account = "77770000000101".into();
    assert_eq!(
        apply_bank_change(&mut m, &mut a, &r, &p, COOL, COOL),
        Err(ChangeRefused::CallbackDetailsDiffer)
    );
}
