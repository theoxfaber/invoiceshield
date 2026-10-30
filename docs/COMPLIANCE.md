# Compliance notes: DPDP Act 2023, data retention, what InvoiceShield stores

> **Not legal advice.** These are engineering notes written by the project team while building a
> hackathon prototype (2026-10-30). No lawyer has reviewed them, InvoiceShield has **no legal
> sign-off**, no DPDP compliance certification and no audit of any kind. Statutory references are
> our reading of public texts and may be incomplete or out of date (the DPDP Rules phase in over
> time). Anyone deploying this must get their own legal advice. Everything below describes the
> code as it is on `main`, plus what a deployer would still have to do.

## 1. What the system processes, and whose data it is

InvoiceShield sits in front of an accounts-payable agent and decides whether a proposed vendor
payment may go out. Most of what it handles is **business data** about companies (GSTIN, legal
name, PO lines, invoice amounts). It becomes **personal data** under the Digital Personal Data
Protection Act, 2023 (DPDP Act) where it identifies an individual:

| Category | Example in this system | Personal data? |
|---|---|---|
| Sole-proprietor / individual vendors | name, PAN (an individual's PAN), bank account, phone on file | Yes |
| Contact people at vendors | email sender address/domain, names on invoices, phone numbers used for call-backs | Yes |
| Buyer-side staff | identity names of agents/operators/approvers (`GOVERNOR_IDENTITIES`), who approved what | Yes (employees) |
| Company vendors | registered company name, GSTIN, company bank account | Generally no, but may contain personal data of signatories |

In DPDP terms, the business deploying InvoiceShield is the **Data Fiduciary** for this data. Hosted
LLM providers it calls (see §4) act as **Data Processors** for it. We (the project) operate no
hosted service and hold no customer data; the repo contains **synthetic data only**, plus
anonymised public-dataset labels (`dataset/real/`, identifiers salted-sha256).

## 2. What is stored, where, and for how long by default

Exactly what the code on `main` writes. "Default retention" is what the code does today, not a
recommendation (that is §3).

| Store | Written by | Contents | Personal data it can hold | Default retention |
|---|---|---|---|---|
| `GOVERNOR_DB` SQLite: `decisions` | governor-server | decision JSON: verdict, reason codes with **evidence detail strings** (may quote a printed bank account, IFSC, GSTIN, sender domain), signals, payee (beneficiary id + bank name from the master), proposer identity | yes (vendor/contact identifiers in reason details; staff identity) | forever (no purge job) |
| `ledger` | governor-server | vendor id, normalised invoice no, amount, currency, IRN, PO ref, status, time | indirectly (vendor id) | forever; needed for duplicate/window checks |
| `audit` | governor-server | append-only hash chain: every proposal (vendor, invoice no, amount, PO, IRN), verdict, reasons, signals; resolutions with approver; bank-change events with maker, approvers, **number dialled** for call-backs | yes | forever; UPDATE refused by trigger, chain verified by anchors |
| `review_resolutions` | governor-server | decision id, approver identity, approve/reject, time | yes (staff) | forever |
| `master_changes`, `master_change_approvals` | governor-server | proposed new bank account/IFSC/bank/beneficiary per vendor, maker, call-back report (number dialled, confirmed account/IFSC), approvers, reasons | yes | forever |
| `idempotency` | governor-server | client key, body sha256, decision id | no | forever |
| anchor file (`<db>.anchors.jsonl`) | governor-server | seq, head hash, signature, time | no | forever (append-only) |
| vendor master JSON (`GOVERNOR_MASTER`) | the deployer | legal name, GSTIN, PAN, bank account, IFSC, email domains, category | yes for individuals | deployer-managed |
| vendor phones file (`GOVERNOR_VENDOR_PHONES`) | the deployer | phone numbers on file for call-backs | yes | deployer-managed |
| structured logs (stdout) | governor-server | method, route template, status, latency, verdict; **no invoice fields or credentials** (governor/src/observability.rs) | no by design | wherever the deployer ships logs |
| extractor sandbox temp dir | extractor | the invoice PDF and request, in a 0700 temp dir | yes (invoice content) | deleted when the extraction ends (`TemporaryDirectory`) |
| `eval/cache/llm/*.json` | providers/llm.py | model outputs (content, reasoning, tool calls, usage) keyed by a hash of the input | for real invoices, outputs can quote invoice content | forever, on the machine running evals; repo copy is synthetic only |
| `integrations/` exports (Tally XML, Zoho JSON) | integrations/ | vendor master and approved payments (amount, invoice key, approver) | yes | whatever the deployer does with the file |

Not stored by the governor: the PDF itself, the cover email body, model prompts, API keys (only
sha256 hashes of identity keys in `GOVERNOR_IDENTITIES`).

## 3. Retention policy (proposed for a deployment; not implemented as code)

Two obligations pull in opposite directions:

* **DPDP Act s.8(7)**: erase personal data once the specified purpose is no longer being served,
  unless retention is necessary for compliance with any law.
* **Books and tax records must be kept**: our understanding is that books of account are kept for
  8 years under the Companies Act, 2013 (s.128(5)) and GST records for 72 months from the due date
  of the annual return under the CGST Act, 2017 (s.36). Payment approvals and the audit trail of
  who approved them are part of those records. **Check with a professional; we may be wrong.**

Proposed schedule (deployer to adopt and document as its own policy):

| Data | Keep | Then |
|---|---|---|
| decisions, ledger, audit chain, review resolutions, master changes, anchors | the longer of the statutory periods above (e.g. 8 years from the end of the financial year of the payment) | archive offline, then delete the whole DB generation |
| BLOCK/REVIEW-rejected decisions with no payment | 3 years (fraud investigation and dispute window, deployer to decide) | delete |
| extractor temp files | duration of one extraction (already the case) | — |
| LLM cache with real invoice data | do not keep in production; disable caching or purge after 30 days | delete |
| logs | 90 days | delete |
| integration export files | until imported, then per the accounting system's own retention | delete local copies |
| staff identity names after an employee leaves | keep in the audit trail for the statutory period (needed to show who approved) | — |

**Known conflict, not solved:** the audit log is an append-only hash chain, deliberately tamper
evident. Deleting or editing one record breaks verification. Honouring an erasure request for data
inside the retention period is normally covered by the legal-obligation exception; for data past
it, the design that fits is to **rotate**: start a new DB and chain each financial year, keep the
old one sealed (anchors kept), and delete a whole year's DB when its retention ends. Per-record
redaction (storing personal fields encrypted per data principal and destroying the key,
"crypto-shredding") is a possible later change; it is **not implemented**.

## 4. DPDP Act 2023 notes, mapped to this system

Engineering checklist only. Section numbers are from the Act as published; the DPDP Rules, 2025
set procedures and phase-in dates that a deployer must check.

| Topic (DPDP Act) | What it means here | Status in code |
|---|---|---|
| Lawful ground (s.4, s.7) | Processing vendor and staff data to pay invoices and prevent fraud; a deployer would likely rely on legitimate uses (e.g. employment, legal obligation) or consent captured at vendor onboarding. Deployer's decision. | not a code concern; documented here |
| Notice (s.5) | Vendors onboarded with personal data should get a notice of what is processed and why | deployer process |
| Purpose limitation / minimisation (s.4, s.6) | Only fields needed to decide a payment are read; the agent cannot supply bank details at all; logs exclude invoice fields | partly by design |
| Accuracy (s.8(3)) | Bank details change only through call-back + two approvers + cooling-off (`/v1/master-changes`); accounting imports never overwrite the master (`integrations/`, DECISIONS 'Accounting adapters never write the vendor master') | implemented |
| Security safeguards (s.8(5)) | per-identity keys (hashed), roles, maker-checker, TLS option, rate limits, signed evidence, sandboxed parsing, HMAC on applied bank changes, anchored audit chain (docs/THREAT_MODEL.md) | implemented; **no external security review** |
| Breach notification (s.8(6)) | A personal-data breach must be reported to the Data Protection Board and affected data principals in the form the Rules set | deployer runbook needed; docs/RUNBOOKS.md has no breach procedure yet |
| Erasure / retention (s.8(7)) | see §3 | policy proposed; purge/rotation not implemented |
| Data principal rights (s.11–s.14): access, correction, erasure, grievance | A vendor contact or employee may ask what is held. Data is findable by vendor id / identity name in the DB | no self-service tooling; manual SQL |
| Processors (s.8(2)) | Hosted LLMs (NVIDIA NIM, Baseten; `providers/llm.py`) receive invoice text for the checker/agent. A contract with each processor is needed; the governor's decision never depends on them (escalate-only) | deployer contracts; the gate works with no LLM at all |
| Cross-border transfer (s.16) | Those LLM endpoints are outside India. Transfers are allowed except to countries the Government restricts by notification; sector rules (e.g. RBI on payment data) may be stricter | deployer to check; a self-hosted model keeps data in India |
| Children's data (s.9) | not expected in AP data | n/a |
| Significant Data Fiduciary (s.10) | only if notified by the Government; unlikely for one AP deployment | n/a |

Payment-system rules (RBI data-localisation for payment system data, bank/NPCI rules for a real
payout rail) would apply once a real rail is connected. **There is no real rail**: the payment
adapter is an interface plus a sandbox stub (`integrations/payment_rail.py`).

## 5. What a deployer still has to do

1. Get legal advice on the lawful ground, notices, retention periods and processor contracts.
2. Adopt (or replace) the retention schedule in §3 and implement yearly DB rotation and log/cache purges.
3. Write a breach-response runbook (who notifies the Board and data principals, within what time).
4. Decide whether invoice text may leave India; if not, run the checker on a self-hosted model or off.
5. Keep `GOVERNOR_IDENTITIES`, `GOVERNOR_STATE_KEY`, anchor and extractor keys in a secrets manager.
6. Have an external security review done (none has been).

## 6. Limits of these notes

No legal review, no DPIA, no penetration test, no real customer data, no live pilot. The project
makes no claim that InvoiceShield is "DPDP compliant"; it records what the software stores and what
we think the obligations around it are, so that a deployer and their counsel can start from facts.
