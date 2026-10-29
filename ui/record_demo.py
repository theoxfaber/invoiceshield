"""Screen-capture the demo UI (real browser render, recorded cases). Usage: python3 -m ui.record_demo [outdir]"""
import sys, os
from playwright.sync_api import sync_playwright
out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/demo_rec"
html = os.path.abspath("ui/demo.html")
with sync_playwright() as p:
    b = p.chromium.launch(executable_path="/usr/bin/chromium", args=["--no-sandbox"])
    ctx = b.new_context(viewport={"width":1600,"height":900}, record_video_dir=out, record_video_size={"width":1600,"height":900})
    pg = ctx.new_page()
    CAPS = {"X04":"Same invoice, same agent. Left: it pays the attacker's account. Right: the governor stops it.",
            "L02":"A believable 'we changed banks' note. Shield OFF pays; shield ON holds for review.",
            "L03":"Another poisoned invoice. The governor's reason is shown in plain words.",
            "X_ifsc":"Plausible bank details, wrong account. Blocked before any money moves.",
            "B01":"A genuine invoice still gets paid with the shield ON."}
    JS = "t=>{let d=document.createElement('div');d.style.cssText='position:fixed;left:0;right:0;bottom:0;padding:16px 24px;background:rgba(0,0,0,.82);color:#fff;font:600 22px sans-serif;text-align:center;z-index:99';d.textContent=t;document.body.appendChild(d)}"
    for c in ["X04","L02","L03","X_ifsc","B01"]:
        pg.goto(f"file://{html}?case={c}"); pg.evaluate(JS, CAPS[c]); pg.wait_for_timeout(14000)
    ctx.close(); b.close()
print(os.listdir(out))
