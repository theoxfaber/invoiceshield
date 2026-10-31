"""Draw Vision Witness findings on the invoice image (post-freeze, feat/meta-vision).

Red boxes = BLOCK findings, amber = REVIEW, green = the visible issuer/total the witness read (context
only). Boxes are in the witness's 0..1000 coordinates. Pure Pillow, no network.
"""
from __future__ import annotations

import io

from PIL import Image, ImageDraw, ImageFont

RED, AMBER, GREEN = (225, 29, 72), (217, 119, 6), (22, 163, 74)


def _font(size: int):
    for p in ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", "DejaVuSans-Bold.ttf"):
        try:
            return ImageFont.truetype(p, size)
        except OSError:
            continue
    return ImageFont.load_default()


def to_px(box, w: int, h: int) -> tuple[int, int, int, int]:
    x0, y0, x1, y1 = box
    return int(x0 * w / 1000), int(y0 * h / 1000), int(x1 * w / 1000), int(y1 * h / 1000)


def draw(image: bytes, findings: list[dict], *, context: list[dict] | None = None, max_side: int = 1400) -> bytes:
    """Return a PNG with every finding that has a box drawn and labelled."""
    im = Image.open(io.BytesIO(image)).convert("RGB")
    if max(im.size) > max_side:
        im.thumbnail((max_side, max_side))
    w, h = im.size
    over = Image.new("RGBA", im.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(over)
    lw = max(3, w // 300)
    f = _font(max(14, w // 55))
    items = [(c, GREEN, c.get("label", "")) for c in (context or []) if c.get("box")]
    items += [(x, RED if x.get("severity") == "block" else AMBER, x.get("kind", "").replace("vision_", "").replace("_", " ").upper())
              for x in findings if x.get("box")]
    for it, col, label in items:
        x0, y0, x1, y1 = to_px(it["box"], w, h)
        pad = lw * 2
        x0, y0, x1, y1 = max(0, x0 - pad), max(0, y0 - pad), min(w - 1, x1 + pad), min(h - 1, y1 + pad)
        d.rectangle([x0, y0, x1, y1], fill=col + (38,), outline=col + (255,), width=lw)
        if label:
            tw = d.textlength(label, font=f)
            th = f.size + 8
            ly = y0 - th if y0 - th > 0 else y1
            d.rectangle([x0, ly, x0 + tw + 12, ly + th], fill=col + (235,))
            d.text((x0 + 6, ly + 3), label, font=f, fill=(255, 255, 255, 255))
    out = Image.alpha_composite(im.convert("RGBA"), over).convert("RGB")
    buf = io.BytesIO()
    out.save(buf, "PNG", optimize=True)
    return buf.getvalue()
