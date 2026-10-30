//! Startup configuration for `governor-server`, validated before anything is bound.
//!
//! Fail fast: every problem is collected and reported together, then the process exits
//! with status 2. A governor that starts with a half-valid configuration is worse than
//! one that does not start, because the agent would see it as healthy.
//!
//! | Variable | Required | Rule |
//! |---|---|---|
//! | `GOVERNOR_API_KEY` | yes | >= 16 chars, no whitespace |
//! | `GOVERNOR_OPERATOR_KEY` | no | if set: >= 16 chars, no whitespace, differs from the API key |
//! | `GOVERNOR_MASTER` | no (default `../dataset/fixtures/master.json`) | readable file that parses as a vendor master |
//! | `GOVERNOR_PORT` | no (default 8080) | integer 1-65535 |
//! | `GOVERNOR_BIND` | no (default `127.0.0.1`) | an IP address; `0.0.0.0` only inside a container |
//! | `GOVERNOR_EVAL_CLOCK` | no | unset, `0` or `1` |
//! | `GOVERNOR_LOG_FORMAT` | no (default `json`) | `json` or `pretty` |
//! | `GOVERNOR_METRICS_TOKEN` | no | if set: >= 16 chars |
//! | `GOVERNOR_ANCHOR_INTERVAL_SECS` | no (default 60) | positive integer (Phase 3 anchoring) |
//! | `GOVERNOR_ANCHOR_URL` | no | if set: an `http://` URL (put a TLS relay in front for https) |
//! | `GOVERNOR_EXTRACTOR_PUBKEY` | yes (unless dev unsigned) | comma-separated Ed25519 public keys, each 64 hex chars or base64 of 32 bytes (Phase 2) |
//! | `GOVERNOR_EVIDENCE_MAX_AGE_SECS` | no (default 300) | integer 1-86400 |
//! | `GOVERNOR_EVIDENCE_MAX_SKEW_SECS` | no (default 30) | integer 0-3600 |
//! | `GOVERNOR_DEV_UNSIGNED_EVIDENCE` | no | unset, `0` or `1`; `1` (tests only) requires `GOVERNOR_EVAL_CLOCK=1` |
//!
//! `GOVERNOR_DB` and the anchor key are validated where they are opened (server.rs / anchor.rs),
//! also with exit status 2.
use crate::master::Master;
use crate::trust::TrustConfig;
use std::net::{IpAddr, SocketAddr};

/// Validated settings. Secrets are kept out of `Debug`.
pub struct Config {
    pub api_key: String,
    pub operator_key: Option<String>,
    pub master_path: String,
    pub master: Master,
    pub port: u16,
    pub bind: IpAddr,
    pub eval_clock: bool,
    /// Phase 2 evidence provenance: extractor public keys, freshness window, dev flag.
    pub trust: TrustConfig,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("api_key", &"<redacted>")
            .field(
                "operator_key",
                &self.operator_key.as_ref().map(|_| "<redacted>"),
            )
            .field("master_path", &self.master_path)
            .field("port", &self.port)
            .field("bind", &self.bind)
            .field("eval_clock", &self.eval_clock)
            .field("extractor_keys", &self.trust.keys.len())
            .field("evidence_max_age_secs", &self.trust.max_age_secs)
            .field("evidence_max_skew_secs", &self.trust.max_skew_secs)
            .field("dev_unsigned_evidence", &self.trust.dev_unsigned)
            .finish()
    }
}

