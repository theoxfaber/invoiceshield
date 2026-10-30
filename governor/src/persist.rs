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
use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

/// Latest schema version this binary knows. Stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 1;

/// Versioned migrations, applied in order inside one IMMEDIATE transaction each.
/// Never edit a released step; add a new one.
const MIGRATIONS: &[(i64, &str)] = &[(
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
    irn          TEXT,
    po_ref       TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('paid','pending','rejected')),
    ts           INTEGER NOT NULL,
    entry        TEXT NOT NULL
);
-- The duplicate-payment guarantee. Rejected entries do not count, exactly like ledger.rs.
CREATE UNIQUE INDEX ledger_live_dedupe ON ledger(vendor_id, invoice_key, amount_paise) WHERE status <> 'rejected';
CREATE UNIQUE INDEX ledger_live_irn ON ledger(irn) WHERE irn IS NOT NULL AND status <> 'rejected';
-- Only a pending entry may change, and only its status.
CREATE TRIGGER ledger_update_guard BEFORE UPDATE ON ledger
WHEN OLD.status <> 'pending' OR NEW.vendor_id <> OLD.vendor_id OR NEW.invoice_key <> OLD.invoice_key
  OR NEW.amount_paise <> OLD.amount_paise OR NEW.decision_id <> OLD.decision_id
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
)];

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
            StoreError::SchemaTooNew(v) => write!(f, "database schema v{v} is newer than this binary (v{SCHEMA_VERSION})"),
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

pub struct Store {
    conn: Connection,
    pub gov: Governor,
    /// `meta.generation` the in-memory `gov` reflects; -1 forces a reload.
    generation: i64,
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
    let conn = if path == ":memory:" { Connection::open_in_memory()? } else { Connection::open(path)? };
    conn.busy_timeout(Duration::from_secs(10))?;
    if path != ":memory:" {
        // WAL: readers never block the writer; a crash leaves uncommitted frames unused.
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Corrupt(format!("could not enable WAL (got {mode})")));
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
        let Some((v, sql)) = MIGRATIONS.iter().find(|(v, _)| *v > current) else { return Ok(current) };
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let again: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if again != current {
            conn.execute_batch("ROLLBACK")?; // someone else migrated; look again
            continue;
        }
        let r = conn.execute_batch(sql).and_then(|_| conn.execute_batch(&format!("PRAGMA user_version = {v}")));
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
    Ok(Ledger { entries })
}

/// Every audit record in seq order, as stored.
pub fn load_audit(c: &Connection) -> Result<Vec<AuditRecord>> {
    let mut st = c.prepare("SELECT seq, prev_hash, payload, hash FROM audit ORDER BY seq")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
    })?;
    let mut out = vec![];
    for row in rows {
        let (seq, prev_hash, payload, hash) = row?;
        out.push(AuditRecord { seq: seq as u64, prev_hash, payload: serde_json::from_str(&payload)?, hash });
    }
    Ok(out)
}

/// Head of the stored chain: (seq, hash, record count).
pub fn audit_head(c: &Connection) -> Result<Option<(u64, String, u64)>> {
    let head = c
        .query_row("SELECT seq, hash FROM audit ORDER BY seq DESC LIMIT 1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .optional()?;
    let n: i64 = c.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))?;
    Ok(head.map(|(s, h)| (s as u64, h, n as u64)))
}

fn insert_ledger(tx: &Transaction, e: &Entry) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO ledger (decision_id, vendor_id, invoice_key, amount_paise, irn, po_ref, status, ts, entry)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            e.decision_id, e.vendor_id, e.invoice_key, e.amount_paise, e.irn, e.po_ref,
            status_str(e.status), e.ts, serde_json::to_string(e).expect("entry json")
        ],
    )
}

fn insert_audit(tx: &Transaction, r: &AuditRecord) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO audit (seq, prev_hash, payload, hash) VALUES (?1, ?2, ?3, ?4)",
        params![r.seq as i64, r.prev_hash, serde_json::to_string(&r.payload).expect("payload json"), r.hash],
    )
}

fn bump_generation(tx: &Transaction) -> rusqlite::Result<i64> {
    tx.query_row("UPDATE meta SET v = v + 1 WHERE k = 'generation' RETURNING v", [], |r| r.get(0))
}

impl Store {
    /// Open (creating and migrating if needed) the DB at `path`; ":memory:" for tests.
    pub fn open(path: &str, master: Master) -> Result<Self> {
        let conn = connect(path)?;
        let mut s = Store { conn, gov: Governor::new(master), generation: -1, fault: None };
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
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        Self::sync(&tx, &mut self.gov, &mut self.generation)?;
        tx.commit()?;
        Ok(())
    }

