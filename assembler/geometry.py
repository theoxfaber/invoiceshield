"""Layout-agnostic item-table parser on word boxes (v3 prep, 2026-10-31).

Used only when the text parsers in `assembler/extract.py` read nothing. Word boxes come from
poppler's `pdftotext -bbox` (already in the image for OCR/page rendering) run inside the
extraction sandbox. The parser knows no template:

1. Words are grouped into rows (same page, overlapping vertical centre) and rows into cells
   (a gap wider than about one space starts a new cell). Rule lines (----, ====) are dropped.
   Monospace (dot-matrix style) pages are re-cut along the header's columns instead, because
   one space there can separate both words and columns.
2. The item table's header is the first row with a description-like cell (Description,
   Particulars, Item, Goods, Services, Details) and an amount-like cell (Amount, Value, Total,
   Taxable). Header lines printed directly under it ("No.", "(Rs)") are merged into it.
3. Body cells are assigned to header columns by alignment (text by overlap, figures by right edge
   or centre). Each amount-like column is tried (a "Taxable" column first, then right to left). A
   row with a money figure in that column is a line; rows with only description text (or only a
   quantity) belong to the nearest line row (wrapped, multi-line cells). A horizontal summary
   table (a row of labels over a row of figures) is read column by column.
4. Summary lines (tax heads, charges, round-off, discounts) become lines like any other; a
   subtotal must equal the running sum; the table ends at a total-style label whose figure is
   the printed total.

Fail closed by arithmetic: the lines must add up exactly to the printed total, and every amount,
quantity, rate and description fragment must be found in the signed text layer
(`verified_in_text`); otherwise the parser returns nothing and the governor REVIEWs
`line_items_unreadable` (or verifies a model proposal instead). Anything it skips cannot be paid:
the payment is the printed total, and that must equal what it read.
"""
from __future__ import annotations

import html
import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

TAX_RE = re.compile(r"\b(CGST|SGST|IGST|UTGST|GST|CESS)\b|\b(?:CGST|SGST|IGST|UTGST)\d|integrated\s+(?:tax|gst)|central\s+(?:tax|gst)|state\s+(?:tax|gst)", re.I)
DESC_HDR = re.compile(r"\b(description|particulars|item|items|goods|services|details|product)\b", re.I)
AMT_HDR = re.compile(r"\b(amount|amt|value|total|taxable)\b", re.I)
QTY_HDR = re.compile(r"\b(qty|quantity|quantit\w*)\b", re.I)
RATE_HDR = re.compile(r"\b(rate|unit\s*price|price)\b", re.I)
TOTAL_LBL = re.compile(r"grand\s*total|\btotal\b|payable|amount\s+due|balance\s+due|net\s+amount|invoice\s+(?:value|amount)|total\s+inv", re.I)
SUBTOTAL_LBL = re.compile(r"sub\s*-?\s*total|taxable|tax'?ble|total\s+before\s+tax|total\s+value\s+of\s+goods|^\s*total\s*:?\s*$", re.I)
FORWARD_LBL = re.compile(r"carried\s+(?:forward|over)|brought\s+(?:forward|over)|\bc/f\b|\bb/f\b|continued", re.I)
DISCOUNT_LBL = re.compile(r"discount|\bless\b|rebate|\bdisc\b", re.I)
SUMMARYISH = re.compile(r"total|payable|due|taxable|tax'?ble|round|r/o|charge|freight|fee|discount|\bless\b|\badd\b|\bnet\b", re.I)
MONEY_RE = re.compile(r"^(?:₹|Rs\.?|INR)?\s*(\()?(-|−)?\s*(?:₹|Rs\.?|INR)?\s*(\d[\d,]*)(?:\.(\d{1,2}))?(\))?\s*(?:/-)?$", re.I)
LEAD_INT_RE = re.compile(r"^(\d[\d,]*)(?:\.0+)?(?:\s+[A-Za-z.]+)?$")
_RULE_RE = re.compile(r"^[-=_.*~+|]{4,}$")


