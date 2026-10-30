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
    /// Amount in minor units of `currency` (paise for INR, cents for USD). The field name is
    /// kept from the INR-only schema so existing agents keep working.
    pub amount_paise: i64,
    /// ISO 4217 code of the payment. Defaults to INR when the agent does not say.
    #[serde(default = "inr")]
    pub currency: String,
    pub po_ref: String,
    #[serde(default)]
    pub irn: Option<String>,
}

fn inr() -> String {
    "INR".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemLine {
    pub description: String,
    /// Line amount in minor units of the invoice currency.
    pub amount_paise: i64,
    /// Quantity printed on the line ("12 x 250.00"), when the extractor could read one.
    #[serde(default)]
    pub qty: Option<i64>,
    /// Unit price printed on the line, minor units, when the extractor could read one.
    #[serde(default)]
    pub unit_price_minor: Option<i64>,
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
    /// "text_layer" or "ocr". OCR-read evidence is never trusted to clear an issuer on its own.
    #[serde(default)]
    pub text_source: Option<String>,
    /// ISO 4217 currency the document's amounts are in (₹/Rs/INR, $/USD, €/EUR, £/GBP...).
    /// None when the document prints no currency marker; the governor then assumes INR.
    #[serde(default)]
    pub currency: Option<String>,
    /// Every currency marker found in the document text. More than one distinct → REVIEW.
    #[serde(default)]
    pub all_currencies: Vec<String>,
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

        // 4. Currency (STRUCTURAL + EXTRACTION): the proposal, the PO and the document must all be in
        // one currency the policy has limits for. Amounts are never compared across currencies.
        let cur = normalize::currency(&p.currency);
        let limits = if normalize::is_iso4217_shape(&cur) { pol.limits(&cur) } else { None };
        if limits.is_none() {
            ctx.add("unknown_currency", Verdict::Review, Control::Structural,
                format!("proposal currency {:?} has no limits configured; a human must check it", p.currency));
        }
        let ev_cur = req.evidence.currency.as_deref().map(normalize::currency).unwrap_or_else(|| "INR".into());
        let mut doc_curs: Vec<String> = req.evidence.all_currencies.iter().map(|c| normalize::currency(c)).collect();
        doc_curs.push(ev_cur.clone());
        doc_curs.sort();
        doc_curs.dedup();
        if doc_curs.len() > 1 {
            ctx.add("currency_ambiguous", Verdict::Review, Control::Extraction,
                format!("the document names more than one currency: {}", doc_curs.join(", ")));
        }
        if doc_curs.iter().any(|c| *c != cur) {
            ctx.add("currency_mismatch_invoice", Verdict::Review, Control::Extraction,
                format!("proposal is in {cur} but the invoice is in {}", doc_curs.join("/")));
        }

        // 5. PO ownership and open balance (same currency only).
        match self.master.pos.get(&p.po_ref) {
            None => ctx.add("unknown_po", Verdict::Block, Control::Structural, format!("{} is not an open PO", p.po_ref)),
            Some(po) if po.owner_vendor_id != p.vendor_id => ctx.add("po_not_owned", Verdict::Block, Control::Structural,
                format!("{} belongs to {}, not {}", po.po_ref, po.owner_vendor_id, p.vendor_id)),
            Some(po) if normalize::currency(&po.currency) != cur => ctx.add("currency_mismatch_po", Verdict::Review, Control::Structural,
                format!("proposal is in {cur} but {} is raised in {}; amounts cannot be compared", po.po_ref, po.currency)),
            Some(po) => {
                let available = po.open_balance_paise - self.ledger.po_committed(&po.po_ref, &cur);
                let hard = available + available.max(0) * pol.po_tolerance_bp / 10_000;
                if p.amount_paise > hard {
                    ctx.add("over_po_balance", Verdict::Block, Control::Structural,
                        format!("amount {} > PO open balance {} + tolerance", money(p.amount_paise, &cur), money(available, &cur)));
                } else if p.amount_paise > available {
                    ctx.add("within_po_tolerance", Verdict::Review, Control::Structural,
                        format!("amount {} exceeds PO open balance {} but is inside tolerance", money(p.amount_paise, &cur), money(available, &cur)));
                }
            }
        }

        // 5b. Caps and approval threshold, per currency (document claims of approval are ignored).
        if let Some(l) = limits {
            if p.amount_paise > l.payout_cap_minor {
                ctx.add("over_payout_cap", Verdict::Block, Control::Structural, format!("amount {} > cap {}", money(p.amount_paise, &cur), money(l.payout_cap_minor, &cur)));
            } else if p.amount_paise > l.approval_threshold_minor {
                ctx.add("over_approval_threshold", Verdict::Review, Control::Structural, format!("amount {} > approval threshold {}", money(p.amount_paise, &cur), money(l.approval_threshold_minor, &cur)));
            }
        }

        // 6. Duplicates: exact key, IRN, same invoice with another amount.
        let key = normalize::invoice_no(&p.invoice_no);
        if let Some(e) = self.ledger.exact_duplicate(&p.vendor_id, &key, p.amount_paise) {
            ctx.add("duplicate_invoice", Verdict::Block, Control::Structural, format!("already {:?} under decision {}", e.status, e.decision_id));
        } else if let Some(e) = self.ledger.same_invoice_other_amount(&p.vendor_id, &key, p.amount_paise) {
            ctx.add("same_invoice_other_amount", Verdict::Review, Control::Structural,
                format!("invoice {} already has an entry of {}", p.invoice_no, money(e.amount_paise, &e.currency)));
        }
        if let Some(irn) = p.irn.as_deref() {
            if let Some(e) = self.ledger.irn_seen(irn) {
                ctx.add("duplicate_irn", Verdict::Block, Control::Structural, format!("IRN already used by decision {}", e.decision_id));
            }
        }

        // 7. Split payments: rolling per-vendor window (paid + pending + this one).
        let prior = self.ledger.window_sum(&p.vendor_id, &cur, now, pol.window_secs);
        if let Some(l) = limits {
            if prior > 0 && prior + p.amount_paise > l.approval_threshold_minor {
                ctx.add("vendor_window_exceeded", Verdict::Review, Control::Structural,
                    format!("{} in the last {} days + this {} > threshold {}", money(prior, &cur), pol.window_secs / 86_400, money(p.amount_paise, &cur), money(l.approval_threshold_minor, &cur)));
            }
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
        if req.evidence.text_source.as_deref() == Some("ocr") {
            ctx.add("evidence_read_by_ocr", Verdict::Review, Control::Extraction,
                "the document is an image; every extracted field came from OCR, so a human must confirm it");
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
        let acct_mismatch = req.evidence.printed_bank_account.iter().chain(req.evidence.all_printed_accounts.iter())
            .any(|a| digits(a) != master_acct);
        let ifsc_mismatch = req.evidence.printed_ifsc.iter().chain(req.evidence.all_printed_ifscs.iter())
            .any(|i| !normalize::same_bank_code(i, &vendor.ifsc));
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
                    format!("proposal {} but the invoice prints Total Payable {}", money(p.amount_paise, &cur), money(total, &ev_cur)));
            }
            if !ev.item_lines.is_empty() {
                let sum: i64 = ev.item_lines.iter().map(|l| l.amount_paise).sum();
                if sum != total {
                    ctx.add("invoice_does_not_add_up", Verdict::Review, Control::Extraction,
                        format!("its lines add up to {} but it prints Total Payable {}", money(sum, &ev_cur), money(total, &ev_cur)));
                }
            }
        }
        if let Some(po) = self.master.pos.get(&p.po_ref) {
            let problems = check_lines_against_po(po, &ev.item_lines, &ev_cur, pol.line_price_tolerance_bp);
            if !problems.is_empty() {
                ctx.add("charge_not_on_po", Verdict::Review, Control::Extraction,
                    format!("charge not covered by {}: {}", po.po_ref, problems.join("; ")));
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
                    currency: normalize::currency(&p.currency),
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

    /// A decision that never reached the rules (e.g. evidence failed provenance checks in
    /// `trust.rs`). Audited like any other; with no proposal it can never be ALLOW.
    pub fn reject(&mut self, rule: &str, verdict: Verdict, detail: String, now: i64) -> Decision {
        let ctx = Ctx { reasons: vec![reason(rule, verdict, Control::Structural, detail)], tamper: false };
        self.finish(None, ctx, vec![], now)
    }

    /// Eval only (the server exposes it solely under GOVERNOR_EVAL_CLOCK=1): forget all state
    /// so independent samples of the same invoice do not trip the duplicate rules.
    pub fn reset_state(&mut self) {
        self.ledger = Ledger::default();
        self.audit = AuditLog::default();
        self.next_id = 1;
    }

    /// Next decision number. Read/set by the durable store (persist.rs) so ids keep
    /// counting across restarts and processes; never affects a verdict.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn set_next_id(&mut self, n: u64) {
        self.next_id = n.max(1);
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

/// A non-tax "Round off" line under one unit of the currency (₹1 = 100 paise) is not a charge.
/// Decided 2026-10-30 before the held-out run (docs/DECISIONS.md, backlog #15).
pub fn is_round_off(l: &ItemLine) -> bool {
    !l.is_tax && l.amount_paise.abs() < 100
        && matches!(normalize::item_key(&l.description).as_str(), "round off" | "rounding" | "roundoff")
}

/// Does an invoice line description name this PO line? The description must be the item (or an
/// alias) itself, start with it as whole words ("Stationery - A4 paper", "Stationery: pens"), or end
/// with it in brackets ("A4 paper (Stationery)"). A word buried in the middle never matches
/// ("Consulting fee - totally not cleaning" does not name "cleaning").
fn names_item(desc: &str, line: &crate::master::PoLine) -> bool {
    let d = normalize::item_key(desc);
    let bracket = trailing_bracket(desc).map(normalize::item_key);
    std::iter::once(&line.item).chain(line.aliases.iter()).map(|n| normalize::item_key(n)).filter(|k| !k.is_empty()).any(|k| {
        d == k || d.strip_prefix(&k).is_some_and(|rest| rest.starts_with(' ')) || bracket.as_deref() == Some(k.as_str())
    })
}

/// The text inside a description's final, balanced bracket pair: "film (Packaging (shrink wrap))"
/// → "Packaging (shrink wrap)". None when the description does not end with ')'.
fn trailing_bracket(desc: &str) -> Option<&str> {
    let t = desc.trim_end();
    if !t.ends_with(')') {
        return None;
    }
    let mut depth = 0i32;
    for (i, c) in t.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&t[i + 1..t.len() - 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Structured PO-line matching (replaces the old substring test). Returns one problem per line that
/// is not covered; empty when every charge line fits a PO line.
pub fn check_lines_against_po(po: &crate::master::PurchaseOrder, items: &[ItemLine], invoice_currency: &str, tol_bp: i64) -> Vec<String> {
    use std::collections::BTreeMap;
    let po_cur = normalize::currency(&po.currency);
    let mut problems = vec![];
    // per PO line: (qty billed, value billed)
    let mut used: BTreeMap<usize, (i64, i64)> = BTreeMap::new();
    for l in items.iter().filter(|l| !l.is_tax) {
        if is_round_off(l) {
            continue;
        }
        let label = format!("\"{}\" {}", l.description, money(l.amount_paise, invoice_currency));
        let cands: Vec<(usize, &crate::master::PoLine)> = po.lines.iter().enumerate()
            .filter(|(_, pl)| names_item(&l.description, pl))
            .filter(|(_, pl)| pl.currency.as_deref().map(normalize::currency).unwrap_or_else(|| po_cur.clone()) == invoice_currency)
            .collect();
        if cands.is_empty() {
            problems.push(format!("{label} matches no {invoice_currency} line of the PO"));
            continue;
        }
        let qty = l.qty.unwrap_or(1);
        let unit = l.unit_price_minor.unwrap_or(if qty != 0 { l.amount_paise / qty } else { 0 });
        // qty x unit must reproduce the line amount; each unit may be rounded by at most one minor unit.
        let exact = qty.checked_mul(unit).map(|v| (v - l.amount_paise).abs() <= qty).unwrap_or(false);
        if qty <= 0 || unit < 0 || !exact {
            problems.push(format!("{label} does not equal its quantity x unit price"));
            continue;
        }
        // first candidate line with room for this charge
        let fit = cands.iter().find(|(i, pl)| {
            let (q, v) = used.get(i).copied().unwrap_or((0, 0));
            if pl.is_lump_sum() {
                v + l.amount_paise <= pl.qty.saturating_mul(pl.unit_price_minor)
            } else {
                let max_unit = pl.unit_price_minor + pl.unit_price_minor.max(0) * tol_bp / 10_000;
                unit <= max_unit && q + qty <= pl.qty
            }
        });
        match fit {
            Some((i, _)) => {
                let e = used.entry(*i).or_insert((0, 0));
                e.0 += qty;
                e.1 += l.amount_paise;
            }
            None => problems.push(format!("{label} bills more than the PO line allows (quantity, unit price or line value)")),
        }
    }
    problems
}

/// Human-readable amount: Indian-format rupees for INR, "USD 1234.56"-style otherwise.
pub fn money(minor: i64, currency: &str) -> String {
    if currency == "INR" {
        return rupees(minor);
    }
    let neg = minor < 0;
    let m = minor.unsigned_abs();
    format!("{}{} {}.{:02}", if neg { "-" } else { "" }, currency, m / 100, m % 100)
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
