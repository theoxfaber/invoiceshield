//! Payment ledger: paid and pending (REVIEW) entries per vendor.
//! Blocked attempts are never recorded, so they cannot inflate or hide a window sum.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    Paid,
    Pending,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub decision_id: String,
    pub vendor_id: String,
    pub invoice_key: String,
    pub irn: Option<String>,
    /// Minor units of `currency` (the name is kept from the INR-only schema).
    pub amount_paise: i64,
    /// ISO 4217 code of the amount. Sums never mix currencies.
    #[serde(default = "inr")]
    pub currency: String,
    pub po_ref: String,
    pub status: EntryStatus,
    pub ts: i64,
    /// `normalize::near_dup_base` of the invoice number, when it has a re-submission suffix
    /// (R4). Stored inside the entry JSON, so the SQLite store persists it with no schema change;
    /// entries written before it existed read as None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_key: Option<String>,
    /// Fingerprint of the signed text layer with the invoice number removed (review item 3,
    /// ATK-10 class: same document re-sent under a bumped number). None when no text was signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key: Option<String>,
}

fn inr() -> String {
    "INR".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub entries: Vec<Entry>,
}

impl Ledger {
    fn live(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.status != EntryStatus::Rejected)
    }

    pub fn exact_duplicate(&self, vendor_id: &str, invoice_key: &str, amount: i64) -> Option<&Entry> {
        self.live()
            .find(|e| e.vendor_id == vendor_id && e.invoice_key == invoice_key && e.amount_paise == amount)
    }

    pub fn same_invoice_other_amount(&self, vendor_id: &str, invoice_key: &str, amount: i64) -> Option<&Entry> {
        self.live()
            .find(|e| e.vendor_id == vendor_id && e.invoice_key == invoice_key && e.amount_paise != amount)
    }

    /// Near-duplicate (R4): same vendor, same amount and currency, inside (now - window, now], and
    /// one invoice number is the other plus a re-submission suffix ("INV-101" / "INV-101A").
    /// Exact matches are `exact_duplicate`'s job and are skipped here.
    #[allow(clippy::too_many_arguments)]
    pub fn near_duplicate(&self, vendor_id: &str, currency: &str, amount: i64, key: &str, base: Option<&str>, now: i64, window_secs: i64) -> Option<&Entry> {
        self.live().find(|e| {
            e.vendor_id == vendor_id
                && e.currency == currency
                && e.amount_paise == amount
                && e.invoice_key != key
                && e.ts > now - window_secs
                && e.ts <= now
                && (base == Some(e.invoice_key.as_str()) || e.near_key.as_deref() == Some(key))
        })
    }

    /// Sequential re-submission (ATK-10 class): same vendor, PO, amount and currency, inside
    /// (now - window, now], and the invoice numbers differ only by a trailing number one or two apart.
    #[allow(clippy::too_many_arguments)]
    pub fn sequential_resubmission(&self, vendor_id: &str, po_ref: &str, currency: &str, amount: i64, key: &str, now: i64, window_secs: i64) -> Option<&Entry> {
        self.live().find(|e| {
            e.vendor_id == vendor_id
                && e.po_ref == po_ref
                && e.currency == currency
                && e.amount_paise == amount
                && e.invoice_key != key
                && e.ts > now - window_secs
                && e.ts <= now
                && crate::normalize::sequential_pair(&e.invoice_key, key)
        })
    }

    /// Same document content (fingerprint of the text layer minus the invoice number) from the same
    /// vendor under another invoice number, inside (now - window, now].
    pub fn same_content(&self, vendor_id: &str, key: &str, content_key: &str, now: i64, window_secs: i64) -> Option<&Entry> {
        self.live().find(|e| {
            e.vendor_id == vendor_id
                && e.invoice_key != key
                && e.content_key.as_deref() == Some(content_key)
                && e.ts > now - window_secs
                && e.ts <= now
        })
    }

    pub fn irn_seen(&self, irn: &str) -> Option<&Entry> {
        self.live().find(|e| e.irn.as_deref() == Some(irn))
    }

    /// Sum of paid + pending for a vendor in one currency in (now - window, now].
    /// None on overflow (the caller fails closed).
    pub fn window_sum(&self, vendor_id: &str, currency: &str, now: i64, window_secs: i64) -> Option<i64> {
        let since = now.checked_sub(window_secs)?;
        self.live()
            .filter(|e| e.vendor_id == vendor_id && e.currency == currency && e.ts > since && e.ts <= now)
            .try_fold(0i64, |s, e| s.checked_add(e.amount_paise))
    }

    /// Amount already committed (paid + pending) against a PO, in the PO's currency.
    /// None on overflow (the caller fails closed).
    pub fn po_committed(&self, po_ref: &str, currency: &str) -> Option<i64> {
        self.live().filter(|e| e.po_ref == po_ref && e.currency == currency)
            .try_fold(0i64, |s, e| s.checked_add(e.amount_paise))
    }

    pub fn set_status(&mut self, decision_id: &str, status: EntryStatus) -> bool {
        match self.entries.iter_mut().find(|e| e.decision_id == decision_id) {
            Some(e) if e.status == EntryStatus::Pending => {
                e.status = status;
                true
            }
            _ => false,
        }
    }
}
