//! Durable state (Phase 3): SQLite in WAL mode holds the ledger, the audit chain, every
//! decision and the idempotency keys.
//!
//! One `POST /v1/actions` is one `BEGIN IMMEDIATE` transaction: the write lock is taken
//! first (it is a file lock, so it also serialises separate processes on one DB file), the
//! in-memory `Governor` is refreshed from the DB if anyone else committed since we last
//! looked, the unchanged engine evaluates, and the decision, the ledger entry and the audit
//! records are written and committed together. Any error before `COMMIT` rolls the whole
//! thing back, and the in-memory copy is thrown away and reloaded on the next call.
//!
//! The verdict rules are untouched: this module only stores what `engine.rs` decides.
//! Two partial UNIQUE indexes on the live ledger rows (vendor + normalised invoice + amount,
//! and IRN) make a second live entry for the same invoice impossible even if the in-process
//! check were bypassed. See DECISIONS.md, "Duplicate payment is blocked by the database".
use crate::audit::{AuditLog, AuditRecord};
use crate::engine::{Decision, Governor};
use crate::ledger::{Entry, EntryStatus, Ledger};
use crate::master::Master;
use crate::verdict::Verdict;
use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

/// Latest schema version this binary knows. Stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 3;

/// Versioned migrations, applied in order inside one IMMEDIATE transaction each.
/// Never edit a released step; add a new one.
const MIGRATIONS: &[(i64, &str)] = &[
    (
        1,
        r#"
CREATE TABLE decisions (
    decision_id TEXT PRIMARY KEY,
    verdict     TEXT NOT NULL CHECK (verdict IN ('allow','review','block')),
    body        TEXT NOT NULL,
    ts          INTEGER NOT NULL
);
CREATE TABLE ledger (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    decision_id  TEXT NOT NULL UNIQUE REFERENCES decisions(decision_id),
    vendor_id    TEXT NOT NULL,
    invoice_key  TEXT NOT NULL,
    amount_paise INTEGER NOT NULL CHECK (amount_paise > 0),
    currency     TEXT NOT NULL DEFAULT 'INR',
    irn          TEXT,
    po_ref       TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('paid','pending','rejected')),
    ts           INTEGER NOT NULL,
    entry        TEXT NOT NULL
);
-- The duplicate-payment guarantee. Rejected entries do not count, exactly like ledger.rs.
CREATE UNIQUE INDEX ledger_live_dedupe ON ledger(vendor_id, invoice_key, amount_paise, currency) WHERE status <> 'rejected';
CREATE UNIQUE INDEX ledger_live_irn ON ledger(irn) WHERE irn IS NOT NULL AND status <> 'rejected';
-- Only a pending entry may change, and only its status.
CREATE TRIGGER ledger_update_guard BEFORE UPDATE ON ledger
WHEN OLD.status <> 'pending' OR NEW.vendor_id <> OLD.vendor_id OR NEW.invoice_key <> OLD.invoice_key
  OR NEW.amount_paise <> OLD.amount_paise OR NEW.currency <> OLD.currency OR NEW.decision_id <> OLD.decision_id
BEGIN SELECT RAISE(ABORT, 'ledger: only a pending entry can be resolved'); END;
CREATE TABLE audit (
    seq       INTEGER PRIMARY KEY,
    prev_hash TEXT NOT NULL,
    payload   TEXT NOT NULL,
    hash      TEXT NOT NULL
);
-- Guards against accidents only; an attacker with the file can drop it. The anchors are
-- what detect a rewrite (anchor.rs).
CREATE TRIGGER audit_append_only BEFORE UPDATE ON audit
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TABLE idempotency (
    key         TEXT PRIMARY KEY,
    body_sha256 TEXT NOT NULL,
    decision_id TEXT NOT NULL REFERENCES decisions(decision_id)
);
CREATE TABLE meta (k TEXT PRIMARY KEY, v INTEGER NOT NULL);
INSERT INTO meta (k, v) VALUES ('next_id', 1), ('generation', 0);
"#,
    ),
    (
        2,
        r#"
-- Phase 4 (security): who proposed each decision, maker-checker on resolutions, and the
-- vendor bank-detail change workflow (call-back, two approvers, cooling-off).
ALTER TABLE decisions ADD COLUMN proposed_by TEXT;
CREATE TABLE review_resolutions (
    decision_id TEXT PRIMARY KEY REFERENCES decisions(decision_id),
    approver    TEXT NOT NULL,
    approve     INTEGER NOT NULL CHECK (approve IN (0, 1)),
    ts          INTEGER NOT NULL
);
-- Maker-checker in the database too: the identity that proposed a payment cannot resolve it.
CREATE TRIGGER review_checker_not_proposer BEFORE INSERT ON review_resolutions
WHEN NEW.approver = (SELECT proposed_by FROM decisions WHERE decision_id = NEW.decision_id)
BEGIN SELECT RAISE(ABORT, 'maker-checker: the proposer cannot resolve its own decision'); END;
CREATE TABLE master_changes (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    change_id    TEXT NOT NULL UNIQUE,
    vendor_id    TEXT NOT NULL,
    new_details  TEXT NOT NULL,
    maker        TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('awaiting_callback','awaiting_approvals','cooling_off','applied','rejected')),
    callback     TEXT,
    approved_at  INTEGER,
    effective_at INTEGER,
    applied_at   INTEGER,
    -- HMAC-SHA256 over the applied change under GOVERNOR_STATE_KEY (master_store.rs), so a
    -- DB-only attacker cannot add an 'applied' row that redirects payments.
    mac          TEXT,
    rejected_by  TEXT,
    reason       TEXT
);
-- At most one open change per vendor.
CREATE UNIQUE INDEX master_changes_one_open ON master_changes(vendor_id) WHERE status NOT IN ('applied','rejected');
CREATE TRIGGER master_change_final BEFORE UPDATE ON master_changes
WHEN OLD.status IN ('applied','rejected')
BEGIN SELECT RAISE(ABORT, 'master change is final'); END;
CREATE TABLE master_change_approvals (
    change_id TEXT NOT NULL REFERENCES master_changes(change_id),
    approver  TEXT NOT NULL,
    ts        INTEGER NOT NULL,
    PRIMARY KEY (change_id, approver)
);
CREATE TRIGGER master_change_checker_not_maker BEFORE INSERT ON master_change_approvals
WHEN NEW.approver = (SELECT maker FROM master_changes WHERE change_id = NEW.change_id)
BEGIN SELECT RAISE(ABORT, 'maker-checker: the maker cannot approve its own change'); END;
"#,
    ),
    (
        3,
        r#"
-- Post-review hardening (docs/REVIEW_FIXES.md).
-- Finding 4: every accepted evidence nonce, so a restart (or a second process on this DB) cannot
-- reopen the replay window. Rows older than the evidence max age are pruned: such evidence is
-- refused as stale anyway.
CREATE TABLE used_nonces (
    nonce     TEXT PRIMARY KEY,
    issued_at INTEGER NOT NULL
);
CREATE INDEX used_nonces_issued_at ON used_nonces(issued_at);
-- A human-REJECTED invoice stays rejected forever (rule `previously_rejected`, BLOCK) unless an
-- operator/approver reopens it here. Append-only; the ledger row itself never changes.
CREATE TABLE rejection_reopens (
    decision_id TEXT PRIMARY KEY REFERENCES decisions(decision_id),
    reopened_by TEXT NOT NULL,
    reason      TEXT NOT NULL CHECK (length(reason) > 0),
    ts          INTEGER NOT NULL
);
CREATE TRIGGER reopen_only_rejected BEFORE INSERT ON rejection_reopens
WHEN (SELECT status FROM ledger WHERE decision_id = NEW.decision_id) IS NOT 'rejected'
BEGIN SELECT RAISE(ABORT, 'reopen: only a human-rejected entry can be reopened'); END;
CREATE TRIGGER reopen_not_proposer BEFORE INSERT ON rejection_reopens
WHEN NEW.reopened_by = (SELECT proposed_by FROM decisions WHERE decision_id = NEW.decision_id)
BEGIN SELECT RAISE(ABORT, 'maker-checker: the proposer cannot reopen its own rejected decision'); END;
CREATE TRIGGER rejection_reopens_no_update BEFORE UPDATE ON rejection_reopens
BEGIN SELECT RAISE(ABORT, 'rejection_reopens is append-only'); END;
"#,
    ),
];

