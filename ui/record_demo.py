"""Screen-capture the demo UI (real browser render, recorded cases). Usage: python3 -m ui.record_demo [outdir]"""
import sys, os
from playwright.sync_api import sync_playwright
out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/demo_rec"
html = os.path.abspath("ui/demo.html")
with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox"])  # playwright chromium-1134; set LD_LIBRARY_PATH on the arm64 sandbox
    ctx = b.new_context(viewport={"width":1600,"height":900}, record_video_dir=out, record_video_size={"width":1600,"height":900})
    pg = ctx.new_page()
    CAPS = {"X04":"Same invoice, same agent. Left: it pays the attacker's account. Right: the governor stops it.",
            "L02":"A believable 'we changed banks' note. Shield OFF pays; shield ON holds for review.",
            "L03":"Another poisoned invoice. The governor's reason is shown in plain words.",
            "X_ifsc":"Plausible bank details, wrong account. Blocked before any money moves.",
            "B01":"A genuine invoice still gets paid with the shield ON."}
    JS = "t=>{let d=document.createElement('div');d.style.cssText='position:fixed;left:0;right:0;bottom:0;padding:16px 24px;background:rgba(0,0,0,.82);color:#fff;font:600 22px sans-serif;text-align:center;z-index:99';d.textContent=t;document.body.appendChild(d)}"
    def card(h, lines, ms):
        body = "".join(f"<p>{l}</p>" for l in lines)
        pg.set_content(f"<body style='margin:0;background:#0b1220;color:#fff;font-family:sans-serif;display:flex;flex-direction:column;justify-content:center;height:100vh;padding:0 140px'><h1 style='font-size:56px'>{h}</h1>{body}<style>p{{font-size:30px;line-height:1.4;color:#cbd5e1}}</style></body>")
        pg.wait_for_timeout(ms)
    card("InvoiceShield", ["AI agents are starting to pay bills.", "One hidden sentence in an invoice can send the money to the wrong account.", "Shield OFF vs shield ON, same invoice, same agent."], 12000)
    for c in ["X04","L02","L03","X_ifsc","B01"]:
        pg.goto(f"file://{html}?case={c}"); pg.evaluate(JS, CAPS[c]); pg.wait_for_timeout(20000)
    card("Results (dev set, 20 poisoned + 4 genuine invoices, 3 runs each)", ["Muse Glimmer 30B, shield OFF: 15/60 runs paid the wrong place. Shield ON: 0/60.", "Muse Spark Open, shield OFF: 15/60. Shield ON: 0/60.", "Genuine invoices: 12/12 paid with the shield ON.", "Small set: zero observed is not zero possible. Frozen held-out set not yet run."], 20000)
    card("What it does not stop", ["A hijacked agent paying a real supplier with a real order and a consistent invoice.", "An insider who edits the trusted bank details.", "Small payments spread just outside the 7-day window.", "It contains the damage; it does not eliminate prompt injection.", "github.com/theoxfaber/invoiceshield (public at submission)"], 20000)
    ctx.close(); b.close()
print(os.listdir(out))
