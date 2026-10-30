"""Builds ui/build/out/demo-v2.html (intermediate; the committed page is ui/demo-v3.html): the premium redesign of ui/demo.html.

Data: the replay payload is taken verbatim from repo/ui/demo.html (the `const D=[...]` built by
ui/build_demo.py from eval/results day1_off/day1_on sample 0), so the replay is the same recorded runs.
Display-only additions, all read from repo files (nothing invented):
  - vendor record per case from dataset/fixtures/master.json (legal name, email domains, bank, GSTIN)
  - the invoice PDF rendered at 150 dpi + bounding boxes of the bank block / GSTIN (pdftotext -bbox)
  - account / IFSC / issuer GSTIN printed on the PDF (pdftotext), asserted equal to what the OFF agent paid
  - the per-sample outcome grid for all 20 poisoned + 4 control cases (eval/results day1_off/day1_on, k=3)
  - the full vendor master for the Vendors view
Run from repo root: python3 -m ui.build.build_demo_v2   (or `make demo-pages`)
"""
import base64, json, pathlib, re, subprocess, tempfile, collections

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
FONTS = HERE / 'fonts'
OUT = HERE / 'out' / 'demo-v2.html'  # intermediate, not committed (input to build_v3.py)

def b64(p): return base64.b64encode(pathlib.Path(p).read_bytes()).decode()

def load_D():
    s = (REPO / 'ui/demo.html').read_text()
    i = s.index('const D=') + 8; j = s.index(';let cur=0', i)
    return json.loads(s[i:j])

def pdf_info(pdf):
    with tempfile.TemporaryDirectory() as t:
        subprocess.run(['pdftoppm', '-png', '-r', '150', '-singlefile', str(pdf), f'{t}/p'], check=True)
        img_path = f'{t}/p.png'
        from PIL import Image
        im = Image.open(img_path); imgW, imgH = im.size
    txt = subprocess.run(['pdftotext', '-layout', str(pdf), '-'], capture_output=True, text=True).stdout
    bbox = subprocess.run(['pdftotext', '-bbox', str(pdf), '-'], capture_output=True, text=True).stdout
    W = float(re.search(r'<page width="([\d.]+)" height="([\d.]+)"', bbox).group(1))
    H = float(re.search(r'<page width="([\d.]+)" height="([\d.]+)"', bbox).group(2))
    words = [(float(a), float(b), float(c), float(d), w) for a, b, c, d, w in
             re.findall(r'<word xMin="([\d.]+)" yMin="([\d.]+)" xMax="([\d.]+)" yMax="([\d.]+)">([^<]*)</word>', bbox)]
    def box(pred):
        ws = [w for w in words if pred(w)]
        if not ws: return None
        x0 = min(w[0] for w in ws); y0 = min(w[1] for w in ws); x1 = max(w[2] for w in ws); y1 = max(w[3] for w in ws)
        return [round(x0 / W, 4), round(y0 / H, 4), round(x1 / W, 4), round(y1 / H, 4)]
    acct = re.search(r'A/c No:\s*(\S+)', txt).group(1)
    ifsc = re.search(r'IFSC:\s*(\S+)', txt).group(1)
    bankname = re.search(r'Bank:\s*(.+)', txt).group(1).strip()
    gst = re.search(r'GSTIN:\s*(\S+)', txt).group(1)
    # bank block = the "Bank details for remittance" heading through the IFSC line
    yb = [w[1] for w in words if w[4] == 'Bank'][0]
    yi = [w[3] for w in words if w[4] == 'IFSC:'][0]
    bank_box = box(lambda w: w[1] >= yb - 1 and w[3] <= yi + 1)
    gst_box = box(lambda w: w[1] < 80 and ('GSTIN' in w[4] or w[4] == gst))
    date = re.search(r'Date:\s*(\S+)', txt)
    # crop the page to its content: top of page down to just below the bank block
    crop = min(1.0, bank_box[3] + 0.07)
    with tempfile.TemporaryDirectory() as t2:
        im.crop((0, 0, imgW, int(imgH * crop))).save(f'{t2}/c.png', optimize=True)
        img = b64(f'{t2}/c.png')
    sc = lambda r: None if r is None else [r[0], round(r[1] / crop, 4), r[2], round(r[3] / crop, 4)]
    bank_box, gst_box = sc(bank_box), sc(gst_box)
    return dict(image=img, pdf_account=acct, pdf_ifsc=ifsc, pdf_bank=bankname, pdf_gstin=gst,
                bank_box=bank_box, gstin_box=gst_box, date=date.group(1) if date else '', aspect=round(H * crop / W, 4))

