//! SHA-256 hash-chained audit log. Each record commits to the previous hash.
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub prev_hash: String,
    pub payload: serde_json::Value,
    pub hash: String,
}

#[derive(Debug, Clone, Default)]
pub struct AuditLog {
    pub records: Vec<AuditRecord>,
}

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn digest(seq: u64, prev: &str, payload: &serde_json::Value) -> String {
    let mut h = Sha256::new();
    h.update(seq.to_be_bytes());
    h.update(prev.as_bytes());
    h.update(serde_json::to_vec(payload).expect("json"));
    hex::encode(h.finalize())
}

impl AuditLog {
    pub fn append(&mut self, payload: serde_json::Value) -> String {
        let seq = self.records.len() as u64;
        let prev = self
            .records
            .last()
            .map(|r| r.hash.clone())
            .unwrap_or_else(|| GENESIS.into());
        let hash = digest(seq, &prev, &payload);
        self.records.push(AuditRecord {
            seq,
            prev_hash: prev,
            payload,
            hash: hash.clone(),
        });
        hash
    }

    /// Returns the index of the first broken record, or None if the chain verifies.
    pub fn verify(&self) -> Option<usize> {
        let mut prev = GENESIS.to_string();
        for (i, r) in self.records.iter().enumerate() {
            if r.seq != i as u64
                || r.prev_hash != prev
                || digest(r.seq, &r.prev_hash, &r.payload) != r.hash
            {
                return Some(i);
            }
            prev = r.hash.clone();
        }
        None
    }
}
