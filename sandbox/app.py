"""Public sandbox ("Fool the Shield", v3 prep 2026-10-31): try to get a payment out of the gate.

    python -m sandbox.app                      # http://127.0.0.1:8000  (PORT env overrides)

What it is: a small stdlib HTTP front in front of the real pipeline, with SYNTHETIC vendor data
only (dataset/fixtures/master.json, all fictional). A visitor uploads an invoice PDF (or picks one
of the synthetic samples); the front plays a fooled AP agent that pays whatever the document asks
(vendor by printed GSTIN/name, printed PO, printed total, printed invoice number), runs the real
sandboxed extraction service (Ed25519-signed evidence), optionally asks Muse Spark Open for item
lines and the quarantined checker, and the real Rust governor decides PAY / REVIEW / BLOCK.
Nothing is ever paid: there is no rail. On PAY the payee is the vendor master's beneficiary id.

Hard caps (all env-configurable, see docs/SANDBOX.md):
  * per-IP token bucket (SANDBOX_IP_PER_MIN, burst SANDBOX_IP_BURST) and per-IP daily cap
    (SANDBOX_IP_PER_DAY);
  * global daily request cap (SANDBOX_DAILY_CAP); past it every request gets HTTP 429;
  * model call caps: a daily cap (SANDBOX_MODEL_CALLS_PER_DAY) and a LIFETIME cap derived from a
    spend ceiling: floor(SANDBOX_SPEND_CAP_USD / SANDBOX_USD_PER_MODEL_CALL) calls, persisted in the
    state file so a restart does not reset it. The spend ceiling is clamped below $10 (HARD_SPEND_CEILING_USD).
    Past either cap no model is called and the request goes on without a checker report, which the
    governor answers with REVIEW (fail closed);
  * upload size (SANDBOX_MAX_BYTES, default 2 MB) and the extraction sandbox's own page/CPU limits.
The governor API key is generated per boot inside the process and never leaves it; the model key
(BASETEN_API_KEY) is read from the server environment only. The governor runs in eval mode with an
in-memory ledger that is reset before every attempt (one lock), so attempts are independent.

Public counter: GET /api/counter -> {"attempts", "paid", "paid_to_wrong_account", ...}.
"paid_to_wrong_account" counts PAY decisions whose payee is not the master beneficiary of the
proposed vendor; by construction the gate has no way to pay anyone else, so it is expected to stay 0,
and the counter exists so anyone can watch it.
"""
from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import re
import threading
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
MASTER_PATH = Path(os.environ.get("SANDBOX_MASTER", REPO / "dataset" / "fixtures" / "master.json"))
STATE = Path(os.environ.get("SANDBOX_STATE", "/tmp/sandbox-counter.json"))
SAMPLES = {
    # synthetic samples shipped in the repo (fictional vendors)
    "genuine-1": REPO / "dataset" / "clean" / "C01.pdf",
    "genuine-2": REPO / "dataset" / "clean" / "C10.pdf",
    "bank-swap": REPO / "eval" / "riskset" / "X04.pdf",
    "prompt-injection": REPO / "eval" / "riskset" / "L02.pdf",
    "lookalike-ifsc": REPO / "eval" / "riskset" / "X_ifsc.pdf",
}


def env_int(name: str, default: int) -> int:
    try:
        return int(os.environ.get(name, default))
    except ValueError:
        return default


IP_PER_MIN = env_int("SANDBOX_IP_PER_MIN", 4)
IP_BURST = env_int("SANDBOX_IP_BURST", 4)
IP_PER_DAY = env_int("SANDBOX_IP_PER_DAY", 40)
DAILY_CAP = env_int("SANDBOX_DAILY_CAP", 500)
MODEL_CALLS_PER_DAY = env_int("SANDBOX_MODEL_CALLS_PER_DAY", 100)


def env_float(name: str, default: float) -> float:
    try:
        v = float(os.environ.get(name, default))
        return v if v == v and v >= 0 else default  # NaN/negative -> default
    except ValueError:
        return default


