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
//! | sandbox had no network namespace        | `sandbox_network_not_isolated` | REVIEW (unless `allow_no_netns`) |
//!
//! Checker binding (post-review hardening, docs/REVIEW_FIXES.md): the quarantined checker's
//! output is accepted only as `signed_checker`, an envelope signed by a *checker* key (never an
//! extractor key) over `{v, kind:"checker", key_id, doc_sha256, evidence_nonce, issued_at, raw}`.
//! It must name the same document and the same single-use evidence nonce, so it cannot be
//! forged, dropped silently, or replayed onto another document. Every failure becomes one REVIEW
//! signal (never PAY): `checker_missing`, `checker_unsigned`, `checker_signature_invalid`,
//! `checker_payload_invalid`, `checker_document_mismatch`, `checker_nonce_mismatch`,
//! `checker_stale`. An unsigned `checker_raw` is read only when `checker_unsigned_ok` is set
//! (GOVERNOR_CHECKER_UNSIGNED_OK=1, warned at startup).
//!
//! BLOCK is used where the only explanation is tampering (a signature that does not verify,
//! evidence for a different document, a reused nonce). REVIEW is used where an honest fault
//! is plausible (missing signature from a misconfigured client, clock skew, a slow queue,
//! a document the sandbox could not parse). Either way nothing is paid.
//!
//! Freshness always uses the real wall clock, never the eval clock header.
//! A raw `evidence` field sent next to `signed_evidence` is ignored: only signed values are read.
use crate::engine::{Evidence, Governor, Request};
use crate::verdict::{parse_checker_signals, Severity, Signal, Verdict};
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
    /// Checker key id → public key (GOVERNOR_CHECKER_PUBKEY). Separate from the extractor keys.
    pub checker_keys: HashMap<String, VerifyingKey>,
    /// Read an unsigned `checker_raw` (old wire format). Default false: fail closed to REVIEW.
    pub checker_unsigned_ok: bool,
    /// Accept evidence whose sandbox reports `network_namespace: false` (e.g. a container without
    /// user namespaces). Default false: fail closed to REVIEW. GOVERNOR_ALLOW_NO_NETNS=1.
    pub allow_no_netns: bool,
}

impl TrustConfig {
    pub fn new(keys: Vec<VerifyingKey>) -> Self {
        TrustConfig {
            keys: keys.into_iter().map(|k| (key_id(&k), k)).collect(),
            max_age_secs: 300,
            max_skew_secs: 30,
            max_nonces: 100_000,
            dev_unsigned: false,
            checker_keys: HashMap::new(),
            checker_unsigned_ok: false,
            allow_no_netns: false,
        }
    }

    /// Builder: the checker's public keys.
    pub fn with_checker_keys(mut self, keys: Vec<VerifyingKey>) -> Self {
        self.checker_keys = keys.into_iter().map(|k| (key_id(&k), k)).collect();
        self
    }

    /// Parse a comma-separated list of public keys, each 64 hex chars or base64 of 32 bytes.
    pub fn parse_keys(s: &str) -> Result<Vec<VerifyingKey>, String> {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(|x| {
                let raw = if x.len() == 64 && x.chars().all(|c| c.is_ascii_hexdigit()) {
                    hex::decode(x).map_err(|e| e.to_string())?
                } else {
                    base64::engine::general_purpose::STANDARD
                        .decode(x)
                        .map_err(|e| e.to_string())?
                };
                let arr: [u8; 32] = raw
                    .try_into()
                    .map_err(|_| "public key must be 32 bytes".to_string())?;
                VerifyingKey::from_bytes(&arr).map_err(|e| e.to_string())
            })
            .collect()
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

/// What the checker signs. Canonical JSON (sorted keys, no whitespace) of this object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckerPayload {
    pub v: u32,
    pub kind: String,
    pub key_id: String,
    pub doc_sha256: String,
    pub evidence_nonce: String,
    pub issued_at: i64,
    /// The checker's raw output, parsed fail-closed by `parse_checker_signals`.
    pub raw: String,
}

/// A signed nonce the caller must record as used (the durable store persists it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedNonce {
    pub nonce: String,
    pub issued_at: i64,
}