impl Config {
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
    }

    /// Read and validate from the process environment.
    pub fn from_env() -> Result<Config, Vec<String>> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Like `from_env`, but prints every error and exits with status 2.
    pub fn from_env_or_exit() -> Config {
        match Self::from_env() {
            Ok(c) => c,
            Err(errs) => {
                for e in &errs {
                    eprintln!("config error: {e}");
                    tracing::error!(event = "config_invalid", error = %e);
                }
                eprintln!(
                    "governor-server: refusing to start ({} configuration error(s))",
                    errs.len()
                );
                std::process::exit(2);
            }
        }
    }

    /// Validation over any key lookup (tests pass a map instead of the real environment).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Config, Vec<String>> {
        let mut errs = Vec::new();

        let api_key = get("GOVERNOR_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            errs.push("GOVERNOR_API_KEY is required".into());
        } else if api_key.len() < 16 {
            errs.push(format!(
                "GOVERNOR_API_KEY must be at least 16 characters (got {})",
                api_key.len()
            ));
        } else if api_key.chars().any(char::is_whitespace) {
            errs.push("GOVERNOR_API_KEY must not contain whitespace".into());
        }

        let operator_key = get("GOVERNOR_OPERATOR_KEY").filter(|s| !s.is_empty());
        if let Some(op) = &operator_key {
            if op.len() < 16 {
                errs.push(format!(
                    "GOVERNOR_OPERATOR_KEY must be at least 16 characters when set (got {})",
                    op.len()
                ));
            } else if op.chars().any(char::is_whitespace) {
                errs.push("GOVERNOR_OPERATOR_KEY must not contain whitespace".into());
            }
            if *op == api_key {
                errs.push("GOVERNOR_OPERATOR_KEY must differ from GOVERNOR_API_KEY (the agent must not hold the operator key)".into());
            }
        }

        let port = match get("GOVERNOR_PORT").filter(|s| !s.is_empty()) {
            None => 8080,
            Some(p) => match p.parse::<u16>() {
                Ok(n) if n > 0 => n,
                _ => {
                    errs.push(format!(
                        "GOVERNOR_PORT must be an integer 1-65535 (got {p:?})"
                    ));
                    0
                }
            },
        };

        let bind = match get("GOVERNOR_BIND").filter(|s| !s.is_empty()) {
            None => IpAddr::from([127, 0, 0, 1]),
            Some(b) => b.parse::<IpAddr>().unwrap_or_else(|_| {
                errs.push(format!(
                    "GOVERNOR_BIND must be an IP address such as 127.0.0.1 (got {b:?})"
                ));
                IpAddr::from([127, 0, 0, 1])
            }),
        };

        let eval_clock = match get("GOVERNOR_EVAL_CLOCK").as_deref() {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                errs.push(format!(
                    "GOVERNOR_EVAL_CLOCK must be 0 or 1 (got {other:?})"
                ));
                false
            }
        };

        match get("GOVERNOR_LOG_FORMAT").as_deref() {
            None | Some("") | Some("json") | Some("pretty") => {}
            Some(other) => errs.push(format!(
                "GOVERNOR_LOG_FORMAT must be json or pretty (got {other:?})"
            )),
        }

        if let Some(t) = get("GOVERNOR_METRICS_TOKEN") {
            if t.len() < 16 {
                errs.push(format!(
                    "GOVERNOR_METRICS_TOKEN must be at least 16 characters when set (got {})",
                    t.len()
                ));
            }
        }

        if let Some(v) = get("GOVERNOR_ANCHOR_INTERVAL_SECS").filter(|s| !s.is_empty()) {
            if !matches!(v.parse::<u64>(), Ok(n) if n > 0) {
                errs.push(format!(
                    "GOVERNOR_ANCHOR_INTERVAL_SECS must be a positive integer (got {v:?})"
                ));
            }
        }
        if let Some(u) = get("GOVERNOR_ANCHOR_URL").filter(|s| !s.is_empty()) {
            if !u.starts_with("http://") {
                errs.push(format!(
                    "GOVERNOR_ANCHOR_URL must be an http:// URL (got {u:?}); put a TLS relay in front for https"
                ));
            }
        }

        // Phase 2: evidence provenance.
        let dev_unsigned = match get("GOVERNOR_DEV_UNSIGNED_EVIDENCE").as_deref() {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                errs.push(format!(
                    "GOVERNOR_DEV_UNSIGNED_EVIDENCE must be 0 or 1 (got {other:?})"
                ));
                false
            }
        };
        if dev_unsigned && !eval_clock {
            errs.push("GOVERNOR_DEV_UNSIGNED_EVIDENCE=1 is for tests only and requires GOVERNOR_EVAL_CLOCK=1".into());
        }
        let extractor_keys =
            match TrustConfig::parse_keys(&get("GOVERNOR_EXTRACTOR_PUBKEY").unwrap_or_default()) {
                Ok(k) => k,
                Err(e) => {
                    errs.push(format!("GOVERNOR_EXTRACTOR_PUBKEY invalid: {e}"));
                    vec![]
                }
            };
        if extractor_keys.is_empty()
            && !dev_unsigned
            && !errs
                .iter()
                .any(|e| e.starts_with("GOVERNOR_EXTRACTOR_PUBKEY"))
        {
            errs.push("GOVERNOR_EXTRACTOR_PUBKEY is required (the extraction service's Ed25519 public key)".into());
        }
        let mut int_var = |k: &str, default: i64, lo: i64, hi: i64| -> i64 {
            match get(k).filter(|s| !s.is_empty()) {
                None => default,
                Some(v) => match v.parse::<i64>() {
                    Ok(n) if (lo..=hi).contains(&n) => n,
                    _ => {
                        errs.push(format!("{k} must be an integer {lo}-{hi} (got {v:?})"));
                        default
                    }
                },
            }
        };
        let max_age_secs = int_var("GOVERNOR_EVIDENCE_MAX_AGE_SECS", 300, 1, 86_400);
        let max_skew_secs = int_var("GOVERNOR_EVIDENCE_MAX_SKEW_SECS", 30, 0, 3_600);
        let mut trust = TrustConfig::new(extractor_keys);
        trust.dev_unsigned = dev_unsigned;
        trust.max_age_secs = max_age_secs;
        trust.max_skew_secs = max_skew_secs;

        let master_path = get("GOVERNOR_MASTER")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "../dataset/fixtures/master.json".into());
        let master = match std::fs::read_to_string(&master_path) {
            Err(e) => {
                errs.push(format!("GOVERNOR_MASTER: cannot read {master_path:?}: {e}"));
                None
            }
            Ok(text) => match Master::from_json(&text) {
                Ok(m) => Some(m),
                Err(e) => {
                    errs.push(format!(
                        "GOVERNOR_MASTER: {master_path:?} is not a valid vendor master: {e}"
                    ));
                    None
                }
            },
        };

        match (errs.is_empty(), master) {
            (true, Some(master)) => Ok(Config {
                api_key,
                operator_key,
                master_path,
                master,
                port,
                bind,
                eval_clock,
                trust,
            }),
            _ => Err(errs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn master_path() -> String {
        format!(
            "{}/../dataset/fixtures/master.json",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    /// RFC 8032 test-vector 1 public key (no private key in the repo).
    const PK: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, Vec<String>> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn valid_minimal_config() {
        let mp = master_path();
        let c = cfg(&[
            ("GOVERNOR_API_KEY", "0123456789abcdef"),
            ("GOVERNOR_EXTRACTOR_PUBKEY", PK),
            ("GOVERNOR_MASTER", &mp),
        ])
        .expect("valid");
        assert_eq!(c.trust.keys.len(), 1);
        assert_eq!(c.trust.max_age_secs, 300);
        assert_eq!(c.port, 8080);
        assert_eq!(c.addr().to_string(), "127.0.0.1:8080");
        assert!(!c.eval_clock);
        assert!(
            !format!("{c:?}").contains("0123456789abcdef"),
            "Debug must redact secrets"
        );
    }

    #[test]
    fn collects_every_error() {
        let errs = cfg(&[
            ("GOVERNOR_API_KEY", "short"),
            ("GOVERNOR_OPERATOR_KEY", "short"),
            ("GOVERNOR_PORT", "99999"),
            ("GOVERNOR_BIND", "localhost"),
            ("GOVERNOR_EVAL_CLOCK", "yes"),
            ("GOVERNOR_LOG_FORMAT", "xml"),
            ("GOVERNOR_MASTER", "/nonexistent/master.json"),
            ("GOVERNOR_ANCHOR_INTERVAL_SECS", "0"),
            ("GOVERNOR_ANCHOR_URL", "https://x"),
            ("GOVERNOR_EXTRACTOR_PUBKEY", "not-a-key"),
            ("GOVERNOR_EVIDENCE_MAX_AGE_SECS", "0"),
            ("GOVERNOR_EVIDENCE_MAX_SKEW_SECS", "-1"),
            ("GOVERNOR_DEV_UNSIGNED_EVIDENCE", "maybe"),
        ])
        .unwrap_err();
        assert_eq!(errs.len(), 14, "{errs:#?}");
    }

    #[test]
    fn operator_key_must_differ_from_api_key() {
        let mp = master_path();
        let k = "0123456789abcdef";
        let errs = cfg(&[
            ("GOVERNOR_API_KEY", k),
            ("GOVERNOR_OPERATOR_KEY", k),
            ("GOVERNOR_EXTRACTOR_PUBKEY", PK),
            ("GOVERNOR_MASTER", &mp),
        ])
        .unwrap_err();
        assert!(errs[0].contains("must differ"), "{errs:?}");
    }

    #[test]
    fn missing_api_key_is_reported() {
        let mp = master_path();
        let errs = cfg(&[("GOVERNOR_EXTRACTOR_PUBKEY", PK), ("GOVERNOR_MASTER", &mp)]).unwrap_err();
        assert_eq!(errs, vec!["GOVERNOR_API_KEY is required".to_string()]);
    }

    #[test]
    fn extractor_pubkey_is_required_unless_dev_unsigned_with_eval_clock() {
        let mp = master_path();
        let k = "0123456789abcdef";
        let errs = cfg(&[("GOVERNOR_API_KEY", k), ("GOVERNOR_MASTER", &mp)]).unwrap_err();
        assert!(
            errs[0].starts_with("GOVERNOR_EXTRACTOR_PUBKEY is required"),
            "{errs:?}"
        );
        // Dev unsigned without the eval clock is refused.
        let errs = cfg(&[
            ("GOVERNOR_API_KEY", k),
            ("GOVERNOR_MASTER", &mp),
            ("GOVERNOR_DEV_UNSIGNED_EVIDENCE", "1"),
        ])
        .unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("requires GOVERNOR_EVAL_CLOCK=1")),
            "{errs:?}"
        );
        // Dev unsigned + eval clock starts without a key.
        let c = cfg(&[
            ("GOVERNOR_API_KEY", k),
            ("GOVERNOR_MASTER", &mp),
            ("GOVERNOR_DEV_UNSIGNED_EVIDENCE", "1"),
            ("GOVERNOR_EVAL_CLOCK", "1"),
        ])
        .expect("dev config");
        assert!(c.trust.dev_unsigned && c.trust.keys.is_empty());
    }
}
