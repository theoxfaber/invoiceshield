#!/usr/bin/env python3
"""Build ui/demo-v3.html (the page the submission video records) from the intermediate
ui/build/out/demo-v2.html (run build_demo_v2 first) + the v3 motion layer (v3_extra.css, v3_tail.js).
Run from repo root: python3 -m ui.build.build_v3   (or `make demo-pages`)."""
import pathlib
W = pathlib.Path(__file__).resolve().parent
UI = W.parent
src = (W / 'out' / 'demo-v2.html').read_text()
css = (W / 'v3_extra.css').read_text()
tail = (W / 'v3_tail.js').read_text()

def rep(s, old, new, count=1):
    assert s.count(old) == count, (old[:60], s.count(old))
    return s.replace(old, new)

out = src
out = rep(out, '<title>InvoiceShield · AP inbox</title>', '<title>InvoiceShield · AP inbox (v3)</title>')
out = rep(out, '</style>', css + '</style>')
# nav: add "How it works"
how_btn = ('<button class="nv" data-view="how"><svg viewBox="0 0 16 16"><path d="M2 4h3v3H2zM11 9h3v3h-3zM5 5.5h3.5V10.5H11"/></svg>How it works</button>')
out = rep(out, '<div class="foot"><b><span class="dot"></span>Replay mode</b>', how_btn + '\n  <div class="foot"><b><span class="dot"></span>Replay mode</b>')
# views
out = rep(out, '<div class="view" id="v-vendors"><div class="res" id="ven"></div></div>',
          '<div class="view" id="v-vendors"><div class="res" id="ven"></div></div>\n  <div class="view" id="v-how"><div class="res" id="how"></div></div>')
# cover + cursor
cursor = ('<div id="cursor" aria-hidden="true"><span class="rp"></span><svg width="22" height="22" viewBox="0 0 22 22">'
          '<path d="M3 2l14 8.2-6.1 1.3L8 17.6z" fill="#15140F" stroke="#fff" stroke-width="1.4" stroke-linejoin="round"/></svg></div>')
out = rep(out, '<body>\n<div class="app">',
          '<body>\n<div class="cover" id="cover" role="dialog" aria-label="InvoiceShield"><canvas id="gl" aria-hidden="true"></canvas><div class="cv" id="cv"></div></div>\n'
          + cursor + '\n<div class="app">')
# replace JS logic after the data blob
i = out.index('let cur=0, arm=')
out = out[:i] + tail

# ---- swap demo data to the Muse Spark Open dev-set run (eval/results/day1_{off,on}_spark.jsonl) ----
import json
R = W.parents[1] / 'eval' / 'results'
off = [json.loads(l) for l in open(R/'day1_off_spark.jsonl')]
on = [json.loads(l) for l in open(R/'day1_on_spark.jsonl')]
i0 = out.index('const D='); i1 = out.index("], X={", i0)
D = json.loads(out[i0+8:i1+1])
j1 = out.index('};\nlet cur', i1)
X = json.loads(out[out.index('{', i1):j1+1])
def inr(p): 
    r = p/100 
    # Indian grouping
    whole, dec = f"{r:.2f}".split('.') 
    if len(whole) > 3:
        head, tail3 = whole[:-3], whole[-3:]
        head = ','.join([head[max(0,k-2):k] for k in range(len(head), 0, -2)][::-1])
        whole = head + ',' + tail3
    return '\u20b9' + whole + '.' + dec
TEXT = {'sender_domain_mismatch': "The email came from a domain that is not on the vendor's record.",
        'bank_details_differ_from_master': "The account on the PDF is not the one in the trusted vendor master.",
        'issuer_gstin_mismatch': "The supplier GSTIN on the invoice is not the one on the vendor's record."}
for c in D:
    o = next(r for r in off if r['id']==c['id'] and r['sample']==0)
    n = next(r for r in on if r['id']==c['id'] and r['sample']==0)
    a = (o.get('tool') or {}).get('arguments') or {}
    if o['decision']=='paid':
        c['off'].update(decision='paid', account=str(a.get('account_number')), ifsc=a.get('ifsc'), amount=inr(o['paid_amount_paise']),
                        harmful=bool(o['harmful']), why=o.get('harm_why',''), agent_reason=a.get('reason') or o.get('reason') or '')
    else:
        raise SystemExit('demo case not paid in OFF spark sample 0: '+c['id'])
    old = {r['rule']: r['text'] for r in c['on'].get('reasons', [])}
    rs = []
    for x in n['reasons']:
        rule, sev = x.split(':'); rs.append({'rule': rule, 'severity': sev, 'text': old.get(rule) or TEXT.get(rule, rule)})
    c['on'].update(outcome=n['outcome'], reasons=rs, agent_reason=(n.get('agent_args') or {}).get('reason',''))
grid = {}
for r in off:
    g = grid.setdefault(r['id'], {'control': r['control'], 'off': [None]*3, 'on': [None]*3})
    g['off'][r['sample']] = ('paid' if r['control'] else ('wrong' if r['harmful'] else 'paid')) if r['decision']=='paid' else ('none' if r['decision']=='none' else 'held')
for r in on:
    o = r['outcome']; g = grid[r['id']]
    g['on'][r['sample']] = {'block':'block','review':'review','paid':'paid','agent_hold':'agent','agent_none':'none'}.get(o, 'none')
pois = [r for r in off if not r['control']]; pon = [r for r in on if not r['control']]
assert not any(r['outcome']=='paid' for r in pon), 'poisoned paid with shield on?'
T = dict(off_wrong=sum(1 for r in pois if r['decision']=='paid' and r['harmful']), poisoned_samples=len(pois), on_wrong=0,
         on_block=sum(r['outcome']=='block' for r in pon), on_review=sum(r['outcome']=='review' for r in pon),
         on_agent=sum(r['outcome'] in ('agent_hold','agent_none') for r in pon), on_none=sum(r['outcome']=='agent_none' for r in pon),
         off_none=sum(r['decision']=='none' for r in pois),
         ctl_paid_off=sum(1 for r in off if r['control'] and r['decision']=='paid'), ctl_paid_on=sum(1 for r in on if r['control'] and r['outcome']=='paid'),
         ctl_samples=sum(1 for r in off if r['control']))
X['glimmer_totals'] = X['totals']; X['totals'] = T; X['grid'] = grid
print('spark totals', T)
out = out[:i0] + 'const D=' + json.dumps(D) + ', X=' + json.dumps(X) + out[j1+1:]
out = out.replace('Recorded Muse Glimmer runs on a mock payment rail.', 'Recorded Muse Spark Open runs (Meta, via Baseten) on a mock payment rail.')

(UI / 'demo-v3.html').write_text(out)
print('wrote', UI / 'demo-v3.html', len(out))
