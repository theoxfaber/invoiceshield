# Runbooks

Operational procedures for an InvoiceShield deployment. Synthetic data only; these are the procedures
we would hand an AP team running the gate, written against the code in this repo. No financial or
compliance claims, and none of this has been exercised by a real AP team (see README "Limits").

**Availability status (2026-10-30 20:15 IST).** Phases 1, 2 (Ed25519-signed evidence, `extractor/`),
3 (SQLite WAL ledger/audit/decisions, idempotency keys, `governor` operator CLI, signed anchors) and 6
(ops) are all on `main`; every procedure below works against the current server. Items still marked
*(Phase 4)* are not built yet.

**Startup requirements (fail fast, exit 2, every error listed; `governor/src/config.rs`).**
`GOVERNOR_API_KEY` (>= 16 chars) and `GOVERNOR_EXTRACTOR_PUBKEY` (the extraction service's Ed25519
public key(s), 64 hex or base64, comma-separated during rotation) are required. Optional:
`GOVERNOR_EVIDENCE_MAX_AGE_SECS` (default 300, 1-86400), `GOVERNOR_EVIDENCE_MAX_SKEW_SECS` (default 30,
0-3600), `GOVERNOR_DB` (default `governor.db`; the container uses `/data/governor.db`),
`GOVERNOR_ANCHOR_KEY`/`_FILE`, `GOVERNOR_ANCHOR_INTERVAL_SECS`, `GOVERNOR_ANCHOR_URL`.
`GOVERNOR_DEV_UNSIGNED_EVIDENCE=1` is a test-only switch and is refused unless `GOVERNOR_EVAL_CLOCK=1`;
it must never appear in a deployment. In docker compose, the one-shot `extractor-key` service creates
the extractor key in its own volume and hands the governor only the public key
(`GOVERNOR_EXTRACTOR_PUBKEY_FILE`, read by `docker/governor.sh`).

The one rule behind every runbook: **the gate fails closed**. When in doubt, nothing is paid; a human
resolves REVIEWs with the operator key. Never "unblock" by bypassing the governor, editing the vendor
master directly, or pointing the agent at a different payment tool.

Common tools:

```bash
GOV=http://127.0.0.1:8080
curl -fsS $GOV/healthz; curl -fsS $GOV/readyz                      # liveness / readiness
curl -fsS ${GOVERNOR_METRICS_TOKEN:+-H "Authorization: Bearer $GOVERNOR_METRICS_TOKEN"} $GOV/metrics | grep -E 'governor_(decisions|parse_failures|ready)'
curl -fsS -H "Authorization: Bearer $GOVERNOR_API_KEY" $GOV/v1/audit/verify   # {"records": N, "first_broken": null}; API key required
# logs: one JSON object per line on stderr (event=listening|http_request|config_invalid|bind_failed|
#        state_store_error|db_rejected_write|anchor_write_failed|anchor_post_failed|dev_unsigned_evidence)
governor verify --db /data/governor.db --anchors /data/governor.db.anchors.jsonl --pubkey <anchor pubkey>   # offline chain + anchor check
docker compose logs governor | jq -c 'select(.level != "INFO" or .status >= 400)'
```

---

## 1. Suspected bank-detail change ("our account has changed")

**Trigger.** A vendor email, call or invoice says the bank account or IFSC changed; or the governor
returned `tamper_alert: true` / reason `bank_details_differ_from_master` (the account printed on the
document is not the one in the master); or `governor_decision_reasons_total{reason="bank_details_differ_from_master"}` jumps.

**What the system already did.** The payee comes only from the vendor master, so no payment went to
the new account. A document with a different account is REVIEW (BLOCK when combined with another
non-allow reason). Nothing needs to be stopped.

**Do.**
1. Treat it as fraud until proven otherwise. Do **not** reply to the email or call any number in it.
2. Leave the REVIEW decision pending. Do not approve it "to keep the vendor happy": approving pays the
   *master* account, which is correct, but it closes the trail before the change is investigated.
3. Call the vendor back on the phone number **already in the vendor master** (never one from the
   document). Ask them to confirm, in their words, whether they changed banks.
4. If they did **not**: reject the REVIEW (`POST /v1/decisions/{id}/resolve {"approve": false, "approver": "<you>"}`
   with the operator key), keep the document and headers, and report it (bank, cyber-crime portal).
5. If they **did**: apply the change only through the vendor-master change procedure
   (`docs/VENDOR_MASTER_CHANGES.md`, `governor/src/master_change.rs`): entered by one person,
   confirmed by the call-back to the number on file, approved by **two distinct approvers who are not
   the person who entered it**, then a cooling-off period, all in the audit log. Until it takes effect
   the old account stays authoritative. *(Today the rule is a tested library function, not an HTTP
   endpoint; Phase 4 wires it to the server.)*
6. After the change is live, re-submit the invoice; it should ALLOW against the new master account.

**Never.** Edit `master.json` or the DB by hand; accept a change confirmed only by email; let the
person who requested the change approve it.

---

## 2. Governor down or not ready