# Spend guard. The per-call figure is a deliberately pessimistic estimate (a checker call or a line
# proposal with page images, up to 6000 output tokens); check it against the provider's bill and lower
# SANDBOX_SPEND_CAP_USD if real calls cost more. The ceiling can only be lowered, never raised past $10.
HARD_SPEND_CEILING_USD = 9.50
SPEND_CAP_USD = min(env_float("SANDBOX_SPEND_CAP_USD", 9.00), HARD_SPEND_CEILING_USD)
USD_PER_MODEL_CALL = max(env_float("SANDBOX_USD_PER_MODEL_CALL", 0.02), 0.001)
MODEL_CALLS_TOTAL = int(SPEND_CAP_USD / USD_PER_MODEL_CALL)
MAX_BYTES = env_int("SANDBOX_MAX_BYTES", 2 * 1024 * 1024)
TRUST_PROXY = os.environ.get("SANDBOX_TRUST_PROXY", "0") == "1"
USE_MODEL = os.environ.get("SANDBOX_USE_MODEL", "1") == "1" and bool(os.environ.get("BASETEN_API_KEY"))
MAX_TRACKED_IPS = 50_000


def today() -> str:
    return dt.datetime.now(dt.timezone.utc).date().isoformat()


class Limits:
    """Per-IP token bucket + per-IP and global daily counters + daily model-call budget.
    In memory (per process); the day rolls over at 00:00 UTC. Bounded: when the IP table is full,
    new IPs are refused (fail closed) until the day rolls over."""

    def __init__(self):
        self.lock = threading.Lock()
        self.day = today()
        self.buckets: dict[str, tuple[float, float]] = {}
        self.ip_day: dict[str, int] = {}
        self.global_day = 0
        self.model_calls = 0

    def _roll(self):
        d = today()
        if d != self.day:
            self.day, self.ip_day, self.global_day, self.model_calls = d, {}, 0, 0
            self.buckets.clear()

    def admit(self, ip: str) -> str | None:
        """None if admitted, else the reason for a 429."""
        with self.lock:
            self._roll()
            if self.global_day >= DAILY_CAP:
                return "the sandbox's daily request cap is reached; try again tomorrow (00:00 UTC)"
            if ip not in self.ip_day and len(self.ip_day) >= MAX_TRACKED_IPS:
                return "too many visitors today; try again tomorrow"
            if self.ip_day.get(ip, 0) >= IP_PER_DAY:
                return "your daily limit for this sandbox is reached"
            tokens, last = self.buckets.get(ip, (float(IP_BURST), time.monotonic()))
            now = time.monotonic()
            tokens = min(float(IP_BURST), tokens + (now - last) * IP_PER_MIN / 60.0)
            if tokens < 1.0:
                self.buckets[ip] = (tokens, now)
                return "slow down: a few attempts per minute per visitor"
            self.buckets[ip] = (tokens - 1.0, now)
            self.ip_day[ip] = self.ip_day.get(ip, 0) + 1
            self.global_day += 1
            return None

    def take_model_calls(self, n: int, counter: "Counter") -> bool:
        """Reserve n model calls against the daily and the lifetime (spend) cap; False = do not call."""
        with self.lock:
            self._roll()
            if self.model_calls + n > MODEL_CALLS_PER_DAY:
                return False
            if not counter.reserve_lifetime_calls(n, MODEL_CALLS_TOTAL):
                return False
            self.model_calls += n
            return True

    def snapshot(self) -> dict:
        with self.lock:
            self._roll()
            return {"day_utc": self.day, "requests_today": self.global_day, "daily_cap": DAILY_CAP,
                    "model_calls_today": self.model_calls, "model_calls_cap": MODEL_CALLS_PER_DAY,
                    "model_calls_lifetime_cap": MODEL_CALLS_TOTAL, "spend_cap_usd": SPEND_CAP_USD,
                    "usd_per_model_call_estimate": USD_PER_MODEL_CALL}


class Counter:
    FIELDS = ("attempts", "paid", "review", "block", "paid_to_wrong_account", "foreign_bank_details_printed",
              "foreign_bank_details_paid_to_master", "model_capped",
              "model_calls_total")

    def __init__(self, path: Path):
        self.path = path
        self.lock = threading.Lock()
        self.c = {k: 0 for k in self.FIELDS}
        self.c["since"] = dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")
        try:
            saved = json.loads(path.read_text())
            self.c.update({k: v for k, v in saved.items() if k in self.c})
        except (OSError, ValueError):
            pass

    def add(self, **kw):
        with self.lock:
            for k, v in kw.items():
                self.c[k] = self.c.get(k, 0) + v
            try:
                self.path.write_text(json.dumps(self.c))
            except OSError:
                pass

    def reserve_lifetime_calls(self, n: int, cap: int) -> bool:
        with self.lock:
            if self.c.get("model_calls_total", 0) + n > cap:
                return False
            self.c["model_calls_total"] = self.c.get("model_calls_total", 0) + n
            try:
                self.path.write_text(json.dumps(self.c))
            except OSError:
                return False  # cannot persist the spend count: fail closed, no model call
            return True

    def get(self) -> dict:
        with self.lock:
            return dict(self.c)


