"""Screen-capture the demo UI (real browser render, recorded cases). Usage: python3 -m ui.record_demo [outdir]"""
import sys, os
from playwright.sync_api import sync_playwright
out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/demo_rec"
html = os.path.abspath("ui/demo.html")
with sync_playwright() as p:
    b = p.chromium.launch(executable_path="/usr/bin/chromium", args=["--no-sandbox"])
    ctx = b.new_context(viewport={"width":1600,"height":900}, record_video_dir=out, record_video_size={"width":1600,"height":900})
    pg = ctx.new_page()
    for c in ["X04","L02","L03","X_ifsc","B01"]:
        pg.goto(f"file://{html}?case={c}"); pg.wait_for_timeout(14000)
    ctx.close(); b.close()
print(os.listdir(out))