fn checker_signal(kind: &str, detail: impl Into<String>) -> Vec<Signal> {
    vec![Signal {
        kind: kind.into(),
        severity: Severity::Review,
        detail: detail.into().chars().take(200).collect(),
    }]
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub rule: &'static str,
    pub verdict: Verdict,
    pub detail: String,
}

fn rej(rule: &'static str, verdict: Verdict, detail: impl Into<String>) -> Rejection {
    Rejection {
        rule,
        verdict,
        detail: detail.into(),
    }
}

pub struct TrustVerifier {
    pub cfg: TrustConfig,
    seen: HashMap<String, i64>,
    order: VecDeque<(i64, String)>,
}

impl TrustVerifier {
    pub fn new(cfg: TrustConfig) -> Self {
        TrustVerifier {
            cfg,
            seen: HashMap::new(),
            order: VecDeque::new(),
        }
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
        self.verify_accept(raw, wall_now).map(|(r, _)| r)
    }

    /// `verify`, also returning the evidence nonce it accepted (None on the dev unsigned path) so
    /// the durable store can persist it and a restart cannot reopen a replay window.
    pub fn verify_accept(
        &mut self,
        raw: &str,
        wall_now: i64,
    ) -> Result<(Request, Option<AcceptedNonce>), Rejection> {
        let mut body: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| rej("request_unparseable", Verdict::Review, e.to_string()))?;
        let obj = body.as_object_mut().ok_or_else(|| {
            rej(
                "request_unparseable",
                Verdict::Review,
                "body is not an object",
            )
        })?;
        let claimed_hash = obj
            .get_mut("proposal")
            .and_then(|p| p.as_object_mut())
            .and_then(|p| p.remove("document_sha256"));
        let Some(env) = obj.remove("signed_evidence") else {
            if self.cfg.dev_unsigned {
                // DEV/TEST ONLY: old path, evidence as sent by the caller.
                return serde_json::from_value::<Request>(body)
                    .map(|r| (r, None))
                    .map_err(|e| rej("request_unparseable", Verdict::Review, e.to_string()));
            }
            return Err(rej("evidence_unsigned", Verdict::Review,
                "no signed evidence: the governor reads evidence only from the extraction service, so a human must check the document"));
        };
        obj.remove("evidence"); // never read caller-supplied evidence next to a signed envelope

