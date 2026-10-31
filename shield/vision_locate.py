"""Put the witness's boxes where the text really is (post-freeze, feat/meta-vision).

Measured on the post-freeze set (2026-10-31): Muse Spark Open READS the page very well, but its box
coordinates are compressed (about 0.8x horizontally and 0.7x vertically, varying per image). So the
boxes it returns are re-anchored by plain code:

1. local tesseract finds the printed lines it CAN read (letterhead, total, the genuine bank box);
2. items the witness also reported (issuer name, total, an account number) are matched to those lines
   by their text, giving anchor pairs (model box -> true box);
3. a per-axis linear map (scale + offset) is fitted to the anchors by least squares and applied to
   every witness box, including the ones OCR could not read (faint lines, handwriting).

Geometry only: this never adds, removes or changes a finding, its kind or its severity. With fewer than
two usable anchors the boxes are left as the model gave them and marked uncalibrated.
"""
from __future__ import annotations

import re

from shield.vision_ocr_baseline import tesseract_lines
from shield.vision_witness import Witness, clean_box


def _fold(s: str | None) -> str:
    return re.sub(r"[^a-z0-9]+", "", (s or "").lower())


def _digits(s: str | None) -> str:
    return re.sub(r"\D", "", s or "")


def anchors(w: Witness, lines: list[dict]) -> list[tuple[list[int], list[int]]]:
    """[(model_box, ocr_box)] for items found by both."""
    out = []
    if w.issuer_name and w.issuer_box:
        key = _fold(w.issuer_name)[:14]
        hit = [ln for ln in lines if key and key in _fold(ln["text"])]
        if hit:
            out.append((w.issuer_box, hit[0]["box"]))
    if w.total_minor and w.total_box:
        rupees = str(w.total_minor // 100)
        hit = [ln for ln in lines if rupees and rupees in _digits(ln["text"]) and re.search(r"total", ln["text"], re.I)]
        if hit:
            out.append((w.total_box, hit[0]["box"]))
    for b in w.bank_details:
        acct = b.get("account") or ""
        if len(acct) >= 9 and b.get("box"):
            hit = [ln for ln in lines if acct in _digits(ln["text"])]
            if len(hit) == 1:
                # the model's bank box covers the whole block; the OCR line is the account row only, so
                # only its vertical centre and left edge are comparable: use the line as a weak anchor
                mb, ob = b["box"], hit[0]["box"]
                out.append(([mb[0], (mb[1] + mb[3]) // 2 - 1, mb[0] + 2, (mb[1] + mb[3]) // 2 + 1],
                            [ob[0], (ob[1] + ob[3]) // 2 - 1, ob[0] + 2, (ob[1] + ob[3]) // 2 + 1]))
    return out


def _fit(pairs: list[tuple[float, float]]) -> tuple[float, float] | None:
    """Least-squares y = a*x + b; None when degenerate or implausible."""
    n = len(pairs)
    if n < 2:
        return None
    sx = sum(p[0] for p in pairs); sy = sum(p[1] for p in pairs)
    sxx = sum(p[0] * p[0] for p in pairs); sxy = sum(p[0] * p[1] for p in pairs)
    den = n * sxx - sx * sx
    if abs(den) < 1e-6:
        return None
    a = (n * sxy - sx * sy) / den
    b = (sy - a * sx) / n
    if not (0.5 <= a <= 2.0):
        return None
    return a, b


def _tokens(s: str) -> set[str]:
    return {t for t in re.findall(r"[a-z0-9]{3,}", (s or "").lower())}


def _union(boxes: list[list[int]]) -> list[int]:
    return [min(b[0] for b in boxes), min(b[1] for b in boxes), max(b[2] for b in boxes), max(b[3] for b in boxes)]


def snap(text: str, account: str | None, lines: list[dict], near: list[int] | None, ifsc: str | None = None) -> list[int] | None:
    """The box of the OCR line(s) that carry this item's text, or None when OCR cannot see it.
    An account number is matched by its digits (plus its IFSC line); a note by word overlap: at least 4
    shared words making up at least 60% of the OCR line's words."""
    hits = []
    if account and len(account) >= 9:
        hits = [ln for ln in lines if account in _digits(ln["text"])]
        if hits and ifsc:
            y = hits[0]["box"][1]
            hits += [ln for ln in lines if ifsc.upper() in ln["text"].upper().replace("O", "0").replace(ifsc[:4].replace("O", "0"), ifsc[:4])
                     and abs(ln["box"][1] - y) < 60 and ln not in hits]
    if not hits:
        want = _tokens(text)
        for ln in lines:
            have = _tokens(ln["text"])
            common = len(want & have)
            if have and common >= 4 and common >= 0.6 * len(have):
                hits.append(ln)
    if not hits:
        return None
    if near:  # several candidates: keep the ones closest to where the (calibrated) model box points
        cy = (near[1] + near[3]) / 2
        hits.sort(key=lambda ln: abs((ln["box"][1] + ln["box"][3]) / 2 - cy))
        top = hits[0]["box"]
        hits = [ln for ln in hits if abs(ln["box"][1] - top[1]) < 60]
    return _union([ln["box"] for ln in hits])


def calibrate(w: Witness, image: bytes, lines: list[dict] | None = None) -> tuple[Witness, dict]:
    """Return the witness with re-anchored boxes, and a small report."""
    if not w.ok:
        return w, {"calibrated": False, "anchors": 0}
    if lines is None:
        lines, _, _ = tesseract_lines(image)
    pairs = anchors(w, lines)
    xs = [(m[0], o[0]) for m, o in pairs] + [(m[2], o[2]) for m, o in pairs if m[2] - m[0] > 4]
    ys = [((m[1] + m[3]) / 2, (o[1] + o[3]) / 2) for m, o in pairs]
    fx, fy = _fit(xs), _fit(ys)
    if fx is None or fy is None:
        return w, {"calibrated": False, "anchors": len(pairs)}
    # vertical extent: scale heights by the y slope
    def mp(b):
        if not b:
            return b
        cy = (b[1] + b[3]) / 2
        hh = (b[3] - b[1]) / 2 * fy[0]
        ncy = fy[0] * cy + fy[1]
        return clean_box([fx[0] * b[0] + fx[1], ncy - hh, fx[0] * b[2] + fx[1], ncy + hh])
    w.issuer_box, w.total_box = mp(w.issuer_box), mp(w.total_box)
    snapped = 0
    for b in w.bank_details:
        b["box"] = mp(b.get("box"))
        sb = snap(b.get("text") or "", b.get("account"), lines, b["box"], b.get("ifsc"))
        if sb:
            b["box"], snapped = sb, snapped + 1
    for n in w.notices:
        n["box"] = mp(n.get("box"))
        sb = snap(n.get("text") or "", None, lines, n["box"])
        if sb:
            n["box"], snapped = sb, snapped + 1
    return w, {"calibrated": True, "anchors": len(pairs), "snapped": snapped, "fx": [round(v, 3) for v in fx], "fy": [round(v, 3) for v in fy]}


def extend_to_ink(box: list[int] | None, image: bytes, *, max_gap: float = 0.025, thr: int = 15) -> list[int] | None:
    """Widen a one-line box sideways to the ink actually printed on that row (geometry only).

    OCR often reads only the first half of a faint line, so a box snapped to the OCR line stops mid-text.
    This scans the row band of the image column by column (darkest pixel vs the column's 90th-percentile brightness, i.e. the paper) and
    grows the box left/right while ink continues with gaps no wider than ``max_gap`` of the image width
    (word spacing). It never shrinks the box or moves it vertically; on any problem it returns the box as is.
    Used by the hero render (tools/vision_hero.py); the measured eval does not call it.
    """
    if not box:
        return box
    try:
        import io

        from PIL import Image
        im = Image.open(io.BytesIO(image)).convert("L")
    except Exception:
        return box
    W, H = im.size
    px = im.load()
    y0, y1 = max(0, int(box[1] * H / 1000)), min(H - 1, int(box[3] * H / 1000))
    if y1 - y0 < 2:
        return box
    ink = []
    for x in range(W):
        col = sorted(px[x, y] for y in range(y0, y1 + 1))
        ink.append(col[(len(col) * 9) // 10] - col[0] > thr)
    gap = max(2, int(max_gap * W))
    x0, x1 = int(box[0] * W / 1000), min(W - 1, int(box[2] * W / 1000))

    def grow(x: int, step: int) -> int:
        last, miss = x, 0
        while 0 <= x + step < W and miss <= gap:
            x += step
            if ink[x]:
                last, miss = x, 0
            else:
                miss += 1
        return last

    nx0, nx1 = grow(x0, -1), grow(x1, 1)
    return clean_box([min(box[0], nx0 * 1000 / W), box[1], max(box[2], (nx1 + 1) * 1000 / W), box[3]])