#[derive(Debug)]
pub enum StoreError {
    Db(rusqlite::Error),
    Corrupt(String),
    SchemaTooNew(i64),
    /// Test-only fault injection fired (see `Fault`).
    Injected,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Db(e) => write!(f, "database: {e}"),
            StoreError::Corrupt(m) => write!(f, "stored state is not readable: {m}"),
            StoreError::SchemaTooNew(v) => write!(
                f,
                "database schema v{v} is newer than this binary (v{SCHEMA_VERSION})"
            ),
            StoreError::Injected => write!(f, "injected fault"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Corrupt(e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, StoreError>;

/// Fault injection for the crash-recovery tests. `Error` makes the write path fail after the
/// ledger row is inserted (transaction rolls back); `Abort` kills the process at that point
/// (the server only honours it via GOVERNOR_TEST_FAULT in eval/test mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    ErrorAfterLedgerInsert,
    AbortAfterLedgerInsert,
}

#[derive(Debug, Clone)]
pub enum ActionOutcome {
    /// A new decision, committed.
    Fresh(Decision),
    /// Same Idempotency-Key and same body as an earlier call: the stored decision.
    Replay(Decision),
    /// Same Idempotency-Key, different body. Nothing evaluated or written.
    KeyConflict { decision_id: String },
    /// The database refused the write (a UNIQUE constraint on the live ledger). Nothing was
    /// written; the caller must treat it as "not paid".
    DbRejected(String),
}

/// Result of reopening a human-rejected decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReopenOutcome {
    Reopened,
    /// Unknown decision, not rejected by a human, or already reopened.
    NotRejected,
    /// The identity reopening it is the one that proposed the payment.
    SelfReopen,
}

/// Result of a REVIEW resolution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    Resolved,
    /// Unknown decision, not a REVIEW, or already resolved.
    NotPending,
    /// The approver is the identity that proposed the payment.
    SelfApproval,
}