def grids(man):
    off = [json.loads(l) for l in (REPO / 'eval/results/day1_off.jsonl').read_text().splitlines()]
    on = [json.loads(l) for l in (REPO / 'eval/results/day1_on.jsonl').read_text().splitlines()]
    order = [c['id'] for c in man]
    def cell_off(r):
        if r['decision'] == 'paid': return 'wrong' if r.get('harmful') else 'paid'
        return 'held'
    def cell_on(r):
        o = r['outcome']
        if o == 'paid': return 'paid'
        if o in ('block', 'review'): return o
        return 'agent'
    g = collections.OrderedDict()
    for cid in order:
        ctl = [c for c in man if c['id'] == cid][0]['klass'] == 'benign'
        g[cid] = {'control': ctl,
                  'off': [cell_off(r) for r in sorted([r for r in off if r['id'] == cid], key=lambda r: r['sample'])],
                  'on': [cell_on(r) for r in sorted([r for r in on if r['id'] == cid], key=lambda r: r['sample'])]}
    pois = [v for v in g.values() if not v['control']]; ctls = [v for v in g.values() if v['control']]
    totals = {
        'off_wrong': sum(x == 'wrong' for v in pois for x in v['off']), 'poisoned_samples': sum(len(v['off']) for v in pois),
        'on_wrong': sum(x == 'paid' for v in pois for x in v['on']),
        'on_block': sum(x == 'block' for v in pois for x in v['on']), 'on_review': sum(x == 'review' for v in pois for x in v['on']),
        'on_agent': sum(x == 'agent' for v in pois for x in v['on']),
        'ctl_paid_off': sum(x == 'paid' for v in ctls for x in v['off']), 'ctl_paid_on': sum(x == 'paid' for v in ctls for x in v['on']),
        'ctl_samples': sum(len(v['off']) for v in ctls)}
    return g, totals

def main():
    D = load_D()
    man = {c['id']: c for c in json.loads((REPO / 'eval/riskset/manifest.json').read_text())}
    master = json.loads((REPO / 'dataset/fixtures/master.json').read_text())['vendors']
    vend = {v['vendor_id']: v for v in master}
    for c in D:
        m = man[c['id']]; v = vend[m['vendor_id']]
        info = pdf_info(REPO / 'eval/riskset' / f"{c['id']}.pdf")
        assert info['pdf_account'] == c['off']['account'] and info['pdf_ifsc'] == c['off']['ifsc'], c['id']
        assert v['bank_account'] == c['on']['trusted_account'] and v['ifsc'] == c['on']['trusted_ifsc'], c['id']
        c['image'] = info.pop('image')
        c['pdf'] = info
        c['po_ref'] = m['po_ref']; c['klass'] = m['klass']
        c['email_subject'] = m['cover_email']['subject']; c['email_to'] = m['cover_email']['to']
        c['vendor'] = {'id': v['vendor_id'], 'name': v['legal_name'], 'domains': v['email_domains'], 'bank': v['bank_name'],
                       'gstin': v['gstin'], 'account': v['bank_account'], 'ifsc': v['ifsc'], 'category': v['category']}
    g, totals = grids(list(man.values()))
    assert totals['off_wrong'] == 15 and totals['on_wrong'] == 0 and totals['poisoned_samples'] == 60, totals
    assert totals['ctl_paid_off'] == 12 and totals['ctl_paid_on'] == 12, totals
    extra = {'grid': g, 'totals': totals, 'klass': {k: v['klass'] for k, v in man.items()},
             'vendors': [{'id': v['vendor_id'], 'name': v['legal_name'], 'domains': v['email_domains'], 'bank': v['bank_name'],
                          'account': v['bank_account'], 'ifsc': v['ifsc'], 'gstin': v['gstin'], 'category': v['category']} for v in master]}
    tpl = (HERE / 'demo-v2.template.html').read_text()
    css_fonts = ''.join(
        f"@font-face{{font-family:'{n}';src:url(data:font/ttf;base64,{b64(FONTS / f)}) format('truetype');font-weight:100 900;font-display:block}}"
        for n, f in [('Geist', 'Geist-VF.ttf'), ('Geist Mono', 'GeistMono-VF.ttf')])
    out = tpl.replace('/*__FONTS__*/', css_fonts).replace('__DATA__', json.dumps(D)).replace('__EXTRA__', json.dumps(extra))
    OUT.parent.mkdir(exist_ok=True)
    OUT.write_text(out)
    print('wrote', OUT, len(out) // 1024, 'KB', totals)

if __name__ == '__main__':
    main()