**Trigger.** `/healthz` fails, `/readyz` returns 503, the agent's tool reports connection errors, or
`governor_ready` is 0 / absent.

**What the system already did.** Nothing can be paid: the agent's only payment path is
`POST /v1/actions`, and the Python shield treats any error as not paid. Invoices queue; that is the
designed failure mode.

**Do.**
1. Look at the last log lines. Config problems are explicit and the process exits with status 2:
   ```
   config error: GOVERNOR_API_KEY must be at least 16 characters (got 5)
   governor-server: refusing to start (1 configuration error(s))
   ```
   Every rule is listed in `governor/src/config.rs`. Fix the variable and restart; do not loosen it.
   The most likely one after Phase 2: `GOVERNOR_EXTRACTOR_PUBKEY is required` / `invalid`: give it the
   extractor's public key (`GET /v1/pubkey` on the extractor, or the compose `extractor-pub` volume).
   Never "fix" it with `GOVERNOR_DEV_UNSIGNED_EVIDENCE=1`.
2. `event=bind_failed`: another process holds the port (`ss -ltnp | grep 8080`) or the bind address
   is wrong (`GOVERNOR_BIND`).
3. Master file errors (`GOVERNOR_MASTER: ... is not a valid vendor master`): restore the last good
   master from version control. A master that fails strict parsing must never be "fixed" by removing
   fields.
4. State store errors (`event=state_store_error`, `cannot open state DB`) return 503 / exit 2 and pay
   nothing; check disk space and the DB file's permissions (`/data` is owned by uid 10001 in the
   image), then section 4 if the DB is damaged. `db_rejected_write` means a UNIQUE index stopped a
   second live ledger entry for the same invoice: that is the duplicate guard working, not an outage.
5. Restart: `docker compose restart governor` (or the systemd unit). Confirm `/readyz` = 200 and a
   test decision on a known-good invoice in a staging copy.
6. After recovery, re-submit queued invoices. A request that may have reached the governor before
   the outage is re-sent **byte-identical** with its original idempotency key: the governor returns the
   recorded decision without re-evaluating (no double payment, no signature re-check). The same key
   with a different body (for example a newly signed envelope) is refused as a key conflict. A request
   that never got a decision is sent fresh: new extractor envelope, new idempotency key. An envelope
   older than `GOVERNOR_EVIDENCE_MAX_AGE_SECS` (default 300 s) is REVIEW `evidence_stale`, and
   re-using an envelope's nonce in a new request is BLOCK `evidence_replayed`.
7. Extractor down (the agent cannot get signed evidence): nothing is paid, invoices queue. Restart the
   extractor with the **same** key file (`--key-file`), check `GET /v1/pubkey` returns the key id the
   governor trusts. A new key needs section 3a.

**Escalate** if it crash-loops after a config fix: keep the logs, roll back to the previous image tag.

---

## 3. Signature-key rotation

Two keys sign things the governor trusts. Rotate on a schedule (e.g. quarterly), on staff changes,
and immediately on suspected exposure.

### 3a. Extractor evidence key
The extraction service signs evidence with Ed25519; the governor accepts only envelopes signed by a
key in `GOVERNOR_EXTRACTOR_PUBKEY` (one or more public keys).

1. Generate the new key on the extractor host (`python -m extractor.service --key-file <new.pem> --pubkey-out <new.pub>`
   creates it if absent; keep the file 0600, never commit it; `*.key` and `secrets/` are git/docker-ignored).
2. Add the new public key to `GOVERNOR_EXTRACTOR_PUBKEY` **alongside** the old one; restart the governor.
3. Switch the extractor to the new key; restart it. Check `GET /v1/pubkey` on the extractor returns the new `key_id`.
4. Watch `governor_decision_reasons_total{reason=~"evidence_.*|document_hash_.*"}` for one evidence
   max-age window (`GOVERNOR_EVIDENCE_MAX_AGE_SECS`, default 300 s): `evidence_signature_invalid`
   (BLOCK, unknown key id) means something still signs with a key the governor does not trust.
5. Remove the old public key from the governor; restart. Destroy the old private key.
6. **Suspected compromise:** skip the overlap: remove the old key from the governor first (evidence
   signed by it becomes REVIEW, nothing is paid), then bring the extractor up on a new key.

### 3b. Audit-anchor key
1. `governor keygen` → new `GOVERNOR_ANCHOR_KEY` (secret) / `GOVERNOR_ANCHOR_PUBKEY`.
2. Run `governor anchor --db governor.db --anchors <file>` with the **old** key to anchor the current head.
3. Switch the server to the new key (`GOVERNOR_ANCHOR_KEY_FILE`), restart; it anchors with the new key.
4. Keep the old public key: verifying older anchors needs it. Record the switch-over `seq` and both
   public keys in the change log.

### 3c. API / operator keys
`GOVERNOR_API_KEY` (agent) and `GOVERNOR_OPERATOR_KEY` (humans) are bearer tokens; config validation
requires >= 16 chars and that they differ. Rotate by setting the new value and restarting; the agent's
old key stops working immediately. Generate with `openssl rand -hex 32`.

