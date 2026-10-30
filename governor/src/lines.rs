//! Verification of item lines proposed by a model (review item 2, 2026-10-31).
//!
//! The deterministic item-table parser (assembler/extract.py) reads our own layouts but read no
//! lines at all on the first blind set. When it reads nothing, a quarantined Meta model may
//! *propose* the lines. Nothing it says is trusted: a proposed line set is used only when every
//! check below passes against the text layer the extraction service signed. Otherwise the
//! invoice goes to REVIEW (`line_items_unreadable`), exactly as if no model existed.
//!
//! Checks (all must hold):
//! 1. 1..=MAX_LINES lines, descriptions 2..=300 chars.
//! 2. The lines add up exactly (checked arithmetic) to the printed total.
//! 3. Every description appears verbatim (case and whitespace folded) in the signed text layer.
//! 4. Every amount, quantity and unit price appears verbatim (as printed digits, with or without
//!    Indian/Western grouping) in the text layer; qty x unit price reproduces the amount.
//! 5. Each amount appears within `NEAR` characters after its description (same row region).
//! 6. A charge (non-tax) amount must appear at least once *not* right after a total-style label
//!    ("Grand Total", "Taxable Total", "Payable"...), so a model cannot collapse the invoice into
//!    one line priced at the total.
//! 7. A line flagged as tax must be named as tax (CGST/SGST/IGST/UTGST/GST/cess), so a fee
//!    cannot be relabelled as tax to dodge the PO-line check.
//! 8. A negative amount needs a printed minus sign/parenthesis or a discount/round-off wording.
//!
//! Stated residual: a model could still pick a *subset* of real rows plus numbers that happen to
//! add up to the total; checks 2, 5 and 6 make that hard, and the PO-line rules then run on the
//! chosen lines as on parsed ones. The model can never add a payee, raise an amount, or clear any
//! other REVIEW/BLOCK reason.
use crate::engine::ItemLine;

pub const MAX_LINES: usize = 100;
/// How far after the start of a description its amount may be printed (folded characters).
const NEAR: usize = 600;
/// How far before an amount a total-style label disqualifies that occurrence.
const LABEL_WINDOW: usize = 24;

/// Lowercase, every whitespace run folded to one space. Works on chars, returns a String whose
/// byte offsets are used consistently for all searches below.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.extend(c.to_lowercase());
    }
    out
}

fn group_western(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn group_indian(n: u64) -> String {
    let s = n.to_string();
    if s.len() <= 3 {
        return s;
    }
    let (head, tail) = s.split_at(s.len() - 3);
    let mut groups: Vec<&str> = vec![];
    let mut i = head.len();
    while i > 0 {
        let start = i.saturating_sub(2);
        groups.push(&head[start..i]);
        i = start;
    }
    groups.reverse();
    format!("{},{}", groups.join(","), tail)
}

/// Printed forms of a minor-unit amount (absolute value).
fn amount_forms(minor: i64) -> Vec<String> {
    let m = minor.unsigned_abs();
    let (r, p) = (m / 100, m % 100);
    let mut v = vec![format!("{r}.{p:02}"), format!("{}.{p:02}", group_western(r)), format!("{}.{p:02}", group_indian(r))];
    if p == 0 {
        v.extend([r.to_string(), group_western(r), group_indian(r)]);
    }
    v.sort();
    v.dedup();
    v
}

fn int_forms(n: i64) -> Vec<String> {
    let m = n.unsigned_abs();
    let mut v = vec![m.to_string(), group_western(m), group_indian(m)];
    v.sort();
    v.dedup();
    v
}

/// Byte offsets in `hay` where `needle` occurs as a whole number (not inside a longer one).
fn number_positions(hay: &str, needle: &str) -> Vec<usize> {
    let b = hay.as_bytes();
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let s = from + i;
        let e = s + needle.len();
        let before_ok = s == 0 || !(b[s - 1].is_ascii_digit() || b[s - 1] == b',' || b[s - 1] == b'.');
        let after_ok = match b.get(e) {
            None => true,
            Some(c) if c.is_ascii_digit() => false,
            Some(b'.') | Some(b',') => !b.get(e + 1).is_some_and(|c| c.is_ascii_digit()),
            _ => true,
        };
        if before_ok && after_ok {
            out.push(s);
        }
        from = s + 1;
        if from >= hay.len() {
            break;
        }
    }
    out
}

fn positions_of_amount(hay: &str, minor: i64) -> Vec<usize> {
    let mut v: Vec<usize> = amount_forms(minor).iter().flat_map(|f| number_positions(hay, f)).collect();
    v.sort();
    v.dedup();
    v
}

fn positions_of_int(hay: &str, n: i64) -> Vec<usize> {
    let mut v: Vec<usize> = int_forms(n).iter().flat_map(|f| number_positions(hay, f)).collect();
    v.sort();
    v.dedup();
    v
}

