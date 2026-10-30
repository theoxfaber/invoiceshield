//! Phase 7 (product): the queue of REVIEW decisions still waiting for a human, for
//! GET /v1/reviews and the approver page at /v1/ui/review (governor/ui/review.*).
//!
//! Read-only. It changes no verdict and no state: a REVIEW is resolved only through the existing
//! POST /v1/decisions/:id/resolve, which keeps maker-checker (the proposer can never resolve its
//! own decision; persist.rs checks it and a SQLite trigger enforces it).
use crate::engine::Decision;
use crate::persist::{Result, Store};
use serde::Serialize;

/// One pending REVIEW: the stored decision (reason codes, signals, payee from the vendor
/// master) plus the ledger row it holds (vendor, invoice, amount, PO) and who proposed it.
#[derive(Debug, Clone, Serialize)]
pub struct PendingReview {
    pub decision_id: String,
    pub proposed_by: Option<String>,
    pub ts: i64,
    pub vendor_id: String,
    pub invoice_key: String,
    pub amount_minor: i64,
    pub currency: String,
    pub po_ref: String,
    pub irn: Option<String>,
    pub decision: Decision,
}

/// Upper bound on one listing; the page shows the oldest first.
pub const MAX_LIST: usize = 200;

type Row = (String, Option<String>, i64, String, String, String, i64, String, String, Option<String>);

impl Store {
    /// REVIEW decisions whose ledger entry is still `pending`, oldest first, at most `limit`
    /// (clamped to 1..=`MAX_LIST`). Resolved or rejected ones drop out of the queue.
    pub fn pending_reviews(&self, limit: usize) -> Result<Vec<PendingReview>> {
        let limit = limit.clamp(1, MAX_LIST) as i64;
        let mut st = self.conn.prepare(
            "SELECT d.decision_id, d.proposed_by, d.ts, d.body, l.vendor_id, l.invoice_key, l.amount_paise, l.currency, l.po_ref, l.irn
               FROM decisions d JOIN ledger l ON l.decision_id = d.decision_id
              WHERE d.verdict = 'review' AND l.status = 'pending'
              ORDER BY d.ts, d.decision_id LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| -> rusqlite::Result<Row> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (decision_id, proposed_by, ts, body, vendor_id, invoice_key, amount_minor, currency, po_ref, irn) = row?;
            let decision: Decision = serde_json::from_str(&body)?;
            out.push(PendingReview { decision_id, proposed_by, ts, vendor_id, invoice_key, amount_minor, currency, po_ref, irn, decision });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::{ActionOutcome, ResolveOutcome};
    use crate::{Master, Verdict};

    fn master() -> Master {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dataset/fixtures/master.json");
        Master::from_json(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn body(inv: &str, rupees: i64, po: &str) -> String {
        serde_json::json!({
            "proposal": {"vendor_id": "V001", "invoice_no": inv, "amount_paise": rupees * 100, "po_ref": po},
            "evidence": {"text_layer_gstin": "24ZZACS4821A1ZX", "sender_domain": "sharmaofficesupplies.in", "has_text_layer": true, "text_source": "text_layer"},
            "checker_raw": r#"{"signals":[]}"#
        })
        .to_string()
    }

    fn fresh(o: ActionOutcome) -> Decision {
        match o {
            ActionOutcome::Fresh(d) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lists_only_pending_reviews_and_drops_resolved_ones() {
        let mut s = Store::open_in_memory(master()).unwrap();
        let allow = fresh(s.act_as(&body("INV-Q-1", 1_000, "PO-2026-0101"), 1_000, None, Some("ap-agent")).unwrap());
        assert_eq!(allow.verdict, Verdict::Allow);
        let r1 = fresh(s.act_as(&body("INV-Q-2", 80_000, "PO-2026-0101"), 1_001, None, Some("ap-agent")).unwrap());
        let r2 = fresh(s.act_as(&body("INV-Q-3", 50_000, "PO-2026-0102"), 1_002, None, Some("ap-agent")).unwrap());
        assert_eq!((r1.verdict, r2.verdict), (Verdict::Review, Verdict::Review));
        let q = s.pending_reviews(50).unwrap();
        assert_eq!(q.iter().map(|p| p.decision_id.as_str()).collect::<Vec<_>>(), vec![r1.decision_id.as_str(), r2.decision_id.as_str()]);
        assert_eq!(q[0].proposed_by.as_deref(), Some("ap-agent"));
        assert_eq!((q[0].vendor_id.as_str(), q[0].amount_minor, q[0].currency.as_str()), ("V001", 8_000_000, "INR"));
        assert!(!q[0].decision.reasons.is_empty(), "reason codes travel with the queue entry");
        assert_eq!(s.pending_reviews(1).unwrap().len(), 1);
        // the proposer cannot clear it; a different approver can
        assert_eq!(s.resolve_as(&r1.decision_id, true, "ap-agent", 1_003).unwrap(), ResolveOutcome::SelfApproval);
        assert_eq!(s.pending_reviews(50).unwrap().len(), 2);
        assert_eq!(s.resolve_as(&r1.decision_id, true, "ravi", 1_004).unwrap(), ResolveOutcome::Resolved);
        assert_eq!(s.resolve_as(&r2.decision_id, false, "ravi", 1_005).unwrap(), ResolveOutcome::Resolved);
        assert!(s.pending_reviews(50).unwrap().is_empty());
    }
}