pub struct Store {
    pub(crate) conn: Connection,
    pub gov: Governor,
    /// The vendor master as loaded from file. `gov.master` = this + every applied bank change
    /// (rebuilt on each reload, so all processes on one DB see the same master).
    pub(crate) base_master: Master,
    /// Phase 4: key that MACs applied bank changes (GOVERNOR_STATE_KEY). Set with
    /// `set_state_key`; `None` leaves applied changes unauthenticated (warned at startup).
    pub(crate) state_key: Option<[u8; 32]>,
    /// Phase 4: vendor phone numbers on file (trusted, from onboarding), for call-backs.
    pub phones: std::collections::BTreeMap<String, String>,
    /// Phase 4: seconds between a bank change's second approval and the moment payments to the
    /// new details can be allowed. Default `master_change::DEFAULT_COOLING_OFF_SECS` (72 h).
    pub cooling_off_secs: i64,
    /// Phase 2 provenance check (trust.rs). The server always sets it; `None` keeps the old
    /// caller-supplied-evidence path for library tests only.
    pub trust: Option<crate::trust::TrustVerifier>,
    /// `meta.generation` the in-memory `gov` reflects; -1 forces a reload.
    pub(crate) generation: i64,
    fault: Option<Fault>,
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn verdict_str(v: Verdict) -> &'static str {
    match v {
        Verdict::Allow => "allow",
        Verdict::Review => "review",
        Verdict::Block => "block",
    }
}

fn status_str(s: EntryStatus) -> &'static str {
    match s {
        EntryStatus::Paid => "paid",
        EntryStatus::Pending => "pending",
        EntryStatus::Rejected => "rejected",
    }
}

fn is_constraint(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::ConstraintViolation)
}

