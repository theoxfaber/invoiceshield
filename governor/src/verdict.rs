//! Verdicts and escalate-only signals.
//!
//! `Severity` has no `Allow` variant on purpose: a model-produced signal can
//! only make a decision stricter. `merge` folds with `max`, so the result is
//! never less strict than the deterministic verdict it started from.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Allow,
    Review,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Review,
    Block,
}

impl From<Severity> for Verdict {
    fn from(s: Severity) -> Self {
        match s {
            Severity::Review => Verdict::Review,
            Severity::Block => Verdict::Block,
        }
    }
}

/// A signal from an AI-dependent layer (checker, segmenter, disagreement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub kind: String,
    pub severity: Severity,
    #[serde(default)]
    pub detail: String,
}

/// Escalate-only merge: `merge(base, s) >= base` for every input.
pub fn merge(base: Verdict, signals: &[Signal]) -> Verdict {
    signals
        .iter()
        .map(|s| Verdict::from(s.severity))
        .fold(base, std::cmp::max)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalList {
    signals: Vec<Signal>,
}

/// Parse a checker's raw output. Anything that is not exactly
/// `{"signals":[...]}` with known fields becomes one REVIEW signal.
/// This is the fail-closed path for garbage, malformed JSON, missing fields,
/// unknown severities ("allow"), timeouts (empty string) and provider errors.
pub fn parse_checker_signals(raw: &str) -> Vec<Signal> {
    const MAX_LEN: usize = 64 * 1024;
    if raw.len() > MAX_LEN {
        return vec![unparseable("checker output too large")];
    }
    match serde_json::from_str::<SignalList>(raw) {
        Ok(list) => list.signals,
        Err(e) => vec![unparseable(&format!("checker output rejected: {e}"))],
    }
}

fn unparseable(detail: &str) -> Signal {
    Signal {
        kind: "checker_unparseable".into(),
        severity: Severity::Review,
        detail: detail.chars().take(200).collect(),
    }
}