    fn sync(tx: &Connection, gov: &mut Governor, generation: &mut i64) -> Result<()> {
        let g = meta(tx, "generation")?;
        if g == *generation {
            return Ok(());
        }
        gov.ledger = load_ledger(tx)?;
        gov.audit = AuditLog { records: load_audit(tx)? };
        gov.set_next_id(meta(tx, "next_id")? as u64);
        *generation = g;
        Ok(())
    }

    /// Evaluate one action and persist it atomically. `idem` is the Idempotency-Key header.
    pub fn act(&mut self, raw: &str, now: i64, idem: Option<&str>) -> Result<ActionOutcome> {
        let r = self.act_inner(raw, now, idem);
        match &r {
            Ok(ActionOutcome::Fresh(_)) | Ok(ActionOutcome::Replay(_)) | Ok(ActionOutcome::KeyConflict { .. }) => {}
            // Rolled back: the in-memory copy may hold the uncommitted entry. Drop it.
            _ => self.generation = -1,
        }
        r
    }

    fn act_inner(&mut self, raw: &str, now: i64, idem: Option<&str>) -> Result<ActionOutcome> {
        let body_hash = sha256_hex(raw.as_bytes());
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = idem {
            let prior: Option<(String, String)> = tx
                .query_row("SELECT body_sha256, decision_id FROM idempotency WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            if let Some((h, did)) = prior {
                if h != body_hash {
                    return Ok(ActionOutcome::KeyConflict { decision_id: did });
                }
                let body: String = tx.query_row("SELECT body FROM decisions WHERE decision_id = ?1", [&did], |r| r.get(0))?;
                return Ok(ActionOutcome::Replay(serde_json::from_str(&body)?));
            }
        }
        Self::sync(&tx, &mut self.gov, &mut self.generation)?;
        // Any failure from here on: the in-memory governor must be reloaded.
        self.generation = -1;
        let (led0, aud0) = (self.gov.ledger.entries.len(), self.gov.audit.records.len());
        let d = self.gov.evaluate_json(raw, now);

        let write = || -> std::result::Result<(), rusqlite::Error> {
            tx.execute(
                "INSERT INTO decisions (decision_id, verdict, body, ts) VALUES (?1, ?2, ?3, ?4)",
                params![d.decision_id, verdict_str(d.verdict), serde_json::to_string(&d).expect("decision json"), now],
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
            tx.execute("INSERT INTO idempotency (key, body_sha256, decision_id) VALUES (?1, ?2, ?3)", params![key, body_hash, d.decision_id])?;
        }
        tx.execute("UPDATE meta SET v = ?1 WHERE k = 'next_id'", [self.gov.next_id() as i64])?;
        let g = bump_generation(&tx)?;
        tx.commit()?;
        self.generation = g;
        Ok(ActionOutcome::Fresh(d))
    }

    pub fn get_decision(&self, id: &str) -> Result<Option<Decision>> {
        let body: Option<String> = self.conn.query_row("SELECT body FROM decisions WHERE decision_id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(match body {
            Some(b) => Some(serde_json::from_str(&b)?),
            None => None,
        })
    }

    /// Human decision on a pending REVIEW, persisted atomically with its audit record.
    pub fn resolve(&mut self, id: &str, approve: bool, approver: &str, now: i64) -> Result<bool> {
        let r = self.resolve_inner(id, approve, approver, now);
        if !matches!(r, Ok(true)) {
            self.generation = -1;
        }
        r
    }

    fn resolve_inner(&mut self, id: &str, approve: bool, approver: &str, now: i64) -> Result<bool> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let verdict: Option<String> = tx.query_row("SELECT verdict FROM decisions WHERE decision_id = ?1", [id], |r| r.get(0)).optional()?;
        if verdict.as_deref() != Some("review") {
            return Ok(false);
        }
        Self::sync(&tx, &mut self.gov, &mut self.generation)?;
        self.generation = -1;
        let aud0 = self.gov.audit.records.len();
        if !self.gov.resolve_review(id, approve, approver, now) {
            return Ok(false);
        }
        let e = self.gov.ledger.entries.iter().find(|e| e.decision_id == id).cloned().expect("resolved entry");
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
        Ok(true)
    }

    /// Eval-only: forget everything (the server exposes it only with GOVERNOR_EVAL_CLOCK=1).
    pub fn reset(&mut self) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("DELETE FROM idempotency; DELETE FROM ledger; DELETE FROM audit; DELETE FROM decisions; UPDATE meta SET v = 1 WHERE k = 'next_id';")?;
        bump_generation(&tx)?;
        tx.commit()?;
        self.generation = -1;
        self.gov.reset_state();
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
        return Err(StoreError::Corrupt(format!("backup audit chain breaks at record {i}")));
    }
    drop(src);
    let mut dst = Connection::open(db_path)?;
    dst.busy_timeout(Duration::from_secs(10))?;
    dst.restore(rusqlite::DatabaseName::Main, backup_file, None::<fn(rusqlite::backup::Progress)>)?;
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
