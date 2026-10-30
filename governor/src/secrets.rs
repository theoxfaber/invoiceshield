//! Secrets from the environment or from a file (Phase 4).
//!
//! Every secret `X` can be given as `X` (the value) or `X_FILE` (a path whose contents are the
//! value; one trailing newline is stripped), the convention Docker/Kubernetes secrets use. Giving
//! both is a configuration error, not a silent precedence rule. A secret file readable by group or
//! others is reported (the caller decides whether that is fatal; `governor-server` warns).
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secret {
    pub value: String,
    /// `Some(path)` when the value came from `X_FILE`.
    pub from_file: Option<String>,
    /// Set when the file's mode lets group/others read it.
    pub loose_permissions: bool,
}

/// Look up `name` / `name_FILE` through `get` (the real environment, or a map in tests).
pub fn secret_from(get: &dyn Fn(&str) -> Option<String>, name: &str) -> Result<Option<Secret>, String> {
    let direct = get(name).filter(|v| !v.is_empty());
    let file_key = format!("{name}_FILE");
    let file = get(&file_key).filter(|v| !v.is_empty());
    match (direct, file) {
        (Some(_), Some(_)) => Err(format!("set {name} or {file_key}, not both")),
        (Some(v), None) => Ok(Some(Secret { value: v, from_file: None, loose_permissions: false })),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{file_key}: cannot read {path:?}: {e}"))?;
            let value = text.strip_suffix('\n').map(|t| t.strip_suffix('\r').unwrap_or(t)).unwrap_or(&text).to_string();
            if value.is_empty() {
                return Err(format!("{file_key}: {path:?} is empty"));
            }
            Ok(Some(Secret { value, loose_permissions: loose(Path::new(&path)), from_file: Some(path) }))
        }
        (None, None) => Ok(None),
    }
}

/// `secret_from` over the process environment.
pub fn secret(name: &str) -> Result<Option<Secret>, String> {
    secret_from(&|k| std::env::var(k).ok(), name)
}

#[cfg(unix)]
fn loose(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn loose(_p: &Path) -> bool {
    false
}
