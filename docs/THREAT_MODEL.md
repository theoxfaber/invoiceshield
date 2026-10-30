# Threat model: what the gate still guarantees when an attacker controls X

Written 2026-10-30 (Phase 4, `hardening/security`). This is our own analysis. **No external security review has been done**; nothing here has been tested by anyone outside the project. Limits are stated as plainly as guarantees.

## The system in one paragraph

An AI accounts-payable agent reads invoices and proposes payments through one tool. The proposal can name only a master vendor, an invoice number, an amount, a currency, a PO and the document's SHA-256; it has no bank field (a bank-like key is BLOCK `bank_fields_rejected`). A separate extraction service (its own process, its own Ed25519 key, sandboxed) reads the PDF and signs what it found. The Rust governor verifies that signature, runs deterministic rules against **trusted state** (vendor master, POs, policy limits, the ledger) and answers ALLOW / REVIEW / BLOCK. The payee on an ALLOW is always the master's beneficiary, never anything from the document. Every decision, resolution and master change is in a SQLite hash chain, optionally anchored with an Ed25519-signed head written outside the DB.

## Trust anchors (what must stay honest for the guarantees below)

| Anchor | Where it lives | Who can change it |
|---|---|---|
| Vendor master (bank account, IFSC, beneficiary, GSTIN, domains), POs, limits | `GOVERNOR_MASTER` file + bank changes applied through the workflow (DB, MAC'd with `GOVERNOR_STATE_KEY`) | host admin; the workflow (operator + call-back + 2 approvers + cooling-off) |
| Vendor phone numbers on file | `GOVERNOR_VENDOR_PHONES` file | host admin |
| Identity key hashes and roles | `GOVERNOR_IDENTITIES[_FILE]` | host admin |
| Extractor public key | `GOVERNOR_EXTRACTOR_PUBKEY` | host admin |
| Anchor signing key, state MAC key | `GOVERNOR_ANCHOR_KEY[_FILE]`, `GOVERNOR_STATE_KEY[_FILE]` | host admin |
| The governor binary and its host | | host admin |

## Identities and roles (Phase 4)

* `agent`: `POST /v1/actions` only (plus reads). The agent role cannot be combined with any other role; the server refuses to start with such an identity.
* `operator`: enters vendor bank-detail changes (maker) and records call-backs.
* `approver`: resolves REVIEW decisions and approves bank changes (checker).
* Maker-checker on identities: the identity that proposed a payment cannot resolve it (code + DB trigger on `review_resolutions`); the identity that entered a bank change cannot confirm its call-back or approve it (code + DB trigger on `master_change_approvals`). The approver recorded is always the authenticated identity, never a name from the request body.
* Keys are random 256-bit values (`governor new-key`); only their SHA-256 is configured. Compared in constant time against every entry.
* Rate limits per IP, per identity, and for failed authentications per IP (429 + `Retry-After`). TLS (rustls) optional; secrets via env or `*_FILE`.

## Scenarios

Each row: what the attacker controls, what the gate still guarantees, and what it does not.

### 1. Compromised agent (prompt-injected model or hijacked agent process)

**Controls:** every field the payment tool takes; which documents it asks about; timing and volume of proposals; anything it writes in free text.

**Still guaranteed**
* No money to an account that is not in the master: the proposal has no bank field and the payee is looked up by vendor id. Injected "pay this account instead" text is at most a tamper alert (REVIEW `bank_details_differ_from_master`).
* Evidence is not the agent's: without a valid extractor signature over the named document's hash, the governor REVIEWs (`evidence_unsigned`) or BLOCKs (bad signature, hash mismatch, replay). Raw `evidence` from the caller is ignored outside dev mode.
* Per-PO balance (+ tolerance), per-currency payout cap and approval threshold, duplicate invoice / IRN (DB UNIQUE index, so not even a second process can pay twice), the 7-day per-vendor window, amount = printed total = sum of lines, every line on the PO.
* The agent key cannot resolve a REVIEW, enter or approve a bank change, record a call-back, or read the bank-change workflow (403, tested).
* Flooding is bounded by the per-identity rate limit (production default 100 req/s, burst 200).

**Not guaranteed**
* A genuine invoice can be paid when it should not be (paid early, or paid although the goods were never received): the gate checks the invoice against the PO, not delivery. Three-way match with goods receipt is not modelled.
* Splits that stay under the approval threshold and outside the 7-day window (slow drip, documented limit S05).
* If the pipeline lets the agent choose which PDF the extractor signs, a compromised agent can submit a document it wrote and get honest evidence for it (Phase 2 limit 1). That document must still pass every master/PO rule, so the money still goes to the real vendor's master account, within PO balance. Fix: the extractor pulls by mailbox message id.
* Approver fatigue: the agent can create many REVIEWs. Rate limits slow this; they do not stop it.

### 2. Compromised extractor (code subverted, or its signing key stolen)

**Controls:** the content of signed evidence (printed account, GSTIN, sender domain, totals, item lines, currency) for any document hash.

**Still guaranteed**
* Payee from the master. A forged "clean" evidence payload removes the tamper alert but cannot change where money goes.
* Everything that is checked against trusted state and the proposal alone: vendor exists, PO exists and is owned by the vendor, PO balance and tolerance, payout cap, approval threshold, currency limits, duplicates, vendor window, roles and maker-checker.
* Replay of an old envelope is refused (nonce store; DB dedupe covers the restart gap).

**Not guaranteed**
* The document-level checks (GSTIN, sender domain, printed total, PO-line match, tamper alert) are only as honest as the extractor. An attacker with the key can make a fake invoice look clean and get real vendors paid, from real PO balance, to their real master accounts. That is a loss (overpayment to a vendor, recoverable in principle) but not a redirection.
* Key compromise is not detected automatically. Rotation: issue a new key, list both in `GOVERNOR_EXTRACTOR_PUBKEY`, drop the old one, restart.

### 3. Stolen agent key (from a config leak, a log, the network)

**Controls:** the same as scenario 1, from anywhere that can reach the governor.

**Still guaranteed:** everything in scenario 1. The key gives the agent role and nothing more. The DB and the identities file hold only SHA-256 hashes, so a copy of either does not yield a usable key. With TLS on, the key is not readable on the wire. Wrong-key guessing is rate limited per IP (production default 30 failures/min) on top of a 256-bit key space.

**Not guaranteed**
* No automatic detection of use from a new place: there is no per-identity IP allow-list, no anomaly alerting, no MFA.
* Revocation needs an edit to the identities file and a restart (no hot reload). Every in-flight use until then is accepted.
* Behind a reverse proxy, every client shares the proxy's IP for the per-IP limits (`X-Forwarded-For` is deliberately not trusted).

### 4. Insider approver (one person holding an approver key)

**Controls:** approve or reject REVIEW decisions; approve bank changes; record call-backs (if they also hold operator, or as approver).

**Still guaranteed**
* Approving a REVIEW pays the **master** account, never one from the document. A single insider can release a held payment (e.g. an over-threshold or tamper-alert invoice) but cannot redirect it.
* They cannot approve a payment they proposed (agent role is separate) or a bank change they entered (maker-checker, code + trigger).
* A bank change needs a call-back to the number on file, **two distinct** approvers who are not the maker, and the cooling-off (default 72 h) after the second approval before any payment can use the new details. One insider alone cannot change where a vendor is paid. Every step is in the audit chain with the authenticated identity.
* Any operator or approver can stop a change during cooling-off.

**Not guaranteed**
* The call-back is **attested, not verified**: the software checks that the number recorded as dialled is the number on file and that the confirmed details match the request; it cannot know whether a call happened. Two colluding insiders (one operator who enters and one approver who "confirms" the call and approves, plus a second approver) can change bank details; the cooling-off and the audit trail make it visible and attributable, not impossible.
* An approver who is careless (approves REVIEWs without reading them) defeats REVIEW as a control. The gate only makes the approval attributable.
* Identity ≠ person: anyone holding the approver key is that approver. No MFA, no hardware keys.

### 5. DB-only attacker (can read and write the SQLite file; no access to config, keys or the binary)

**Controls:** any row in `decisions`, `ledger`, `audit`, `idempotency`, `master_changes`, `review_resolutions`. Can drop triggers.

**Still guaranteed**
* Key hashes are not in the DB; reading it yields no credential.
* **Bank details cannot be redirected through the DB** when `GOVERNOR_STATE_KEY` is set: each applied bank change carries an HMAC-SHA256 under that key; a forged or edited `applied` row without a valid MAC makes the governor drop that vendor from the effective master, so payments to it BLOCK (`unknown_vendor`) instead of going to the forged account (tested). The base master is a file, not in the DB.
* Rewrites of history are **detected** (not prevented) by the anchors: `governor verify --pubkey` checks every signed anchor against the stored chain, so an edited, truncated or fully recomputed chain fails, as long as the anchors live somewhere the attacker cannot reach (`GOVERNOR_ANCHOR_URL` or a copied file).

**Not guaranteed**
* Without `GOVERNOR_STATE_KEY` (warned at startup), a forged `applied` bank-change row **would** be replayed onto the master. Set the key in production.
* Deleting ledger rows re-opens PO balance and the duplicate check for those invoices: a second payment of the same invoice becomes possible until someone verifies the chain. The server does not verify anchors on every start.
* Marking a pending entry as paid, or inserting fake decisions, changes the record but does not by itself move money (the payment rail acts on the governor's HTTP answer); it does corrupt the audit trail, which the anchor check reports.

### 6. Vendor email compromise (attacker sends mail from the vendor's real domain)

**Controls:** invoices and letters that pass sender-domain checks; "our bank has changed" requests; urgency.

**Still guaranteed**
* An invoice printing new bank details: REVIEW with a tamper alert; if a human approves it anyway, the master account is paid.
* A bank-change request by email changes nothing by itself. The maker enters it; the call-back must go to the phone number captured at onboarding (a number from the email is refused, tested); two approvers; cooling-off; the old details keep being paid until then, and invoices printing the new details stay REVIEW.

**Not guaranteed**
* If the number on file is itself wrong (bad onboarding), or the vendor's phone line is hijacked too, the call-back confirms the attacker.
* Social engineering of the approvers ("the CFO says skip the wait"): the software has no override for the cooling-off, but people can be talked into approving.
* Fake invoices from the real domain against real POs get the real vendor paid (overpayment risk, scenario 2's bound).

### 7. Host compromise (root, or the service account, on the governor host)

**Controls:** everything: the binary, memory (keys in process memory), the master and phone files, the identities file, the DB, the anchor and state keys, the TLS key.

**Still guaranteed (locally): nothing.** An attacker who controls the host can make the governor say ALLOW to anything.

**What remains**
* Anchors already posted to a system the attacker does not control still prove what the chain looked like up to the compromise.
* Controls outside the governor: beneficiary registration and cooling-off at the bank or payout rail, the bank's own confirmation of payee name, reconciliation. InvoiceShield does not replace them.
* Run the governor on a separate host from the agent so that compromising the agent's host (much more likely: it runs a model on untrusted documents) is not host compromise of the governor.

## Network attacker (for completeness)

Without TLS, keys and decisions are readable and alterable on the wire; the default bind is 127.0.0.1 for that reason. With `GOVERNOR_TLS_CERT_FILE` / `GOVERNOR_TLS_KEY_FILE`, rustls serves TLS 1.2/1.3 only and plain HTTP on that port gets no answer (tested). No mutual TLS; clients authenticate with bearer keys.

## Known gaps and follow-ups

* No external security review or penetration test.
* No hot reload of identities or keys; no per-identity IP allow-lists; no MFA for approvers.
* Rate limits are per process (two governor processes double the allowance).
* Used evidence nonces are in memory only (Phase 2 note); the DB dedupe still prevents a double payment.
* The call-back is an attestation by an authenticated identity, not a verified telephone event.
* Dependency note: `sha2 0.10` pulls `block-buffer 0.10.4`, which has GHSA-qwgh-2vcv-g2f7 (moderate; a *caught* panic inside a hash compression callback can corrupt the buffer position; fixed only in `block-buffer 0.12.1`, which needs `sha2 0.11`). The governor never catches panics around hashing, so we judge it not reachable here; it pre-dates Phase 4. Upgrading `sha2` is the fix when `ed25519-dalek` supports it.

## Appendix: deploying with separate identities

```
governor new-key --name ap-agent --role agent        # prints key=... (give to the agent only) and identity={...}
governor new-key --name anita    --role operator
governor new-key --name ravi     --role approver
governor new-key --name meena    --role approver
# collect the identity={...} lines into a JSON array in /etc/invoiceshield/identities.json (chmod 600)
export GOVERNOR_IDENTITIES_FILE=/etc/invoiceshield/identities.json
export GOVERNOR_STATE_KEY_FILE=/etc/invoiceshield/state.key        # 64 hex chars from a CSPRNG, chmod 600
export GOVERNOR_VENDOR_PHONES=/etc/invoiceshield/vendor-phones.json
export GOVERNOR_TLS_CERT_FILE=/etc/invoiceshield/tls/cert.pem GOVERNOR_TLS_KEY_FILE=/etc/invoiceshield/tls/key.pem
```

Rate limits (`GOVERNOR_RATE_LIMIT_RPS`/`_BURST`, `_IP_RPS`/`_IP_BURST`, `GOVERNOR_AUTH_FAIL_PER_MIN`/`_BURST`) default on in production and off in eval mode (`GOVERNOR_EVAL_CLOCK=1`) unless set explicitly, so cached eval replays and the load test measure the gate rather than the limiter. The legacy `GOVERNOR_API_KEY` / `GOVERNOR_OPERATOR_KEY` still work (identities `agent` and `operator`; the legacy operator holds operator + approver, one person as before, and the server warns).
