# Durable state (Phase 3)

The governor's ledger, hash-chained audit log, every decision and the idempotency keys live in
one SQLite database in WAL mode (`governor/src/persist.rs`). The verdict rules in `engine.rs` are
unchanged; this layer only stores what the engine decides.

## Configuration (`governor-server`)

| Env | Meaning | Default |
|---|---|---|
| `GOVERNOR_DB` | SQLite file | `governor.db`; `:memory:` when `GOVERNOR_EVAL_CLOCK=1` (eval runs stay stateless unless you set it) |
| `GOVERNOR_ANCHOR_KEY` / `GOVERNOR_ANCHOR_KEY_FILE` | Ed25519 seed (64 hex chars) that signs audit anchors | anchoring off |
| `GOVERNOR_ANCHOR_FILE` | append-only anchor file | `<db>.anchors.jsonl` |
| `GOVERNOR_ANCHOR_INTERVAL_SECS` | how often the head is anchored (only when it moved) | 60 |
| `GOVERNOR_ANCHOR_URL` | optional `http://` URL each anchor is POSTed to (put a TLS relay in front for https) | none |

`PRAGMA synchronous=FULL`, `foreign_keys=ON`, busy timeout 10 s. The schema version is
`PRAGMA user_version`; migrations are listed in `persist.rs` and run under the write lock, so two
processes starting together migrate once. A binary refuses a DB with a newer schema.

## One request = one transaction

`POST /v1/actions` runs inside `BEGIN IMMEDIATE`: take the write lock (a file lock, so it also
serialises other processes on the same file) → check the `Idempotency-Key` → reload the in-memory
ledger/audit if another connection committed since (a `generation` counter in `meta`) → evaluate →
insert decision, ledger entry, audit records, idempotency row → `COMMIT`. Any failure before
`COMMIT` rolls everything back and the in-memory copy is discarded. If the store is unavailable the
server answers 503 and nothing is paid.

## Duplicate payment

Partial UNIQUE indexes on live (`paid`/`pending`) ledger rows:
`(vendor_id, invoice_key, amount_paise, currency)` and `(irn)`. A trigger allows only a pending
row to change, and only its status. The in-process mutex stays as defence in depth. See
DECISIONS.md, "Duplicate payment is blocked by the database".

## Idempotency-Key

Optional header on `POST /v1/actions` (1–200 printable ASCII). Same key + same body (SHA-256 of
the raw bytes) → the stored decision, `200` with `idempotent-replayed: true`. Same key + different
body → `409 idempotency_key_reused_with_different_body`. Keys survive restarts.

## Anchoring and verification

Every interval the server signs `invoiceshield-audit-anchor-v1\nseq=..\nhead=..\nts=..\n` with the
anchor key and appends `{v, seq, head_hash, ts, pubkey, sig}` to the anchor file (fsync), then
POSTs it to `GOVERNOR_ANCHOR_URL` if set. Verification pins the public key (the `pubkey` field in
the file is never trusted) and checks: the chain links, every anchor's signature, that anchors do
not go backwards, and that the stored record at each anchored `seq` still has the anchored hash. A
rewrite that recomputes every hash passes the chain check but fails the anchor check.

```
governor keygen                                  # prints GOVERNOR_ANCHOR_KEY / GOVERNOR_ANCHOR_PUBKEY; store the key as a secret
governor verify --db governor.db --anchors governor.db.anchors.jsonl --pubkey <hex>   # exit 0 ok, 1 tampered
governor anchor --db governor.db --anchors governor.db.anchors.jsonl                  # anchor now (key from env)
```
`GET /v1/audit/verify` (API key) reports the chain and, when anchoring is on, the anchor check.

Limit: an attacker who controls the host can also delete anchor lines from the local file. Posting
anchors to a system they do not control (`GOVERNOR_ANCHOR_URL`, a WORM bucket, a ticket system) is
what makes the anchor external; the file alone protects against DB-only tampering.

## Backup / restore

```
governor backup  --db governor.db --out /backups/governor-$(date +%F).db   # online, server may keep running
governor restore --from /backups/governor-2026-10-30.db --db governor.db --force   # stop the server first
```
Backup uses the SQLite online backup API and runs `integrity_check` on the copy. Restore refuses a
backup whose integrity check, schema version or audit chain fails, refuses to overwrite an
existing DB without `--force`, then reopens it with WAL and migrations. After a restore, verify
against the anchor file: anchors newer than the backup will report "truncated", which is the
expected, visible signal that decisions after the backup were lost.

## Tests

`governor/tests/state.rs` (library) and `governor/tests/state_process.rs` (real binaries):
restart, injected failure and a real `abort()` between the ledger insert and `COMMIT`, 24
connections / 40 requests across two server processes on one DB file → exactly one PAY,
idempotency over HTTP, anchors catching a whole-chain rewrite, truncation and a forged anchor,
backup/restore (library and CLI).
