//! Trusted state the document cannot change: vendor master, purchase orders, policy.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vendor {
    pub vendor_id: String,
    pub legal_name: String,
    /// None for foreign vendors without a GSTIN.
    pub gstin: Option<String>,
    pub pan: Option<String>,
    /// Payout beneficiary registered on the rail. The payee always comes from here.
    pub beneficiary_id: String,
    pub bank_account: String,
    pub ifsc: String,
    pub bank_name: String,
    pub email_domains: Vec<String>,
    pub category: String,
}

fn inr() -> String {
    "INR".into()
}

/// One structured line of a PO. Every non-tax invoice line must be matched to one of these.
///
/// `unit` decides how the line is checked:
/// * a rate line (any unit other than "lot", e.g. "nos", "ream", "hour"): each invoice line's
///   unit price must be <= `unit_price_minor` (+ policy tolerance) and the quantities billed
///   against it on one invoice must add up to <= `qty`;
/// * a lump-sum line (`unit = "lot"`): the invoice lines billed against it must add up to
///   <= `qty` x `unit_price_minor` (a value cap; quantity and rate printed on the invoice are only
///   checked for their own arithmetic).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoLine {
    pub item: String,
    /// Other names the same item is billed under (matched exactly like `item`).
    #[serde(default)]
    pub aliases: Vec<String>,
    pub qty: i64,
    /// Price per unit in minor units of `currency` (paise, cents).
    pub unit_price_minor: i64,
    /// ISO 4217; defaults to the PO's currency. A line in another currency never matches.
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default = "lot")]
    pub unit: String,
}

fn lot() -> String {
    "lot".into()
}

impl PoLine {
    pub fn is_lump_sum(&self) -> bool {
        self.unit.eq_ignore_ascii_case("lot")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurchaseOrder {
    pub po_ref: String,
    pub owner_vendor_id: String,
    pub category: String,
    /// ISO 4217 currency the PO is raised in. Proposal and invoice must be in the same one.
    #[serde(default = "inr")]
    pub currency: String,
    /// Open balance in minor units of `currency` (paise for INR, cents for USD). The name is kept
    /// from the INR-only schema so existing callers keep working.
    pub open_balance_paise: i64,
    /// What this PO buys, line by line. A charge line on an invoice that matches none of these,
    /// or bills more than the line allows, is an unlisted charge → REVIEW.
    #[serde(default)]
    pub lines: Vec<PoLine>,
}

/// Amount limits for one currency, in its minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Amounts strictly above this need a human (REVIEW).
    pub approval_threshold_minor: i64,
    /// Amounts strictly above this are refused (BLOCK).
    pub payout_cap_minor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// INR approval threshold in paise. Amounts strictly above this need a human (REVIEW).
    pub approval_threshold_paise: i64,
    /// INR payout cap in paise. Amounts strictly above this are refused (BLOCK).
    pub payout_cap_paise: i64,
    /// PO over-billing tolerance in basis points (200 = 2%).
    pub po_tolerance_bp: i64,
    /// Rolling per-vendor window for split-payment detection.
    pub window_secs: i64,
    /// Limits for currencies other than INR (INR uses the two fields above unless listed here).
    /// A currency with no limits is unknown to the governor: every proposal in it is REVIEW.
    #[serde(default = "default_currency_limits")]
    pub currency_limits: BTreeMap<String, Limits>,
    /// Allowed excess of an invoice line's unit price over the PO line's, in basis points (0 = none).
    #[serde(default)]
    pub line_price_tolerance_bp: i64,
    /// Look-back for the near-duplicate rule (R4: same vendor + amount, suffixed number). 90 days.
    #[serde(default = "default_near_dup_window")]
    pub near_duplicate_window_secs: i64,
    /// The buying entity's own GSTIN (review item 3). Its first two digits are the buyer's state,
    /// compared with the vendor's state (from the master GSTIN) to check IGST vs CGST/SGST.
    /// None: the tax-type rule does not run.
    #[serde(default)]
    pub buyer_gstin: Option<String>,
    /// Look-back for the resubmission rules (same itemized lines / bumped number, same PO and
    /// amount) and the per-PO drip rule. 30 days.
    #[serde(default = "default_resubmission_window")]
    pub resubmission_window_secs: i64,
}

fn default_resubmission_window() -> i64 {
    30 * 24 * 3600
}

fn default_near_dup_window() -> i64 {
    90 * 24 * 3600
}

/// USD limits are the INR ones at the eval rate 84.00, rounded down to whole dollars
/// (so USD is never looser than INR): ₹1,00,000 → $1,190; ₹5,00,000 → $5,952.
pub fn default_currency_limits() -> BTreeMap<String, Limits> {
    BTreeMap::from([("USD".to_string(), Limits { approval_threshold_minor: 1_190_00, payout_cap_minor: 5_952_00 })])
}

impl Policy {
    /// Limits for an ISO 4217 code, or None when the currency is not supported.
    pub fn limits(&self, currency: &str) -> Option<Limits> {
        if let Some(l) = self.currency_limits.get(currency) {
            return Some(*l);
        }
        (currency == "INR").then_some(Limits { approval_threshold_minor: self.approval_threshold_paise, payout_cap_minor: self.payout_cap_paise })
    }
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            approval_threshold_paise: 1_00_000_00,
            payout_cap_paise: 5_00_000_00,
            po_tolerance_bp: 200,
            window_secs: 7 * 24 * 3600,
            currency_limits: default_currency_limits(),
            line_price_tolerance_bp: 0,
            near_duplicate_window_secs: default_near_dup_window(),
            buyer_gstin: None,
            resubmission_window_secs: default_resubmission_window(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Master {
    pub vendors: BTreeMap<String, Vendor>,
    pub pos: BTreeMap<String, PurchaseOrder>,
    pub policy: Policy,
    /// Other accounts a vendor has registered (v3 prep, 2026-10-31; v1 BEN-19 class). A printed
    /// account/IFSC that matches one of these is not a tamper alert. Payment still goes ONLY to the
    /// vendor's primary `beneficiary_id`; these are never a payee. Loaded from the master file only
    /// (same trust as the initial primary account); changes go through the same review as any master edit.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub registered_accounts: BTreeMap<String, Vec<RegisteredAccount>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredAccount {
    pub vendor_id: String,
    pub bank_account: String,
    pub ifsc: String,
}

impl Master {
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            vendors: Vec<Vendor>,
            pos: Vec<PurchaseOrder>,
            #[serde(default)]
            policy: Option<Policy>,
            #[serde(default)]
            registered_accounts: Vec<RegisteredAccount>,
        }
        let raw: Raw = serde_json::from_str(s)?;
        Ok(Master {
            vendors: raw.vendors.into_iter().map(|v| (v.vendor_id.clone(), v)).collect(),
            pos: raw.pos.into_iter().map(|p| (p.po_ref.clone(), p)).collect(),
            policy: raw.policy.unwrap_or_default(),
            registered_accounts: raw.registered_accounts.into_iter().fold(BTreeMap::new(), |mut m, a| {
                m.entry(a.vendor_id.clone()).or_insert_with(Vec::new).push(a);
                m
            }),
        })
    }
}
