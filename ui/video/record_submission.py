#!/usr/bin/env python3
"""Record the ~3:00 InvoiceShield submission video as a real screen capture of ui/demo-v3.html.

Real Playwright mouse clicks and key presses drive the UI (the page's own synthetic cursor follows the
real mouse so viewers can see it). Frames come from a CDP screencast of the live page (1536x864 CSS px at
x1.25 CSS zoom = 1920x1080 real px; CDP screencast frames are in DIPs, so a
device_scale_factor would not raise the capture resolution), timed against the narration timeline in vo/timeline.json (see build_audio.py).

Usage (from repo/ui/video):  python3 build_audio.py && python3 record_submission.py && python3 encode.py OUTDIR
Env: LD_LIBRARY_PATH must point at the extracted chromium libs on the arm64 sandbox.
"""
import asyncio, base64, json, time, pathlib
from playwright.async_api import async_playwright

W = pathlib.Path(__file__).resolve().parent
HTML = (W.parent / 'demo-v3.html').resolve()
TL = json.load(open(W / 'vo/timeline.json')); S = TL['starts']; TOTAL = TL['total']
FR = W / 'frames'; FR.mkdir(exist_ok=True)
for f in FR.glob('*.jpg'): f.unlink()
URL = f'file://{HTML}?view=intro&case=X04&arm=off&pace=1100'

Z = 1.25  # layout of a 1536x864 window, rendered at 1920x1080
INIT = r"""(()=>{const Z=1.25,c=document.getElementById('cursor');document.documentElement.style.zoom=Z;
document.addEventListener('mousemove',e=>{c.style.left=e.clientX/Z+'px';c.style.top=e.clientY/Z+'px'},true);
document.addEventListener('mousedown',()=>{c.classList.remove('click');void c.offsetWidth;c.classList.add('click')},true);
const st=document.createElement('style');st.textContent=`
.app{height:864px!important}.cv{padding:0 138px!important}.cv .fine{left:138px!important;bottom:43px!important}.cv h1{font-size:86px!important}.cv p{font-size:23px!important}
#keyhint{position:fixed;right:28px;bottom:26px;z-index:90;display:flex;gap:8px;align-items:center;padding:8px 12px;border-radius:10px;background:#15140F;color:#fff;font:600 13px Geist,sans-serif;opacity:0;transform:translateY(6px);transition:opacity .2s,transform .2s}
#keyhint.on{opacity:1;transform:none}#keyhint kbd{font:600 13px 'Geist Mono',monospace;background:#fff;color:#15140F;border-radius:5px;padding:2px 7px;box-shadow:0 2px 0 #A29E94}
.annot{position:fixed;z-index:70;border:2px solid #D23A1A;border-radius:8px;box-shadow:0 0 0 4px rgba(210,58,26,.15);pointer-events:none;animation:pop .35s both}
.annot .tip{position:absolute;left:-2px;top:calc(100% + 8px);white-space:nowrap;background:#15140F;color:#fff;font:500 13px Geist,sans-serif;padding:7px 10px;border-radius:8px}
.annot .tip b{font-family:'Geist Mono',monospace;font-weight:600;color:#3ED68F}.annot .tip s{font-family:'Geist Mono',monospace;color:#F3C6B8}
@keyframes pop{from{opacity:0;transform:scale(1.06)}to{opacity:1;transform:none}}
#limits{position:fixed;left:224px;right:0;top:0;bottom:0;z-index:60;background:var(--bg);padding:58px 72px;opacity:0;transition:opacity .4s}
#limits.on{opacity:1}#limits h2{font-size:34px;letter-spacing:-.02em;margin:0 0 6px;font-weight:650}
#limits .sub{color:var(--mu);font-size:15px;margin-bottom:26px}
#limits .li{display:flex;gap:16px;align-items:flex-start;background:var(--panel);border:1px solid var(--line);border-radius:12px;box-shadow:var(--sh);padding:18px 22px;margin-bottom:14px;opacity:0;transform:translateY(8px);transition:opacity .45s,transform .45s}
#limits .li.on{opacity:1;transform:none}#limits .li b{display:block;font-size:18px;font-weight:620;letter-spacing:-.01em}
#limits .li span{color:var(--ink2);font-size:14.5px}#limits .n{flex:none;width:30px;height:30px;border-radius:8px;background:var(--amb-bg);color:var(--amb);display:grid;place-items:center;font-weight:700;border:1px solid var(--amb-line)}
#limits .li.k .n{background:var(--ink);color:#3ED68F;border-color:var(--ink)}
#limits .repo{margin-top:22px;font:500 15px 'Geist Mono',monospace;color:var(--ink2)}#limits .repo em{font-style:normal;color:var(--mu);font-family:Geist,sans-serif}
#repoline{position:fixed;right:64px;bottom:43px;z-index:95;text-align:right;line-height:1.6;font:500 15px 'Geist Mono',monospace;color:#3A3832;opacity:0;transition:opacity .6s}
#repoline.on{opacity:1}#repoline em{font-style:normal;color:#757167;font-family:Geist,sans-serif}`;
document.head.appendChild(st);
const k=document.createElement('div');k.id='keyhint';document.body.appendChild(k);
window.__key=(key,label)=>{k.innerHTML=`<kbd>${key}</kbd>${label}`;k.classList.add('on');clearTimeout(window.__kt);window.__kt=setTimeout(()=>k.classList.remove('on'),1800)};
window.__annot=(sel,html)=>{const el=document.querySelector(sel);if(!el)return false;const r=el.getBoundingClientRect(),a=document.createElement('div');a.className='annot';
 a.style.cssText+=`left:${r.left/Z-6}px;top:${r.top/Z-4}px;width:${r.width/Z+12}px;height:${r.height/Z+8}px`;a.innerHTML=`<div class="tip">${html}</div>`;document.body.appendChild(a);return true};
window.__unannot=()=>document.querySelectorAll('.annot').forEach(a=>a.remove());
window.__limits=()=>{const L=document.createElement('div');L.id='limits';L.innerHTML=`<h2>What it does not stop</h2>
 <div class="sub">InvoiceShield contains the damage. It does not eliminate prompt injection.</div>
 <div class="li k"><div class="n">!</div><div><b>Prompt injection itself</b><span>The agent can still be fooled. The shield limits what a fooled agent can do: it can only propose, and the payee always comes from the vendor record.</span></div></div>
 <div class="li"><div class="n">1</div><div><b>A wrong but real vendor, with a consistent invoice</b><span>A hijacked agent proposing a real supplier that has a real open order and a matching invoice can still be paid.</span></div></div>
 <div class="li"><div class="n">2</div><div><b>A poisoned vendor master</b><span>An insider who edits the trusted bank details defeats it. The vendor record is the root of trust.</span></div></div>
 <div class="li"><div class="n">3</div><div><b>Slow-drip splits</b><span>Small payments spread out just beyond the 7-day vendor-total window.</span></div></div>
 <div class="repo">github.com/theoxfaber/invoiceshield <em>· public at submission</em></div>`;document.body.appendChild(L);
 document.getElementById('crumb').innerHTML='<b>Limits</b>';requestAnimationFrame(()=>L.classList.add('on'))};
window.__limit=i=>{const e=document.querySelectorAll('#limits .li')[i];if(e)e.classList.add('on')};
window.__repo=()=>{const r=document.createElement('div');r.id='repoline';r.innerHTML='github.com/theoxfaber/invoiceshield<br><em>public at submission · Meta Global AI Developer Hackathon 2026</em>';document.body.appendChild(r);setTimeout(()=>r.classList.add('on'),1600)};
})()"""

