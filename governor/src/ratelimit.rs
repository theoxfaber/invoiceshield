//! Token-bucket rate limiting per key (Phase 4): per client IP before authentication, per
//! identity after it, and a stricter per-IP bucket for failed authentications (slows key
//! guessing). Over the limit the server answers 429 with `Retry-After`.
//!
//! In-memory and per process: two governor processes each allow the configured rate. The map is
//! bounded; when it is full, idle buckets (refilled to capacity) are dropped first, and if none
//! are idle the request is refused (fail closed) rather than letting the map grow without bound.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    /// Tokens added per second.
    pub per_sec: f64,
    /// Bucket size (largest burst).
    pub burst: f64,
}

impl Rate {
    /// `None` when `per_sec` is 0 (limiting off for this bucket).
    pub fn new(per_sec: f64, burst: f64) -> Option<Rate> {
        (per_sec > 0.0).then(|| Rate { per_sec, burst: burst.max(1.0) })
    }
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

pub struct Limiter {
    rate: Rate,
    max_keys: usize,
    buckets: Mutex<HashMap<String, Bucket>>,
}

/// Outcome of one check: allowed, or refused with seconds until a token is available.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Check {
    Allowed,
    Limited { retry_after_secs: u64 },
}

impl Limiter {
    pub fn new(rate: Rate, max_keys: usize) -> Self {
        Limiter { rate, max_keys: max_keys.max(1), buckets: Mutex::new(HashMap::new()) }
    }

    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Take one token for `key` at time `now`.
    pub fn check_at(&self, key: &str, now: Instant) -> Check {
        self.take(key, now, 1.0, true)
    }

    pub fn check(&self, key: &str) -> Check {
        self.check_at(key, Instant::now())
    }

    /// Peek: would a request be refused right now? (Does not take a token.)
    pub fn is_limited(&self, key: &str) -> Check {
        self.take(key, Instant::now(), 1.0, false)
    }

    fn take(&self, key: &str, now: Instant, cost: f64, consume: bool) -> Check {
        let mut m = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if !m.contains_key(key) {
            if !consume {
                return Check::Allowed;
            }
            if m.len() >= self.max_keys {
                let r = self.rate;
                m.retain(|_, b| b.tokens + now.saturating_duration_since(b.at).as_secs_f64() * r.per_sec < r.burst);
                if m.len() >= self.max_keys {
                    return Check::Limited { retry_after_secs: 1 };
                }
            }
            m.insert(key.to_string(), Bucket { tokens: self.rate.burst, at: now });
        }
        let b = m.get_mut(key).expect("inserted");
        let elapsed = now.saturating_duration_since(b.at).as_secs_f64();
        let tokens = (b.tokens + elapsed * self.rate.per_sec).min(self.rate.burst);
        if tokens >= cost {
            if consume {
                b.tokens = tokens - cost;
                b.at = now;
            }
            Check::Allowed
        } else {
            if consume {
                b.tokens = tokens;
                b.at = now;
            }
            let wait = ((cost - tokens) / self.rate.per_sec).ceil().max(1.0);
            Check::Limited { retry_after_secs: wait as u64 }
        }
    }
}

/// The three limiters the server uses; each is optional (off when its rate is 0).
pub struct Limits {
    pub per_ip: Option<Limiter>,
    pub per_identity: Option<Limiter>,
    pub auth_fail_per_ip: Option<Limiter>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitsConfig {
    pub ip: Option<Rate>,
    pub identity: Option<Rate>,
    pub auth_fail: Option<Rate>,
}

impl LimitsConfig {
    /// Read `GOVERNOR_RATE_LIMIT_*` from a lookup. Defaults (production): 100 req/s burst 200 per
    /// identity, 200 req/s burst 400 per IP, 30 failed authentications per minute per IP (burst
    /// 10). In eval mode (`eval_clock`) the defaults are off so the replay harness and load test
    /// measure the gate, not the limiter; explicit values still apply. 0 turns a limiter off.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>, eval_clock: bool) -> Result<LimitsConfig, Vec<String>> {
        let mut errs = vec![];
        let mut num = |k: &str, d: f64| -> f64 {
            match get(k).filter(|v| !v.is_empty()) {
                None => d,
                Some(v) => match v.parse::<f64>() {
                    Ok(n) if n.is_finite() && n >= 0.0 => n,
                    _ => {
                        errs.push(format!("{k} must be a non-negative number (got {v:?})"));
                        d
                    }
                },
            }
        };
        let off = |d: f64| if eval_clock { 0.0 } else { d };
        let id_rps = num("GOVERNOR_RATE_LIMIT_RPS", off(100.0));
        let id_burst = num("GOVERNOR_RATE_LIMIT_BURST", (id_rps * 2.0).max(1.0));
        let ip_rps = num("GOVERNOR_RATE_LIMIT_IP_RPS", off(200.0));
        let ip_burst = num("GOVERNOR_RATE_LIMIT_IP_BURST", (ip_rps * 2.0).max(1.0));
        let fail_pm = num("GOVERNOR_AUTH_FAIL_PER_MIN", off(30.0));
        let fail_burst = num("GOVERNOR_AUTH_FAIL_BURST", 10.0);
        if errs.is_empty() {
            Ok(LimitsConfig {
                identity: Rate::new(id_rps, id_burst),
                ip: Rate::new(ip_rps, ip_burst),
                auth_fail: Rate::new(fail_pm / 60.0, fail_burst),
            })
        } else {
            Err(errs)
        }
    }

    pub fn build(&self) -> Limits {
        const MAX_KEYS: usize = 100_000;
        Limits {
            per_ip: self.ip.map(|r| Limiter::new(r, MAX_KEYS)),
            per_identity: self.identity.map(|r| Limiter::new(r, MAX_KEYS)),
            auth_fail_per_ip: self.auth_fail.map(|r| Limiter::new(r, MAX_KEYS)),
        }
    }
}
