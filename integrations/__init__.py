"""Phase 7 product adapters: accounting systems (Tally, Zoho Books) and a payment-rail interface.

NOT TESTED AGAINST LIVE SYSTEMS. Every adapter here is exercised only with mock fixtures and
mock transports (tests/test_integrations.py). The Tally XML and Zoho Books JSON shapes follow
the vendors' public documentation as we read it; field names have not been verified against a
running TallyPrime company or a Zoho Books organisation. See docs/DECISIONS.md
('Accounting adapters never write the vendor master').

Rules shared by every adapter:
  * Importers never change the vendor master. A bank-detail difference becomes a proposed
    POST /v1/master-changes body (maker-checker, call-back, two approvers, cooling-off).
  * Exporters only export payments the governor already approved (ledger status `paid`).
  * Money is integer minor units (paise) end to end; decimals are formatted from integers.
"""
LIVE_TESTED = False
