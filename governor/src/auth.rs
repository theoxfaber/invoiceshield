//! Per-identity authentication and roles (Phase 4).
//!
//! Every caller presents `Authorization: Bearer <key>`. The server never stores the keys: it holds
//! the SHA-256 of each one next to an identity name and its roles, compares the hash of the
//! presented key against every entry in constant time, and hands the route the identity.
//! Keys are random 256-bit values (`governor new-key`), so a fast hash is enough: there is no
//! low-entropy password to brute-force offline, and a slow KDF would only add latency per request.
//!
//! Roles:
//! * `agent`: proposes payments (`POST /v1/actions`). Can never hold another role, so an agent
//!   identity can never approve anything, enter a vendor-master change, or confirm a call-back.
//! * `operator`: enters vendor bank-detail changes (the maker) and records call-backs.
//! * `approver`: resolves REVIEW decisions and approves vendor bank-detail changes (the checker).
//!   Maker-checker is enforced on identities: the identity that proposed a payment, or entered a
//!   master change, cannot approve it.
//!
//! Configuration (first match wins per identity name; names and key hashes must be unique):
//! * `GOVERNOR_IDENTITIES` / `GOVERNOR_IDENTITIES_FILE`: JSON array of
//!   `{"name": "...", "roles": ["agent"|"operator"|"approver", ...], "key_sha256": "<64 hex>"}`.
//! * Legacy single keys, kept so existing deployments and the eval harness keep working:
//!   `GOVERNOR_API_KEY[_FILE]` becomes identity `agent` (role agent);
//!   `GOVERNOR_OPERATOR_KEY[_FILE]` becomes identity `operator` (roles operator + approver: one
//!   person, as before). Real separation of duties needs `GOVERNOR_IDENTITIES`.
use crate::secrets::secret_from;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Agent,
    Operator,
    Approver,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Agent => "agent",
            Role::Operator => "operator",
            Role::Approver => "approver",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub roles: BTreeSet<Role>,
}

impl Identity {
    pub fn has(&self, r: Role) -> bool {
        self.roles.contains(&r)
    }
    pub fn has_any(&self, rs: &[Role]) -> bool {
        rs.iter().any(|r| self.has(*r))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentitySpec {
    name: String,
    roles: Vec<Role>,
    key_sha256: String,
}

struct Entry {
    id: Identity,
    hash: [u8; 32],
}

/// The set of identities the server accepts. Holds only key hashes.
pub struct KeyRing {
    entries: Vec<Entry>,
    /// Problems worth a startup warning (legacy keys, loose file permissions).
    pub warnings: Vec<String>,
}

impl std::fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = self.entries.iter().map(|e| (&e.id.name, &e.id.roles)).collect();
        f.debug_struct("KeyRing").field("identities", &names).finish()
    }
}

pub fn hash_key(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

pub fn hash_key_hex(key: &str) -> String {
    hex::encode(hash_key(key))
}

/// A new random API key: 32 bytes from the OS, hex encoded.
pub fn new_key() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).expect("OS randomness");
    hex::encode(b)
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
}