def fold(s: str) -> str:
    return re.sub(r"\s+", " ", (s or "").lower()).strip()


def fooled_proposal(master: dict, ev: dict) -> dict:
    """The agent pays what the document asks (same mapping as tools/heldout_eval.v2_proposal)."""
    from assembler.fields import printed_invoice_no
    text = ev.get("text_layer") or ""
    ft = fold(text)
    vend = None
    g = (ev.get("text_layer_gstin") or "").upper()
    if g:
        vend = next((v["vendor_id"] for v in master["vendors"] if (v.get("gstin") or "").upper() == g), None)
    if vend is None:
        named = [v for v in master["vendors"] if fold(v["legal_name"]) in ft]
        vend = max(named, key=lambda v: len(v["legal_name"]))["vendor_id"] if named else None
    nospace = re.sub(r"\s+", "", text)
    hits = [p for p in master["pos"] if re.sub(r"\s+", "", p["po_ref"]) in nospace]
    po = max(hits, key=lambda p: len(p["po_ref"])) if hits else None
    if vend is None and po is not None:
        vend = po["owner_vendor_id"]
    amt = ev.get("printed_total_paise")
    return {"vendor_id": vend or "UNKNOWN", "invoice_no": printed_invoice_no(text) or "UNREAD",
            "amount_paise": int(amt) if amt else 1, "currency": ev.get("currency") or "INR",
            "po_ref": po["po_ref"] if po else "PO-NOT-PRINTED"}


class Pipeline:
    def __init__(self):
        from shield.client import ExtractorProc, GovernorProc
        self.master = json.loads(MASTER_PATH.read_text())
        self.ex = ExtractorProc()
        self.gov = GovernorProc(MASTER_PATH, extractor=self.ex)
        self.lock = threading.Lock()
        self.provider = None
        if USE_MODEL:
            from providers.llm import get_provider
            self.provider = get_provider("spark")

    def run(self, pdf: bytes, limits: Limits, counter: Counter) -> dict:
        from extractor.line_proposer import needs_model_lines, propose_lines, render_pages
        env = self.ex.extract(pdf, {})
        ev = json.loads(env["payload"]).get("evidence") or {}
        prop = fooled_proposal(self.master, ev)
        lines, checker_raw, capped = None, None, False
        if self.provider is not None:
            need = 2 if needs_model_lines(ev) else 1
            if limits.take_model_calls(need, counter):
                from assembler.evidence import Evidence
                from checker.checker import run_checker
                text = ev.get("text_layer") or ""
                if need == 2:
                    lp = propose_lines(self.provider, text, images=render_pages(pdf))
                    lines = lp.lines if lp.ok else None
                asm = Evidence(pdf_sha256=hashlib.sha256(pdf).hexdigest(), pages=1, text=text)
                checker_raw = run_checker(self.provider, asm, sample=0, reasoning="low").raw_for_governor
            else:
                capped = True  # no checker report -> the governor answers REVIEW (checker_missing)
        with self.lock:
            self.gov.reset()
            d = self.gov.evaluate_signed(prop, self.ex.extract(pdf, {}), checker_raw, hashlib.sha256(pdf).hexdigest(),
                                         now=int(time.time()), proposed_item_lines=lines)
        vend = next((v for v in self.master["vendors"] if v["vendor_id"] == prop["vendor_id"]), None)
        payee = (d.get("payee") or {}).get("beneficiary_id")
        wrong = d["verdict"] == "allow" and (vend is None or payee != vend.get("beneficiary_id"))
        printed = sorted(set((ev.get("all_printed_accounts") or []) + ([ev["printed_bank_account"]] if ev.get("printed_bank_account") else [])))
        foreign = bool(vend) and any(re.sub(r"\D", "", a) != re.sub(r"\D", "", vend.get("bank_account", "")) for a in printed)
        return {"verdict": {"allow": "PAY", "review": "REVIEW", "block": "BLOCK"}[d["verdict"]],
                "reasons": [{"rule": r["rule"], "verdict": r["verdict"], "detail": r["detail"]} for r in d["reasons"]],
                "signals": d.get("signals", []), "tamper_alert": d.get("tamper_alert"),
                "proposal": prop, "payee_beneficiary_id": payee if d["verdict"] == "allow" else None,
                "model": {"used": checker_raw is not None, "capped": capped, "line_proposal_offered": lines is not None},
                "_wrong": wrong, "_foreign": foreign}


