from playwright.sync_api import sync_playwright
import sys
with sync_playwright() as p:
    b=p.chromium.launch(executable_path="/usr/bin/chromium",args=["--no-sandbox"])
    pg=b.new_page(viewport={"width":1600,"height":900})
    errs=[]
    pg.on("pageerror",lambda e:errs.append(str(e)))
    for c in ["X04","L02"]:
        pg.goto(f"file://{sys.argv[1]}?case={c}"); pg.wait_for_timeout(9000)
        pg.screenshot(path=f"/tmp/{c}.png")
    print(errs)
