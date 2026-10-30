//! Phase 4 security settings for `governor-server`, validated together at startup.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `GOVERNOR_IDENTITIES` / `_FILE` | none | JSON identities `[{name, roles, key_sha256}]` (auth.rs) |
//! | `GOVERNOR_API_KEY` / `_FILE` | none | legacy agent key (identity `agent`) |
//! | `GOVERNOR_OPERATOR_KEY` / `_FILE` | none | legacy operator key (identity `operator`, operator + approver) |
//! | `GOVERNOR_TLS_CERT_FILE` + `GOVERNOR_TLS_KEY_FILE` | plain HTTP | PEM chain + key; both or neither |
//! | `GOVERNOR_RATE_LIMIT_RPS` / `_BURST` | 100 / 200 (off in eval mode) | per identity |
//! | `GOVERNOR_RATE_LIMIT_IP_RPS` / `_IP_BURST` | 200 / 400 (off in eval mode) | per client IP |
//! | `GOVERNOR_AUTH_FAIL_PER_MIN` / `_BURST` | 30 / 10 (off in eval mode) | failed authentications per IP |
//! | `GOVERNOR_VENDOR_PHONES` | none (call-backs refused) | JSON file `{vendor_id: phone on file}` |
//! | `GOVERNOR_MASTER_CHANGE_COOLING_OFF_SECS` | 259200 (72 h) | second approval → new bank details usable |
//! | `GOVERNOR_STATE_KEY` / `_FILE` | none (warned) | 64 hex chars; HMAC key for applied bank changes in the DB |
use crate::auth::KeyRing;
use crate::master_change::DEFAULT_COOLING_OFF_SECS;
use crate::ratelimit::LimitsConfig;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct SecurityConfig {
    pub keys: KeyRing,
    pub limits: LimitsConfig,
    pub tls: Option<(PathBuf, PathBuf)>,
    pub phones: BTreeMap<String, String>,
    pub cooling_off_secs: i64,
    pub state_key: Option<[u8; 32]>,
    pub warnings: Vec<String>,
}

impl std::fmt::Debug for SecurityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityConfig")
            .field("keys", &self.keys)
            .field("limits", &self.limits)
            .field("tls", &self.tls)
            .field("phones", &self.phones.len())
            .field("cooling_off_secs", &self.cooling_off_secs)
            .finish()
    }
}

impl SecurityConfig {
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>, eval_clock: bool) -> Result<SecurityConfig, Vec<String>> {
        let mut errs = vec![];
        let keys = KeyRing::from_lookup(get).map_err(|e| errs.extend(e)).ok();
        let limits = LimitsConfig::from_lookup(get, eval_clock).map_err(|e| errs.extend(e)).ok();
        let nonempty = |k: &str| get(k).filter(|v| !v.is_empty());
        let tls = match (nonempty("GOVERNOR_TLS_CERT_FILE"), nonempty("GOVERNOR_TLS_KEY_FILE")) {
            (None, None) => None,
            (Some(c), Some(k)) => {
                // Load once now so a bad certificate stops startup with a clear message.
                match crate::tls::server_config(c.as_ref(), k.as_ref()) {
                    Ok(_) => Some((PathBuf::from(c), PathBuf::from(k))),
                    Err(e) => {
                        errs.push(e);
                        None
                    }
                }
            }
            _ => {
                errs.push("set both GOVERNOR_TLS_CERT_FILE and GOVERNOR_TLS_KEY_FILE, or neither".into());
                None
            }
        };
        let phones = match nonempty("GOVERNOR_VENDOR_PHONES") {
            None => BTreeMap::new(),
            Some(p) => match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<BTreeMap<String, String>>(&t).map_err(|e| e.to_string())) {
                Ok(m) => m,
                Err(e) => {
                    errs.push(format!("GOVERNOR_VENDOR_PHONES: {p:?} must be a JSON object {{vendor_id: phone}}: {e}"));
                    BTreeMap::new()
                }
            },
        };
        let cooling_off_secs = match nonempty("GOVERNOR_MASTER_CHANGE_COOLING_OFF_SECS") {
            None => DEFAULT_COOLING_OFF_SECS,
            Some(v) => match v.parse::<i64>() {
                Ok(n) if n >= 0 => n,
                _ => {
                    errs.push(format!("GOVERNOR_MASTER_CHANGE_COOLING_OFF_SECS must be a non-negative integer (got {v:?})"));
                    0
                }
            },
        };
        let mut warnings = vec![];
        let state_key = match crate::secrets::secret_from(get, "GOVERNOR_STATE_KEY") {
            Err(e) => {
                errs.push(e);
                None
            }
            Ok(None) => {
                if !eval_clock {
                    warnings.push("GOVERNOR_STATE_KEY not set: applied vendor bank changes in the DB are not MAC'd, so a DB-only attacker could forge one (docs/THREAT_MODEL.md)".into());
                }
                None
            }
            Ok(Some(s)) => match hex::decode(s.value.trim()) {
                Ok(b) if b.len() == 32 => {
                    if s.loose_permissions {
                        warnings.push("GOVERNOR_STATE_KEY_FILE is readable by group/others; chmod 600".into());
                    }
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&b);
                    Some(k)
                }
                _ => {
                    errs.push("GOVERNOR_STATE_KEY must be 64 hex chars (32 random bytes)".into());
                    None
                }
            },
        };
        if cooling_off_secs < 3600 && !eval_clock {
            warnings.push(format!("GOVERNOR_MASTER_CHANGE_COOLING_OFF_SECS={cooling_off_secs} is under an hour"));
        }
        if tls.is_none() && !eval_clock {
            warnings.push("TLS off: put the governor behind a TLS-terminating proxy on a private network, or set GOVERNOR_TLS_CERT_FILE/KEY_FILE".into());
        }
        match (errs.is_empty(), keys, limits) {
            (true, Some(keys), Some(limits)) => {
                warnings.extend(keys.warnings.iter().cloned());
                Ok(SecurityConfig { keys, limits, tls, phones, cooling_off_secs, state_key, warnings })
            }
            _ => Err(errs),
        }
    }

    pub fn from_env(eval_clock: bool) -> Result<SecurityConfig, Vec<String>> {
        Self::from_lookup(&|k| std::env::var(k).ok(), eval_clock)
    }
}