@dataclass
class Cell:
    page: int
    x0: float
    x1: float
    y: float
    text: str


@dataclass
class Row:
    page: int
    yc: float
    h: float
    cells: list[Cell] = field(default_factory=list)
    words: list = field(default_factory=list)

    def text(self) -> str:
        return " ".join(c.text for c in self.cells)


def _valid_grouping(s: str) -> bool:
    g = s.split(",")
    if len(g) == 1:
        return True
    if len(g[-1]) != 3 or not g[0] or len(g[0]) > 3:
        return False
    mid = g[1:-1]
    return all(len(x) == 3 for x in mid) or (all(len(x) == 2 for x in mid) and len(g[0]) <= 2)


def money(cell: str) -> int | None:
    """Minor units of a cell that is only a money figure (₹/Rs./INR, grouping commas, minus or
    parentheses for negatives, an optional '/-'); None otherwise."""
    m = MONEY_RE.match(cell.strip())
    if not m or not _valid_grouping(m.group(3)):
        return None
    if bool(m.group(1)) != bool(m.group(5)):
        return None
    v = int(m.group(3).replace(",", "")) * 100 + int((m.group(4) or "").ljust(2, "0") or 0)
    neg = bool(m.group(2)) or bool(m.group(1))
    return -v if neg else v


def is_money_cell(cell: str) -> bool:
    """A money figure with paise, grouping or a currency mark (a bare small integer is a qty)."""
    s = cell.strip()
    return money(s) is not None and bool(re.search(r"[.,₹]|rs|inr", s, re.I))


def lead_int(cell: str) -> int | None:
    """Integer of a quantity cell, unit allowed after it ("1450 Plate", "3 Nos", "1,440")."""
    m = LEAD_INT_RE.match(cell.strip())
    if not m or not _valid_grouping(m.group(1)):
        return None
    return int(m.group(1).replace(",", ""))


def words(pdf: str | Path, timeout: float = 20.0) -> list[tuple[int, float, float, float, float, str]]:
    """(page, xMin, yMin, xMax, yMax, text) for every word of the text layer, via pdftotext -bbox."""
    try:
        out = subprocess.run(["pdftotext", "-bbox", "-q", str(pdf), "-"], capture_output=True, text=True,
                             timeout=timeout, check=False).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    page, res = 0, []
    rx = re.compile(r'<word xMin="([\d.]+)" yMin="([\d.]+)" xMax="([\d.]+)" yMax="([\d.]+)">(.*?)</word>')
    for line in out.splitlines():
        if "<page " in line:
            page += 1
        m = rx.search(line)
        if m:
            res.append((page, float(m[1]), float(m[2]), float(m[3]), float(m[4]), html.unescape(m[5])))
    return res


def _numonly(t: str) -> bool:
    """A digits-only cell (HSN/SAC code, serial number) is not description text."""
    return bool(re.fullmatch(r"[\d,.\s/-]+", t.strip()))


def _cw(c: Cell) -> float:
    return (c.x1 - c.x0) / max(len(c.text), 1)


def rows_of(ws) -> list[Row]:
    ws = sorted((w for w in ws if not _RULE_RE.match(w[5])), key=lambda w: (w[0], (w[2] + w[4]) / 2, w[1]))
    rows: list[Row] = []
    groups: list[list] = []
    for w in ws:
        yc, h = (w[2] + w[4]) / 2, max(w[4] - w[2], 1.0)
        if rows and rows[-1].page == w[0] and abs(rows[-1].yc - yc) <= 0.45 * max(h, rows[-1].h):
            groups[-1].append(w)
            rows[-1].h = max(rows[-1].h, h)
        else:
            rows.append(Row(w[0], yc, h))
            groups.append([w])
    for r, g in zip(rows, groups):
        g.sort(key=lambda w: w[1])
        cells: list[Cell] = []
        for w in g:
            # one space apart = same cell: the threshold is the larger of half the text height and
            # 1.3 character widths (a monospace space is ~0.6 em wide)
            cw = min((w[3] - w[1]) / max(len(w[5]), 1), _cw(cells[-1])) if cells else 0.0
            if cells and w[1] - cells[-1].x1 < max(0.55 * r.h, 1.3 * cw):
                c = cells[-1]
                cells[-1] = Cell(c.page, min(c.x0, w[1]), max(c.x1, w[3]), r.yc, c.text + " " + w[5])
            else:
                cells.append(Cell(w[0], w[1], w[3], r.yc, w[5]))
        r.cells = cells
        r.words = g
    return rows


