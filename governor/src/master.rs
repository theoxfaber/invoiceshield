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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurchaseOrder {
    pub po_ref: String,
    pub owner_vendor_id: String,
    pub category: String,
    pub open_balance_paise: i64,
    /// What this PO actually buys: a charge line on an invoice must name one of these
    /// (case-insensitive substring). Anything else is an unlisted charge → REVIEW.
    #[serde(default)]
    pub contracted_items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Amounts strictly above this need a human (REVIEW).
    pub approval_threshold_paise: i64,
    /// Amounts strictly above this are refused (BLOCK).
    pub payout_cap_paise: i64,
    /// PO over-billing tolerance in basis points (200 = 2%).
    pub po_tolerance_bp: i64,
    /// Rolling per-vendor window for split-payment detection.
    pub window_secs: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            approval_threshold_paise: 1_00_000_00,
            payout_cap_paise: 5_00_000_00,
            po_tolerance_bp: 200,
            window_secs: 7 * 24 * 3600,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Master {
    pub vendors: BTreeMap<String, Vendor>,
    pub pos: BTreeMap<String, PurchaseOrder>,
    pub policy: Policy,
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
        }
        let raw: Raw = serde_json::from_str(s)?;
        Ok(Master {
            vendors: raw.vendors.into_iter().map(|v| (v.vendor_id.clone(), v)).collect(),
            pos: raw.pos.into_iter().map(|p| (p.po_ref.clone(), p)).collect(),
            policy: raw.policy.unwrap_or_default(),
        })
    }
}
