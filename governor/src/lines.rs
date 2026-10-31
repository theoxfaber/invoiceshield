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

#[cfg(test)]
fn group_western(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
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

/// A number printed in the text: byte offset in the folded text and its canonical value.
/// `minor` is the value in minor units (two decimals; more decimals only when the extra digits are
/// zeros); `int` is the whole-number value when it has no fractional part. Groupings are read only
/// when valid (Western 3-3-3 or Indian 2-2-3 before the last three digits); anything else is split
/// at the commas into separate numbers, so "12,34" is never read as 1234.
#[derive(Debug, Clone, Copy)]
struct Num {
    pos: usize,
    end: usize,
    minor: Option<i64>,
    int: Option<i64>,
}

fn valid_grouping(groups: &[&str]) -> bool {
    if groups.len() < 2 {
        return true;
    }
    let (last, head) = groups.split_last().unwrap();
    if last.len() != 3 || head[0].is_empty() || head[0].len() > 3 {
        return false;
    }
    let western = head[1..].iter().all(|g| g.len() == 3) && head[0].len() <= 3;
    let indian = head[1..].iter().all(|g| g.len() == 2) && head[0].len() <= 2;
    western || indian
}

fn parse_num(int_part: &str, frac: Option<&str>) -> (Option<i64>, Option<i64>) {
    let whole: Option<i64> = int_part.parse().ok();
    let Some(w) = whole else { return (None, None) };
    match frac {
        None => (w.checked_mul(100), Some(w)),
        Some(f) => {
            let (head, rest) = f.split_at(f.len().min(2));
            if !rest.bytes().all(|b| b == b'0') {
                return (None, None);
            }
            let p: i64 = format!("{head:0<2}").parse().unwrap_or(0);
            let minor = w.checked_mul(100).and_then(|v| v.checked_add(p));
            (minor, (p == 0).then_some(w))
        }
    }
}

/// Every number printed in `hay`, under canonical numeric normalisation: the currency glyph or code
/// before it (₹, Rs., INR) and grouping commas do not matter, only the digits and the decimal point.
fn numbers(hay: &str) -> Vec<Num> {
    let b = hay.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && b[i - 1].is_ascii_digit()) {
            i += 1;
            continue;
        }
        // a digit glued to a letter-dot-digit run ("v1.2") or a preceding decimal ("0.5") is a part, not a start
        if i > 1 && (b[i - 1] == b'.' || b[i - 1] == b',') && b[i - 2].is_ascii_digit() {
            i += 1;
            continue;
        }
        let s = i;
        let mut j = i;
        while j < b.len()
            && (b[j].is_ascii_digit()
                || (b[j] == b',' && j + 1 < b.len() && b[j + 1].is_ascii_digit()))
        {
            j += 1;
        }
        let mut frac = None;
        let mut e = j;
        if j + 1 < b.len() && b[j] == b'.' && b[j + 1].is_ascii_digit() {
            let mut k = j + 1;
            while k < b.len() && b[k].is_ascii_digit() {
                k += 1;
            }
            frac = Some(&hay[j + 1..k]);
            e = k;
        }
        let int_str = &hay[s..j];
        let groups: Vec<&str> = int_str.split(',').collect();
        if valid_grouping(&groups) {
            let (minor, int) = parse_num(&groups.concat(), frac);
            out.push(Num {
                pos: s,
                end: e,
                minor,
                int,
            });
        } else {
            // invalid grouping: each comma-separated run is its own number (the last keeps the decimals)
            let mut off = s;
            let n = groups.len();
            for (gi, g) in groups.iter().enumerate() {
                let last = gi + 1 == n;
                let (minor, int) = parse_num(g, if last { frac } else { None });
                let gend = if last { e } else { off + g.len() };
                if !g.is_empty() {
                    out.push(Num {
                        pos: off,
                        end: gend,
                        minor,
                        int,
                    });
                }
                off += g.len() + 1;
            }
        }
        i = e.max(i + 1);
    }
    out
}

fn positions_of_amount_in(nums: &[Num], minor: i64) -> Vec<usize> {
    let m = minor.checked_abs().unwrap_or(-1);
    nums.iter()
        .filter(|n| n.minor == Some(m))
        .map(|n| n.pos)
        .collect()
}