        let bad_sig = |d: &str| rej("evidence_signature_invalid", Verdict::Block, d.to_string());
        let env: Envelope = serde_json::from_value(env)
            .map_err(|e| bad_sig(&format!("malformed envelope: {e}")))?;
        let pre: serde_json::Value =
            serde_json::from_str(&env.payload).map_err(|_| bad_sig("payload is not JSON"))?;
        let kid = pre.get("key_id").and_then(|v| v.as_str()).unwrap_or("");
        let key = self
            .cfg
            .keys
            .get(kid)
            .ok_or_else(|| bad_sig(&format!("unknown extractor key id {kid:?}")))?;
        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(env.sig.trim())
            .map_err(|_| bad_sig("signature is not base64"))?;
        let sig_arr: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| bad_sig("signature must be 64 bytes"))?;
        key.verify_strict(env.payload.as_bytes(), &Signature::from_bytes(&sig_arr))
            .map_err(|_| bad_sig("Ed25519 signature does not verify against the extractor key"))?;

        // From here on the payload is the extractor's own words.
        let p: SignedPayload = serde_json::from_value(pre)
            .map_err(|e| rej("evidence_payload_invalid", Verdict::Review, e.to_string()))?;
        if p.v != PAYLOAD_VERSION {
            return Err(rej(
                "evidence_payload_invalid",
                Verdict::Review,
                format!("payload version {} not supported", p.v),
            ));
        }
        if p.issued_at < wall_now - self.cfg.max_age_secs {
            return Err(rej(
                "evidence_stale",
                Verdict::Review,
                format!(
                    "evidence issued {}s ago (max {}s)",
                    wall_now - p.issued_at,
                    self.cfg.max_age_secs
                ),
            ));
        }
        if p.issued_at > wall_now + self.cfg.max_skew_secs {
            return Err(rej(
                "evidence_from_future",
                Verdict::Review,
                format!(
                    "evidence issued {}s in the future (max skew {}s)",
                    p.issued_at - wall_now,
                    self.cfg.max_skew_secs
                ),
            ));
        }
        if p.nonce.len() < 32
            || p.nonce.len() > 128
            || !p.nonce.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(rej(
                "evidence_payload_invalid",
                Verdict::Review,
                "nonce must be 32-128 hex chars",
            ));
        }
        self.prune(wall_now);
        if self.seen.contains_key(&p.nonce) {
            return Err(rej(
                "evidence_replayed",
                Verdict::Block,
                format!("nonce {} was already used", &p.nonce[..16]),
            ));
        }
        if self.seen.len() >= self.cfg.max_nonces {
            return Err(rej(
                "evidence_nonce_store_full",
                Verdict::Review,
                "too many live nonces; refusing to decide",
            ));
        }
        self.seen.insert(p.nonce.clone(), p.issued_at);
        self.order.push_back((p.issued_at, p.nonce.clone()));
        // keep `order` sorted enough for pruning: issued_at values arrive roughly in order;
        // an out-of-order one only lives a little longer than needed (still bounded by max_nonces).

        let claimed = claimed_hash
            .as_ref()
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase());
        match claimed {
            None => {
                return Err(rej(
                    "document_hash_missing",
                    Verdict::Review,
                    "proposal does not name the document (document_sha256)",
                ))
            }
            Some(h) if h != p.doc_sha256.to_ascii_lowercase() => {
                return Err(rej(
                    "document_hash_mismatch",
                    Verdict::Block,
                    format!(
                        "proposal is for document {} but the signed evidence is for {}",
                        short(&h),
                        short(&p.doc_sha256)
                    ),
                ));
            }
            _ => {}
        }
        if p.status != "ok" {
            return Err(rej(
                "extraction_failed",
                Verdict::Review,
                format!(
                    "extractor could not read the document safely: {}",
                    p.failure.as_deref().unwrap_or("unknown")
                ),
            ));
        }
        // Finding 2: the extractor signs whether its worker really ran in a fresh network
        // namespace. Without one, only Python-level socket patching stood between a hostile PDF
        // parser and the network, so a human checks the document unless the operator explicitly
        // accepted that (GOVERNOR_ALLOW_NO_NETNS=1).
        let netns = p
            .sandbox
            .as_ref()
            .and_then(|s| s.get("network_namespace"))
            .and_then(|v| v.as_bool());
        if netns != Some(true) && !self.cfg.allow_no_netns {
            return Err(rej(
                "sandbox_network_not_isolated",
                Verdict::Review,
                format!(
                    "the extraction sandbox reported network_namespace={}; the document was parsed without network isolation, so a human must check it (set GOVERNOR_ALLOW_NO_NETNS=1 only if that is accepted)",
                    netns.map(|b| b.to_string()).unwrap_or_else(|| "missing".into())
                ),
            ));
        }
        let ev: Evidence = serde_json::from_value(p.evidence.unwrap_or(serde_json::Value::Null))
            .map_err(|e| {
                rej(
                    "evidence_payload_invalid",
                    Verdict::Review,
                    format!("signed evidence fields: {e}"),
                )
            })?;
        let proposal = obj.remove("proposal").unwrap_or(serde_json::Value::Null);
        let checker_raw = match obj.remove("checker_raw") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => Some(s),
            Some(_) => {
                return Err(rej(
                    "request_unparseable",
                    Verdict::Review,
                    "checker_raw must be a string",
                ))
            }
        };
        let signed_checker = obj.remove("signed_checker");
        let checker_signals = Some(self.checker_signals(
            signed_checker,
            checker_raw.as_deref(),
            &p.doc_sha256,
            &p.nonce,
            wall_now,
        ));
        // Model-proposed item lines are untrusted input like the proposal; the engine verifies them
        // against the signed text layer (lines.rs). Malformed → REVIEW, as for any bad field.
        let proposed_item_lines = match obj.remove("proposed_item_lines") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(
                serde_json::from_value::<Vec<crate::engine::ItemLine>>(v).map_err(|e| {
                    rej(
                        "request_unparseable",
                        Verdict::Review,
                        format!("proposed_item_lines: {e}"),
                    )
                })?,
            ),
        };
        if let Some(k) = obj.keys().next() {
            return Err(rej(
                "request_unparseable",
                Verdict::Review,
                format!("unknown field {k:?}"),
            ));
        }
        Ok((
            Request {
                proposal,
                evidence: ev,
                checker_raw: None,
                proposed_item_lines,
                checker_signals,
            },
            Some(AcceptedNonce {
                nonce: p.nonce,
                issued_at: p.issued_at,
            }),
        ))
    }

    /// Finding 1: the checker's signals, only from a checker-signed envelope bound to this
    /// document and this evidence nonce. Any failure is one REVIEW signal; nothing here can
    /// lower a verdict.
    fn checker_signals(
        &self,
        signed: Option<serde_json::Value>,
        unsigned_raw: Option<&str>,
        doc_sha256: &str,
        evidence_nonce: &str,
        wall_now: i64,
    ) -> Vec<Signal> {
        let env = match signed {
            None | Some(serde_json::Value::Null) => {
                return match unsigned_raw {
                    Some(r) if self.cfg.checker_unsigned_ok => parse_checker_signals(r),
                    Some(_) => checker_signal(
                        "checker_unsigned",
                        "checker output was not signed by the checker; ignored (a compromised agent could have edited it)",
                    ),
                    None => checker_signal("checker_missing", "no checker report"),
                };
            }
            Some(v) => v,
        };
        let bad = |d: &str| checker_signal("checker_signature_invalid", d);
        let env: Envelope = match serde_json::from_value(env) {
            Ok(e) => e,
            Err(e) => return bad(&format!("malformed checker envelope: {e}")),
        };
        let pre: serde_json::Value = match serde_json::from_str(&env.payload) {
            Ok(v) => v,
            Err(_) => return bad("checker payload is not JSON"),
        };
        let kid = pre.get("key_id").and_then(|v| v.as_str()).unwrap_or("");
        let Some(key) = self.cfg.checker_keys.get(kid) else {
            return bad(&format!("unknown checker key id {kid:?}"));
        };
        let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(env.sig.trim()) else {
            return bad("checker signature is not base64");
        };
        let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes) else {
            return bad("checker signature must be 64 bytes");
        };
        if key
            .verify_strict(env.payload.as_bytes(), &Signature::from_bytes(&sig_arr))
            .is_err()
        {
            return bad("Ed25519 signature does not verify against the checker key");
        }
        let c: CheckerPayload = match serde_json::from_value(pre) {
            Ok(c) => c,
            Err(e) => return checker_signal("checker_payload_invalid", e.to_string()),
        };
        if c.v != 1 || c.kind != "checker" {
            return checker_signal(
                "checker_payload_invalid",
                format!("checker payload v={} kind={:?} not supported", c.v, c.kind),
            );
        }
        if !c.doc_sha256.eq_ignore_ascii_case(doc_sha256) {
            return checker_signal(
                "checker_document_mismatch",
                format!(
                    "checker report is for document {} but the evidence is for {}",
                    short(&c.doc_sha256),
                    short(doc_sha256)
                ),
            );
        }
        if c.evidence_nonce != evidence_nonce {
            return checker_signal(
                "checker_nonce_mismatch",
                "checker report is bound to other evidence (replayed or reused)",
            );
        }
        if c.issued_at < wall_now - self.cfg.max_age_secs
            || c.issued_at > wall_now + self.cfg.max_skew_secs
        {
            return checker_signal(
                "checker_stale",
                format!("checker report issued_at {} is outside the freshness window", c.issued_at),
            );
        }
        parse_checker_signals(&c.raw)
    }
}

fn short(h: &str) -> String {
    h.chars().take(12).collect()
}

impl Governor {
    /// Wire entry point with provenance checks. `now` is the (possibly eval) ledger clock,
    /// `wall_now` the real clock used for signature freshness.
    pub fn evaluate_signed_json(
        &mut self,
        raw: &str,
        now: i64,
        wall_now: i64,
        trust: &mut TrustVerifier,
    ) -> Decision {
        match trust.verify(raw, wall_now) {
            Ok(req) => self.evaluate(&req, now),
            Err(r) => self.reject(r.rule, r.verdict, r.detail, now),
        }
    }
}