---

## 4. DB restore

**Trigger.** Disk failure, a corrupted `governor.db`, `governor verify` exit 1, or a bad deploy that
wrote wrong state.

1. Stop the governor (nothing is paid while it is down).
2. Keep the damaged DB and its `-wal`/`-shm` files for investigation (copy them aside, do not delete).
3. Pick the newest backup made by `governor backup --db governor.db --out /backups/governor-<date>.db`
   (online, SQLite backup API, integrity-checked).
4. `governor restore --from /backups/governor-<date>.db --db governor.db --force`. It refuses a backup
   whose integrity check, schema version or audit chain fails.
5. `governor verify --db governor.db --anchors governor.db.anchors.jsonl --pubkey <anchor pubkey>`.
   Anchors newer than the backup report **truncated**: that is the expected, visible record of what was
   lost.
6. Reconcile the lost window: every payment the rail executed after the backup time must be checked
   against the bank statement and re-entered through the normal path, so the duplicate and PO-balance
   checks see it. Until that is done, hold new payments to the vendors involved.
7. Start the governor, check `/readyz`, `/v1/audit/verify`.

Backups: run `governor backup` at least daily and before every deploy; keep them off-host; test a
restore monthly (the restore path is covered by Phase 3's library and CLI tests).

---

## 5. Audit-chain verification failure

**Trigger.** `GET /v1/audit/verify` returns a non-null `first_broken`, or `governor verify` exits 1
(chain does not link, an anchor signature is invalid, anchors go backwards, or a stored record no longer
matches its anchored hash).

**Meaning.** Someone or something changed history: a record was edited, deleted or re-ordered. Treat
it as a security incident, not a bug to patch over.

1. Freeze payments: stop the governor or remove the agent's API key. Keep the operator key with the
   incident lead only.
2. Preserve evidence: copy the DB (and WAL), anchor file, logs and the host's access logs read-only.
3. Find the first bad index (`first_broken`) and the last good anchor (`governor verify` output).
   Everything before the last good anchor is trustworthy; everything after is suspect.
4. Compare the suspect window with the payment rail / bank statement: which payments happened, to
   whom, and do they match decisions in the log?
5. Restore from the newest backup that verifies (section 4), then re-enter genuine activity after it.
6. Rotate every key that lived on the affected host (section 3) and the API/operator keys.
7. Write it up: what changed, how, which control caught it, what changes.

**Do not** "repair" the chain by recomputing hashes: that is exactly what an attacker would do, and
the external anchors exist to make it visible.

---

## 6. Forged, replayed or mismatched evidence

**Trigger.** Decisions with reason `evidence_signature_invalid`, `evidence_replayed` or
`document_hash_mismatch` (all BLOCK), or the alert above.

**Meaning.** Something between the extractor and the governor (usually the agent process) sent
evidence the extractor did not sign, reused a signed envelope, or attached evidence for a different
document. An honest fault does not produce these (an honest fault is REVIEW: stale, unsigned,
extraction failed). Nothing was paid.

1. Treat the agent host as compromised: revoke `GOVERNOR_API_KEY` (section 3c) so it cannot submit.
2. Pull the decision(s) and their audit records (`GET /v1/decisions/{id}`, `/v1/audit/verify`); keep
   the agent's logs and the document(s) involved.
3. Check the extractor's key file permissions and host: if its private key may have leaked, rotate it
   with the compromise path in section 3a.
4. Resume only after the agent is redeployed from a clean image with a new API key.

---

## Alerting suggestions (from `/metrics`)

| Alert | Expression (PromQL) | Why |
|---|---|---|
| Governor down | `up{job="governor"} == 0 or governor_ready == 0` for 2m | Nothing can be paid |
| Parse failures | `increase(governor_parse_failures_total[15m]) > 5` | A broken agent/checker or an attack; all fail closed to REVIEW |
| Bank-change spike | `increase(governor_decision_reasons_total{reason="bank_details_differ_from_master"}[1h]) > 3` | Section 1 |
| Block spike | `increase(governor_decisions_total{verdict="block"}[1h]) > 10` | Hijacked agent or bad data |
| Latency SLO burn | `histogram_quantile(0.99, rate(governor_http_request_duration_seconds_bucket{route="/v1/actions"}[5m])) > 0.05` | `docs/SLO.md` |
| Auth failures | `increase(governor_http_requests_total{status="401"}[10m]) > 20` | Key misuse or probing |
| Forged / replayed evidence | `increase(governor_decision_reasons_total{reason=~"evidence_signature_invalid\|evidence_replayed\|document_hash_mismatch"}[15m]) > 0` | Section 6: only tampering explains these |
| Stale / unsigned evidence | `increase(governor_decision_reasons_total{reason=~"evidence_stale\|evidence_unsigned\|evidence_from_future"}[15m]) > 5` | Clock skew, slow queue, or a client not using the extractor |
| State store errors | `increase(governor_http_requests_total{route="/v1/actions",status="503"}[5m]) > 0` | Section 2 step 4 |
