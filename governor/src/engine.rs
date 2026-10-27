//! The authoritative gate. Every check here is deterministic code over data the
//! document cannot change (STRUCTURAL) or over values extracted from the document
//! by deterministic code (EXTRACTION). Model output only enters as escalate-only signals.
use crate::audit::AuditLog;
use crate::ledger::{Entry, EntryStatus, Ledger};
use crate::master::Master;
use crate::normalize;
use crate::verdict::{merge, parse_checker_signals, Signal, Verdict};
use serde::{Deserialize, Serialize};

/// What the agent is allowed to say. No bank field exists, so the agent can
/// only choose which master vendor, which invoice and how much.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub vendor_id: String,
    pub invoice_no: String,
    pub amount_paise: i64,
    pub po_ref: String,
    #[serde(default)]
    pub irn: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemLine {
    pub description: String,
    pub amount_paise: i64,
    /// True for CGST / SGST / IGST / cess lines.
    #[serde(default)]
    pub is_tax: bool,
}

/// Values read from the document/email by deterministic code (assembler).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// GSTIN from the PDF text layer; None when the document has no text layer.
    #[serde(default)]
    pub text_layer_gstin: Option<String>,
    #[serde(default)]
    pub sender_domain: Option<String>,
    #[serde(default)]
    pub printed_bank_account: Option<String>,
    #[serde(default)]
    pub printed_ifsc: Option<String>,
    /// Every account-number-shaped value found anywhere in the document text, including
    /// text outside the bank box (a pasted "correct account" line is one more value here).
    #[serde(default)]
    pub all_printed_accounts: Vec<String>,
    /// Every IFSC-shaped value found anywhere in the document text.
    #[serde(default)]
    pub all_printed_ifscs: Vec<String>,
    /// The "Total Payable" figure the document prints, in paise. None when absent.
    #[serde(default)]
    pub printed_total_paise: Option<i64>,
    /// Charge and tax lines of the item table, read by deterministic code.
    #[serde(default)]
    pub item_lines: Vec<ItemLine>,
    /// True when the vendor's foreign / no-GSTIN status means GSTIN is not expected.
    #[serde(default)]
    pub has_text_layer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub proposal: serde_json::Value,
    #[serde(default)]
    pub evidence: Evidence,
    /// Raw checker output, parsed fail-closed by the governor itself.
    #[serde(default)]
    pub checker_raw: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Control {
    Structural,
    Extraction,
    Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reason {
    pub rule: String,
    pub verdict: Verdict,
    pub control: Control,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payee {
    pub beneficiary_id: String,
    pub bank_name: String,
    pub account_last4: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub decision_id: String,
    pub verdict: Verdict,
    pub deterministic_verdict: Verdict,
    pub reasons: Vec<Reason>,
    pub signals: Vec<Signal>,
    /// Set only when the verdict is ALLOW or REVIEW; always from the master.
    pub payee: Option<Payee>,
    pub tamper_alert: bool,
    pub audit_hash: String,
}

/// Keys that look like payment instructions. An agent that tries to send any
/// of them is either hijacked or misconfigured: BLOCK.
const BANK_KEYS: &[&str] = &[
    "account", "account_number", "bank_account", "bank_account_number", "acct", "ifsc", "bank_ifsc",
    "iban", "swift", "upi", "vpa", "beneficiary", "beneficiary_id", "beneficiary_details", "payee",
    "bank_name", "routing_number",
];

pub struct Governor {
    pub master: Master,
    pub ledger: Ledger,
    pub audit: AuditLog,
    next_id: u64,
}

struct Ctx {
    reasons: Vec<Reason>,
    tamper: bool,
}

impl Ctx {
    fn add(&mut self, rule: &str, verdict: Verdict, control: Control, detail: impl Into<String>) {
        self.reasons.push(Reason { rule: rule.into(), verdict, control, detail: detail.into() });
    }
    fn verdict(&self) -> Verdict {
        self.reasons.iter().map(|r| r.verdict).max().unwrap_or(Verdict::Allow)
    }
}

impl Governor {
    pub fn new(master: Master) -> Self {
        Governor { master, ledger: Ledger::default(), audit: AuditLog::default(), next_id: 1 }
    }

    /// Entry point for raw JSON from the wire. Unparseable request → REVIEW.
    pub fn evaluate_json(&mut self, raw: &str, now: i64) -> Decision {
        match serde_json::from_str::<Request>(raw) {
            Ok(req) => self.evaluate(&req, now),
            Err(e) => self.finish(
                None,
                Ctx { reasons: vec![reason("request_unparseable", Verdict::Review, Control::Structural, e.to_string())], tamper: false },
                vec![],
                now,
            ),
        }
    }

    pub fn evaluate(&mut self, req: &Request, now: i64) -> Decision {
        let mut ctx = Ctx { reasons: vec![], tamper: false };
        let signals = req.checker_raw.as_deref().map(parse_checker_signals).unwrap_or_else(|| {
            vec![Signal { kind: "checker_missing".into(), severity: crate::verdict::Severity::Review, detail: "no checker report".into() }]
        });

        // 1. Bank fields in the proposal → BLOCK (agent tried to choose the payee).
        if let Some(obj) = req.proposal.as_object() {
            let bad: Vec<&String> = obj.keys().filter(|k| BANK_KEYS.contains(&k.to_ascii_lowercase().as_str())).collect();
            if !bad.is_empty() {
                ctx.add("bank_fields_rejected", Verdict::Block, Control::Structural,
                    format!("proposal carried payment-instruction fields {bad:?}; the payee comes only from the vendor master"));
                return self.finish(None, ctx, signals, now);
            }
        }
        // 2. Strict proposal schema → else REVIEW.
        let p: Proposal = match serde_json::from_value(req.proposal.clone()) {
            Ok(p) => p,
            Err(e) => {
                ctx.add("proposal_invalid", Verdict::Review, Control::Structural, e.to_string());
                return self.finish(None, ctx, signals, now);
            }
        };
        if p.amount_paise <= 0 {
            ctx.add("amount_nonpositive", Verdict::Block, Control::Structural, "amount must be > 0");
            return self.finish(Some(&p), ctx, signals, now);
        }

        // 3. Vendor must exist in the master (fail closed).
        let Some(vendor) = self.master.vendors.get(&p.vendor_id).cloned() else {
            ctx.add("unknown_vendor", Verdict::Block, Control::Structural, format!("{} is not in the vendor master", p.vendor_id));
            return self.finish(Some(&p), ctx, signals, now);
        };
        let pol = self.master.policy.clone();

        // 4. PO ownership and open balance.
        match self.master.pos.get(&p.po_ref) {
            None => ctx.add("unknown_po", Verdict::Block, Control::Structural, format!("{} is not an open PO", p.po_ref)),
            Some(po) if po.owner_vendor_id != p.vendor_id => ctx.add("po_not_owned", Verdict::Block, Control::Structural,
                format!("{} belongs to {}, not {}", po.po_ref, po.owner_vendor_id, p.vendor_id)),
            Some(po) => {
                let available = po.open_balance_paise - self.ledger.po_committed(&po.po_ref);
                let hard = available + available.max(0) * pol.po_tolerance_bp / 10_000;
                if p.amount_paise > hard {
                    ctx.add("over_po_balance", Verdict::Block, Control::Structural,
                        format!("amount {} > PO open balance {} + tolerance", rupees(p.amount_paise), rupees(available)));
                } else if p.amount_paise > available {
                    ctx.add("within_po_tolerance", Verdict::Review, Control::Structural,
                        format!("amount {} exceeds PO open balance {} but is inside tolerance", rupees(p.amount_paise), rupees(available)));
                }
            }
        }

        // 5. Caps and approval threshold (document claims of approval are ignored).
        if p.amount_paise > pol.payout_cap_paise {
            ctx.add("over_payout_cap", Verdict::Block, Control::Structural, format!("amount {} > cap {}", rupees(p.amount_paise), rupees(pol.payout_cap_paise)));
        } else if p.amount_paise > pol.approval_threshold_paise {
            ctx.add("over_approval_threshold", Verdict::Review, Control::Structural, format!("amount {} > approval threshold {}", rupees(p.amount_paise), rupees(pol.approval_threshold_paise)));
        }

        // 6. Duplicates: exact key, IRN, same invoice with another amount.
        let key = normalize::invoice_no(&p.invoice_no);
        if let Some(e) = self.ledger.exact_duplicate(&p.vendor_id, &key, p.amount_paise) {
            ctx.add("duplicate_invoice", Verdict::Block, Control::Structural, format!("already {:?} under decision {}", e.status, e.decision_id));
        } else if let Some(e) = self.ledger.same_invoice_other_amount(&p.vendor_id, &key, p.amount_paise) {
            ctx.add("same_invoice_other_amount", Verdict::Review, Control::Structural,
                format!("invoice {} already has an entry of {}", p.invoice_no, rupees(e.amount_paise)));
        }
        if let Some(irn) = p.irn.as_deref() {
            if let Some(e) = self.ledger.irn_seen(irn) {
                ctx.add("duplicate_irn", Verdict::Block, Control::Structural, format!("IRN already used by decision {}", e.decision_id));
            }
        }

        // 7. Split payments: rolling per-vendor window (paid + pending + this one).
        let prior = self.ledger.window_sum(&p.vendor_id, now, pol.window_secs);
        if prior > 0 && prior + p.amount_paise > pol.approval_threshold_paise {
            ctx.add("vendor_window_exceeded", Verdict::Review, Control::Structural,
                format!("{} in the last {} days + this {} > threshold {}", rupees(prior), pol.window_secs / 86_400, rupees(p.amount_paise), rupees(pol.approval_threshold_paise)));
        }

        // 8. Issuer binding (EXTRACTION): the document must come from this vendor.
        if let Some(master_gstin) = vendor.gstin.as_deref() {
            match req.evidence.text_layer_gstin.as_deref() {
                Some(g) if normalize::gstin(g) == normalize::gstin(master_gstin) => {}
                Some(g) => ctx.add("issuer_gstin_mismatch", Verdict::Block, Control::Extraction,
                    format!("invoice GSTIN {g} is not {}'s GSTIN", vendor.vendor_id)),
                None => ctx.add("issuer_gstin_unreadable", Verdict::Review, Control::Extraction,
                    "no GSTIN in the text layer (scan or photo); a human must confirm the issuer"),
            }
        }
        if let Some(d) = req.evidence.sender_domain.as_deref() {
            let d = normalize::domain(d);
            if !vendor.email_domains.iter().any(|m| normalize::domain(m) == d) {
                ctx.add("sender_domain_mismatch", Verdict::Block, Control::Extraction,
                    format!("sent from {d}, which is not a domain on file for {}", vendor.vendor_id));
            }
        }

        // 9. Bank box on the document ≠ master → tamper alert (never changes the payee).
        let master_acct = digits(&vendor.bank_account);
        let master_ifsc = normalize::gstin(&vendor.ifsc);
        let acct_mismatch = req.evidence.printed_bank_account.iter().chain(req.evidence.all_printed_accounts.iter())
            .any(|a| digits(a) != master_acct);
        let ifsc_mismatch = req.evidence.printed_ifsc.iter().chain(req.evidence.all_printed_ifscs.iter())
            .any(|i| normalize::gstin(i) != master_ifsc);
        if acct_mismatch || ifsc_mismatch {
            ctx.tamper = true;
            ctx.add("bank_details_differ_from_master", Verdict::Review, Control::Extraction,
                format!("invoice asks for different bank details than {} has on file ({}). Payment can only go to the account on file; confirm with the vendor by phone.", vendor.vendor_id, vendor.bank_name));
        }

        // 10. Amount rules (EXTRACTION). The proposal must equal what the document asks for, the
        // document must add up, and every charge line must be something the PO actually buys.
        let ev = &req.evidence;
        if let Some(total) = ev.printed_total_paise {
            if total != p.amount_paise {
                ctx.add("amount_differs_from_printed_total", Verdict::Review, Control::Extraction,
                    format!("proposal {} but the invoice prints Total Payable {}", rupees(p.amount_paise), rupees(total)));
            }
            if !ev.item_lines.is_empty() {
                let sum: i64 = ev.item_lines.iter().map(|l| l.amount_paise).sum();
                if sum != total {
                    ctx.add("invoice_does_not_add_up", Verdict::Review, Control::Extraction,
                        format!("its lines add up to {} but it prints Total Payable {}", rupees(sum), rupees(total)));
                }
            }
        }
        if let Some(po) = self.master.pos.get(&p.po_ref) {
            let unlisted: Vec<&ItemLine> = ev.item_lines.iter().filter(|l| !l.is_tax)
                .filter(|l| {
                    let d = l.description.to_lowercase();
                    !po.contracted_items.iter().any(|c| !c.is_empty() && d.contains(&c.to_lowercase()))
                }).collect();
            if !unlisted.is_empty() {
                let names: Vec<String> = unlisted.iter().map(|l| format!("\"{}\" {}", l.description, rupees(l.amount_paise))).collect();
                ctx.add("charge_not_on_po", Verdict::Review, Control::Extraction,
                    format!("charge not covered by {}: {}", po.po_ref, names.join("; ")));
            }
        }

        self.finish(Some(&p), ctx, signals, now)
    }

    fn finish(&mut self, p: Option<&Proposal>, mut ctx: Ctx, signals: Vec<Signal>, now: i64) -> Decision {
        let deterministic = ctx.verdict();
        let mut verdict = merge(deterministic, &signals);
        // A tamper alert combined with any other non-allow reason escalates to BLOCK (design §4a).
        if ctx.tamper && ctx.reasons.iter().filter(|r| r.verdict >= Verdict::Review).count() > 1 {
            verdict = Verdict::Block;
        }
        if p.is_none() && verdict == Verdict::Allow {
            verdict = Verdict::Review; // no valid proposal can never pay
        }
        for s in &signals {
            if s.kind.starts_with("tamper") { ctx.tamper = true; }
        }
        let decision_id = format!("dec_{:06}", self.next_id);
        self.next_id += 1;

        let payee = match (p, verdict) {
            (Some(p), Verdict::Allow | Verdict::Review) => self.master.vendors.get(&p.vendor_id).map(|v| Payee {
                beneficiary_id: v.beneficiary_id.clone(),
                bank_name: v.bank_name.clone(),
                account_last4: v.bank_account.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect(),
            }),
            _ => None,
        };
        if let Some(p) = p {
            let status = match verdict {
                Verdict::Allow => Some(EntryStatus::Paid),
                Verdict::Review => Some(EntryStatus::Pending),
                Verdict::Block => None,
            };
            if let Some(status) = status {
                self.ledger.entries.push(Entry {
                    decision_id: decision_id.clone(),
                    vendor_id: p.vendor_id.clone(),
                    invoice_key: normalize::invoice_no(&p.invoice_no),
                    irn: p.irn.clone(),
                    amount_paise: p.amount_paise,
                    po_ref: p.po_ref.clone(),
                    status,
                    ts: now,
                });
            }
        }
        let audit_hash = self.audit.append(serde_json::json!({
            "decision_id": decision_id, "ts": now, "proposal": p, "verdict": verdict,
            "deterministic_verdict": deterministic, "reasons": ctx.reasons, "signals": signals,
        }));
        Decision { decision_id, verdict, deterministic_verdict: deterministic, reasons: ctx.reasons, signals, payee, tamper_alert: ctx.tamper, audit_hash }
    }

    /// Eval only (the server exposes it solely under GOVERNOR_EVAL_CLOCK=1): forget all state
    /// so independent samples of the same invoice do not trip the duplicate rules.
    pub fn reset_state(&mut self) {
        self.ledger = Ledger::default();
        self.audit = AuditLog::default();
        self.next_id = 1;
    }

    /// Human decision on a pending REVIEW (out of band, never via the agent).
    pub fn resolve_review(&mut self, decision_id: &str, approve: bool, approver: &str, now: i64) -> bool {
        let ok = self.ledger.set_status(decision_id, if approve { EntryStatus::Paid } else { EntryStatus::Rejected });
        if ok {
            self.audit.append(serde_json::json!({"resolve": decision_id, "approve": approve, "approver": approver, "ts": now}));
        }
        ok
    }
}

fn reason(rule: &str, verdict: Verdict, control: Control, detail: String) -> Reason {
    Reason { rule: rule.into(), verdict, control, detail }
}

fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Indian-format rupees for human-readable reasons, e.g. ₹1,00,000.00
pub fn rupees(paise: i64) -> String {
    let neg = paise < 0;
    let p = paise.unsigned_abs();
    let (r, ps) = (p / 100, p % 100);
    let s = r.to_string();
    let out = if s.len() <= 3 {
        s
    } else {
        let (head, tail) = s.split_at(s.len() - 3);
        let mut groups: Vec<String> = vec![];
        let hb = head.as_bytes();
        let mut i = hb.len();
        while i > 0 {
            let start = i.saturating_sub(2);
            groups.push(head[start..i].to_string());
            i = start;
        }
        groups.reverse();
        format!("{},{}", groups.join(","), tail)
    };
    format!("{}₹{}.{:02}", if neg { "-" } else { "" }, out, ps)
}
