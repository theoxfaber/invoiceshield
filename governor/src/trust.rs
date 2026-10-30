//! Evidence provenance: the governor only trusts evidence signed by the extraction service.
//!
//! The agent process forwards an envelope `{"payload": "<canonical JSON>", "sig": "<base64>"}`
//! produced by the extractor, which alone holds the Ed25519 signing key. The payload commits to
//! the SHA-256 of the invoice document, the extractor version, an issue time and a nonce. The
//! governor checks, in order:
//!
//! | check                                   | rule                       | verdict |
//! |-----------------------------------------|----------------------------|---------|
//! | no `signed_evidence` (strict mode)      | `evidence_unsigned`        | REVIEW  |
//! | envelope malformed / bad base64         | `evidence_signature_invalid` | BLOCK |
//! | unknown key id                          | `evidence_signature_invalid` | BLOCK |
//! | Ed25519 signature does not verify       | `evidence_signature_invalid` | BLOCK |
//! | signed payload not the expected schema  | `evidence_payload_invalid` | REVIEW  |
//! | issued_at older than max age            | `evidence_stale`           | REVIEW  |
//! | issued_at in the future beyond skew     | `evidence_from_future`     | REVIEW  |
//! | nonce already used                      | `evidence_replayed`        | BLOCK   |
//! | nonce store full (fail closed)          | `evidence_nonce_store_full`| REVIEW  |
//! | proposal has no `document_sha256`       | `document_hash_missing`    | REVIEW  |
//! | proposal hash ≠ signed document hash    | `document_hash_mismatch`   | BLOCK   |
//! | extractor reported a sandbox failure    | `extraction_failed`        | REVIEW  |
//!
//! BLOCK is used where the only explanation is tampering (a signature that does not verify,
//! evidence for a different document, a reused nonce). REVIEW is used where an honest fault
//! is plausible (missing signature from a misconfigured client, clock skew, a slow queue,
//! a document the sandbox could not parse). Either way nothing is paid.
//!
//! Freshness always uses the real wall clock, never the eval clock header.
//! A raw `evidence` field sent next to `signed_evidence` is ignored: only signed values are read.
use crate::engine::{Evidence, Governor, Request};
use crate::verdict::Verdict;
use crate::Decision;
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

pub const PAYLOAD_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct TrustConfig {
    /// key id → public key. Key id = first 16 hex chars of sha256(raw 32-byte public key).
    pub keys: HashMap<String, VerifyingKey>,
    pub max_age_secs: i64,
    pub max_skew_secs: i64,
    pub max_nonces: usize,
    /// DEV/TEST ONLY: accept the old unsigned `evidence` field when no envelope is sent.
    pub dev_unsigned: bool,
}

impl TrustConfig {
    pub fn new(keys: Vec<VerifyingKey>) -> Self {
        TrustConfig {
            keys: keys.into_iter().map(|k| (key_id(&k), k)).collect(),
            max_age_secs: 300,
            max_skew_secs: 30,
            max_nonces: 100_000,
            dev_unsigned: false,
        }
    }

    /// Parse a comma-separated list of public keys, each 64 hex chars or base64 of 32 bytes.
    pub fn parse_keys(s: &str) -> Result<Vec<VerifyingKey>, String> {
        s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(|x| {
            let raw = if x.len() == 64 && x.chars().all(|c| c.is_ascii_hexdigit()) {
                hex::decode(x).map_err(|e| e.to_string())?
            } else {
                base64::engine::general_purpose::STANDARD.decode(x).map_err(|e| e.to_string())?
            };
            let arr: [u8; 32] = raw.try_into().map_err(|_| "public key must be 32 bytes".to_string())?;
            VerifyingKey::from_bytes(&arr).map_err(|e| e.to_string())
        }).collect()
    }
}

pub fn key_id(k: &VerifyingKey) -> String {
    hex::encode(Sha256::digest(k.as_bytes()))[..16].to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    sig: String,
}

