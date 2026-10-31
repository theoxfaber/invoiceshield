//! External anchoring of the audit hash chain (Phase 3).
//!
//! The hash chain alone proves that no record was changed *relative to the others*; someone
//! with write access to the DB can still recompute every hash and replace the whole chain.
//! An anchor is a signed statement "at time T the chain had record `seq` with hash H",
//! appended to a file that lives outside the DB (and optionally POSTed to another system).
//! `verify_chain` replays the stored chain against every anchor, so a whole-chain rewrite,
//! a truncation or an edited record before the newest anchor is detected.
//!
//! Key: Ed25519, 32-byte seed as hex in GOVERNOR_ANCHOR_KEY or in the file named by
//! GOVERNOR_ANCHOR_KEY_FILE. Verifiers pin the public key (GOVERNOR_ANCHOR_PUBKEY or
//! `--pubkey`); the key inside an anchor line is informational and never trusted.
use crate::audit::{AuditLog, AuditRecord};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

pub const DOMAIN: &str = "invoiceshield-audit-anchor-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub v: u32,
    /// seq of the newest audit record covered.
    pub seq: u64,
    pub head_hash: String,
    pub ts: i64,
    pub pubkey: String,
    pub sig: String,
}

/// Exactly what is signed. Fixed text layout, so other languages can verify it too.
pub fn message(seq: u64, head_hash: &str, ts: i64) -> Vec<u8> {
    format!("{DOMAIN}\nseq={seq}\nhead={head_hash}\nts={ts}\n").into_bytes()
}

pub fn sign(key: &SigningKey, seq: u64, head_hash: &str, ts: i64) -> Anchor {
    let sig = key.sign(&message(seq, head_hash, ts));
    Anchor {
        v: 1,
        seq,
        head_hash: head_hash.to_string(),
        ts,
        pubkey: hex::encode(key.verifying_key().to_bytes()),
        sig: hex::encode(sig.to_bytes()),
    }
}

pub fn check_sig(a: &Anchor, trusted: &VerifyingKey) -> bool {
    let Ok(bytes) = hex::decode(&a.sig) else {
        return false;
    };
    let Ok(arr) = <[u8; 64]>::try_from(bytes.as_slice()) else {
        return false;
    };
    a.v == 1
        // verify_strict (finding 5): rejects small-order keys and non-canonical signatures,
        // the same check trust.rs applies to evidence.
        && trusted
            .verify_strict(
                &message(a.seq, &a.head_hash, a.ts),
                &Signature::from_bytes(&arr),
            )
            .is_ok()
}

pub fn generate_key() -> SigningKey {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("OS randomness");
    SigningKey::from_bytes(&seed)
}

pub fn key_from_hex(h: &str) -> Result<SigningKey, String> {
    let b = hex::decode(h.trim()).map_err(|e| format!("anchor key is not hex: {e}"))?;
    let seed = <[u8; 32]>::try_from(b.as_slice())
        .map_err(|_| "anchor key must be 32 bytes (64 hex chars)".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn pubkey_from_hex(h: &str) -> Result<VerifyingKey, String> {
    let b = hex::decode(h.trim()).map_err(|e| format!("pubkey is not hex: {e}"))?;
    let arr =
        <[u8; 32]>::try_from(b.as_slice()).map_err(|_| "pubkey must be 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&arr).map_err(|e| e.to_string())
}

/// GOVERNOR_ANCHOR_KEY (hex seed) or GOVERNOR_ANCHOR_KEY_FILE. None when neither is set.
pub fn key_from_env() -> Result<Option<SigningKey>, String> {
    if let Ok(h) = std::env::var("GOVERNOR_ANCHOR_KEY") {
        return key_from_hex(&h).map(Some);
    }
    if let Ok(p) = std::env::var("GOVERNOR_ANCHOR_KEY_FILE") {
        let h = std::fs::read_to_string(&p).map_err(|e| format!("read {p}: {e}"))?;
        return key_from_hex(&h).map(Some);
    }
    Ok(None)
}

/// Append one anchor as a JSON line and fsync. The file is only ever opened for append.
pub fn append(path: &Path, a: &Anchor) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut line = serde_json::to_vec(a).expect("anchor json");
    line.push(b'\n');
    f.write_all(&line)?;
    f.sync_all()
}

pub fn read_all(path: &Path) -> std::io::Result<Vec<Anchor>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let f = std::fs::File::open(path)?;
    let mut out = vec![];
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("anchor line {}: {e}", i + 1),
            )
        })?);
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub records: usize,
    /// First record whose hash link is broken, if any.
    pub first_broken: Option<usize>,
    pub anchors_checked: usize,
    pub anchor_errors: Vec<String>,
    pub ok: bool,
}

