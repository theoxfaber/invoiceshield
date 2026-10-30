//! Invoice-number normalisation for the duplicate key.
use unicode_normalization::UnicodeNormalization;

/// NFKC, drop zero-width/format characters, map every dash-like character to
/// '-', drop whitespace, uppercase. "inv 2231", "INV–2231" and "INV\u{200B}2231"
/// all normalise to "INV2231"-style keys that compare equal.
pub fn invoice_no(raw: &str) -> String {
    raw.nfkc()
        .filter(|c| !is_invisible(*c))
        .map(|c| if is_dash(c) { '-' } else { c })
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .filter(|c| *c != '-' && *c != '/' && *c != '_' && *c != '.')
        .collect()
}

fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{00AD}' | '\u{202A}'..='\u{202E}')
}

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE58}' | '\u{FE63}' | '\u{FF0D}')
}

pub fn gstin(raw: &str) -> String {
    raw.nfkc().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase()
}

pub fn domain(raw: &str) -> String {
    raw.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Upper-case alphanumerics only (NFKC first). For codes compared as plain tokens (SWIFT/BIC).
pub fn upper_alnum(raw: &str) -> String {
    raw.nfkc().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase()
}

/// IFSC: NFKC, upper-case, whitespace removed, then validated against `^[A-Z]{4}0[A-Z0-9]{6}$`.
/// None when the value is not a well-formed IFSC (it is never "fixed up" into one).
pub fn ifsc(raw: &str) -> Option<String> {
    let s: String = raw.nfkc().filter(|c| !c.is_whitespace() && !is_invisible(*c)).flat_map(|c| c.to_uppercase()).collect();
    let b = s.as_bytes();
    let ok = b.len() == 11
        && b[..4].iter().all(|c| c.is_ascii_uppercase())
        && b[4] == b'0'
        && b[5..].iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    ok.then_some(s)
}

/// Does a bank code printed on a document name the same branch as the one on file?
/// Both valid IFSCs: compared after `ifsc()`. Neither an IFSC (foreign vendor, SWIFT/BIC on file):
/// compared as upper-case alphanumerics. One valid and one not, or an empty value: different.
pub fn same_bank_code(printed: &str, on_file: &str) -> bool {
    match (ifsc(printed), ifsc(on_file)) {
        (Some(a), Some(b)) => a == b,
        (None, None) => {
            let (a, b) = (upper_alnum(printed), upper_alnum(on_file));
            !a.is_empty() && a == b
        }
        _ => false,
    }
}

/// ISO 4217 code: trimmed, upper-case. Common symbols and names are mapped to their code;
/// anything that is not three ASCII letters afterwards is returned as-is (and has no limits,
/// so the governor treats it as an unknown currency → REVIEW).
pub fn currency(raw: &str) -> String {
    let t = raw.trim();
    match t.to_lowercase().as_str() {
        "₹" | "rs" | "rs." | "inr" | "rupee" | "rupees" => "INR".into(),
        "$" | "us$" | "usd" | "us dollar" | "us dollars" => "USD".into(),
        "€" | "eur" | "euro" | "euros" => "EUR".into(),
        "£" | "gbp" => "GBP".into(),
        _ => t.to_uppercase(),
    }
}

/// True for a three-letter upper-case code (shape only; whether it is supported is policy).
pub fn is_iso4217_shape(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|c| c.is_ascii_uppercase())
}

/// Item-name key for PO-line matching: NFKC, lower-case, every run of non-alphanumerics → one space.
pub fn item_key(raw: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in raw.nfkc().filter(|c| !is_invisible(*c)).flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            out.push(c);
            gap = false;
        } else {
            gap = true;
        }
    }
    out
}
