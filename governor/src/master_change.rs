//! Changing a vendor's bank details in the master (the one legitimate way the payee changes).
//!
//! The payment gate never takes an account from a document, so the attack moves to the master
//! itself. A change is applied only when all of these hold, checked here and fail-closed:
//! 1. a call-back was made to the phone number already on file for the vendor (not one taken
//!    from the change request, email or invoice);
//! 2. two distinct approvers signed off, neither of them the person who entered the change
//!    (maker-checker);
//! 3. the cooling-off period since the request has passed;
//! 4. the change is written to the hash-chained audit log (old and new details).
//!
//! Scope: a library function with tests. It is not exposed on the HTTP server, and approver IDs
//! are plain strings here; binding them to authenticated operator identities is design only.
use crate::audit::AuditLog;
use crate::master::Master;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Callback {
    /// The number actually dialled. Must equal the number on file for the vendor.
    pub number_dialled: String,
    pub called_by: String,
    pub at: i64,
    /// What the vendor's known contact confirmed on the call.
    pub confirmed_account: String,
    pub confirmed_ifsc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BankChangeRequest {
    pub vendor_id: String,
    pub new_account: String,
    pub new_ifsc: String,
    pub new_bank_name: String,
    /// New payout beneficiary registered on the rail for the new account.
    pub new_beneficiary_id: String,
    /// The maker: who entered the change.
    pub requested_by: String,
    pub requested_at: i64,
    pub callback: Option<Callback>,
    /// Checker approvals (approver id). Duplicates and the maker do not count.
    pub approvals: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeRefused {
    UnknownVendor,
    NoNumberOnFile,
    NoCallback,
    CallbackNotToNumberOnFile,
    CallbackDetailsDiffer,
    NeedTwoDistinctApprovers { have: usize },
    CoolingOff { remaining_secs: i64 },
}

/// Seconds a bank change must wait after it is entered (default 72 h).
pub const DEFAULT_COOLING_OFF_SECS: i64 = 72 * 3600;

fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Applies the change to `master` and audits it, or refuses with the first rule that fails.
/// `numbers_on_file` maps vendor_id to the phone captured at onboarding (trusted data).
pub fn apply_bank_change(
    master: &mut Master,
    audit: &mut AuditLog,
    req: &BankChangeRequest,
    numbers_on_file: &BTreeMap<String, String>,
    now: i64,
    cooling_off_secs: i64,
) -> Result<String, ChangeRefused> {
    let v = master
        .vendors
        .get(&req.vendor_id)
        .ok_or(ChangeRefused::UnknownVendor)?;
    let on_file = numbers_on_file
        .get(&req.vendor_id)
        .map(|n| digits(n))
        .filter(|n| !n.is_empty())
        .ok_or(ChangeRefused::NoNumberOnFile)?;
    let cb = req.callback.as_ref().ok_or(ChangeRefused::NoCallback)?;
    if digits(&cb.number_dialled) != on_file {
        return Err(ChangeRefused::CallbackNotToNumberOnFile);
    }
    if cb.confirmed_account.trim() != req.new_account.trim()
        || !cb
            .confirmed_ifsc
            .trim()
            .eq_ignore_ascii_case(req.new_ifsc.trim())
    {
        return Err(ChangeRefused::CallbackDetailsDiffer);
    }
    let maker = req.requested_by.trim().to_lowercase();
    let mut approvers: Vec<String> = req
        .approvals
        .iter()
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty() && *a != maker)
        .collect();
    approvers.sort();
    approvers.dedup();
    if approvers.len() < 2 {
        return Err(ChangeRefused::NeedTwoDistinctApprovers {
            have: approvers.len(),
        });
    }
    let ready_at = req.requested_at + cooling_off_secs;
    if now < ready_at {
        return Err(ChangeRefused::CoolingOff {
            remaining_secs: ready_at - now,
        });
    }
    let before = json!({"bank_account": v.bank_account, "ifsc": v.ifsc, "bank_name": v.bank_name,
                        "beneficiary_id": v.beneficiary_id});
    let v = master
        .vendors
        .get_mut(&req.vendor_id)
        .expect("checked above");
    v.bank_account = req.new_account.trim().to_string();
    v.ifsc = req.new_ifsc.trim().to_uppercase();
    v.bank_name = req.new_bank_name.clone();
    v.beneficiary_id = req.new_beneficiary_id.clone();
    Ok(audit.append(json!({
        "event": "vendor_bank_change", "vendor_id": req.vendor_id, "before": before,
        "after": {"bank_account": v.bank_account, "ifsc": v.ifsc, "bank_name": v.bank_name,
                  "beneficiary_id": v.beneficiary_id},
        "maker": maker, "approvers": approvers, "callback_by": cb.called_by, "callback_at": cb.at,
        "requested_at": req.requested_at, "applied_at": now,
    })))
}