/// Check the stored chain on its own and against every anchor signed by `trusted`.
pub fn verify_chain(
    records: &[AuditRecord],
    anchors: &[Anchor],
    trusted: &VerifyingKey,
) -> VerifyReport {
    let first_broken = AuditLog {
        records: records.to_vec(),
    }
    .verify();
    let mut errs = vec![];
    let mut last_seq: Option<u64> = None;
    for (i, a) in anchors.iter().enumerate() {
        if !check_sig(a, trusted) {
            errs.push(format!(
                "anchor {i} (seq {}): bad signature or not signed by the pinned key",
                a.seq
            ));
            continue;
        }
        if let Some(prev) = last_seq {
            if a.seq < prev {
                errs.push(format!(
                    "anchor {i}: seq {} goes backwards (previous {prev})",
                    a.seq
                ));
            }
        }
        last_seq = Some(a.seq);
        match records.get(a.seq as usize) {
            None => errs.push(format!(
                "anchor {i}: chain has {} records but the anchor covers seq {} (truncated)",
                records.len(),
                a.seq
            )),
            Some(r) if r.seq != a.seq || r.hash != a.head_hash => errs.push(format!(
                "anchor {i}: record {} hash {} != anchored {} (chain rewritten)",
                a.seq,
                short(&r.hash),
                short(&a.head_hash)
            )),
            Some(_) => {}
        }
    }
    let ok = first_broken.is_none() && errs.is_empty();
    VerifyReport {
        records: records.len(),
        first_broken,
        anchors_checked: anchors.len(),
        anchor_errors: errs,
        ok,
    }
}

fn short(h: &str) -> &str {
    &h[..h.len().min(12)]
}

/// POST an anchor as JSON to a plain-http URL (e.g. a log collector on the internal network,
/// or a TLS-terminating relay). Best effort: the file anchor is the record; errors are returned
/// for logging and never block a payment decision.
pub async fn post(url: &str, a: &Anchor) -> Result<u16, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url
        .strip_prefix("http://")
        .ok_or("only http:// anchor URLs are supported (put a TLS relay in front)")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let body = serde_json::to_string(a).map_err(|e| e.to_string())?;
    let fut = async {
        let mut s = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| e.to_string())?;
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut resp = vec![];
        s.read_to_end(&mut resp).await.map_err(|e| e.to_string())?;
        let head = String::from_utf8_lossy(&resp);
        head.split_whitespace()
            .nth(1)
            .and_then(|c| c.parse::<u16>().ok())
            .ok_or_else(|| "bad HTTP response".to_string())
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), fut)
        .await
        .map_err(|_| "timeout".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    /// Finding 5: a small-order (identity) public key with the all-zero-scalar signature passes
    /// the lenient `verify` but must fail `check_sig`, which now uses `verify_strict`.
    #[test]
    fn weak_key_signature_rejected_by_strict_check() {
        let mut id = [0u8; 32];
        id[0] = 1; // compressed Edwards identity
        let weak = VerifyingKey::from_bytes(&id).expect("identity decodes");
        let mut sig = [0u8; 64];
        sig[0] = 1; // R = identity, S = 0
        let msg = message(7, "abc", 1_800_000_000);
        assert!(
            weak.verify(&msg, &Signature::from_bytes(&sig)).is_ok(),
            "precondition: the lenient check accepts this forgery"
        );
        let a = Anchor {
            v: 1,
            seq: 7,
            head_hash: "abc".into(),
            ts: 1_800_000_000,
            pubkey: hex::encode(id),
            sig: hex::encode(sig),
        };
        assert!(!check_sig(&a, &weak));
    }

    #[test]
    fn genuine_anchor_still_verifies() {
        let k = generate_key();
        let a = sign(&k, 3, "deadbeef", 1_800_000_000);
        assert!(check_sig(&a, &k.verifying_key()));
    }
}
