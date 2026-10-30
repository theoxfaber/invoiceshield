//! Vendor bank-detail changes as a persisted, identity-bound workflow (Phase 4).
//!
//! `master_change::apply_bank_change` holds the rule (call-back to the number on file, two
//! distinct approvers who are not the maker, cooling-off, audit). This module walks a change
//! through those steps over HTTP with authenticated identities and stores every step in SQLite:
//!
//! 1. `request` (operator = maker) → `awaiting_callback`. The gate keeps paying the old account;
//!    an invoice printing the new one still REVIEWs with a tamper alert (engine unchanged).
//! 2. `callback` (operator or approver, not the maker): the number dialled must be the vendor's
//!    number on file (`GOVERNOR_VENDOR_PHONES`), and what the vendor's known contact confirmed
//!    must equal the requested account + IFSC. Wrong number → refused, change stays open.
//!    Different details → change rejected. Success → `awaiting_approvals`.
//! 3. `approve` (approver, not the maker; DB trigger too) ×2 distinct identities → `cooling_off`
//!    with `effective_at = second approval + cooling-off`.
//! 4. The change is applied (master updated, `vendor_bank_change` audit record via
//!    `apply_bank_change`) inside the first write transaction whose clock is ≥ `effective_at`
//!    (the next payment request or workflow call; the eval clock counts when it is on). Until then
//!    no payment to the new details can be ALLOW, because the master still holds the old ones.
//!
//! Every step appends to the hash-chained audit log in the same transaction.
use crate::audit::AuditLog;
use crate::engine::Governor;
use crate::master::Master;
use crate::master_change::{apply_bank_change, BankChangeRequest, Callback, ChangeRefused};
use crate::persist::{Result, Store, StoreError};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// New bank details a maker enters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDetails {
    pub new_account: String,
    pub new_ifsc: String,
    pub new_bank_name: String,
    pub new_beneficiary_id: String,
}

/// What the person making the call-back records.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallbackReport {
    pub number_dialled: String,
    pub confirmed_account: String,
    pub confirmed_ifsc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeView {
    pub change_id: String,
    pub vendor_id: String,
    pub status: String,
    pub maker: String,
    pub requested_at: i64,
    pub new_details: serde_json::Value,
    pub callback: Option<serde_json::Value>,
    pub approvals: Vec<String>,
    pub approved_at: Option<i64>,
    pub effective_at: Option<i64>,
    pub applied_at: Option<i64>,
    pub rejected_by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum McError {
    NotFound,
    UnknownVendor,
    InvalidDetails(String),
    /// Another change for this vendor is still open.
    OpenChangeExists(String),
    WrongState(String),
    /// The maker tried to confirm the call-back or approve its own change.
    MakerCannot,
    AlreadyApproved,
    Refused(ChangeRefused),
}

impl McError {
    pub fn code(&self) -> &'static str {
        match self {
            McError::NotFound => "not_found",
            McError::UnknownVendor => "unknown_vendor",
            McError::InvalidDetails(_) => "invalid_details",
            McError::OpenChangeExists(_) => "open_change_exists",
            McError::WrongState(_) => "wrong_state",
            McError::MakerCannot => "maker_cannot_check_own_change",
            McError::AlreadyApproved => "already_approved_by_this_identity",
            McError::Refused(ChangeRefused::NoNumberOnFile) => "no_number_on_file",
            McError::Refused(ChangeRefused::CallbackNotToNumberOnFile) => "callback_not_to_number_on_file",
            McError::Refused(ChangeRefused::CallbackDetailsDiffer) => "callback_details_differ",
            McError::Refused(_) => "refused",
        }
    }
}

pub type McResult = std::result::Result<ChangeView, McError>;

fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn set_bank(master: &mut Master, vendor_id: &str, d: &NewDetails) {
    if let Some(v) = master.vendors.get_mut(vendor_id) {
        v.bank_account = d.new_account.trim().to_string();
        v.ifsc = d.new_ifsc.trim().to_uppercase();
        v.bank_name = d.new_bank_name.clone();
        v.beneficiary_id = d.new_beneficiary_id.clone();
    }
}

/// HMAC-SHA256 (RFC 2104) with the `sha2` crate already in the tree.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

fn change_mac(key: &[u8; 32], change_id: &str, vendor_id: &str, new_details: &str, applied_at: i64) -> String {
    let msg = format!("invoiceshield-master-change-v1\n{change_id}\n{vendor_id}\n{new_details}\n{applied_at}\n");
    hex::encode(hmac_sha256(key, msg.as_bytes()))
}

/// `base` with every applied change replayed in order. With a state key, an applied row whose
/// MAC is missing or wrong was not written by this governor: that vendor is removed from the
/// effective master (every payment to it BLOCKs `unknown_vendor`) and the problem is logged.
pub fn effective_master(c: &Connection, base: &Master, key: Option<&[u8; 32]>) -> Result<Master> {
    let mut m = base.clone();
    let mut st = c.prepare("SELECT change_id, vendor_id, new_details, applied_at, mac FROM master_changes WHERE status = 'applied' ORDER BY applied_at, seq")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, Option<String>>(4)?))
    })?;
    let mut tampered = vec![];
    for row in rows {
        let (id, vid, nd, applied_at, mac) = row?;
        if let Some(k) = key {
            let want = change_mac(k, &id, &vid, &nd, applied_at.unwrap_or(i64::MIN));
            let ok = mac.as_deref().is_some_and(|got| bool::from(subtle::ConstantTimeEq::ct_eq(got.as_bytes(), want.as_bytes())));
            if !ok {
                eprintln!("STATE INTEGRITY: applied bank change {id} for {vid} has no valid MAC; vendor {vid} is blocked until this is investigated");
                tampered.push(vid);
                continue;
            }
        }
        set_bank(&mut m, &vid, &serde_json::from_str(&nd)?);
    }
    for vid in tampered {
        m.vendors.remove(&vid);
    }
    Ok(m)
}

fn view(c: &Connection, id: &str) -> Result<Option<ChangeView>> {
    let row = c
        .query_row(
            "SELECT change_id, vendor_id, status, maker, requested_at, new_details, callback, approved_at, effective_at, applied_at, rejected_by, reason
             FROM master_changes WHERE change_id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, Option<i64>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                ))
            },
        )
        .optional()?;
    let Some((change_id, vendor_id, status, maker, requested_at, nd, cb, approved_at, effective_at, applied_at, rejected_by, reason)) = row else {
        return Ok(None);
    };
    let mut st = c.prepare("SELECT approver FROM master_change_approvals WHERE change_id = ?1 ORDER BY ts, approver")?;
    let approvals = st.query_map([id], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(ChangeView {
        change_id,
        vendor_id,
        status,
        maker,
        requested_at,
        new_details: serde_json::from_str(&nd)?,
        callback: cb.map(|c| serde_json::from_str(&c)).transpose()?,
        approvals,
        approved_at,
        effective_at,
        applied_at,
        rejected_by,
        reason,
    }))
}

