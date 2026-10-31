# Meta photo-witness (feat/meta-vision)

**Status:** draft, post-freeze. Nothing here touches `freeze-v3`, `eval/sealed` or `eval/results`. These numbers are
from a separate post-freeze set and are not comparable with the sealed results.

## What it does

When an invoice arrives as a phone photo or a scan, Muse Spark Open (via Baseten) reads the page image and returns a
JSON "witness": payee, bank account, IFSC, total, plus any notices it sees, each with the exact text it read. Deterministic
code (`shield/vision_assess.py`) then compares that against the vendor master and raises **escalate-only** signals:
`vision_bank_mismatch`, `vision_payee_lookalike`, `vision_payment_instruction` / `vision_injected_instruction`,
`vision_amount_mismatch`. These signals reach the Rust governor only inside the signed checker report.

- The model never approves anything. A photo is REVIEW at best, and a witness signal can only raise the verdict.
  `governor/tests/vision_signals.rs` checks this: every vision signal, at every base verdict, gives `merge(base, s) >= base`.
  A signal that asks for `allow`, or one that is malformed, fails closed to `checker_unparseable` (REVIEW).
- Boxes drawn on the photo come from `shield/vision_locate.py`: each verbatim string is re-found on the image and
  boxed there, because the boxes the model returned were unusable (0/18 pinpointed raw).
- Model calls go through `shield/vision_budget.py` (one HTTP attempt per call, $5 lifetime and 50 calls/day caps,
  fails closed if the ledger can't be locked or written). Cached replays are free.
- The approval-claim rule is unchanged.

## Results: `eval/vision_postfreeze/v1/` (dataset `dataset/vision_v1`)

36 synthetic phone and scan images: 18 genuine and 18 attacks, n=6 for each attack class. **Synthetic, post-freeze,
small n.** Re-run offline from `eval/cache/vision` with 0 model calls and $0 paid.

| | frozen rules (governor) | tesseract OCR witness | rules + Spark Open witness |
|---|---|---|---|
| Faint printed instruction ("bank details changed…") | 1/6 blocked | 0/6 caught | **6/6 blocked** |
| Bank swap | 6/6 | 6/6 | 6/6 |
| Handwritten note | 6/6 | 6/6 | 6/6 |
| **All attacks blocked** | **13/18** | 12/18 caught | **18/18** |
| Genuine invoices blocked (false block) | 2/18 | 1/18 | **4/18** |
| Genuine invoices paid automatically | 0 | n/a | 0 (photos never auto-PAY) |

- The extra false blocks are **P20 and P23**. Both are genuine invoices carrying a benign handwritten
  "Approved for GRN" pen note, which the witness flags as a payment instruction. This is a real cost of the feature,
  and we have not tuned it away.
- Pinpointing: the witness's findings were boxed on the correct region in 12/18 attacks after re-anchoring
  (bank swap 6/6, handwritten 4/6, faint 2/6). The raw model boxes scored 0/18.
- Provenance: the ocr and vision arms were re-run offline on 2026-11-01. The gate arm (the real governor on each photo,
  sent as an image-only PDF) used the same cached witness outputs and ran around 2026-11-01 00:00 IST. Gate verdicts
  depend on signals only, not on boxes. See `summary.json` → `provenance`.

### Changed after seeing results (disclosed)

- **Box re-anchoring** (`shield/vision_locate.py`) was added after the first pass showed the model's boxes were unusable.
- **Witness parser fix:** `invoice_no` is now accepted as an int. On the first pass, case P04 failed to parse.
- On the first pass, the 36 eval calls sent full-size images. The hero and sandbox paths downscale to 1200 px.

## Honest caveat: what the hero shows

With the shield **off**, the Spark Open payment agent was **not fooled** (0/2): it rejected the faint-instruction
invoice and held the quiet bank swap. So the claim is **not** "the model resists the attack". The claim is narrower:
**the OCR missed the faint instruction and the model read it.** The witness turns what the model read into a
deterministic, escalate-only governor signal and a box on the photo. In the hero case (`ui/vision-hero/`), the frozen
rules also reach BLOCK through OCR (`bank_details_differ_from_master`). The witness adds the pinpointed reason
(bank mismatch, plus the "bank details have changed" instruction).

## Environment notes

- Sandbox runs used `GOVERNOR_ALLOW_NO_NETNS=1` because the sandbox has no network namespaces, so the extractor jail ran
  without netns isolation. Production and CI keep the netns jail.
- The generator's pixel noise uses PIL `effect_noise`, which is unseeded. Labels and geometry reproduce from the seed,
  but JPEG bytes do not. The committed images in `dataset/vision_v1` are the record. Regenerating them would invalidate
  `eval/cache/vision`.
- Not done: a sandbox `/api/vision` endpoint and a seeded-noise generator fix (both deferred).

## Reproduce (no model calls)

```
python -m tools.vision_eval --set dataset/vision_v1 --out eval/vision_postfreeze/v1 --arms ocr,vision --offline
python -m tools.vision_hero          # replays the cache; --live would make budgeted calls
```
