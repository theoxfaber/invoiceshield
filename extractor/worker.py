"""Sandboxed extraction worker: `python -m extractor.worker <dir>`.

Runs inside the sandbox started by extractor/sandbox.py (rlimits, no network namespace,
empty environment). Reads <dir>/doc.pdf and <dir>/req.json, prints one JSON object on stdout:
{"ok": true, "evidence": {...}, "pages": n} or {"ok": false, "failure": "<reason>"}.
Never trusted to sign anything: it has no key and its output is re-validated by the parent.
"""
from __future__ import annotations

import json
import socket
import sys
from pathlib import Path


def _no_network(*_a, **_k):
    raise OSError("network disabled in extraction sandbox")


def main() -> int:
    # Defence in depth on top of the network namespace: no sockets from Python code.
    socket.socket = _no_network  # type: ignore[assignment]
    socket.create_connection = _no_network  # type: ignore[assignment]
    d = Path(sys.argv[1])
    req = json.loads((d / "req.json").read_text())
    pdf = d / "doc.pdf"
    try:
        from pypdf import PdfReader

        reader = PdfReader(str(pdf), strict=False)
        # Cheap check of the declared page count before walking the page tree.
        try:
            declared = int(reader.trailer["/Root"]["/Pages"].get("/Count", 0))
        except Exception:
            declared = 0
        if declared > req["max_pages"]:
            print(json.dumps({"ok": False, "failure": f"too_many_pages:{declared}>{req['max_pages']}"}))
            return 0
        n = len(reader.pages)
        if n > req["max_pages"]:
            print(json.dumps({"ok": False, "failure": f"too_many_pages:{n}>{req['max_pages']}"}))
            return 0
        if n == 0:
            print(json.dumps({"ok": False, "failure": "no_pages"}))
            return 0
        from assembler.evidence import assemble
        from assembler.extract import governor_evidence

        ev = assemble(pdf, req.get("cover_email") or {})
        out = governor_evidence(ev, req["buyer_gstin"])
        print(json.dumps({"ok": True, "evidence": out, "pages": ev.pages}))
    except MemoryError:
        print(json.dumps({"ok": False, "failure": "memory_limit"}))
    except RecursionError:
        print(json.dumps({"ok": False, "failure": "recursion_limit"}))
    except Exception as e:  # malformed document: never a crash, always a REVIEW upstream
        print(json.dumps({"ok": False, "failure": f"parse_error:{type(e).__name__}"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