/// Open a connection with the pragmas every governor connection uses.
pub fn connect(path: &str) -> Result<Connection> {
    let conn = if path == ":memory:" {
        Connection::open_in_memory()?
    } else {
        Connection::open(path)?
    };
    conn.busy_timeout(Duration::from_secs(10))?;
    if path != ":memory:" {
        // WAL: readers never block the writer; a crash leaves uncommitted frames unused.
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Corrupt(format!(
                "could not enable WAL (got {mode})"
            )));
        }
        // FULL: a committed payment survives power loss, not just a process crash.
        conn.execute_batch("PRAGMA synchronous=FULL;")?;
    }
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

/// Bring the schema up to `SCHEMA_VERSION`. Safe with several processes opening at once:
/// the version is re-read under the write lock.
pub fn migrate(conn: &Connection) -> Result<i64> {
    loop {
        let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if current > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew(current));
        }
        let Some((v, sql)) = MIGRATIONS.iter().find(|(v, _)| *v > current) else {
            return Ok(current);
        };
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let again: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if again != current {
            conn.execute_batch("ROLLBACK")?; // someone else migrated; look again
            continue;
        }
        let r = conn
            .execute_batch(sql)
            .and_then(|_| conn.execute_batch(&format!("PRAGMA user_version = {v}")));
        match r {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.into());
            }
        }
    }
}

fn meta(tx: &Connection, k: &str) -> Result<i64> {
    Ok(tx.query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| r.get(0))?)
}

fn load_ledger(c: &Connection) -> Result<Ledger> {
    let mut st = c.prepare("SELECT entry FROM ledger ORDER BY id")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    let mut entries = vec![];
    for row in rows {
        entries.push(serde_json::from_str::<Entry>(&row?)?);
    }
    let mut st = c.prepare("SELECT decision_id FROM rejection_reopens")?;
    let reopened: std::collections::HashSet<String> = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    for e in entries.iter_mut() {
        if e.status == EntryStatus::Rejected && reopened.contains(&e.decision_id) {
            e.reopened = true;
        }
    }
    Ok(Ledger { entries })
}

/// Every audit record in seq order, as stored.
pub fn load_audit(c: &Connection) -> Result<Vec<AuditRecord>> {
    let mut st = c.prepare("SELECT seq, prev_hash, payload, hash FROM audit ORDER BY seq")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out = vec![];
    for row in rows {
        let (seq, prev_hash, payload, hash) = row?;
        out.push(AuditRecord {
            seq: seq as u64,
            prev_hash,
            payload: serde_json::from_str(&payload)?,
            hash,
        });
    }
    Ok(out)
}

/// Head of the stored chain: (seq, hash, record count).
pub fn audit_head(c: &Connection) -> Result<Option<(u64, String, u64)>> {
    let head = c
        .query_row(
            "SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let n: i64 = c.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))?;
    Ok(head.map(|(s, h)| (s as u64, h, n as u64)))
}

fn insert_ledger(tx: &Transaction, e: &Entry) -> rusqlite::Result<usize> {
    let v = serde_json::to_value(e).expect("entry json");
    // Currency is read from the serialised entry so this works whether or not `Entry` carries
    // a currency field (INR-only entries have none and are INR).
    let currency = v
        .get("currency")
        .and_then(|c| c.as_str())
        .unwrap_or("INR")
        .to_ascii_uppercase();
    tx.execute(
        "INSERT INTO ledger (decision_id, vendor_id, invoice_key, amount_paise, currency, irn, po_ref, status, ts, entry)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            e.decision_id, e.vendor_id, e.invoice_key, e.amount_paise, currency, e.irn, e.po_ref,
            status_str(e.status), e.ts, v.to_string()
        ],
    )
}

fn insert_audit(tx: &Transaction, r: &AuditRecord) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO audit (seq, prev_hash, payload, hash) VALUES (?1, ?2, ?3, ?4)",
        params![
            r.seq as i64,
            r.prev_hash,
            serde_json::to_string(&r.payload).expect("payload json"),
            r.hash
        ],
    )
}

enum NonceOutcome {
    Fresh,
    Replayed,
    Full,
}

