"""Screenshot the evidence console, the Fool the Shield page and the live approver page.

    make build && python3 -m ui.build.screenshots [--chromium /usr/bin/chromium]

Writes docs/screenshots/*.png. Needs playwright (pip) and a Chromium binary. The approver page is the
real one served by the governor, filled by ui.review_demo with recorded inputs.
"""
from __future__ import annotations

import argparse
import pathlib

from playwright.sync_api import sync_playwright

REPO = pathlib.Path(__file__).resolve().parents[2]
OUT = REPO / "docs" / "screenshots"
VIEW = {"width": 1440, "height": 900}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chromium", default="/usr/bin/chromium")
    a = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    errors: list[str] = []
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=a.chromium, args=["--no-sandbox"])
        pg = b.new_page(viewport=VIEW, device_scale_factor=1)
        pg.on("pageerror", lambda e: errors.append(str(e)))
        console = (REPO / "ui" / "console.html").as_uri()
        for sec in ("failclosed", "calls", "side", "approver", "results"):
            pg.goto(f"{console}#{sec}")
            pg.wait_for_timeout(400)
            pg.screenshot(path=str(OUT / f"console-{sec}.png"), full_page=True)
        pg.goto(f"{console}#calls")
        pg.click(".clist button:nth-child(4)")   # L02 checker call
        pg.wait_for_timeout(200)
        pg.screenshot(path=str(OUT / "console-calls-l02-checker.png"))
        pg.goto((REPO / "docs" / "fool-the-shield" / "index.html").as_uri())
        pg.wait_for_timeout(400)
        pg.screenshot(path=str(OUT / "fool-the-shield.png"), full_page=True)
        pg.click("button.cat >> text=prompt injection")
        pg.wait_for_timeout(300)
        pg.screenshot(path=str(OUT / "fool-the-shield-prompt-injection.png"), full_page=True)

        from ui.review_demo import start
        gov, key, rows, queued = start()
        try:
            pg.goto(f"{gov.base}/v1/ui/review")
            pg.screenshot(path=str(OUT / "approver-signin.png"))
            pg.fill("#key", key)
            pg.click("#keyform button")
            pg.wait_for_selector("#items li", timeout=10000)
            pg.wait_for_timeout(300)
            pg.screenshot(path=str(OUT / "approver-queue.png"), full_page=True)
        finally:
            gov.close()
        b.close()
    for f in sorted(OUT.glob("*.png")):
        print(f.relative_to(REPO))
    if errors:
        raise SystemExit("page errors: " + "; ".join(errors))


if __name__ == "__main__":
    main()
