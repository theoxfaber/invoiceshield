# How the vendor master gets updated safely

InvoiceShield's payment gate never takes a payee account from an invoice or email: it always pays the account in the vendor master. That moves the fraudster's target from "the PDF" to "the master". Real vendors do change banks, so there has to be one legitimate path, and it must not be something a forged letter can drive.

## The flow

1. **Request comes in** (vendor letter, email, portal). It is treated as untrusted input, exactly like an invoice. Nothing in it is used to verify itself: not its phone number, not its signatory, not its letterhead.
2. **Maker enters the change** (new account, IFSC, bank name, new rail beneficiary). The gate keeps paying the *old* account; invoices that print the new one keep going to REVIEW with a tamper alert.
3. **Out-of-band call-back** to the vendor's phone number **already on file** from onboarding (never one from the request). The caller records the number dialled and what the vendor's known contact confirmed (account + IFSC).
4. **Two distinct approvers (checkers)**, neither of them the maker, approve the change.
5. **Cooling-off period** (default 72 h from the request) before the change takes effect, so a vendor whose mailbox was compromised has time to notice, and so the change is not rushed by an "urgent, pay today" story.
6. **Audit-log entry**: old and new details, maker, approvers, who made the call-back and when, appended to the governor's SHA-256 hash-chained audit log.

## What is implemented vs designed

| Step | Status |
|---|---|
| Call-back must be to the number on file, and must confirm the same account + IFSC | **Implemented** in `governor/src/master_change.rs::apply_bank_change` and the HTTP workflow (`master_store.rs`), tested |
| Two distinct approvers, maker excluded, duplicates/case variants of one name count once | **Implemented**, tested (`governor/tests/master_change.rs`, `tests/security.rs`); also a DB trigger refuses the maker as approver |
| Cooling-off period (default 72 h, `GOVERNOR_MASTER_CHANGE_COOLING_OFF_SECS`), counted from the **second approval** | **Implemented** (Phase 4), eval-clock aware, tested |
| Hash-chained audit entry for every step (requested, call-back verified/refused, approved, cooling-off, applied/rejected) | **Implemented**, tested (chain verifies) |
| Exposed on the HTTP server (`governor-server`) | **Implemented** (Phase 4): `POST /v1/master-changes`, `GET /v1/master-changes/:id`, `POST .../callback`, `.../approve`, `.../reject` |
| Approver IDs bound to authenticated identities (separate keys per approver) | **Implemented** (Phase 4): maker = the operator identity, call-back by an operator/approver identity other than the maker, approvals by approver identities; never a name from the request body |
| Numbers on file | **Implemented as a separate trusted file** `GOVERNOR_VENDOR_PHONES` (`{vendor_id: phone}`); the master schema still has no phone field |
| Persistence across restarts | **Implemented** (Phase 4): SQLite schema v2 (`master_changes`, `master_change_approvals`); applied changes are replayed on top of the master file at every load and carry an HMAC under `GOVERNOR_STATE_KEY` so a DB-only edit cannot redirect a vendor |
| First payment after a change forced to REVIEW; notify the vendor's old contact | **Design only** |

### HTTP flow (Phase 4)

```
operator  POST /v1/master-changes            {vendor_id, new_account, new_ifsc, new_bank_name, new_beneficiary_id}  -> awaiting_callback
op/appr   POST /v1/master-changes/MC-000001/callback  {number_dialled, confirmed_account, confirmed_ifsc}          -> awaiting_approvals
approver  POST /v1/master-changes/MC-000001/approve   (x2, distinct, neither the maker)                           -> cooling_off (effective_at)
          ... first payment request or workflow call at/after effective_at applies it                           -> applied
op/appr   POST /v1/master-changes/MC-000001/reject    {reason}   (any time before applied)                      -> rejected
```

Until `effective_at` the master still holds the old details, so an invoice printing the new account stays a tamper REVIEW and no payment to the new details can be ALLOW. A call-back to any other number is refused (change stays open); a call-back in which the vendor confirms different details rejects the change. One open change per vendor.

This is a control a human process runs; the code only makes the "two people, a call-back and a wait" rule impossible to skip in software. It does not detect an insider who controls both approver identities, or a vendor's own phone line being hijacked (see docs/THREAT_MODEL.md, scenarios 4 and 6).