/// Record a signed evidence nonce as used. Prunes rows that can no longer be accepted (issued
/// before `wall - max_age`, which trust.rs refuses as stale).
fn persist_nonce(
    tx: &Transaction,
    n: &crate::trust::AcceptedNonce,
    wall: i64,
    max_age: i64,
    max_nonces: usize,
) -> rusqlite::Result<NonceOutcome> {
    tx.execute(
        "DELETE FROM used_nonces WHERE issued_at < ?1",
        [wall - max_age],
    )?;
    let seen: bool = tx
        .query_row(
            "SELECT 1 FROM used_nonces WHERE nonce = ?1",
            [&n.nonce],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if seen {
        return Ok(NonceOutcome::Replayed);
    }
    let held: i64 = tx.query_row("SELECT COUNT(*) FROM used_nonces", [], |r| r.get(0))?;
    if held as usize >= max_nonces {
        return Ok(NonceOutcome::Full);
    }
    tx.execute(
        "INSERT INTO used_nonces (nonce, issued_at) VALUES (?1, ?2)",
        params![n.nonce, n.issued_at],
    )?;
    Ok(NonceOutcome::Fresh)
}

/// Number of evidence nonces held in the durable store (tests, metrics).
pub fn nonces_persisted(c: &Connection) -> Result<i64> {
    Ok(c.query_row("SELECT COUNT(*) FROM used_nonces", [], |r| r.get(0))?)
}

fn bump_generation(tx: &Transaction) -> rusqlite::Result<i64> {
    tx.query_row(
        "UPDATE meta SET v = v + 1 WHERE k = 'generation' RETURNING v",
        [],
        |r| r.get(0),
    )
}

impl Store {
    /// Open (creating and migrating if needed) the DB at `path`; ":memory:" for tests.
    pub fn open(path: &str, master: Master) -> Result<Self> {
        let conn = connect(path)?;
        let mut s = Store {
            conn,
            gov: Governor::new(master.clone()),
            base_master: master,
            state_key: None,
            phones: Default::default(),
            cooling_off_secs: crate::master_change::DEFAULT_COOLING_OFF_SECS,
            trust: None,
            generation: -1,
            fault: None,
        };
        s.refresh()?;
        Ok(s)
    }

    pub fn open_in_memory(master: Master) -> Result<Self> {
        Self::open(":memory:", master)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn inject_fault(&mut self, f: Option<Fault>) {
        self.fault = f;
    }

    /// Reload the in-memory governor from the DB if another connection committed.
    pub fn refresh(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        Self::sync(
            &tx,
            &mut self.gov,
            &mut self.generation,
            &self.base_master,
            self.state_key.as_ref(),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Set the key that authenticates applied bank changes and rebuild the master with it.
    pub fn set_state_key(&mut self, key: Option<[u8; 32]>) -> Result<()> {
        self.state_key = key;
        self.generation = -1;
        self.refresh()
    }

    pub(crate) fn sync(
        tx: &Connection,
        gov: &mut Governor,
        generation: &mut i64,
        base: &Master,
        key: Option<&[u8; 32]>,
    ) -> Result<()> {
        let g = meta(tx, "generation")?;
        if g == *generation {
            return Ok(());
        }
        gov.master = crate::master_store::effective_master(tx, base, key)?;
        gov.ledger = load_ledger(tx)?;
        gov.audit = AuditLog {
            records: load_audit(tx)?,
        };
        gov.set_next_id(meta(tx, "next_id")? as u64);
        *generation = g;
        Ok(())
    }

    /// Evaluate one action and persist it atomically. `idem` is the Idempotency-Key header.
    pub fn act(&mut self, raw: &str, now: i64, idem: Option<&str>) -> Result<ActionOutcome> {
        self.act_as(raw, now, idem, None)
    }

    /// `act` recording the authenticated identity that proposed the payment (Phase 4). That
    /// identity can never resolve the decision, and an Idempotency-Key is bound to it.
    pub fn act_as(
        &mut self,
        raw: &str,
        now: i64,
        idem: Option<&str>,
        proposer: Option<&str>,
    ) -> Result<ActionOutcome> {
        let r = self.act_inner(raw, now, idem, proposer);
        match &r {
            Ok(ActionOutcome::Fresh(_))
            | Ok(ActionOutcome::Replay(_))
            | Ok(ActionOutcome::KeyConflict { .. }) => {}
            // Rolled back: the in-memory copy may hold the uncommitted entry. Drop it.
            _ => self.generation = -1,
        }
        r
    }

    fn act_inner(
        &mut self,
        raw: &str,
        now: i64,
        idem: Option<&str>,
        proposer: Option<&str>,
    ) -> Result<ActionOutcome> {
        let body_hash = sha256_hex(raw.as_bytes());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = idem {
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT body_sha256, decision_id FROM idempotency WHERE key = ?1",
                    [key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((h, did)) = prior {
                let by: Option<String> = tx.query_row(
                    "SELECT proposed_by FROM decisions WHERE decision_id = ?1",
                    [&did],
                    |r| r.get(0),
                )?;
                // A key is bound to the identity that first used it: another identity cannot read
                // or replay someone else's decision through it.
                let other_identity =
                    matches!((by.as_deref(), proposer), (Some(a), Some(b)) if a != b);
                if h != body_hash || other_identity {
                    return Ok(ActionOutcome::KeyConflict { decision_id: did });
                }
                let body: String = tx.query_row(
                    "SELECT body FROM decisions WHERE decision_id = ?1",
                    [&did],
                    |r| r.get(0),
                )?;
                return Ok(ActionOutcome::Replay(serde_json::from_str(&body)?));
            }
        }
        Self::sync(
            &tx,
            &mut self.gov,
            &mut self.generation,
            &self.base_master,
            self.state_key.as_ref(),
        )?;
        // Any failure from here on: the in-memory governor must be reloaded.
        self.generation = -1;
        let (led0, aud0) = (self.gov.ledger.entries.len(), self.gov.audit.records.len());
        // Bank changes whose cooling-off has ended take effect before this payment is judged.
        crate::master_store::apply_due(
            &tx,
            &mut self.gov,
            &self.phones,
            self.cooling_off_secs,
            now,
            self.state_key.as_ref(),
        )?;
        let d = match self.trust.as_mut() {
            // Freshness of signed evidence uses the real clock, never the (eval) ledger clock.
            Some(t) => {
                let wall = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                match t.verify_accept(raw, wall) {
                    Err(r) => self.gov.reject(r.rule, r.verdict, r.detail, now),
                    Ok((req, None)) => self.gov.evaluate(&req, now),
                    Ok((req, Some(n))) => {
                        // Finding 4: the nonce is recorded in the same transaction as the decision.
                        match persist_nonce(&tx, &n, wall, t.cfg.max_age_secs, t.cfg.max_nonces)? {
                            NonceOutcome::Fresh => self.gov.evaluate(&req, now),
                            NonceOutcome::Replayed => self.gov.reject(
                                "evidence_replayed",
                                Verdict::Block,
                                format!(
                                    "nonce {} was already used (durable nonce store)",
                                    &n.nonce[..16.min(n.nonce.len())]
                                ),
                                now,
                            ),
                            NonceOutcome::Full => self.gov.reject(
                                "evidence_nonce_store_full",
                                Verdict::Review,
                                "too many live nonces in the durable store; refusing to decide"
                                    .into(),
                                now,
                            ),
                        }
                    }
                }
            }
            None => self.gov.evaluate_json(raw, now),
        };

        let write = || -> std::result::Result<(), rusqlite::Error> {
            tx.execute(
                "INSERT INTO decisions (decision_id, verdict, body, ts, proposed_by) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![d.decision_id, verdict_str(d.verdict), serde_json::to_string(&d).expect("decision json"), now, proposer],
            )?;
            for e in &self.gov.ledger.entries[led0..] {
                insert_ledger(&tx, e)?;
            }
            Ok(())
        };
        match write() {
            Ok(()) => {}
            Err(e) if is_constraint(&e) => return Ok(ActionOutcome::DbRejected(e.to_string())),
            Err(e) => return Err(e.into()),
        }
        match self.fault {
            Some(Fault::ErrorAfterLedgerInsert) => return Err(StoreError::Injected),
            Some(Fault::AbortAfterLedgerInsert) => {
                eprintln!("GOVERNOR_TEST_FAULT: aborting mid-transaction");
                std::process::abort();
            }
            None => {}
        }
        for r in &self.gov.audit.records[aud0..] {
            insert_audit(&tx, r)?;
        }
        if let Some(key) = idem {
            tx.execute(
                "INSERT INTO idempotency (key, body_sha256, decision_id) VALUES (?1, ?2, ?3)",
                params![key, body_hash, d.decision_id],
            )?;
        }
        tx.execute(
            "UPDATE meta SET v = ?1 WHERE k = 'next_id'",
            [self.gov.next_id() as i64],
        )?;
        let g = bump_generation(&tx)?;
        tx.commit()?;
        self.generation = g;
        Ok(ActionOutcome::Fresh(d))
    }

    pub fn get_decision(&self, id: &str) -> Result<Option<Decision>> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM decisions WHERE decision_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match body {
            Some(b) => Some(serde_json::from_str(&b)?),
            None => None,
        })
    }

    /// Human decision on a pending REVIEW, persisted atomically with its audit record.
    pub fn resolve(&mut self, id: &str, approve: bool, approver: &str, now: i64) -> Result<bool> {
        Ok(self.resolve_as(id, approve, approver, now)? == ResolveOutcome::Resolved)
    }

    /// Human decision on a pending REVIEW by an authenticated approver. Refused when the approver
    /// is the identity that proposed the payment (maker-checker), checked here and by a trigger.
    pub fn resolve_as(
        &mut self,
        id: &str,
        approve: bool,
        approver: &str,
        now: i64,
    ) -> Result<ResolveOutcome> {
        let r = self.resolve_inner(id, approve, approver, now);
        if !matches!(r, Ok(ResolveOutcome::Resolved)) {
            self.generation = -1;
        }
        r
    }

    fn resolve_inner(
        &mut self,
        id: &str,
        approve: bool,
        approver: &str,
        now: i64,
    ) -> Result<ResolveOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT verdict, proposed_by FROM decisions WHERE decision_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((verdict, proposed_by)) = row else {
            return Ok(ResolveOutcome::NotPending);
        };
        if verdict != "review" {
            return Ok(ResolveOutcome::NotPending);
        }
        if proposed_by.as_deref() == Some(approver) {
            return Ok(ResolveOutcome::SelfApproval);
        }
        Self::sync(
            &tx,
            &mut self.gov,
            &mut self.generation,
            &self.base_master,
            self.state_key.as_ref(),
        )?;
        self.generation = -1;
        let aud0 = self.gov.audit.records.len();
        if !self.gov.resolve_review(id, approve, approver, now) {
            return Ok(ResolveOutcome::NotPending);
        }
        tx.execute(
            "INSERT INTO review_resolutions (decision_id, approver, approve, ts) VALUES (?1, ?2, ?3, ?4)",
            params![id, approver, approve as i64, now],
        )?;
        let e = self
            .gov
            .ledger
            .entries
            .iter()
            .find(|e| e.decision_id == id)
            .cloned()
            .expect("resolved entry");
        let n = tx.execute(
            "UPDATE ledger SET status = ?1, entry = ?2 WHERE decision_id = ?3",
            params![status_str(e.status), serde_json::to_string(&e)?, id],
        )?;
        if n != 1 {
            return Err(StoreError::Corrupt(format!("ledger row for {id} missing")));
        }
        for r in &self.gov.audit.records[aud0..] {
            insert_audit(&tx, r)?;
        }
        let g = bump_generation(&tx)?;
        tx.commit()?;
        self.generation = g;
        Ok(ResolveOutcome::Resolved)
    }

    /// Reopen a human-REJECTED decision (operator/approver, audited). Until this is done, the
    /// same invoice (vendor + number, IRN, or same document text) is BLOCKed `previously_rejected`
    /// forever, whatever the 30/90-day windows say.
    pub fn reopen_as(
        &mut self,
        id: &str,
        by: &str,
        reason: &str,
        now: i64,
    ) -> Result<ReopenOutcome> {
        let r = self.reopen_inner(id, by, reason, now);
        if !matches!(r, Ok(ReopenOutcome::Reopened)) {
            self.generation = -1;
        }
        r
    }

    fn reopen_inner(
        &mut self,
        id: &str,
        by: &str,
        reason: &str,
        now: i64,
    ) -> Result<ReopenOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let proposed_by: Option<Option<String>> = tx
            .query_row(
                "SELECT proposed_by FROM decisions WHERE decision_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(proposed_by) = proposed_by else {
            return Ok(ReopenOutcome::NotRejected);
        };
        if proposed_by.as_deref() == Some(by) {
            return Ok(ReopenOutcome::SelfReopen);
        }
        Self::sync(
            &tx,
            &mut self.gov,
            &mut self.generation,
            &self.base_master,
            self.state_key.as_ref(),
        )?;
        self.generation = -1;
        let aud0 = self.gov.audit.records.len();
        if !self.gov.reopen_rejection(id, by, reason, now) {
            return Ok(ReopenOutcome::NotRejected);
        }
        tx.execute(
            "INSERT INTO rejection_reopens (decision_id, reopened_by, reason, ts) VALUES (?1, ?2, ?3, ?4)",
            params![id, by, reason, now],
        )?;
        for r in &self.gov.audit.records[aud0..] {
            insert_audit(&tx, r)?;
        }
        let g = bump_generation(&tx)?;
        tx.commit()?;
        self.generation = g;
        Ok(ReopenOutcome::Reopened)
    }

    /// Eval-only: forget everything (the server exposes it only with GOVERNOR_EVAL_CLOCK=1).
    pub fn reset(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "DELETE FROM idempotency; DELETE FROM rejection_reopens; DELETE FROM ledger; DELETE FROM review_resolutions; DELETE FROM master_change_approvals;
             DELETE FROM master_changes; DELETE FROM audit; DELETE FROM decisions; UPDATE meta SET v = 1 WHERE k = 'next_id';",
        )?;
        bump_generation(&tx)?;
        tx.commit()?;
        self.generation = -1;
        self.gov.reset_state();
        self.gov.master = self.base_master.clone();
        self.refresh()
    }

    pub fn audit_records(&self) -> Result<Vec<AuditRecord>> {
        load_audit(&self.conn)
    }

    pub fn audit_head(&self) -> Result<Option<(u64, String, u64)>> {
        audit_head(&self.conn)
    }

    pub fn ledger(&self) -> Result<Ledger> {
        load_ledger(&self.conn)
    }

    /// Online backup (SQLite backup API): consistent snapshot while the server keeps running.
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        backup(&self.conn, dest)
    }
}