fn positions_of_int_in(nums: &[Num], v: i64) -> Vec<usize> {
    let m = v.checked_abs().unwrap_or(-1);
    nums.iter()
        .filter(|n| n.int == Some(m))
        .map(|n| n.pos)
        .collect()
}

#[cfg(test)]
fn positions_of_amount(hay: &str, minor: i64) -> Vec<usize> {
    positions_of_amount_in(&numbers(hay), minor)
}

#[cfg(test)]
fn positions_of_int(hay: &str, n: i64) -> Vec<usize> {
    positions_of_int_in(&numbers(hay), n)
}

/// Percentages printed in hay[from..to] ("5%", "2.5 %"), in basis points.
fn percents_between(hay: &str, nums: &[Num], from: usize, to: usize) -> Vec<i64> {
    let b = hay.as_bytes();
    nums.iter()
        .filter(|n| n.pos >= from && n.pos < to)
        .filter(|n| {
            let mut k = n.end;
            while k < b.len() && b[k] == b' ' {
                k += 1;
            }
            b.get(k) == Some(&b'%')
        })
        .filter_map(|n| n.minor)
        .filter(|&bp| bp > 0 && bp < 100_00)
        .collect()
}

/// Folded text with every space removed, and the byte offset in the folded text of each byte.
fn compact(hay: &str) -> (String, Vec<usize>) {
    let mut s = String::with_capacity(hay.len());
    let mut map = Vec::with_capacity(hay.len());
    for (i, c) in hay.char_indices() {
        if c == ' ' {
            continue;
        }
        let before = s.len();
        s.push(c);
        for k in 0..(s.len() - before) {
            map.push(i + k);
        }
    }
    (s, map)
}

/// How far (folded bytes) the second half of a wrapped description may sit after the first.
const WRAP: usize = 240;