fn window_before(hay: &str, pos: usize, n: usize) -> &str {
    let mut s = pos.saturating_sub(n);
    while !hay.is_char_boundary(s) {
        s -= 1;
    }
    &hay[s..pos]
}

const TOTAL_LABELS: &[&str] = &["total", "taxable", "payable", "balance", "amount due", "net amount", "in words"];

fn total_labelled(hay: &str, pos: usize) -> bool {
    let w = window_before(hay, pos, LABEL_WINDOW);
    TOTAL_LABELS.iter().any(|l| w.contains(l))
}

fn negative_marked(hay: &str, pos: usize) -> bool {
    let w = window_before(hay, pos, 4);
    w.contains('-') || w.contains('(') || w.contains('−')
}

const TAX_WORDS: &[&str] = &["cgst", "sgst", "igst", "utgst", "gst", "cess"];

pub fn names_tax(desc_folded: &str) -> bool {
    TAX_WORDS.iter().any(|t| {
        desc_folded.strip_prefix(t).is_some_and(|rest| rest.is_empty() || !rest.as_bytes()[0].is_ascii_alphanumeric())
    }) || desc_folded.starts_with("output ") && names_tax(&desc_folded["output ".len()..])
}

fn discount_like(desc_folded: &str) -> bool {
    ["discount", "less", "rebate", "round", "credit", "adjust"].iter().any(|w| desc_folded.contains(w))
}