fn approvals(c: &Connection, id: &str) -> Result<Vec<String>> {
    let mut st = c.prepare("SELECT approver FROM master_change_approvals WHERE change_id = ?1 ORDER BY approver")?;
    let v = st.query_map([id], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// Apply every change in `cooling_off` whose `effective_at` has passed, through the library rule.
pub fn apply_due(tx: &Transaction, gov: &mut Governor, phones: &BTreeMap<String, String>, cooling: i64, now: i64, key: Option<&[u8; 32]>) -> Result<()> {
    let due: Vec<(String, String, String, String, i64, String)> = {
        let mut st = tx.prepare(
            "SELECT change_id, vendor_id, new_details, maker, requested_at, COALESCE(callback, '')
             FROM master_changes WHERE status = 'cooling_off' AND effective_at <= ?1 ORDER BY effective_at, seq",
        )?;
        let rows = st.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, vid, nd, maker, requested_at, cb) in due {
        let d: NewDetails = serde_json::from_str(&nd)?;
        let cb: Callback = serde_json::from_str(&cb)?;
        let req = BankChangeRequest {
            vendor_id: vid.clone(),
            new_account: d.new_account.clone(),
            new_ifsc: d.new_ifsc.clone(),
            new_bank_name: d.new_bank_name.clone(),
            new_beneficiary_id: d.new_beneficiary_id.clone(),
            requested_by: maker,
            requested_at,
            callback: Some(cb),
            approvals: approvals(tx, &id)?,
        };
        let mut audit = std::mem::take(&mut gov.audit);
        let r = apply_bank_change(&mut gov.master, &mut audit, &req, phones, now, cooling);
        match r {
            Ok(_) => {
                let mac = key.map(|k| change_mac(k, &id, &vid, &nd, now));
                tx.execute("UPDATE master_changes SET status = 'applied', applied_at = ?1, mac = ?2 WHERE change_id = ?3", params![now, mac, id])?;
                // apply_bank_change's record lacks the change id; add a pointer record.
                audit.append(json!({"event": "vendor_bank_change_applied", "change_id": id, "vendor_id": vid, "ts": now}));
            }
            Err(e) => {
                // The rule no longer holds (e.g. the number on file changed since): fail closed.
                let reason = format!("apply refused: {e:?}");
                tx.execute(
                    "UPDATE master_changes SET status = 'rejected', rejected_by = 'governor', reason = ?1 WHERE change_id = ?2",
                    params![reason, id],
                )?;
                audit.append(json!({"event": "vendor_bank_change_rejected", "change_id": id, "vendor_id": vid, "by": "governor", "reason": reason, "ts": now}));
            }
        }
        gov.audit = audit;
    }
    Ok(())
}

impl Store {
    /// One write transaction for a workflow step: reload, apply due changes, run `f`, persist the
    /// new audit records, commit. Any error rolls everything back.
    fn mc_write(&mut self, now: i64, f: impl FnOnce(&Transaction, &mut AuditLog, &BTreeMap<String, String>, &Master) -> Result<McResult>) -> Result<McResult> {
        let r = (|| {
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            Store::sync(&tx, &mut self.gov, &mut self.generation, &self.base_master, self.state_key.as_ref())?;
            self.generation = -1;
            let aud0 = self.gov.audit.records.len();
            apply_due(&tx, &mut self.gov, &self.phones, self.cooling_off_secs, now, self.state_key.as_ref())?;
            let out = f(&tx, &mut self.gov.audit, &self.phones, &self.gov.master)?;
            for rec in &self.gov.audit.records[aud0..] {
                tx.execute(
                    "INSERT INTO audit (seq, prev_hash, payload, hash) VALUES (?1, ?2, ?3, ?4)",
                    params![rec.seq as i64, rec.prev_hash, serde_json::to_string(&rec.payload)?, rec.hash],
                )?;
            }
            let g: i64 = tx.query_row("UPDATE meta SET v = v + 1 WHERE k = 'generation' RETURNING v", [], |r| r.get(0))?;
            tx.commit()?;
            Ok::<_, StoreError>((out, g))
        })();
        match r {
            Ok((out, g)) => {
                self.generation = g;
                Ok(out)
            }
            Err(e) => {
                self.generation = -1;
                Err(e)
            }
        }
    }

    /// Step 1: an operator (the maker) enters new bank details for a vendor.
    pub fn mc_request(&mut self, maker: &str, vendor_id: &str, d: NewDetails, now: i64) -> Result<McResult> {
        let vendor_id = vendor_id.to_string();
        let maker = maker.to_string();
        self.mc_write(now, move |tx, audit, _phones, master| {
            if !master.vendors.contains_key(&vendor_id) {
                return Ok(Err(McError::UnknownVendor));
            }
            let acct = digits(&d.new_account);
            if acct.len() < 6 || acct.len() > 18 || acct != d.new_account.trim() {
                return Ok(Err(McError::InvalidDetails("new_account must be 6-18 digits".into())));
            }
            let ifsc = d.new_ifsc.trim();
            if ifsc.len() != 11 || !ifsc.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Ok(Err(McError::InvalidDetails("new_ifsc must be 11 alphanumeric characters".into())));
            }
            if d.new_beneficiary_id.trim().is_empty() || d.new_bank_name.trim().is_empty() {
                return Ok(Err(McError::InvalidDetails("new_bank_name and new_beneficiary_id are required".into())));
            }
            let open: Option<String> = tx
                .query_row("SELECT change_id FROM master_changes WHERE vendor_id = ?1 AND status NOT IN ('applied','rejected')", [&vendor_id], |r| r.get(0))
                .optional()?;
            if let Some(o) = open {
                return Ok(Err(McError::OpenChangeExists(o)));
            }
            let n: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM master_changes", [], |r| r.get(0))?;
            let id = format!("MC-{n:06}");
            tx.execute(
                "INSERT INTO master_changes (change_id, vendor_id, new_details, maker, requested_at, status) VALUES (?1, ?2, ?3, ?4, ?5, 'awaiting_callback')",
                params![id, vendor_id, serde_json::to_string(&d)?, maker, now],
            )?;
            audit.append(json!({"event": "vendor_bank_change_requested", "change_id": id, "vendor_id": vendor_id, "maker": maker,
                                "new": d, "ts": now}));
            Ok(Ok(view(tx, &id)?.expect("inserted")))
        })
    }

    /// Step 2: the out-of-band call-back to the number on file.
    pub fn mc_callback(&mut self, id: &str, by: &str, cb: CallbackReport, now: i64) -> Result<McResult> {
        let (id, by) = (id.to_string(), by.to_string());
        self.mc_write(now, move |tx, audit, phones, _master| {
            let Some(v) = view(tx, &id)? else { return Ok(Err(McError::NotFound)) };
            if v.status != "awaiting_callback" {
                return Ok(Err(McError::WrongState(v.status)));
            }
            if v.maker == by {
                return Ok(Err(McError::MakerCannot));
            }
            let d: NewDetails = serde_json::from_value(v.new_details.clone())?;
            let on_file = phones.get(&v.vendor_id).map(|n| digits(n)).filter(|n| !n.is_empty());
            let Some(on_file) = on_file else {
                audit.append(json!({"event": "vendor_bank_change_callback_refused", "change_id": id, "by": by, "reason": "no_number_on_file", "ts": now}));
                return Ok(Err(McError::Refused(ChangeRefused::NoNumberOnFile)));
            };
            if digits(&cb.number_dialled) != on_file {
                audit.append(json!({"event": "vendor_bank_change_callback_refused", "change_id": id, "by": by,
                                    "reason": "callback_not_to_number_on_file", "number_dialled": cb.number_dialled, "ts": now}));
                return Ok(Err(McError::Refused(ChangeRefused::CallbackNotToNumberOnFile)));
            }
            if cb.confirmed_account.trim() != d.new_account.trim() || !cb.confirmed_ifsc.trim().eq_ignore_ascii_case(d.new_ifsc.trim()) {
                let reason = "callback_details_differ: the vendor's known contact confirmed other details";
                tx.execute(
                    "UPDATE master_changes SET status = 'rejected', rejected_by = ?1, reason = ?2 WHERE change_id = ?3",
                    params![by, reason, id],
                )?;
                audit.append(json!({"event": "vendor_bank_change_rejected", "change_id": id, "by": by, "reason": reason,
                                    "confirmed_account": cb.confirmed_account, "confirmed_ifsc": cb.confirmed_ifsc, "ts": now}));
                return Ok(Err(McError::Refused(ChangeRefused::CallbackDetailsDiffer)));
            }
            let stored = Callback {
                number_dialled: cb.number_dialled.clone(),
                called_by: by.clone(),
                at: now,
                confirmed_account: cb.confirmed_account.trim().to_string(),
                confirmed_ifsc: cb.confirmed_ifsc.trim().to_string(),
            };
            tx.execute(
                "UPDATE master_changes SET status = 'awaiting_approvals', callback = ?1 WHERE change_id = ?2",
                params![serde_json::to_string(&stored)?, id],
            )?;
            audit.append(json!({"event": "vendor_bank_change_callback_verified", "change_id": id, "by": by, "number_dialled": cb.number_dialled, "ts": now}));
            Ok(Ok(view(tx, &id)?.expect("exists")))
        })
    }

    /// Step 3: a checker approves. The second distinct approver starts the cooling-off.
    pub fn mc_approve(&mut self, id: &str, approver: &str, now: i64) -> Result<McResult> {
        let (id, approver) = (id.to_string(), approver.to_string());
        let cooling = self.cooling_off_secs;
        self.mc_write(now, move |tx, audit, _phones, _master| {
            let Some(v) = view(tx, &id)? else { return Ok(Err(McError::NotFound)) };
            if v.status != "awaiting_approvals" {
                return Ok(Err(McError::WrongState(v.status)));
            }
            if v.maker == approver {
                return Ok(Err(McError::MakerCannot));
            }
            if v.approvals.contains(&approver) {
                return Ok(Err(McError::AlreadyApproved));
            }
            tx.execute("INSERT INTO master_change_approvals (change_id, approver, ts) VALUES (?1, ?2, ?3)", params![id, approver, now])?;
            audit.append(json!({"event": "vendor_bank_change_approved", "change_id": id, "approver": approver, "ts": now}));
            if approvals(tx, &id)?.len() >= 2 {
                let effective = now.saturating_add(cooling.max(0));
                tx.execute(
                    "UPDATE master_changes SET status = 'cooling_off', approved_at = ?1, effective_at = ?2 WHERE change_id = ?3",
                    params![now, effective, id],
                )?;
                audit.append(json!({"event": "vendor_bank_change_cooling_off", "change_id": id, "effective_at": effective, "ts": now}));
            }
            Ok(Ok(view(tx, &id)?.expect("exists")))
        })
    }

    /// Any open change can be stopped by an operator or approver (including during cooling-off).
    pub fn mc_reject(&mut self, id: &str, by: &str, reason: &str, now: i64) -> Result<McResult> {
        let (id, by, reason) = (id.to_string(), by.to_string(), reason.chars().take(500).collect::<String>());
        self.mc_write(now, move |tx, audit, _phones, _master| {
            let Some(v) = view(tx, &id)? else { return Ok(Err(McError::NotFound)) };
            if v.status == "applied" || v.status == "rejected" {
                return Ok(Err(McError::WrongState(v.status)));
            }
            tx.execute(
                "UPDATE master_changes SET status = 'rejected', rejected_by = ?1, reason = ?2 WHERE change_id = ?3",
                params![by, reason, id],
            )?;
            audit.append(json!({"event": "vendor_bank_change_rejected", "change_id": id, "by": by, "reason": reason, "ts": now}));
            Ok(Ok(view(tx, &id)?.expect("exists")))
        })
    }

    /// Apply whatever is due at `now` (normally done inside each payment transaction).
    pub fn mc_tick(&mut self, now: i64) -> Result<()> {
        self.mc_write(now, |_, _, _, _| Ok(Err(McError::NotFound))).map(|_| ())
    }

    pub fn mc_get(&self, id: &str) -> Result<Option<ChangeView>> {
        view(&self.conn, id)
    }
}