SCROLL_PDF = """(()=>{const p=document.getElementById('paper'),d=document.getElementById('detail');
 const y=(p.getBoundingClientRect().top-d.getBoundingClientRect().top)/1.25+d.scrollTop+p.offsetHeight*0.8-d.clientHeight*0.62;
 d.scrollTo({top:y,behavior:'smooth'})})()"""


def build_cues(pg):
    async def move(sel, dx=0, dy=0, fx=.5, fy=.55):
        box = await pg.locator(sel).first.bounding_box()
        if not box: print('no box', sel); return None
        x, y = box['x'] + box['width'] * fx + dx, box['y'] + box['height'] * fy + dy
        await pg.mouse.move(x, y); return x, y
    async def click(sel, **kw):  # cursor already glided there; a real mouse click on the element
        p = await move(sel, **kw)
        if p: await pg.mouse.click(*p)
    async def key(k, label):
        await pg.evaluate(f"__key({json.dumps(k.upper())},{json.dumps(label)})"); await pg.keyboard.press(k)
    ev = lambda js: (lambda: pg.evaluate(js))
    def sched(click_at, targets, run=900):  # step start times as absolute timeline seconds -> ms after the action
        return ev(f"__sched({json.dumps([int((t - click_at) * 1000) for t in targets])},{run})")
    fast = lambda: pg.evaluate("__sched([200,650,1100,1550,2000],380)")

    s = S
    C = [
        # s01 intro cover (real page, view=intro)
        (s['s01'] + 7.5, lambda: pg.mouse.move(980, 560)),
        (s['s02'] - 1.6, ev("window.__hold=true")),
        (s['s02'] - 1.4, lambda: move('#cover', fy=.62)),
        (s['s02'] - 0.6, lambda: click('#cover', fy=.62)),
        # s02 inbox: pick the look-alike invoice, point at amount, sender, PDF bank box
        (s['s02'] + 0.5, lambda: move('.row[data-case=L02]')),
        (s['s02'] + 1.4, lambda: click('.row[data-case=L02]')),
        (s['s02'] + 5.3, lambda: move('#amtn', dx=-10)),
        (s['s02'] + 7.9, lambda: move('#detail .dh .kick .mono', fx=.62)),
        (s['s02'] + 8.6, ev("__annot('#detail .dh .kick .mono','On the vendor record: <b>sharmaofficesupplies.in</b> &nbsp; This email: <s>sharma-officesupplies.co.in</s>')")),
        (s['s02'] + 12.0, ev("__unannot();" + SCROLL_PDF)),
        (s['s02'] + 12.9, lambda: move('#paper .hl', dy=-4)),
        # s03 shield OFF: replay, pays the wrong account
        (s['s03'] - 0.5, ev("__scrollDetail(0)")),
        (s['s03'] + 0.2, lambda: move('#seg button[data-arm=off]')),
        (s['s03'] + 1.0, lambda: move('#play')),
        (s['s03'] + 1.75, ev("window.__hold=false")),
        (s['s03'] + 1.8, sched(s['s03'] + 1.8, [s['s03'] + 2.4, s['s03'] + 8.0, s['s03'] + 10.2])),
        (s['s03'] + 1.8, lambda: click('#play')),
        (s['s03'] + 4.0, lambda: move('#tl .stp:nth-child(1)', fx=.3)),
        (s['s03'] + 8.2, lambda: move('#tl .stp:nth-child(2)', fx=.3)),
        (s['s03'] + 11.6, lambda: move('#banner', fx=.35)),
        # s04 shield ON, same invoice
        (s['s04'] + 0.1, lambda: move('#seg button[data-arm=on]')),
        (s['s04'] + 0.9, ev("window.__hold=true")),
        (s['s04'] + 1.0, lambda: click('#seg button[data-arm=on]')),
        (s['s04'] + 4.2, ev("window.__hold=false")),
        (s['s04'] + 4.3, sched(s['s04'] + 4.3, [s['s04'] + 5.6, s['s05'] + 0.3, s['s05'] + 4.9, s['s05'] + 10.9, s['s05'] + 14.3])),
        (s['s04'] + 4.3, lambda: key('r', 'Replay')),
        (s['s04'] + 6.3, lambda: move('#tl .stp:nth-child(1)', fx=.3)),
        (s['s05'] + 0.6, lambda: move('#tl .stp:nth-child(2)', fx=.3)),
        (s['s05'] + 5.2, lambda: move('#tl .stp:nth-child(3)', fx=.3)),
        (s['s05'] + 8.0, lambda: move('#checks', fx=.7, fy=.4)),
        (s['s05'] + 11.3, lambda: move('#tl .stp:nth-child(4)', fx=.3)),
        (s['s05'] + 15.6, lambda: move('#banner', fx=.3)),
        # s06 other attacks, by keyboard (shield stays on)
        (s['s06'] + 2.4, fast), (s['s06'] + 2.5, lambda: key('1', 'Bank details swapped')),
        (s['s06'] + 5.0, lambda: move('#banner', fx=.3)),
        (s['s06'] + 6.0, fast), (s['s06'] + 6.1, lambda: key('4', 'New IFSC branch')),
        (s['s06'] + 8.8, lambda: move('#banner', fx=.35)),
        # s07 genuine invoice still paid
        (s['s07'] + 0.4, fast), (s['s07'] + 0.5, lambda: key('5', 'Genuine invoice')),
        (s['s07'] + 3.3, lambda: move('#banner', fx=.3)),
        (s['s07'] + 6.0, lambda: move('#checks', fx=.6, fy=.5)),
        # s08 how it works
        (s['s08'] - 1.3, lambda: move('.nv[data-view=how]')),
        (s['s08'] - 0.5, lambda: click('.nv[data-view=how]')),
        (s['s08'] + 2.2, lambda: move('.r2 .card:nth-child(1)', fy=.3)),
        (s['s08'] + 5.0, lambda: move('.r2 .card:nth-child(3)', fy=.3)),
        (s['s08'] + 9.6, lambda: move('.nd.dark')),
        (s['s08'] + 12.6, lambda: move('.note2', fx=.4)),
        (s['s08'] + 17.0, lambda: move('.nd.dark', dy=12)),
        # s09 vendor record
        (s['s09'] - 1.0, lambda: move('.nv[data-view=vendors]')),
        (s['s09'] - 0.3, lambda: click('.nv[data-view=vendors]')),
        (s['s09'] + 1.5, lambda: move('#ven tr:nth-child(2) td:nth-child(1)')),
        (s['s09'] + 4.2, lambda: move('#ven tr:nth-child(2) td:nth-child(4)')),
        # s10 results
        (s['s10'] - 1.0, lambda: move('.nv[data-view=results]')),
        (s['s10'] - 0.3, lambda: click('.nv[data-view=results]')),
        (s['s10'] + 2.5, lambda: move('#res .sub', fx=.3)),
        (s['s10'] + 10.8, lambda: move('#k1', dx=30)),
        (s['s10'] + 13.9, lambda: move('#k2', dx=30)),
        (s['s10'] + 15.6, lambda: move('#k3', dx=30)),
        (s['s10'] + 18.2, lambda: move('#res .sub', fx=.75, fy=.8)),
        (s['s10'] + 20.6, lambda: move('#res .foot', fx=.3)),
        # s11 limits
        (s['s11'] - 0.7, ev("__limits()")),
        (s['s11'] + 0.3, ev("__limit(0)")),
        (s['s11'] + 1.0, lambda: move('#limits .li:nth-child(3)', fx=.2, fy=.35)),
        (s['s11'] + 4.6, ev("__limit(1)")),
        (s['s11'] + 5.0, lambda: move('#limits .li:nth-child(4)', fx=.2, fy=.35)),
        (s['s11'] + 9.6, ev("__limit(2)")),
        (s['s11'] + 10.0, lambda: move('#limits .li:nth-child(5)', fx=.2, fy=.35)),
        (s['s11'] + 12.4, ev("__limit(3)")),
        (s['s11'] + 12.8, lambda: move('#limits .li:nth-child(6)', fx=.2, fy=.35)),
        # s12 close
        (s['s12'] - 0.8, ev("document.getElementById('limits').remove();setView('close');__repo()")),
        (s['s12'] - 0.7, ev("document.getElementById('cursor').style.opacity=0")),
    ]
    return sorted(C, key=lambda c: c[0])