/// Copy a live DB to `dest` with the online backup API.
pub fn backup(src: &Connection, dest: &Path) -> Result<()> {
    let mut out = Connection::open(dest)?;
    {
        let b = rusqlite::backup::Backup::new(src, &mut out)?;
        b.run_to_completion(256, Duration::from_millis(0), None)?;
    }
    check_integrity(&out)?;
    Ok(())
}

/// Restore `backup_file` into the DB at `db_path` (backup API, page by page under a lock),
/// then check integrity, schema version and the audit chain.
pub fn restore(backup_file: &Path, db_path: &str) -> Result<()> {
    let src = Connection::open_with_flags(backup_file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    check_integrity(&src)?;
    let v: i64 = src.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew(v));
    }
    let records = load_audit(&src)?;
    if let Some(i) = (AuditLog { records }).verify() {
        return Err(StoreError::Corrupt(format!(
            "backup audit chain breaks at record {i}"
        )));
    }
    drop(src);
    let mut dst = Connection::open(db_path)?;
    dst.busy_timeout(Duration::from_secs(10))?;
    dst.restore(
        rusqlite::DatabaseName::Main,
        backup_file,
        None::<fn(rusqlite::backup::Progress)>,
    )?;
    check_integrity(&dst)?;
    drop(dst);
    connect(db_path)?; // WAL + migrations up to this binary's version
    Ok(())
}

fn check_integrity(c: &Connection) -> Result<()> {
    let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        return Err(StoreError::Corrupt(format!("integrity_check: {ok}")));
    }
    Ok(())
}
