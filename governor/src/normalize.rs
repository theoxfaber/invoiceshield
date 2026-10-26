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