/// What the extractor signs. Canonical JSON (sorted keys, no whitespace) of this object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedPayload {
    pub v: u32,
    pub key_id: String,
    pub doc_sha256: String,
    #[serde(default)]
    pub email_sha256: Option<String>,
    pub extractor_version: String,
    pub issued_at: i64,
    pub nonce: String,
    /// "ok" or "failed".
    pub status: String,
    #[serde(default)]
    pub failure: Option<String>,
    #[serde(default)]
    pub sandbox: Option<serde_json::Value>,
    #[serde(default)]
    pub evidence: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub rule: &'static str,
    pub verdict: Verdict,
    pub detail: String,
}

fn rej(rule: &'static str, verdict: Verdict, detail: impl Into<String>) -> Rejection {
    Rejection { rule, verdict, detail: detail.into() }
}

pub struct TrustVerifier {
    pub cfg: TrustConfig,
    seen: HashMap<String, i64>,
    order: VecDeque<(i64, String)>,
}

impl TrustVerifier {
    pub fn new(cfg: TrustConfig) -> Self {
        TrustVerifier { cfg, seen: HashMap::new(), order: VecDeque::new() }
    }

    pub fn nonces_held(&self) -> usize {
        self.seen.len()
    }

    fn prune(&mut self, wall_now: i64) {
        let horizon = wall_now - self.cfg.max_age_secs;
        while let Some((t, _)) = self.order.front() {
            if *t >= horizon {
                break;
            }
            let (_, n) = self.order.pop_front().unwrap();
            self.seen.remove(&n);
        }
    }

    /// Turn a wire body into a Request whose evidence is only what the extractor signed.
    pub fn verify(&mut self, raw: &str, wall_now: i64) -> Result<Request, Rejection> {
        let mut body: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| rej("request_unparseable", Verdict::Review, e.to_string()))?;
        let obj = body.as_object_mut().ok_or_else(|| rej("request_unparseable", Verdict::Review, "body is not an object"))?;
        let claimed_hash = obj.get_mut("proposal").and_then(|p| p.as_object_mut())
            .and_then(|p| p.remove("document_sha256"));
        let Some(env) = obj.remove("signed_evidence") else {
            if self.cfg.dev_unsigned {
                // DEV/TEST ONLY: old path, evidence as sent by the caller.
                return serde_json::from_value::<Request>(body)
                    .map_err(|e| rej("request_unparseable", Verdict::Review, e.to_string()));
            }
            return Err(rej("evidence_unsigned", Verdict::Review,
                "no signed evidence: the governor reads evidence only from the extraction service, so a human must check the document"));
        };
        obj.remove("evidence"); // never read caller-supplied evidence next to a signed envelope