/// Verify model-proposed lines against the signed text layer and printed total.
/// Ok(()) when every check passes; Err(list of problems) otherwise.
pub fn verify_proposed(lines: &[ItemLine], text_layer: Option<&str>, printed_total: Option<i64>) -> Result<(), Vec<String>> {
    let mut problems = vec![];
    let Some(text) = text_layer.filter(|t| !t.trim().is_empty()) else {
        return Err(vec!["no signed text layer to verify the lines against".into()]);
    };
    let Some(total) = printed_total else {
        return Err(vec!["the invoice prints no total to verify the lines against".into()]);
    };
    if lines.is_empty() || lines.len() > MAX_LINES {
        return Err(vec![format!("{} lines proposed (allowed 1..={MAX_LINES})", lines.len())]);
    }
    let hay = fold(text);
    let mut sum: Option<i64> = Some(0);
    for (i, l) in lines.iter().enumerate() {
        let n = i + 1;
        sum = sum.and_then(|s| s.checked_add(l.amount_paise));
        let d = fold(&l.description);
        if d.chars().count() < 2 || d.chars().count() > 300 {
            problems.push(format!("line {n}: description length out of range"));
            continue;
        }
        if l.is_tax && !names_tax(&d) {
            problems.push(format!("line {n}: flagged as tax but \"{}\" is not a tax line", l.description));
        }
        let desc_pos: Vec<usize> = hay.match_indices(d.as_str()).map(|(p, _)| p).collect();
        if desc_pos.is_empty() {
            problems.push(format!("line {n}: description \"{}\" is not printed on the document", l.description));
            continue;
        }
        if l.amount_paise == 0 {
            problems.push(format!("line {n}: zero amount"));
            continue;
        }
        let amt_pos = positions_of_amount(&hay, l.amount_paise);
        if amt_pos.is_empty() {
            problems.push(format!("line {n}: amount {} is not printed on the document", l.amount_paise));
            continue;
        }
        let near = |p: &usize| desc_pos.iter().any(|&dp| *p > dp && *p <= dp + d.len() + NEAR);
        let usable: Vec<usize> = amt_pos.iter().copied().filter(near)
            .filter(|&p| l.is_tax || !total_labelled(&hay, p)).collect();
        if usable.is_empty() {
            problems.push(format!("line {n}: amount {} is not printed in \"{}\"'s row (or only as a total)", l.amount_paise, l.description));
            continue;
        }
        if l.amount_paise < 0 && !discount_like(&d) && !usable.iter().any(|&p| negative_marked(&hay, p)) {
            problems.push(format!("line {n}: negative amount without a printed minus sign or discount wording"));
        }
        if let Some(q) = l.qty {
            if q <= 0 || positions_of_int(&hay, q).is_empty() {
                problems.push(format!("line {n}: quantity {q} is not printed on the document"));
            }
        }
        if let Some(u) = l.unit_price_minor {
            if u <= 0 || positions_of_amount(&hay, u).is_empty() {
                problems.push(format!("line {n}: unit price {u} is not printed on the document"));
            }
        }
        if let (Some(q), Some(u)) = (l.qty, l.unit_price_minor) {
            let ok = q.checked_mul(u).and_then(|v| v.checked_sub(l.amount_paise.abs())).is_some_and(|diff| diff.abs() <= q.abs());
            if !ok {
                problems.push(format!("line {n}: quantity x unit price does not give the amount"));
            }
        }
    }
    match sum {
        None => problems.push("line amounts overflow".into()),
        Some(s) if s != total => problems.push(format!("lines add up to {s} but the invoice prints {total}")),
        _ => {}
    }
    if problems.is_empty() { Ok(()) } else { Err(problems) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(d: &str, a: i64, q: Option<i64>, u: Option<i64>, tax: bool) -> ItemLine {
        ItemLine { description: d.into(), amount_paise: a, qty: q, unit_price_minor: u, is_tax: tax }
    }

    const TEXT: &str = "TAX INVOICE\nSl\nDescription\nQty\nRate\nTaxable Value\n1\nCorrugated box 5-ply\n4819\n1000\nNOS\n42.50\n18\n42,500.00\n2\nPrinted label A6\n4821\n5000\n1.20\n6,000.00\nTaxable Total\n₹ 48,500.00\nCGST\n₹ 4,365.00\nSGST\n₹ 4,365.00\nGRAND TOTAL\n₹ 57,230.00\n";

    fn good() -> Vec<ItemLine> {
        vec![
            line("Corrugated box 5-ply", 42_500_00, Some(1000), Some(42_50), false),
            line("Printed label A6", 6_000_00, Some(5000), Some(1_20), false),
            line("CGST", 4_365_00, None, None, true),
            line("SGST", 4_365_00, None, None, true),
        ]
    }

    #[test]
    fn verbatim_lines_that_add_up_verify() {
        assert_eq!(verify_proposed(&good(), Some(TEXT), Some(57_230_00)), Ok(()));
    }

    #[test]
    fn no_text_or_no_total_fails_closed() {
        assert!(verify_proposed(&good(), None, Some(57_230_00)).is_err());
        assert!(verify_proposed(&good(), Some(TEXT), None).is_err());
        assert!(verify_proposed(&[], Some(TEXT), Some(57_230_00)).is_err());
    }

    #[test]
    fn invented_amount_fails() {
        let mut l = good();
        l[0].amount_paise = 42_600_00;
        l[0].unit_price_minor = Some(42_60);
        assert!(verify_proposed(&l, Some(TEXT), Some(57_330_00)).is_err());
    }

    #[test]
    fn lines_not_adding_up_fail() {
        let mut l = good();
        l.pop();
        assert!(verify_proposed(&l, Some(TEXT), Some(57_230_00)).is_err());
    }

    #[test]
    fn collapse_into_one_line_at_the_taxable_total_fails() {
        let l = vec![
            line("Corrugated box 5-ply", 48_500_00, None, None, false),
            line("CGST", 4_365_00, None, None, true),
            line("SGST", 4_365_00, None, None, true),
        ];
        let e = verify_proposed(&l, Some(TEXT), Some(57_230_00)).unwrap_err();
        assert!(e.iter().any(|p| p.contains("only as a total") || p.contains("not printed")), "{e:?}");
    }

    #[test]
    fn invented_description_fails() {
        let mut l = good();
        l[1].description = "Corrugated box 7-ply".into();
        assert!(verify_proposed(&l, Some(TEXT), Some(57_230_00)).is_err());
    }

    #[test]
    fn fee_relabelled_as_tax_fails() {
        let mut l = good();
        l[1].is_tax = true;
        let e = verify_proposed(&l, Some(TEXT), Some(57_230_00)).unwrap_err();
        assert!(e.iter().any(|p| p.contains("flagged as tax")), "{e:?}");
    }

    #[test]
    fn qty_times_rate_must_reproduce_amount() {
        let mut l = good();
        l[0].qty = Some(5000);
        assert!(verify_proposed(&l, Some(TEXT), Some(57_230_00)).is_err());
    }

    #[test]
    fn number_boundaries_are_respected() {
        // "2,500.00" must not be found inside "42,500.00"
        assert!(positions_of_amount(&fold(TEXT), 2_500_00).is_empty());
        assert!(!positions_of_amount(&fold(TEXT), 42_500_00).is_empty());
        assert!(!positions_of_int(&fold(TEXT), 1000).is_empty());
    }

    #[test]
    fn huge_amounts_do_not_overflow_the_sum() {
        let l = vec![line("Corrugated box 5-ply", i64::MAX, None, None, false), line("Printed label A6", i64::MAX, None, None, false)];
        let e = verify_proposed(&l, Some(TEXT), Some(1)).unwrap_err();
        assert!(e.iter().any(|p| p.contains("overflow") || p.contains("not printed")), "{e:?}");
    }

    #[test]
    fn indian_grouping_is_read() {
        assert_eq!(group_indian(1_23_45_678), "1,23,45,678");
        assert_eq!(group_western(12_345_678), "12,345,678");
        assert!(names_tax("cgst @9%") && names_tax("igst") && !names_tax("gstin filing fee") && !names_tax("expedite fee"));
    }
}