/// Where a description is printed: start offsets in the folded text. Matched (a) verbatim with
/// whitespace folded, (b) ignoring whitespace entirely ("Grade )" vs "Grade)"), or (c) wrapped once
/// inside a fixed-width row: the first part printed, other cells of the same row, then the rest of
/// the description within WRAP bytes. Every character of the description must be printed, in order.
fn description_positions(hay: &str, comp: &(String, Vec<usize>), d: &str) -> Vec<usize> {
    let mut v: Vec<usize> = hay.match_indices(d).map(|(p, _)| p).collect();
    if !v.is_empty() {
        return v;
    }
    let (cs, map) = comp;
    let dc: String = d.chars().filter(|c| *c != ' ').collect();
    if dc.is_empty() {
        return v;
    }
    v = cs.match_indices(dc.as_str()).map(|(p, _)| map[p]).collect();
    if !v.is_empty() {
        return v;
    }
    let idx: Vec<usize> = dc.char_indices().map(|(i, _)| i).collect();
    for &k in idx.iter().skip(3) {
        if dc.len() - k < 3 {
            break;
        }
        let (a, rest) = dc.split_at(k);
        for (p, _) in cs.match_indices(a) {
            let a_end = p + a.len();
            let Some(&hay_a_end) = map.get(a_end - 1) else {
                continue;
            };
            if let Some(q) = cs[a_end..].find(rest) {
                let qs = a_end + q;
                if map[qs].saturating_sub(hay_a_end) <= WRAP {
                    v.push(map[p]);
                }
            }
        }
        if !v.is_empty() {
            break;
        }
    }
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

const TOTAL_LABELS: &[&str] = &[
    "total",
    "taxable",
    "payable",
    "balance",
    "amount due",
    "net amount",
    "in words",
];

fn total_labelled(hay: &str, pos: usize) -> bool {
    let w = window_before(hay, pos, LABEL_WINDOW);
    TOTAL_LABELS.iter().any(|l| w.contains(l))
}

fn negative_marked(hay: &str, pos: usize) -> bool {
    let w = window_before(hay, pos, 4);
    w.contains('-') || w.contains('(') || w.contains('−')
}

const TAX_WORDS: &[&str] = &["cgst", "sgst", "igst", "utgst", "gst", "cess"];
/// Spelled-out GST heads as Indian invoices print them.
const TAX_PHRASES: &[&str] = &[
    "integrated gst",
    "central gst",
    "state gst",
    "union territory gst",
    "integrated tax",
    "central tax",
    "state tax",
    "state/ut tax",
    "ut tax",
    "compensation cess",
];

/// True when the folded description names a tax head: a GST head word, optionally followed by a
/// rate glued on as digits ("IGST18 (18%)"), or a spelled-out head ("Integrated GST @ 18%"), after
/// at most one of the bookkeeping prefixes "output", "add", "less". "GSTIN filing fee" is not tax:
/// a head word followed by a letter never counts.
pub fn names_tax(desc_folded: &str) -> bool {
    for pre in ["output ", "add ", "add: ", "less ", "add:"] {
        if let Some(rest) = desc_folded.strip_prefix(pre) {
            if names_tax_head(rest.trim_start()) {
                return true;
            }
        }
    }
    names_tax_head(desc_folded)
}

fn names_tax_head(d: &str) -> bool {
    TAX_WORDS.iter().any(|t| {
        d.strip_prefix(t)
            .is_some_and(|rest| rest.is_empty() || !rest.as_bytes()[0].is_ascii_alphabetic())
    }) || TAX_PHRASES.iter().any(|t| {
        d.strip_prefix(t)
            .is_some_and(|rest| rest.is_empty() || !rest.as_bytes()[0].is_ascii_alphabetic())
    })
}

fn discount_like(desc_folded: &str) -> bool {
    ["discount", "less", "rebate", "round", "credit", "adjust"]
        .iter()
        .any(|w| desc_folded.contains(w))
}

/// Verify model-proposed lines against the signed text layer and printed total.
/// Ok(lines the rules should run on) when every check passes; Err(list of problems) otherwise.
/// Zero-amount lines ("Round Off 0.00", "Less Disc 0.00") are dropped: they cannot change the
/// sum, the PO check or the payment, and a model listing them is not an error.
pub fn verify_proposed(
    lines: &[ItemLine],
    text_layer: Option<&str>,
    printed_total: Option<i64>,
) -> Result<Vec<ItemLine>, Vec<String>> {
    let mut problems = vec![];
    let Some(text) = text_layer.filter(|t| !t.trim().is_empty()) else {
        return Err(vec![
            "no signed text layer to verify the lines against".into()
        ]);
    };
    let Some(total) = printed_total else {
        return Err(vec![
            "the invoice prints no total to verify the lines against".into(),
        ]);
    };
    if lines.is_empty() || lines.len() > MAX_LINES {
        return Err(vec![format!(
            "{} lines proposed (allowed 1..={MAX_LINES})",
            lines.len()
        )]);
    }
    let kept: Vec<ItemLine> = lines
        .iter()
        .filter(|l| l.amount_paise != 0)
        .cloned()
        .collect();
    if kept.is_empty() {
        return Err(vec!["every proposed line has a zero amount".into()]);
    }
    let hay = fold(text);
    let nums = numbers(&hay);
    let comp = compact(&hay);
    let mut sum: Option<i64> = Some(0);
    for (i, l) in kept.iter().enumerate() {
        let n = i + 1;
        sum = sum.and_then(|s| s.checked_add(l.amount_paise));
        let d = fold(&l.description);
        if d.chars().count() < 2 || d.chars().count() > 300 {
            problems.push(format!("line {n}: description length out of range"));
            continue;
        }
        if l.is_tax && !names_tax(&d) {
            problems.push(format!(
                "line {n}: flagged as tax but \"{}\" is not a tax line",
                l.description
            ));
        }
        let desc_pos = description_positions(&hay, &comp, &d);
        if desc_pos.is_empty() {
            problems.push(format!(
                "line {n}: description \"{}\" is not printed on the document",
                l.description
            ));
            continue;
        }
        let amt_pos = positions_of_amount_in(&nums, l.amount_paise);
        if amt_pos.is_empty() {
            problems.push(format!(
                "line {n}: amount {} is not printed on the document",
                l.amount_paise
            ));
            continue;
        }
        let near = |p: &usize| {
            desc_pos
                .iter()
                .any(|&dp| *p > dp && *p <= dp + d.len() + NEAR)
        };
        let usable: Vec<usize> = amt_pos
            .iter()
            .copied()
            .filter(near)
            .filter(|&p| l.is_tax || !total_labelled(&hay, p))
            .collect();
        if usable.is_empty() {
            problems.push(format!(
                "line {n}: amount {} is not printed in \"{}\"'s row (or only as a total)",
                l.amount_paise, l.description
            ));
            continue;
        }
        if l.amount_paise < 0
            && !discount_like(&d)
            && !usable.iter().any(|&p| negative_marked(&hay, p))
        {
            problems.push(format!(
                "line {n}: negative amount without a printed minus sign or discount wording"
            ));
        }
        if let Some(q) = l.qty {
            if q <= 0 || positions_of_int_in(&nums, q).is_empty() {
                problems.push(format!(
                    "line {n}: quantity {q} is not printed on the document"
                ));
            }
        }
        if let Some(u) = l.unit_price_minor {
            if u <= 0 || positions_of_amount_in(&nums, u).is_empty() {
                problems.push(format!(
                    "line {n}: unit price {u} is not printed on the document"
                ));
            }
        }
        if let (Some(q), Some(u)) = (l.qty, l.unit_price_minor) {
            let gross = q.checked_mul(u);
            let tol = q.abs().max(1);
            let exact = gross
                .and_then(|v| v.checked_sub(l.amount_paise.abs()))
                .is_some_and(|diff| diff.abs() <= tol);
            // A printed line discount: qty x rate less a percentage printed in the same row
            // (between the description and the amount) must give the amount exactly (± one minor
            // unit per unit). The line can only bill less than qty x rate, never more.
            let discounted = !exact
                && gross.is_some_and(|g| g > l.amount_paise.abs())
                && desc_pos.iter().any(|&dp| {
                    usable.iter().filter(|&&ap| ap > dp).any(|&ap| {
                        percents_between(&hay, &nums, dp, ap).iter().any(|&bp| {
                            gross
                                .and_then(|g| g.checked_mul(10_000 - bp))
                                .map(|v| v / 10_000)
                                .and_then(|net| net.checked_sub(l.amount_paise.abs()))
                                .is_some_and(|diff| diff.abs() <= tol)
                        })
                    })
                });
            if !exact && !discounted {
                problems.push(format!(
                    "line {n}: quantity x unit price does not give the amount"
                ));
            }
        }
    }
    match sum {
        None => problems.push("line amounts overflow".into()),
        Some(s) if s != total => problems.push(format!(
            "lines add up to {s} but the invoice prints {total}"
        )),
        _ => {}
    }
    if problems.is_empty() {
        Ok(kept)
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(d: &str, a: i64, q: Option<i64>, u: Option<i64>, tax: bool) -> ItemLine {
        ItemLine {
            description: d.into(),
            amount_paise: a,
            qty: q,
            unit_price_minor: u,
            is_tax: tax,
        }
    }

    const TEXT: &str = "TAX INVOICE\nSl\nDescription\nQty\nRate\nTaxable Value\n1\nCorrugated box 5-ply\n4819\n1000\nNOS\n42.50\n18\n42,500.00\n2\nPrinted label A6\n4821\n5000\n1.20\n6,000.00\nTaxable Total\n₹ 48,500.00\nCGST\n₹ 4,365.00\nSGST\n₹ 4,365.00\nGRAND TOTAL\n₹ 57,230.00\n";

    fn good() -> Vec<ItemLine> {
        vec![
            line(
                "Corrugated box 5-ply",
                42_500_00,
                Some(1000),
                Some(42_50),
                false,
            ),
            line("Printed label A6", 6_000_00, Some(5000), Some(1_20), false),
            line("CGST", 4_365_00, None, None, true),
            line("SGST", 4_365_00, None, None, true),
        ]
    }

    #[test]
    fn verbatim_lines_that_add_up_verify() {
        assert_eq!(
            verify_proposed(&good(), Some(TEXT), Some(57_230_00)).map(|v| v.len()),
            Ok(4)
        );
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
        assert!(
            e.iter()
                .any(|p| p.contains("only as a total") || p.contains("not printed")),
            "{e:?}"
        );
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
        let l = vec![
            line("Corrugated box 5-ply", i64::MAX, None, None, false),
            line("Printed label A6", i64::MAX, None, None, false),
        ];
        let e = verify_proposed(&l, Some(TEXT), Some(1)).unwrap_err();
        assert!(
            e.iter()
                .any(|p| p.contains("overflow") || p.contains("not printed")),
            "{e:?}"
        );
    }

    #[test]
    fn indian_grouping_is_read() {
        assert_eq!(group_indian(1_23_45_678), "1,23,45,678");
        assert_eq!(group_western(12_345_678), "12,345,678");
        assert!(
            names_tax("cgst @9%")
                && names_tax("igst")
                && !names_tax("gstin filing fee")
                && !names_tax("expedite fee")
        );
    }
}
