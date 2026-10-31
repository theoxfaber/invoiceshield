//! Invoice-number normalisation for the duplicate key.
use unicode_normalization::UnicodeNormalization;

/// NFKC, drop zero-width/format characters, map every dash-like character to
/// '-', drop whitespace, uppercase. "inv 2231", "INV–2231" and "INV\u{200B}2231"
/// all normalise to "INV2231"-style keys that compare equal.
///
/// Iterated to a fixpoint: uppercasing after NFKC can leave a decomposed sequence
/// ("ΐ" → "Ι\u{308}\u{301}") whose composed twin ("Ϊ́") would otherwise get a different
/// key and slip past the duplicate check. Found by tests/fuzz_inputs.rs (Phase 2).
pub fn invoice_no(raw: &str) -> String {
    let mut s = invoice_no_pass(raw);
    for _ in 0..4 {
        let n = invoice_no_pass(&s);
        if n == s {
            break;
        }
        s = n;
    }
    s
}

fn invoice_no_pass(raw: &str) -> String {
    raw.nfkc()
        .filter(|c| !is_invisible(*c))
        .map(|c| if is_dash(c) { '-' } else { c })
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .filter(|c| *c != '-' && *c != '/' && *c != '_' && *c != '.')
        .collect()
}

/// Near-duplicate base (red-team R4): the duplicate key of `raw` with one trailing re-submission
/// suffix removed, or None when the number has no such suffix. A suffix is, right after a digit,
/// either 1-3 letters with an optional separator ("INV-101A", "INV-101-A", "0900/REV") or a
/// separator and 1-2 digits ("INV-101-1", "INV-101/02"). Digits without a separator are never
/// stripped ("INV-1011" is a different invoice from "INV-101"). The base goes through
/// `invoice_no()`, so it compares with the ledger's keys.
pub fn near_dup_base(raw: &str) -> Option<String> {
    // Same pass as invoice_no() but separators kept, so "-1" can be told from "1".
    let s: Vec<char> = raw
        .nfkc()
        .filter(|c| !is_invisible(*c))
        .map(|c| if is_dash(c) { '-' } else { c })
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .collect();
    let is_sep = |c: char| matches!(c, '-' | '/' | '_' | '.');
    let mut end = s.len();
    while end > 0 && is_sep(s[end - 1]) {
        end -= 1; // trailing separators carry nothing
    }
    let s = &s[..end];
    let n = s.len();
    let letters = s.iter().rev().take_while(|c| c.is_alphabetic()).count();
    let base_end = if (1..=3).contains(&letters) {
        let mut i = n - letters;
        if i > 0 && is_sep(s[i - 1]) {
            i -= 1;
        }
        (i > 0 && s[i - 1].is_numeric()).then_some(i)
    } else if letters == 0 {
        let nd = s.iter().rev().take_while(|c| c.is_ascii_digit()).count();
        let i = n - nd;
        ((1..=2).contains(&nd) && i >= 2 && is_sep(s[i - 1]) && s[i - 2].is_numeric())
            .then(|| i - 1)
    } else {
        None
    }?;
    let base = invoice_no(&s[..base_end].iter().collect::<String>());
    (!base.is_empty() && base != invoice_no(raw)).then_some(base)
}

/// True when a bank field (account number, IFSC) carries anything outside printable ASCII:
/// Cyrillic/Greek look-alikes, fullwidth digits, invisible characters (red-team R6).
pub fn has_non_ascii(raw: &str) -> bool {
    raw.chars().any(|c| !c.is_ascii() || c.is_ascii_control())
}

fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{00AD}' | '\u{202A}'..='\u{202E}')
}

fn is_dash(c: char) -> bool {
    matches!(
        c,
        '-' | '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE58}' | '\u{FE63}' | '\u{FF0D}'
    )
}

pub fn gstin(raw: &str) -> String {
    raw.nfkc()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

pub fn domain(raw: &str) -> String {
    raw.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Upper-case alphanumerics only (NFKC first). For codes compared as plain tokens (SWIFT/BIC).
pub fn upper_alnum(raw: &str) -> String {
    raw.nfkc()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

/// IFSC: NFKC, upper-case, whitespace removed, then validated against `^[A-Z]{4}0[A-Z0-9]{6}$`.
/// None when the value is not a well-formed IFSC (it is never "fixed up" into one).
pub fn ifsc(raw: &str) -> Option<String> {
    let s: String = raw
        .nfkc()
        .filter(|c| !c.is_whitespace() && !is_invisible(*c))
        .flat_map(|c| c.to_uppercase())
        .collect();
    let b = s.as_bytes();
    let ok = b.len() == 11
        && b[..4].iter().all(|c| c.is_ascii_uppercase())
        && b[4] == b'0'
        && b[5..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
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
    for c in raw
        .nfkc()
        .filter(|c| !is_invisible(*c))
        .flat_map(|c| c.to_lowercase())
    {
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

/// Two (already normalised) invoice numbers that are identical except for their LAST run of digits,
/// whose values differ by 1 or 2 ("vec/26-27/0233" / "vec/26-27/0234"). Used only together with
/// equal vendor, PO and amount (v3 prep, bumped-number resubmissions).
pub fn numbers_bumped(a: &str, b: &str) -> bool {
    fn split(s: &str) -> Option<(&str, &str, &str)> {
        let bytes = s.as_bytes();
        let end = bytes.iter().rposition(|c| c.is_ascii_digit())? + 1;
        let start = bytes[..end]
            .iter()
            .rposition(|c| !c.is_ascii_digit())
            .map(|i| i + 1)
            .unwrap_or(0);
        Some((&s[..start], &s[start..end], &s[end..]))
    }
    let (Some((pa, da, sa)), Some((pb, db, sb))) = (split(a), split(b)) else {
        return false;
    };
    if pa != pb || sa != sb || da.len() > 12 || db.len() > 12 {
        return false;
    }
    match (da.parse::<u64>(), db.parse::<u64>()) {
        (Ok(x), Ok(y)) => (1..=2).contains(&x.abs_diff(y)),
        _ => false,
    }
}
