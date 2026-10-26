# Decisions

## 2026-10-26 22:30 IST — Rules: rebuild the governor (Plan B)
Official rules (Cerebral Valley event guide §4): "New work only: What you present at this Hackathon must have been built during the event… Startups may build on existing products or codebases." We are on the Indie track, so no pre-existing code is reused. The governor is rewritten from scratch in-event.

## 2026-10-26 — Models: open-weight Meta models only
The Meta Model API requires a credit card on file to use the hackathon credits. We built without one, so the stack is Meta's open-weight models: **Muse Glimmer 30B** (Apache-2.0, text + image) as the accounts-payable agent and, in a separate quarantined context, as the document checker; **SAM 3.1** (open weights) for document region masks. Bonus upside: the whole thing can run on an office PC, so invoices need not leave the building.

Day 0 observations (Glimmer on NVIDIA's free trial endpoint): `response_format` JSON schema is accepted but not enforced, so every model output is strictly parsed and anything that fails becomes REVIEW. Image input works (~1 min per invoice image at medium reasoning), so model calls are cached by input hash.

## 2026-10-26 — Fixtures must look plausible
A smoke test showed that an attack invoice with an obviously fake IFSC (containing "FAKE") makes the unshielded agent refuse. That would make the baseline look safer than it is. Attack fixtures therefore use real-format bank details and believable stories, while every name, GSTIN and account stays fictional.
