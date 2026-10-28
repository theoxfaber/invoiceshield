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
| Call-back must be to the number on file, and must confirm the same account + IFSC | **Implemented** in `governor/src/master_change.rs::apply_bank_change`, tested |
| Two distinct approvers, maker excluded, duplicates/case variants of one name count once | **Implemented**, tested (`governor/tests/master_change.rs`) |
| Cooling-off period (default 72 h), refused with the time remaining | **Implemented**, tested |
| Hash-chained audit entry with before/after, maker, approvers, call-back | **Implemented**, tested (chain verifies) |
| Exposed on the HTTP server (`governor-server`) | **Not yet**: library function only |
| Approver IDs bound to authenticated operator identities (separate keys per approver) | **Design only**: IDs are plain strings in the library |
| Numbers on file stored in the master | **Design only**: passed in as a separate trusted map; the master schema has no phone field yet |
| First payment after a change forced to REVIEW; notify the vendor's old contact | **Design only** |
| Persistence of the change across restarts | **Not yet** (master and audit log are in memory; see review backlog item 7) |

The four tests: `needs_two_distinct_approvers_not_the_maker`, `callback_must_go_to_number_already_on_file`, `cooling_off_then_applied_and_audited`, `callback_confirming_other_details_is_refused`. Run with `cd governor && cargo test --test master_change`.

This is a control a human process runs; the code only makes the "two people, a call-back and a wait" rule impossible to skip in software. It does not detect an insider who controls both approver identities, or a vendor's own phone line being hijacked (residual limit R2 in the threat model covers an insider edit to the master).