impl KeyRing {
    /// Build from the environment (or a lookup map in tests). Every problem is returned at once.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<KeyRing, Vec<String>> {
        let mut errs = vec![];
        let mut ring = KeyRing { entries: vec![], warnings: vec![] };
        match secret_from(get, "GOVERNOR_IDENTITIES") {
            Err(e) => errs.push(e),
            Ok(None) => {}
            Ok(Some(s)) => {
                if s.loose_permissions {
                    ring.warnings.push(format!("GOVERNOR_IDENTITIES_FILE {:?} is readable by group/others; chmod 600", s.from_file.unwrap_or_default()));
                }
                match serde_json::from_str::<Vec<IdentitySpec>>(&s.value) {
                    Err(e) => errs.push(format!("GOVERNOR_IDENTITIES: not a JSON array of {{name, roles, key_sha256}}: {e}")),
                    Ok(specs) => {
                        for sp in specs {
                            let h = match hex::decode(sp.key_sha256.trim()) {
                                Ok(b) if b.len() == 32 => {
                                    let mut a = [0u8; 32];
                                    a.copy_from_slice(&b);
                                    a
                                }
                                _ => {
                                    errs.push(format!("GOVERNOR_IDENTITIES: {}: key_sha256 must be 64 hex chars (the SHA-256 of the key, see `governor new-key`)", sp.name));
                                    continue;
                                }
                            };
                            if let Err(e) = ring.push(&sp.name, sp.roles.into_iter().collect(), h) {
                                errs.push(format!("GOVERNOR_IDENTITIES: {e}"));
                            }
                        }
                    }
                }
            }
        }
        for (var, name, roles) in [
            ("GOVERNOR_API_KEY", "agent", vec![Role::Agent]),
            ("GOVERNOR_OPERATOR_KEY", "operator", vec![Role::Operator, Role::Approver]),
        ] {
            match secret_from(get, var) {
                Err(e) => errs.push(e),
                Ok(None) => {}
                Ok(Some(s)) => {
                    if s.value.len() < 16 {
                        errs.push(format!("{var} must be at least 16 characters (got {})", s.value.len()));
                        continue;
                    }
                    if s.value.chars().any(char::is_whitespace) {
                        errs.push(format!("{var} must not contain whitespace"));
                        continue;
                    }
                    if s.loose_permissions {
                        ring.warnings.push(format!("{var}_FILE {:?} is readable by group/others; chmod 600", s.from_file.clone().unwrap_or_default()));
                    }
                    if let Err(e) = ring.push(name, roles.into_iter().collect(), hash_key(&s.value)) {
                        errs.push(format!("{var}: {e}"));
                    }
                }
            }
        }
        if ring.entries.iter().any(|e| e.id.name == "operator" && e.id.has(Role::Approver)) {
            ring.warnings.push("legacy GOVERNOR_OPERATOR_KEY in use: one key both enters and approves; configure GOVERNOR_IDENTITIES for separate operator/approver identities".into());
        }
        if !ring.entries.iter().any(|e| e.id.has(Role::Agent)) {
            errs.push("no agent identity: set GOVERNOR_API_KEY (>= 16 chars) or an identity with role \"agent\" in GOVERNOR_IDENTITIES".into());
        }
        if errs.is_empty() {
            Ok(ring)
        } else {
            Err(errs)
        }
    }

    pub fn from_env() -> Result<KeyRing, Vec<String>> {
        Self::from_lookup(&|k| std::env::var(k).ok())
    }

    fn push(&mut self, name: &str, roles: BTreeSet<Role>, hash: [u8; 32]) -> Result<(), String> {
        if !valid_name(name) {
            return Err(format!("identity name {name:?} must be 1-64 chars of [A-Za-z0-9._-@]"));
        }
        if roles.is_empty() {
            return Err(format!("{name}: at least one role is required"));
        }
        if roles.contains(&Role::Agent) && roles.len() > 1 {
            return Err(format!("{name}: the agent role cannot be combined with operator/approver (an agent must never approve)"));
        }
        if self.entries.iter().any(|e| e.id.name == name) {
            return Err(format!("{name}: duplicate identity name"));
        }
        if self.entries.iter().any(|e| bool::from(e.hash.ct_eq(&hash))) {
            return Err(format!("{name}: same key as another identity (every identity needs its own key)"));
        }
        self.entries.push(Entry { id: Identity { name: name.into(), roles }, hash });
        Ok(())
    }

    /// Identity for a presented bearer token, comparing against every entry in constant time.
    pub fn authenticate(&self, token: &str) -> Option<Identity> {
        if token.is_empty() {
            return None;
        }
        let h = hash_key(token);
        let mut found: Option<&Entry> = None;
        for e in &self.entries {
            if bool::from(e.hash.ct_eq(&h)) {
                found = Some(e);
            }
        }
        found.map(|e| e.id.clone())
    }

    pub fn identities(&self) -> impl Iterator<Item = &Identity> {
        self.entries.iter().map(|e| &e.id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The bearer token from an `Authorization` header value.
pub fn bearer(h: Option<&str>) -> &str {
    h.and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("")
}
