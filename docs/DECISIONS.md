# Decisions

## 2026-10-26 22:30 IST — Rules: rebuild the governor (Plan B)
Official rules (Cerebral Valley event guide §4): "New work only: What you present at this Hackathon must have been built during the event… Startups may build on existing products or codebases." We are on the Indie track, so no pre-existing code is reused. The governor is rewritten from scratch in-event.

## 2026-10-26 — Models: open-weight Meta models only
The Meta Model API requires a credit card on file to use the hackathon credits. We built without one, so the stack is Meta's open-weight models: **Muse Glimmer 30B** (Apache-2.0, text + image) as the accounts-payable agent and, in a separate quarantined context, as the document checker; **SAM 3.1** (open weights) for document region masks. Bonus upside: the whole thing can run on an office PC, so invoices need not leave the building.

Day 0 observations (Glimmer on NVIDIA's free trial endpoint): `response_format` JSON schema is accepted but not enforced, so every model output is strictly parsed and anything that fails becomes REVIEW. Image input works (~1 min per invoice image at medium reasoning), so model calls are cached by input hash.

## 2026-10-26 — Fixtures must look plausible
A smoke test showed that an attack invoice with an obviously fake IFSC (containing "FAKE") makes the unshielded agent refuse. That would make the baseline look safer than it is. Attack fixtures therefore use real-format bank details and believable stories, while every name, GSTIN and account stays fictional.

## 2026-10-26 23:30 IST — Fixture identifiers: real format, fictional values
Replaces the pre-event `FAKE`/`7777`/`9999`/`.test` conventions. GSTINs use a ZZ-series PAN with a valid check character; IFSCs use real bank codes (BARB, CNRB, HDFC, ICIC, SBIN, UTIB, KKBK) in each bank's real branch-code shape with made-up branch codes; account numbers have each bank's usual length; domains are plausible `.in`/`.co.in` names. V004 renamed to Visakha Steel Fabricators to avoid RINL's "Vizag Steel" brand. No email is sent and no payment is made to any of these; README carries a "fictional, any match is coincidental" line. The Day 0 smoke showed fake-looking identifiers make the unshielded agent refuse, which would understate the risk.

## 2026-10-26 23:30 IST — Model calls: cache by full-request hash + sample index
Glimmer runs at temperature 1.0, so each eval case is sampled k times (sample = 0..k-1) and each sample cached once. Failed calls are never cached. The demo replays the cache (`offline=True`).