def is_monospace(ws) -> bool:
    """Dot-matrix/plain-text layouts: every word is (character count x one width) wide."""
    rs = sorted((w[3] - w[1]) / len(w[5]) for w in ws if len(w[5]) >= 3)
    if len(rs) < 20:
        return False
    med = rs[len(rs) // 2]
    return sum(abs(x - med) <= 0.06 * med for x in rs) >= 0.9 * len(rs)


def mono_header_cells(r: Row) -> list[Cell]:
    """Header columns of a monospace table: one per header word; '(Rs)', '%', 'No.' join the word before."""
    cols: list[Cell] = []
    for w in r.words:
        t = w[5]
        if cols and (t.startswith(("(", "%")) or t.lower() in {"no", "no.", "code", "rs", "rs."}):
            c = cols[-1]
            cols[-1] = Cell(c.page, c.x0, w[3], c.y, c.text + " " + t)
        else:
            cols.append(Cell(w[0], w[1], w[3], r.yc, t))
    return cols


def recell(r: Row, cols: list[Cell]) -> None:
    """Re-cut a monospace row along the header columns: a word belongs to the column whose left edge
    is the last one at or before it, except that a figure reaching into the next column's header
    (right-aligned) belongs to that column. Summary rows outside the table keep phrase cells."""
    starts = [c.x0 for c in cols]
    buckets: dict[int, list] = {}
    for w in r.words:
        k = max((i for i, x in enumerate(starts) if x <= w[1] + 0.5), default=0)
        if k + 1 < len(cols) and re.match(r"^[\d,.%₹()-]+$", w[5]) and w[3] > starts[k + 1] + 0.5:
            k += 1
        buckets.setdefault(k, []).append(w)
    r.cells = [Cell(g[0][0], g[0][1], g[-1][3], r.yc, " ".join(w[5] for w in g)) for _, g in sorted(buckets.items())]


def _gap(a: Cell, x0: float, x1: float) -> float:
    if a.x1 < x0:
        return x0 - a.x1
    if a.x0 > x1:
        return a.x0 - x1
    return -min(a.x1, x1) + max(a.x0, x0)  # negative overlap: more overlap = smaller


def _is_header(r: Row) -> bool:
    has_desc = any(DESC_HDR.search(c.text) and not is_money_cell(c.text) for c in r.cells)
    has_amt = any(AMT_HDR.search(c.text) for c in r.cells if not DESC_HDR.search(c.text) or AMT_HDR.search(c.text.split()[-1]))
    return has_desc and has_amt and not any(is_money_cell(c.text) for c in r.cells)


@dataclass
class Header:
    cols: list[Cell]
    desc: int

    def col_of(self, c: Cell) -> int:
        """Text cells: the header box they overlap most (or are nearest to). Figures: the header whose
        box contains the figure's right edge (right-aligned under the header); otherwise columns
        partition the row at the header cells' left edges (left-aligned labels, figures ending before
        the next column starts) and the figure belongs to the column it overlaps most."""
        if not (is_money_cell(c.text) or lead_int(c.text) is not None):
            return min(range(len(self.cols)), key=lambda i: _gap(c, self.cols[i].x0, self.cols[i].x1))
        hit = [i for i, h in enumerate(self.cols) if h.x0 - 2 <= c.x1 <= h.x1 + 2]
        if len(hit) == 1:
            return hit[0]
        xs = [h.x0 for h in self.cols]
        best, bo = 0, -1e18
        for k in range(len(xs)):
            lo = xs[k] if k else -1e9
            hi = xs[k + 1] if k + 1 < len(xs) else 1e9
            o = min(c.x1, hi) - max(c.x0, lo)
            if o > bo:
                best, bo = k, o
        return best

    def desc_span(self) -> tuple[float, float]:
        d = self.cols[self.desc]
        nxt = [h.x0 for h in self.cols if h.x0 > d.x1 - 1]
        prv = [h.x1 for h in self.cols if h.x1 < d.x0 + 1]
        return (max(prv) + 1 if prv else 0.0), (min(nxt) - 1 if nxt else 1e9)

    def in_desc(self, c: Cell) -> bool:
        lo, hi = self.desc_span()
        return c.x0 >= lo - 2 and c.x1 <= hi + 2


def _continuation(rows: list[Row], j: int, ref: Row) -> bool:
    r = rows[j]
    return r.page == ref.page and r.yc - rows[j - 1].yc < 1.8 * ref.h and \
        not any(is_money_cell(c.text) or lead_int(c.text) is not None for c in r.cells)


def _header(rows: list[Row], i: int) -> Header:
    cols = [Cell(c.page, c.x0, c.x1, c.y, c.text) for c in rows[i].cells]
    j = i + 1
    while j < len(rows) and _continuation(rows, j, rows[i]):
        for c in rows[j].cells:
            k = min(range(len(cols)), key=lambda n: _gap(c, cols[n].x0, cols[n].x1))
            if _gap(c, cols[k].x0, cols[k].x1) <= 6:
                cols[k] = Cell(c.page, min(cols[k].x0, c.x0), max(cols[k].x1, c.x1), cols[k].y, cols[k].text + " " + c.text)
        j += 1
    pref = [n for n, c in enumerate(cols) if re.search(r"description|particulars", c.text, re.I)]
    desc = pref[0] if pref else next(n for n, c in enumerate(cols) if DESC_HDR.search(c.text))
    return Header(cols, desc)


def _aligned(c: Cell, h: Cell) -> bool:  # kept for callers; column assignment does the alignment
    return True


def _label(row: Row, hdr: Header, amt_cell: Cell) -> str:
    """Label of a line row: for a summary line, the phrase right before the figure ("CGST @ 9%",
    "Sub Total"); otherwise the text in the description column."""
    allleft = [c for c in row.cells if c.x1 <= amt_cell.x0 + 1 and not is_money_cell(c.text)]
    phrase: list[Cell] = []
    for c in reversed(allleft):
        if phrase and phrase[0].x0 - c.x1 > max(3 * _cw(phrase[0]), row.h):
            break
        phrase.insert(0, c)
    ptxt = " ".join(c.text for c in phrase).strip().rstrip(":").strip()
    if ptxt and re.search(r"[A-Za-z]", ptxt) and (TAX_RE.search(ptxt) or SUMMARYISH.search(ptxt) or TOTAL_LBL.search(ptxt)):
        return ptxt
    txt = [c.text for c in row.cells if c is not amt_cell and hdr.in_desc(c) and not is_money_cell(c.text) and not _numonly(c.text)]
    if txt:
        return " ".join(txt)
    left = [c for c in allleft if re.search(r"[A-Za-z]", c.text)]
    return left[-1].text if left else ""


def _summary(lines: list[dict], label: str, amt: int | None, total: int) -> str:
    """'done' at the total, 'ok' when the label/figure was absorbed, 'fail' when it cannot be placed."""
    if amt is None:
        return "fail"
    running = sum(x["amount_paise"] for x in lines)
    if amt == total and TOTAL_LBL.search(label) and not re.search(r"sub\s*-?\s*total|taxable|tax'?ble", label, re.I):
        return "done" if running == total else "fail"
    if SUBTOTAL_LBL.search(label) or (re.match(r"^\s*total\b", label, re.I) and amt == running):
        return "ok" if amt == running else "fail"
    if amt == 0:
        return "ok"
    if DISCOUNT_LBL.search(label) and amt > 0:
        amt = -amt
    lines.append({"description": label.strip().rstrip(":").strip(), "amount_paise": amt, "is_tax": bool(TAX_RE.search(label))})
    return "ok"


def _cols(hdr: Header, amt_idx: int) -> tuple[int | None, int | None]:
    q = next((n for n, c in enumerate(hdr.cols) if QTY_HDR.search(c.text) and n != hdr.desc), None)
    r = next((n for n, c in enumerate(hdr.cols) if RATE_HDR.search(c.text) and n not in (hdr.desc, amt_idx)), None)
    return q, r


def _try_column(rows: list[Row], start: int, hdr0: Header, amt_idx: int, total: int) -> list[dict]:
    hdr = hdr0
    amt_hdr_text = hdr.cols[amt_idx].text
    qcol, rcol = _cols(hdr, amt_idx)
    lines: list[dict] = []
    items: list[dict] = []
    loose: list[tuple] = []
    in_summary = False
    done = False
    i = start
    while i < len(rows):
        r = rows[i]
        if _is_header(r):  # repeated header on a later page
            hdr = _header(rows, i)
            idx = next((n for n, c in enumerate(hdr.cols) if c.text == amt_hdr_text), None)
            if idx is None:
                return []
            amt_idx = idx
            qcol, rcol = _cols(hdr, amt_idx)
            i += 1
            while i < len(rows) and _continuation(rows, i, r):
                i += 1
            continue
        if FORWARD_LBL.search(r.text()):
            i += 1
            continue
        moneys = [c for c in r.cells if is_money_cell(c.text)]
        prev = rows[i - 1] if i > start else None
        # horizontal summary: a row of labels directly above a row of figures
        if len(moneys) >= 3 and prev is not None and prev.page == r.page and r.yc - prev.yc < 3 * r.h and \
                len([c for c in prev.cells if not is_money_cell(c.text)]) >= 3 and not any(is_money_cell(c.text) for c in prev.cells) \
                and not any(hdr.in_desc(c) and not is_money_cell(c.text) for c in r.cells):
            in_summary = True
            for m in moneys:
                lab = min(prev.cells, key=lambda c: abs((c.x0 + c.x1) / 2 - (m.x0 + m.x1) / 2)).text
                res = _summary(lines, lab, money(m.text), total)
                if res == "fail":
                    return []
                if res == "done":
                    done = True
                    break
            if done:
                break
            i += 1
            continue
        amt_cells = [c for c in moneys if hdr.col_of(c) == amt_idx]
        if not amt_cells:
            in_desc = [c for c in r.cells if hdr.in_desc(c) and not is_money_cell(c.text)]
            if moneys and (in_summary or lines) and not in_desc:
                # a summary figure printed under another column: rightmost figure, summary-style label
                lab = _label(r, hdr, moneys[-1])
                if lab and (TAX_RE.search(lab) or SUMMARYISH.search(lab) or TOTAL_LBL.search(lab)):
                    amt_cells = [moneys[-1]]
            if not amt_cells:
                if not in_summary:
                    tx = [c for c in in_desc if not _numonly(c.text)]
                    q = None
                    if qcol is not None:
                        qc = [c for c in r.cells if hdr.col_of(c) == qcol and lead_int(c.text) is not None and _aligned(c, hdr.cols[qcol])]
                        q = lead_int(qc[0].text) if qc else None
                    if tx or q is not None:
                        loose.append((r, " ".join(c.text for c in tx), q, max((c.x1 for c in tx), default=0.0)))
                i += 1
                continue
        a = amt_cells[-1]
        amt = money(a.text)
        lab = _label(r, hdr, a)
        if not lab and in_summary:
            return []
        summaryish = bool(lab) and bool(TOTAL_LBL.search(lab) or SUBTOTAL_LBL.search(lab) or TAX_RE.search(lab))
        # an item row: text in the description column, or (multi-line cells) no label on the figure's own
        # line, in which case the description must come from the wrapped lines around it
        is_item = not in_summary and not summaryish and \
            (not lab or any(hdr.in_desc(c) for c in r.cells if c is not a and not is_money_cell(c.text)))
        if is_item:
            dcells = [c for c in r.cells if c is not a and hdr.in_desc(c) and not is_money_cell(c.text) and not _numonly(c.text)]
            row = {"description": lab, "amount_paise": amt, "is_tax": False, "_row": r, "_hi": hdr.desc_span()[1],
                   "_parts": [(r.yc, lab, max((c.x1 for c in dcells), default=0.0))] if lab else [],
                   "_cw": min((_cw(c) for c in r.cells), default=5.0)}
            if qcol is not None and rcol is not None:
                qc = [c for c in r.cells if hdr.col_of(c) == qcol and lead_int(c.text) is not None and _aligned(c, hdr.cols[qcol])]
                rc = [c for c in r.cells if hdr.col_of(c) == rcol and money(c.text) is not None and _aligned(c, hdr.cols[rcol])]
                if qc:
                    row["_q"] = lead_int(qc[0].text)
                if rc:
                    row["unit_price_minor"] = money(rc[0].text)
            lines.append(row)
            items.append(row)
        else:
            in_summary = True
            res = _summary(lines, lab, amt, total)
            if res == "fail":
                return []
            if res == "done":
                done = True
                break
        i += 1
    if not done:
        return []
    for r, txt, q, x1 in loose:  # wrapped description lines and quantities on another line of the item
        cands = [it for it in items if it["_row"].page == r.page and abs(it["_row"].yc - r.yc) < 2.4 * r.h]
        if not cands:
            continue
        it = min(cands, key=lambda it: abs(it["_row"].yc - r.yc))
        if txt:
            it["_parts"].append((r.yc, txt, x1))
        if q is not None and "_q" not in it:
            it["_q"] = q
    out = []
    for ln in lines:
        if "_row" in ln:
            parts = sorted(ln["_parts"])
            if not parts:
                return []
            # a fragment that runs to the right edge of the description column is a hard wrap of a
            # fixed-width layout ("Grade) 2" / "5 kg bag"): joined without a space; else a word wrap
            desc = parts[0][1]
            for (_, _, px1), (_, t, _) in zip(parts, parts[1:]):
                desc += ("" if px1 >= ln["_hi"] - 1.5 * ln["_cw"] else " ") + t
            ln["description"] = desc
            ln["fragments"] = [t for _, t, _ in parts]
            if "_q" in ln and "unit_price_minor" in ln:
                ln["qty"] = ln["_q"]
            else:
                ln.pop("unit_price_minor", None)
        out.append({k: v for k, v in ln.items() if not k.startswith("_")})
    if not out or sum(x["amount_paise"] for x in out) != total:
        return []
    return out


def geometry_lines(ws, total: int | None) -> list[dict]:
    if total is None or not ws:
        return []
    rows = rows_of(ws)
    hi = next((i for i, r in enumerate(rows) if _is_header(r)), None)
    if hi is None:
        return []
    if is_monospace(ws):
        hcols = mono_header_cells(rows[hi])
        rows[hi].cells = hcols
        for r in rows[hi + 1:]:
            if _is_header(r):
                hcols = mono_header_cells(r)
                r.cells = hcols
            else:
                recell(r, hcols)
    hdr = _header(rows, hi)
    start = hi + 1
    while start < len(rows) and _continuation(rows, start, rows[hi]):
        start += 1
    cands = [n for n, c in enumerate(hdr.cols) if n != hdr.desc and AMT_HDR.search(c.text)]
    cands.sort(key=lambda n: (0 if re.search(r"taxable", hdr.cols[n].text, re.I) else 1, -hdr.cols[n].x1))
    for n in cands:
        got = _try_column(rows, start, hdr, n, total)
        if got:
            return got
    return []


# --- verification against the signed text layer -----------------------------------------------
_NUM_RE = re.compile(r"(?<![\d.,])\d[\d,]*(?:\.\d+)?")


def text_amounts(text: str) -> set[int]:
    """Every number in the text in minor units, under canonical numeric normalisation (grouping
    commas and currency marks do not matter; an invalid grouping is read as separate numbers)."""
    out = set()
    for m in _NUM_RE.finditer(text):
        s = m.group(0).rstrip(",.")
        ip, _, fp = s.partition(".")
        groups = [ip] if _valid_grouping(ip) else ip.split(",")
        for k, g in enumerate(groups):
            if not g:
                continue
            frac = fp if k == len(groups) - 1 else ""
            if frac and set(frac[2:]) - {"0"}:
                continue
            out.add(int(g.replace(",", "")) * 100 + int(frac[:2].ljust(2, "0") or 0))
    return out


def _compact(s: str) -> str:
    return re.sub(r"\s+", "", s or "").lower()


def verified_in_text(lines: list[dict], text: str) -> bool:
    """Every amount (absolute), quantity and rate is printed in the text layer under canonical
    numeric normalisation; every description fragment is printed (whitespace ignored); qty x rate
    gives the amount or more (a printed line discount bills less, never more)."""
    nums = text_amounts(text)
    ct = _compact(text)
    for ln in lines:
        if abs(ln["amount_paise"]) not in nums:
            return False
        for f in ln.get("fragments") or [ln["description"]]:
            cf = _compact(f)
            if len(cf) < 2 or cf not in ct:
                return False
        if "qty" in ln:
            if ln["qty"] * 100 not in nums or ln["unit_price_minor"] not in nums:
                return False
            if ln["qty"] * ln["unit_price_minor"] + ln["qty"] < ln["amount_paise"]:
                return False
    return True


# --- GST heads printed as table columns ----------------------------------------------------------
_HEAD_TOKEN = re.compile(r"(?<![A-Za-z])(CGST|SGST|UTGST|IGST)(?![A-Za-z])", re.I)


def tax_heads_words(ws) -> tuple[set[str], set[str]]:
    """GST heads that the word boxes show as charged at zero only: a head printed as a column header
    whose figure in that column is 0.00 (e.g. a "CGST | SGST | IGST" tax summary with IGST 0.00),
    and never printed with a non-zero figure on its own line or in its column; plus the heads shown
    with a non-zero figure. Returns (zero-only heads, charged heads). The text-order heuristic
    (`extract.tax_kinds`) counts the next figure in reading order, which on a column layout belongs
    to another column."""
    rows = rows_of(ws)
    zero: set[str] = set()
    charged: set[str] = set()
    for i, r in enumerate(rows):
        heads = [(c, m.group(1).upper()) for c in r.cells for m in [_HEAD_TOKEN.search(c.text)] if m]
        if not heads:
            continue
        colheads = []
        for c, h in heads:  # inline: "CGST @ 9% : 840.00" (only rate/colon tokens between head and figure)
            k = r.cells.index(c)
            val = None
            for c2 in r.cells[k + 1:]:
                if is_money_cell(c2.text):
                    val = money(c2.text)
                    break
                if not re.fullmatch(r"[@:%\d.()\s-]*", c2.text):
                    break
            if val is None and re.search(r"[@:]\s*[\d.]*\s*%?\s*[:]?\s*$", c.text) is None and is_money_cell(c.text.split()[-1]):
                val = money(c.text.split()[-1])
            if val is None:
                colheads.append((c, h))
            else:
                (charged if val else zero).add(h)
        if not colheads:
            continue
        hdr = Header(cols=r.cells, desc=0)
        for r2 in rows[i + 1:i + 5]:
            if r2.page != r.page or r2.yc - r.yc > 6 * r.h:
                break
            vals = [m for m in r2.cells if is_money_cell(m.text)]
            if len(vals) < 2:
                continue
            for c, h in colheads:
                col = r.cells.index(c)
                mine = [m for m in vals if hdr.col_of(m) == col]
                if mine:
                    (charged if any(money(m.text) for m in mine) else zero).add(h)
            break
    return zero - charged, charged


def zero_tax_heads(ws) -> set[str]:
    return tax_heads_words(ws)[0]