        let bad_sig = |d: &str| rej("evidence_signature_invalid", Verdict::Block, d.to_string());
        let env: Envelope = serde_json::from_value(env).map_err(|e| bad_sig(&format!("malformed envelope: {e}")))?;
        let pre: serde_json::Value = serde_json::from_str(&env.payload).map_err(|_| bad_sig("payload is not JSON"))?;
        let kid = pre.get("key_id").and_then(|v| v.as_str()).unwrap_or("");
        let key = self.cfg.keys.get(kid).ok_or_else(|| bad_sig(&format!("unknown extractor key id {kid:?}")))?;
        let sig_bytes = base64::engine::general_purpose::STANDARD.decode(env.sig.trim()).map_err(|_| bad_sig("signature is not base64"))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| bad_sig("signature must be 64 bytes"))?;
        key.verify_strict(env.payload.as_bytes(), &Signature::from_bytes(&sig_arr))
            .map_err(|_| bad_sig("Ed25519 signature does not verify against the extractor key"))?;

        // From here on the payload is the extractor's own words.
        let p: SignedPayload = serde_json::from_value(pre)
            .map_err(|e| rej("evidence_payload_invalid", Verdict::Review, e.to_string()))?;
        if p.v != PAYLOAD_VERSION {
            return Err(rej("evidence_payload_invalid", Verdict::Review, format!("payload version {} not supported", p.v)));
        }
        if p.issued_at < wall_now - self.cfg.max_age_secs {
            return Err(rej("evidence_stale", Verdict::Review,
                format!("evidence issued {}s ago (max {}s)", wall_now - p.issued_at, self.cfg.max_age_secs)));
        }
        if p.issued_at > wall_now + self.cfg.max_skew_secs {
            return Err(rej("evidence_from_future", Verdict::Review,
                format!("evidence issued {}s in the future (max skew {}s)", p.issued_at - wall_now, self.cfg.max_skew_secs)));
        }
        if p.nonce.len() < 32 || p.nonce.len() > 128 || !p.nonce.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(rej("evidence_payload_invalid", Verdict::Review, "nonce must be 32-128 hex chars"));
        }
        self.prune(wall_now);
        if self.seen.contains_key(&p.nonce) {
            return Err(rej("evidence_replayed", Verdict::Block, format!("nonce {} was already used", &p.nonce[..16])));
        }
        if self.seen.len() >= self.cfg.max_nonces {
            return Err(rej("evidence_nonce_store_full", Verdict::Review, "too many live nonces; refusing to decide"));
        }
        self.seen.insert(p.nonce.clone(), p.issued_at);
        self.order.push_back((p.issued_at, p.nonce.clone()));
        // keep `order` sorted enough for pruning: issued_at values arrive roughly in order;
        // an out-of-order one only lives a little longer than needed (still bounded by max_nonces).

        let claimed = claimed_hash.as_ref().and_then(|v| v.as_str()).map(|s| s.trim().to_ascii_lowercase());
        match claimed {
            None => return Err(rej("document_hash_missing", Verdict::Review, "proposal does not name the document (document_sha256)")),
            Some(h) if h != p.doc_sha256.to_ascii_lowercase() => {
                return Err(rej("document_hash_mismatch", Verdict::Block,
                    format!("proposal is for document {} but the signed evidence is for {}", short(&h), short(&p.doc_sha256))));
            }
            _ => {}
        }
        if p.status != "ok" {
            return Err(rej("extraction_failed", Verdict::Review,
                format!("extractor could not read the document safely: {}", p.failure.as_deref().unwrap_or("unknown"))));
        }
        let ev: Evidence = serde_json::from_value(p.evidence.unwrap_or(serde_json::Value::Null))
            .map_err(|e| rej("evidence_payload_invalid", Verdict::Review, format!("signed evidence fields: {e}")))?;
        let proposal = obj.remove("proposal").unwrap_or(serde_json::Value::Null);
        let checker_raw = match obj.remove("checker_raw") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => Some(s),
            Some(_) => return Err(rej("request_unparseable", Verdict::Review, "checker_raw must be a string")),
        };
        // Model-proposed item lines are untrusted input like the proposal; the engine verifies them
        // against the signed text layer (lines.rs). Malformed → REVIEW, as for any bad field.
        let proposed_item_lines = match obj.remove("proposed_item_lines") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(serde_json::from_value::<Vec<crate::engine::ItemLine>>(v)
                .map_err(|e| rej("request_unparseable", Verdict::Review, format!("proposed_item_lines: {e}")))?),
        };
        if let Some(k) = obj.keys().next() {
            return Err(rej("request_unparseable", Verdict::Review, format!("unknown field {k:?}")));
        }
        Ok(Request { proposal, evidence: ev, checker_raw, proposed_item_lines })
    }
}

fn short(h: &str) -> String {
    h.chars().take(12).collect()
}

impl Governor {
    /// Wire entry point with provenance checks. `now` is the (possibly eval) ledger clock,
    /// `wall_now` the real clock used for signature freshness.
    pub fn evaluate_signed_json(&mut self, raw: &str, now: i64, wall_now: i64, trust: &mut TrustVerifier) -> Decision {
        match trust.verify(raw, wall_now) {
            Ok(req) => self.evaluate(&req, now),
            Err(r) => self.reject(r.rule, r.verdict, r.detail, now),
        }
    }
}