async def main():
    async with async_playwright() as p:
        b = await p.chromium.launch(args=['--no-sandbox', '--use-gl=swiftshader', '--enable-webgl', '--ignore-gpu-blocklist', '--hide-scrollbars'])
        pg = await b.new_page(viewport={'width': 1920, 'height': 1080}, device_scale_factor=1)
        errs = []; pg.on('pageerror', lambda e: errs.append(str(e)))
        await pg.goto(URL); await pg.wait_for_timeout(1500); await pg.evaluate(INIT)
        cues = build_cues(pg)
        cdp = await pg.context.new_cdp_session(pg)
        frames = []
        async def on_frame(ev):
            ts = ev['metadata'].get('timestamp', time.time()); fn = FR / f'f{len(frames):05d}.jpg'
            fn.write_bytes(base64.b64decode(ev['data'])); frames.append((ts, fn.name))
            try: await cdp.send('Page.screencastFrameAck', {'sessionId': ev['sessionId']})
            except Exception: pass
        cdp.on('Page.screencastFrame', lambda ev: asyncio.ensure_future(on_frame(ev)))
        await cdp.send('Page.startScreencast', {'format': 'jpeg', 'quality': 92, 'maxWidth': 1920, 'maxHeight': 1080, 'everyNthFrame': 1})
        await pg.wait_for_timeout(300)
        start = time.time(); late = []
        for at, fn in cues:
            d = start + max(0, at) - time.time()
            if d > 0: await asyncio.sleep(d)
            lag = time.time() - start - at
            if lag > 0.25: late.append((round(at, 2), round(lag, 2)))
            try: await fn()
            except Exception as e: errs.append(f'cue {at:.2f}: {e}')
        d = start + TOTAL - time.time()
        if d > 0: await asyncio.sleep(d)
        await cdp.send('Page.stopScreencast'); await asyncio.sleep(0.3)
        json.dump({'start': start, 'total': TOTAL, 'frames': frames, 'errors': errs, 'late': late}, open(W / 'frames.json', 'w'))
        print('frames', len(frames), 'errors', errs, 'late', late)
        await b.close()

asyncio.run(main())