def multipart_file(body: bytes, ctype: str) -> bytes | None:
    m = re.search(r"boundary=([^;]+)", ctype or "")
    if not m:
        return None
    boundary = b"--" + m.group(1).strip().strip('"').encode()
    for part in body.split(boundary):
        head, sep, data = part.partition(b"\r\n\r\n")
        if sep and b'name="file"' in head:
            return data.rsplit(b"\r\n", 1)[0]
    return None


class Handler(BaseHTTPRequestHandler):
    server_version = "InvoiceShieldSandbox/1"
    pipeline: Pipeline | None = None
    limits = Limits()
    counter = Counter(STATE)

    def log_message(self, fmt, *args):  # no request logging of IPs beyond what the platform does
        pass

    def client_ip(self) -> str:
        if TRUST_PROXY:
            xff = self.headers.get("X-Forwarded-For", "")
            if xff:
                return xff.split(",")[0].strip()
        return self.client_address[0]

    def send(self, code: int, obj, ctype="application/json"):
        data = obj if isinstance(obj, bytes) else json.dumps(obj, indent=1).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self'; frame-ancestors 'none'")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path in ("/", "/index.html"):
            return self.send(200, (REPO / "sandbox" / "index.html").read_bytes(), "text/html; charset=utf-8")
        if self.path == "/app.js":
            return self.send(200, (REPO / "sandbox" / "app.js").read_bytes(), "text/javascript; charset=utf-8")
        if self.path == "/api/counter":
            return self.send(200, {**self.counter.get(), **self.limits.snapshot(), "samples": sorted(SAMPLES)})
        if self.path == "/healthz":
            return self.send(200, {"ok": True})
        return self.send(404, {"error": "not found"})

    def do_POST(self):
        if not self.path.startswith("/api/try"):
            return self.send(404, {"error": "not found"})
        why = self.limits.admit(self.client_ip())
        if why:
            return self.send(HTTPStatus.TOO_MANY_REQUESTS, {"error": why})
        n = int(self.headers.get("Content-Length") or 0)
        if n > MAX_BYTES + 64 * 1024:
            return self.send(413, {"error": f"upload larger than {MAX_BYTES} bytes"})
        body = self.rfile.read(n) if n else b""
        m = re.search(r"[?&]sample=([a-z0-9-]+)", self.path)
        if m:
            p = SAMPLES.get(m.group(1))
            if p is None or not p.exists():
                return self.send(400, {"error": "unknown sample"})
            pdf = p.read_bytes()
        else:
            pdf = multipart_file(body, self.headers.get("Content-Type", "")) or b""
        if not pdf or len(pdf) > MAX_BYTES or b"%PDF-" not in pdf[:1024]:
            return self.send(400, {"error": "send one PDF (field name 'file', at most 2 MB) or ?sample=<name>"})
        try:
            out = self.pipeline.run(pdf, self.limits, self.counter)
        except Exception as e:  # noqa: BLE001 - any failure is reported, never a payment
            return self.send(500, {"error": f"pipeline error ({type(e).__name__}); nothing was paid"})
        v = out["verdict"]
        self.counter.add(attempts=1, paid=int(v == "PAY"), review=int(v == "REVIEW"), block=int(v == "BLOCK"),
                         paid_to_wrong_account=int(out.pop("_wrong")), foreign_bank_details_printed=int(out["_foreign"]),
                         foreign_bank_details_paid_to_master=int(out.pop("_foreign") and v == "PAY"),
                         model_capped=int(out["model"]["capped"]))
        return self.send(200, out)


def main() -> int:
    Handler.pipeline = Pipeline()
    port = env_int("PORT", 8000)
    srv = ThreadingHTTPServer((os.environ.get("SANDBOX_BIND", "0.0.0.0"), port), Handler)
    print(f"sandbox on :{port} (model {'on' if USE_MODEL else 'off'}; caps: {DAILY_CAP}/day, {MODEL_CALLS_PER_DAY} model calls/day, {MODEL_CALLS_TOTAL} model calls lifetime = ${SPEND_CAP_USD:.2f} at ${USD_PER_MODEL_CALL}/call)", flush=True)
    try:
        srv.serve_forever()
    finally:
        Handler.pipeline.gov.close()
        Handler.pipeline.ex.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
