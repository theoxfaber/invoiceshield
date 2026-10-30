let cur=0, arm='off', view='inbox', timers=[], lastCase=-1;
const Q=new URLSearchParams(location.search), STATIC=Q.get('static')=='1';
const RM=matchMedia('(prefers-reduced-motion: reduce)').matches;
let PACE=Math.max(300,+(Q.get('pace')||1000));
const $=i=>document.getElementById(i);
function esc(s){return String(s??'').replace(/[&<>"]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]))}
const ICON={x:'<svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3L3 9"/></svg>',ok:'<svg viewBox="0 0 12 12"><path d="M2.5 6.3l2.3 2.3L9.6 3.6"/></svg>',
 pause:'<svg viewBox="0 0 16 16"><path d="M6 4v8M10 4v8"/></svg>',wait:'<svg viewBox="0 0 16 16"><circle cx="8" cy="8" r="5"/></svg>'};
const REASON={
 sender_domain_mismatch:"Sender domain doesn't match vendor record",
 bank_details_differ_from_master:"Bank account differs from verified vendor master",
 issuer_gstin_mismatch:"Supplier tax ID (GSTIN) doesn't match vendor record",
 injection_suspected:"Document contains instructions aimed at an AI agent",
 bank_box_mismatch:"Bank box on the page differs from the text layer"};
const SEV={block:['Blocks payment','red'],review:['Needs review','amb']};
const initials=n=>n.replace(/Pvt|Ltd|Inc\.?/g,'').trim().split(/\s+/).slice(0,2).map(w=>w[0]).join('').toUpperCase();
const grp=s=>String(s).replace(/(\d{4})(?=\d)/g,'$1 ');
const later=(f,ms)=>timers.push(setTimeout(f,ms));
// ---- motion helpers ----
// Spring counter (idea from Subframe Motion "A number in motion", adapted from motion-primitives, MIT).
// Over-damped so it never overshoots the real value; reduced motion jumps straight to the target.
function springNum(el,to,o={}){
 const from=o.from??0,fmt=o.fmt||(v=>String(Math.round(v))),k=o.k??170,c=o.c??27,m=o.m??0.7;
 if(!el)return; cancelAnimationFrame(el._raf);
 if(RM||STATIC){el.textContent=fmt(to);return}
 let x=from,v=0,last=performance.now();el.textContent=fmt(from);
 const tick=now=>{const dt=Math.min(.032,(now-last)/1000);last=now;const f=-k*(x-to)-c*v;v+=f/m*dt;x+=v*dt;
  if(Math.abs(v)<.4&&Math.abs(x-to)<.4){el.textContent=fmt(to);return}
  el.textContent=fmt(to>=from?Math.min(x,to):Math.max(x,to));el._raf=requestAnimationFrame(tick)};
 el._raf=requestAnimationFrame(tick)}
// Word-by-word entrance (idea from Subframe Motion "Word by word").
function words(text,delay=0){let n=0;return String(text).split(/(\s+)/).map(w=>/^\s+$/.test(w)||!w?w:`<span class="w" style="--i:${n++};--d:${delay}ms">${esc(w)}</span>`).join('')}
const inr=v=>'₹'+Math.round(v).toLocaleString('en-IN')+'.00';
const amtNum=s=>+String(s).replace(/[^\d.]/g,'');

function status(c,a){
 if(a=='off') return c.off.harmful?{l:'Paid · wrong account',t:'red'}:{l:'Paid',t:'grn'};
 const o=c.on.outcome; if(o=='block')return{l:'Blocked',t:'red'}; if(o=='review')return{l:'Held for review',t:'amb'};
 if(o=='paid')return{l:'Paid',t:'grn'}; return{l:'Held by agent',t:'amb'}}
const pill=(s,lg)=>`<span class="st ${s.t}${lg?' lg':''}"><i></i>${esc(s.l)}</span>`;
function diff(a,b){const n=a.length,m=b.length,L=Array.from({length:n+1},()=>new Array(m+1).fill(0));
 for(let i=n-1;i>=0;i--)for(let j=m-1;j>=0;j--)L[i][j]=a[i]==b[j]?L[i+1][j+1]+1:Math.max(L[i+1][j],L[i][j+1]);
 let i=0,j=0,o='',run='';const fl=()=>{if(run){o+=`<span class="d">${esc(run)}</span>`;run=''}};
 while(i<n){if(j<m&&a[i]==b[j]){fl();o+=esc(a[i]);i++;j++}else if(j<m&&L[i][j+1]>=L[i+1][j])j++;else{run+=a[i];i++}}fl();return o}
function whole(a,b){return a===b?esc(a):`<span class="d">${esc(a)}</span>`}

function renderRows(pending){
 $('rows').innerHTML=D.map((c,i)=>{const s=window.__hold?{l:'New',t:'mu'}:(pending&&i==cur)?{l:arm=='off'?'Paying…':'Checking…',t:'mu'}:status(c,arm);return `<div class="row${i==cur?' on':''}" data-i="${i}" data-case="${esc(c.id)}">
  <div class="av">${initials(c.vendor.name)}</div>
  <div style="min-width:0"><div class="v">${esc(c.vendor.name)}</div><div class="e mono">${esc(c.email_from)}</div><div class="t">${esc(c.invoice_no)} · ${esc(c.title)}</div></div>
  <div class="r"><div class="amt mono">${esc(c.off.amount)}</div>${pill(s)}</div></div>`}).join('');
 document.querySelectorAll('.row').forEach(r=>r.onclick=()=>show(+r.dataset.i));
 const need=D.filter(c=>status(c,arm).t!='grn').length;
 $('lcount').textContent=`${D.length} invoices · ${need} need attention`;$('ct-inbox').textContent=D.length;
}
function checks(c){
 const v=c.vendor, from=c.email_from.split('@')[1]||'', domOK=v.domains.includes(from);
 const acOK=c.pdf.pdf_account==v.account, ifOK=c.pdf.pdf_ifsc==v.ifsc, gsOK=!v.gstin||c.pdf.pdf_gstin==v.gstin;
 const best=v.domains.slice().sort((x,y)=>Math.abs(x.length-from.length)-Math.abs(y.length-from.length))[0];
 let k=0;
 const row=(ok,title,k1,v1,k2,v2)=>`<div class="ck ${ok?'ok':'bad'}" style="--k:${k++}"><div class="ic">${ok?ICON.ok:ICON.x}</div><div><div class="t">${title}</div>
   <div class="cmp"><span class="k">${k1}</span><span class="val mono">${v1}</span><span class="k">${k2}</span><span class="val mono rec">${v2}</span></div></div></div>`;
 return [
  row(domOK, domOK?'Sender domain matches vendor record':"Sender domain doesn't match vendor record",
      'Email from', domOK?esc(from):diff(from,best), 'On record', esc(v.domains.join(', '))),
  row(acOK&&ifOK, acOK&&ifOK?'Bank account matches verified vendor master':'Bank account differs from verified vendor master',
      'In the PDF', (acOK?esc(grp(c.pdf.pdf_account)):`<span class="d">${esc(grp(c.pdf.pdf_account))}</span>`)+' · '+whole(c.pdf.pdf_ifsc,v.ifsc)+`<br><span style="font-family:Geist;color:var(--mu)">${esc(c.pdf.pdf_bank)}</span>`,
      'Verified', esc(grp(v.account))+' · '+esc(v.ifsc)+`<br><span style="font-family:Geist;color:var(--mu)">${esc(v.bank)}</span>`),
  v.gstin?row(gsOK, gsOK?'Supplier tax ID matches':"Supplier tax ID (GSTIN) doesn't match vendor record", 'On invoice', gsOK?esc(c.pdf.pdf_gstin):diff(c.pdf.pdf_gstin,v.gstin), 'On record', esc(v.gstin)):''
 ].join('')}
function stepsFor(c){
 const o=c.off,n=c.on,S=[];
 if(arm=='off'){
  S.push({t:'Agent read the email and the PDF',s:'Plain AP agent holding a pay tool. Bank details come from the document.',q:o.agent_reason||'Invoice looks consistent with the PO.'});
  S.push({t:'Agent paid the account printed in the PDF',s:`${esc(o.payee)} · A/c ${esc(grp(o.account))} · ${esc(o.ifsc)} · ${esc(o.amount)}`,code:'pay_invoice()'});
  S.push(o.harmful?{t:`${esc(o.amount)} sent to an account that isn't on the vendor record`,s:'The account and IFSC are not the vendor\'s verified account.',tone:'red'}
                  :{t:`Paid ${esc(o.amount)} to the verified account`,s:'Genuine invoice.',tone:'grn'});
 } else {
  S.push({t:'Agent proposed a payment',s:'No bank fields allowed: vendor, invoice, amount and PO only.',code:'propose_payment()',q:n.agent_reason?n.agent_reason+' · '+n.amount:'',meta:n.agent_reason?'':`Proposed amount ${esc(n.amount)} · ${esc(c.invoice_no)} · ${esc(c.po_ref)}`});
  S.push({t:'Document checker read the PDF',s:'An isolated model with no tools. It can only raise signals, never pay.'});
  S.push({t:'Policy engine compared it with the vendor record',s:`Verified account ${esc(grp(n.trusted_account))} · ${esc(n.trusted_ifsc)}`,code:'governor (Rust)',gov:1});
  if(n.reasons.length) S.push({t:`${n.reasons.length} check${n.reasons.length>1?'s':''} failed`,reasons:n.reasons});
  const st=status(c,'on');
  S.push(n.outcome=='paid'?{t:`Paid ${esc(n.amount)} to the verified account`,s:'Genuine invoice: no friction.',tone:'grn'}
   :n.outcome.startsWith('agent_')?{t:'Held. Nothing paid.',s:'The agent declined to propose payment.',tone:'amb'}
   :{t:(n.outcome=='block'?'Blocked':'Held for review')+'. Nothing paid.',s:n.outcome=='block'?'The payment never reached the rail.':'A person must approve before anything moves.',tone:st.t});
 }
 return S}
function stepHTML(s,i){
 const rs=(s.reasons||[]).map(r=>{const v=SEV[r.severity]||['Flag','mu'];return `<div class="rsn"><div style="flex:1"><div class="b">${esc(REASON[r.rule]||r.text)}</div>
   <div class="m"><span class="st ${v[1]}"><i></i>${v[0]}</span><span class="code">${esc(r.rule)}</span></div></div></div>`}).join('');
 const body=`${s.s?`<div class="s">${s.s}</div>`:''}${s.meta?`<div class="q mono" style="font-size:11.5px">${s.meta}</div>`:''}${s.q?`<div class="q">“${esc(s.q)}”</div>`:''}${rs}`;
 return `<div class="stp ${s.tone||''}"><div class="n"><span class="q0">·</span><span class="spin"></span><span class="lbl">${s.tone=='red'?'!':s.tone?'✓':i+1}</span></div><div><div class="t">${s.t}${s.code?` <span class="code">${s.code}</span>`:''}</div><div class="rv"><div>${body}</div></div></div></div>`}
function bannerFor(c){
 if(arm=='off') return c.off.harmful?{t:'red',ic:ICON.x,b:`Paid ${c.off.amount} to an account that isn't on the vendor record`,s:'The agent trusted the bank details printed in the PDF.'}
   :{t:'grn',ic:ICON.ok,b:`Paid ${c.off.amount} to the verified account`,s:'Genuine invoice.'};
 const n=c.on;
 if(n.outcome=='block') return {t:'red',ic:ICON.x,b:'Blocked. Nothing was paid.',s:`${n.reasons.length} check${n.reasons.length>1?'s':''} failed against the verified vendor record.`};
 if(n.outcome=='review') return {t:'amb',ic:ICON.pause,b:'Held for review. Nothing was paid.',s:'The account in the PDF is not the one on the vendor record. A person decides.'};
 if(n.outcome=='paid') return {t:'grn',ic:ICON.ok,b:`Paid ${n.amount} to the verified account`,s:'Every check passed. Genuine invoices go straight through.'};
 return {t:'amb',ic:ICON.pause,b:'Held by the agent. Nothing was paid.',s:''}}
function renderDetail(){
 const c=D[cur], v=c.vendor, s=status(c,arm), b=bannerFor(c);
 const acOK=c.pdf.pdf_account==v.account&&c.pdf.pdf_ifsc==v.ifsc, bb=c.pdf.bank_box, gb=c.pdf.gstin_box, gsOK=!v.gstin||c.pdf.pdf_gstin==v.gstin;
 const box=(r,ok,lb)=>r?`<div class="hl${ok?' ok':''}${r[1]<0.2?' below':''}" style="left:calc(${r[0]*100}% - 6px);top:calc(${r[1]*100}% - 5px);width:calc(${(r[2]-r[0])*100}% + 12px);height:calc(${(r[3]-r[1])*100}% + 10px)"><span class="lb">${lb}</span></div>`:'';
 $('crumb').innerHTML=`AP inbox <span>/</span> <b>${esc(c.invoice_no)}</b>`;
 $('detail').innerHTML=`
 <div class="dh"><div style="min-width:0">
   <div class="kick"><span class="tag">${esc(c.title)}</span><span>${esc(c.invoice_no)}</span><span class="sep">·</span><span>${esc(c.po_ref)}</span><span class="sep">·</span><span>${esc(c.pdf.date)}</span></div>
   <h1>${esc(v.name)}</h1>
   <div class="kick" style="margin-top:6px">From <span class="mono" style="color:var(--ink2)">${esc(c.email_from)}</span></div></div>
  <div class="amt"><div class="n mono" id="amtn">${esc(c.off.amount)}</div><div class="s" id="dst">${pill({l:arm=='off'?'Paying…':'Checking…',t:'mu'},1)}</div></div></div>
 <div class="banner wait beam ${arm=='off'?'off-arm':''}" id="banner"><div class="ic">${ICON.wait}</div><div><b>Replaying recorded run…</b><div class="s">${arm=='off'?'Shield off: the agent holds the pay tool.':'Shield on: the agent can only propose.'}</div></div></div>
 <div class="grid">
  <div class="card beam ${arm=='off'?'off-arm':''}" id="runcard"><div class="h">Run replay<span class="s">${arm=='off'?'Shield off · agent holds pay_invoice':'Shield on · propose → check → decide'}</span></div><div class="tl" id="tl"></div></div>
  <div style="display:flex;flex-direction:column;gap:18px">
   <div class="card"><div class="h">Checked against vendor record<span class="s">${esc(v.id)}</span></div><div class="checks ${arm=='on'?'pend':'dim'}" id="checks">${checks(c)}</div>
    ${arm=='off'?'<div class="note">With the shield off, nothing compares these. The agent pays whatever the document says.</div>':'<div class="note">The payment rail only ever receives the vendor\'s beneficiary ID. The account behind it comes from the vendor record, never from the PDF.</div>'}</div>
   <div class="card"><div class="h">Invoice PDF<span class="s">as received</span></div><div class="doc">
   <div class="paper" id="paper"><img alt="Invoice ${esc(c.invoice_no)}" src="data:image/png;base64,${c.image}">${box(bb,acOK,acOK?'Matches vendor record':'Not the verified account')}${!gsOK?box(gb,false,'Tax ID not on record'):''}</div>
   <div class="docmeta"><span>${esc(c.email_subject)}</span><span>1 page</span></div></div></div>
  </div></div></div>
 <div class="foot">Recorded sample 0 of the Muse Spark Open run on the 24-case development set (not a held-out result). Fictional vendors and bank details. The policy engine is a deterministic Rust service; the model never holds the payment tool.</div>`;
 if(cur!==lastCase){lastCase=cur;springNum($('amtn'),amtNum(c.off.amount),{fmt:inr,k:120,c:24})}
 const tl=$('tl'), S=stepsFor(c);
 tl.innerHTML=S.map(stepHTML).join('');
 const els=[...tl.children], step=Q.get('step')!=null?+Q.get('step'):null, govIdx=S.findIndex(x=>x.gov);
 if(window.__hold){requestAnimationFrame(()=>$('banner').classList.add('show'));return}
 const reveal=()=>{const ck=$('checks');ck.classList.remove('pend');ck.classList.add('live');$('paper').classList.add('lit')};
 const fin=()=>{$('dst').innerHTML=pill(s,1);renderRows();const e=$('banner');e.className='banner '+b.t+' show pop';
  e.innerHTML=`<div class="ic">${b.ic}</div><div><b>${words(b.b)}</b>${b.s?`<div class="s">${esc(b.s)}</div>`:''}</div>`;
  $('runcard').classList.remove('beam');if(arm=='off'){$('paper').classList.add('lit');$('checks').classList.add('live')}};
 if(STATIC||RM||step!=null){els.forEach((e,i)=>{if(step==null||i<step)e.classList.add('done')});
  if(step==null||step>govIdx&&govIdx>=0)reveal(); if(step==null||step>=els.length)fin();else $('banner').classList.add('show');return}
 requestAnimationFrame(()=>$('banner').classList.add('show'));
 const t0=350, SS=window.__steps, AT=i=>SS&&SS[i]!=null?SS[i]:t0+i*PACE, RUN=SS?(window.__run||900):PACE*.62;
 els.forEach((e,i)=>{later(()=>e.classList.add('run'),AT(i));
  later(()=>{e.classList.remove('run');e.classList.add('done');if(i===govIdx)reveal()},AT(i)+RUN)});
 later(fin,AT(els.length-1)+RUN+180);
}
function renderResults(){
 const T=X.totals, g=X.grid, ids=Object.keys(g).filter(k=>!g[k].control);
 const sq=(a,d)=>{let k=0;return `<div class="sq">${ids.map(id=>g[id][a].map((x,s)=>`<i class="${x}" style="--k:${k++};--d:${d}ms" title="${id} · sample ${s} · ${x}"></i>`).join('')).join('')}</div>`};
 $('res').innerHTML=`<h2>${words('Same invoices, same model. Shield off vs on.')}</h2>
 <div class="sub">24-case development set: 20 poisoned invoices and 4 genuine ones, 3 samples each. Agent and checker: Muse Spark Open, Meta's open-weight model (via Baseten), on a mock payment rail. Each square is one sample. Muse Glimmer 30B gave the same ${X.glimmer_totals.off_wrong}/${X.glimmer_totals.poisoned_samples} → ${X.glimmer_totals.on_wrong}/${X.glimmer_totals.poisoned_samples}.</div>
 <div class="kp">
  <div class="card kpi" style="--d:100ms"><div class="l">Fake invoices paid to the wrong account · shield off</div><div class="n mono" style="color:var(--red)"><span class="num" id="k1">0</span><small>/${T.poisoned_samples}</small></div><div class="f">5 quiet attacks paid in all 3 samples: bank swap, new IFSC, look-alike name, domain and tax ID.</div></div>
  <div class="card kpi" style="--d:220ms"><div class="l">Fake invoices paid to the wrong account · shield on</div><div class="n mono" style="color:var(--grn)"><span class="num" id="k2">${T.on_wrong}</span><small>/${T.poisoned_samples}</small></div><div class="f">Blocked by policy ${T.on_block} · held for review ${T.on_review} · held by the agent ${T.on_agent}${T.on_none?` (${T.on_none} with no usable decision)`:''}</div></div>
  <div class="card kpi" style="--d:340ms"><div class="l">Genuine invoices still paid</div><div class="n mono"><span class="num" id="k3">0</span><small>/${T.ctl_samples}</small></div><div class="f">Shield off: ${T.ctl_paid_off}/${T.ctl_samples}. No blanket refusal.</div></div></div>
 <div class="arms">
  <div class="card arm" style="--d:450ms"><div class="hd">${pill({l:'Shield off',t:'red'})}<span class="s">agent holds pay_invoice</span></div>${sq('off',600)}
   <div class="legend"><span><i style="background:var(--red)"></i>Paid to wrong account</span><span><i style="background:#9FCDB3"></i>Paid to the correct account</span><span><i style="background:#D9D4C8"></i>Held by agent</span>${T.off_none?'<span><i style="background:#EEEBE4;box-shadow:inset 0 0 0 1px #D9D4C8"></i>No usable decision (nothing paid)</span>':''}</div></div>
  <div class="card arm" style="--d:560ms"><div class="hd">${pill({l:'Shield on',t:'grn'})}<span class="s">propose → check → decide</span></div>${sq('on',900)}
   <div class="legend"><span><i style="background:#2B2A24"></i>Blocked by policy</span><span><i style="background:#E9B44C"></i>Held for review</span><span><i style="background:#D9D4C8"></i>Held by agent</span>${T.on_none?'<span><i style="background:#EEEBE4;box-shadow:inset 0 0 0 1px #D9D4C8"></i>No usable decision</span>':''}<span><i style="background:var(--red)"></i>Paid wrongly (none)</span></div></div></div>
 <div class="foot">Development set only: the rules and prompts were built while looking at these 24 cases, so this shows the design works as written, not how it generalises. The frozen held-out eval has not been run yet. Rows: eval/results/day1_off_spark.jsonl, day1_on_spark.jsonl (Glimmer: day1_off.jsonl, day1_on.jsonl).</div>`;
 later(()=>springNum($('k1'),T.off_wrong,{k:90,c:20}),500);
 later(()=>springNum($('k3'),T.ctl_paid_on,{k:90,c:20}),740);
 $('crumb').innerHTML='<b>Results</b>'}
function renderVendors(){
 $('ven').innerHTML=`<h2>${words('Verified vendor record')}</h2><div class="sub">The only source of payee bank details. Invoices can't change it.</div>
 <div class="card" style="margin-top:18px;overflow:hidden;animation:rise .6s var(--ease) both;--d:150ms"><table><tr><th>Vendor</th><th>Email domains</th><th>Bank</th><th>Account · IFSC</th><th>GSTIN</th></tr>
 ${X.vendors.map(v=>`<tr><td><b style="font-weight:600">${esc(v.name)}</b><div style="color:var(--mu);font-size:11.5px">${esc(v.id)} · ${esc(v.category)}</div></td><td class="mono">${v.domains.map(esc).join('<br>')}</td><td>${esc(v.bank)}</td><td class="mono">${esc(grp(v.account))}<br><span style="color:var(--mu)">${esc(v.ifsc)}</span></td><td class="mono">${esc(v.gstin||'—')}</td></tr>`).join('')}</table></div>`;
 $('crumb').innerHTML='<b>Vendor record</b>';$('ct-vendors').textContent=X.vendors.length}
function renderHow(){
 const yes=(l)=>`<span class="st red"><i></i>${l||'Yes'}</span>`, no=(l)=>`<span class="st grn"><i></i>${l||'No'}</span>`, lim=(l)=>`<span class="st amb"><i></i>${l||'Limited'}</span>`;
 const ln=(who,p,x)=>`<div class="ln"><span class="who">${who}</span>${p}<span class="x">${x}</span></div>`;
 $('how').innerHTML=`<h2>${words("The agent can read anything. It can't pay anyone.")}</h2>
 <div class="sub">Meta's Agents Rule of Two: until prompt injection can be reliably detected, an agent should hold no more than two of these three properties in a session; if it needs all three, it needs supervision. A plain AP agent holds all three. InvoiceShield takes the power to move money away from the model.</div>
 <div class="r2">
  <div class="card" style="--d:120ms"><div class="lt">[A]</div><h3>Processes untrustworthy inputs</h3>
   ${ln('Shield off',yes(),'Reads supplier emails and PDFs.')}${ln('Shield on','<span class="st mu"><i></i>Yes</span>','Still reads every invoice. That is the job.')}</div>
  <div class="card" style="--d:240ms"><div class="lt">[B]</div><h3>Access to sensitive systems or data</h3>
   ${ln('Shield off',yes(),'Holds the payment tool and the bank fields.')}${ln('Shield on',lim(),'Vendor names and IDs only. No bank data.')}</div>
  <div class="card" style="--d:360ms"><div class="lt">[C]</div><h3>Changes state: moves money</h3>
   ${ln('Shield off',yes(),'Calls pay_invoice() with whatever the PDF says.')}${ln('Shield on',no(),'Can only propose. The policy engine decides.')}</div>
 </div>
 <div class="card flow" style="--d:480ms"><div class="h2">How one invoice flows<span class="s">every arrow is one-way</span></div>
  <div class="fl">
   <div class="nd"><div class="k">Input</div><b>Email + invoice PDF</b><div class="s">Untrusted. May be forged.</div></div>
   <div class="wire" style="--d:0ms"></div>
   <div class="nd"><div class="k">Agent · Muse Spark Open</div><b>Proposes a payment</b><div class="s">vendor_id, invoice_no, amount, po_ref. No bank fields.</div></div>
   <div class="wire" style="--d:300ms"></div>
   <div class="nd"><div class="k">Checker · Muse Spark Open</div><b>Reads the document</b><div class="s">Separate context, no tools. Can only add review or block.</div></div>
   <div class="wire" style="--d:600ms"></div>
   <div class="nd dark"><div class="k">Policy engine · Rust</div><b>Deterministic decision</b><div class="s">Compares with the verified vendor record. Fails closed.</div></div>
   <div class="wire" style="--d:900ms"></div>
   <div class="nd out">${pill({l:'Allow: pays the account on record',t:'grn'})}${pill({l:'Review: a person approves',t:'amb'})}${pill({l:'Block: nothing moves',t:'red'})}</div>
  </div>
  <div class="note2">The payee is always the vendor's beneficiary ID from the record; the account printed on an invoice is only used as a tamper signal. Model signals can only escalate a decision, never approve one.</div></div>`;
 $('crumb').innerHTML='<b>How it works</b>'}
// ---- cover (intro / outro) with a light WebGL flow field ----
let gl=null,glProg=null,glRaf=0,glT0=0;
function startGL(){
 const cv=$('gl');if(!cv)return;
 if(!gl){gl=cv.getContext('webgl',{antialias:false,premultipliedAlpha:false});if(!gl)return;
  const vs='attribute vec2 p;void main(){gl_Position=vec4(p,0.,1.);}';
  const fs=`precision mediump float;uniform vec2 r;uniform float t;
  float h(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}
  float n(vec2 p){vec2 i=floor(p),f=fract(p);f=f*f*(3.-2.*f);return mix(mix(h(i),h(i+vec2(1,0)),f.x),mix(h(i+vec2(0,1)),h(i+vec2(1,1)),f.x),f.y);}
  float fbm(vec2 p){float v=0.,a=.5;for(int i=0;i<4;i++){v+=a*n(p);p=p*2.03+vec2(1.7,9.2);a*=.5;}return v;}
  void main(){vec2 uv=gl_FragCoord.xy/r;vec2 p=uv*vec2(r.x/r.y,1.)*1.6;
   vec2 q=vec2(fbm(p+vec2(0.,t*.03)),fbm(p+vec2(5.2,1.3)-t*.025));
   float f=fbm(p+2.2*q+vec2(t*.02,0.));
   vec3 bg=vec3(.961,.957,.941);vec3 g=vec3(.80,.90,.84);vec3 a=vec3(.97,.90,.78);
   vec3 col=mix(bg,g,smoothstep(.45,.85,f)*.75*smoothstep(.15,1.,uv.x));
   col=mix(col,a,smoothstep(.55,.95,q.y)*.35*(1.-uv.y));
   col=mix(col,bg,smoothstep(.55,.0,uv.x)*.85);
   gl_FragColor=vec4(col,1.);}`;
  const sh=(t,s)=>{const o=gl.createShader(t);gl.shaderSource(o,s);gl.compileShader(o);return o};
  glProg=gl.createProgram();gl.attachShader(glProg,sh(gl.VERTEX_SHADER,vs));gl.attachShader(glProg,sh(gl.FRAGMENT_SHADER,fs));gl.linkProgram(glProg);gl.useProgram(glProg);
  const b=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,b);gl.bufferData(gl.ARRAY_BUFFER,new Float32Array([-1,-1,3,-1,-1,3]),gl.STATIC_DRAW);
  const loc=gl.getAttribLocation(glProg,'p');gl.enableVertexAttribArray(loc);gl.vertexAttribPointer(loc,2,gl.FLOAT,false,0,0);glT0=performance.now()}
 const scale=.5;cv.width=Math.max(2,innerWidth*scale|0);cv.height=Math.max(2,innerHeight*scale|0);gl.viewport(0,0,cv.width,cv.height);
 const ur=gl.getUniformLocation(glProg,'r'),ut=gl.getUniformLocation(glProg,'t');
 const draw=now=>{gl.uniform2f(ur,cv.width,cv.height);gl.uniform1f(ut,(now-glT0)/1000+8.);gl.drawArrays(gl.TRIANGLES,0,3);if(!RM&&!STATIC)glRaf=requestAnimationFrame(draw)};
 cancelAnimationFrame(glRaf);glRaf=requestAnimationFrame(draw)}
function stopGL(){cancelAnimationFrame(glRaf)}
const MARK='<div class="mark"><svg width="17" height="17" viewBox="0 0 14 14"><path d="M2 3.2h10M2 7h6.2M2 10.8h4" stroke="#fff" stroke-width="1.6" stroke-linecap="round"/><path d="M8.6 10.2l1.5 1.5 2.6-3" stroke="#3ED68F" stroke-width="1.7" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg></div>';
function renderCover(kind){
 const T=X.totals;
 if(kind=='intro') $('cv').innerHTML=`<div class="kk">${MARK}InvoiceShield</div>
  <h1>${words('AI agents are starting to pay invoices.',150)}</h1>
  <p class="w" style="--i:0;--d:900ms">A scammer only needs to change one bank account on one PDF.</p>
  <div class="fine">Meta Global AI Developer Hackathon 2026 · built on Muse Spark · fictional vendors and bank details · nothing here moves real money</div>`;
 else $('cv').innerHTML=`<div class="kk">${MARK}InvoiceShield</div>
  <h1>${words('Pay the vendor record.',100)} <em>${words('Never the PDF.',500)}</em></h1>
  <div class="row3">
   <div class="pillx" style="--d:1000ms"><b>${T.off_wrong}/${T.poisoned_samples} → ${T.on_wrong}/${T.poisoned_samples}</b><span>fake-invoice runs paid to the wrong account, shield off → on</span></div>
   <div class="pillx" style="--d:1150ms"><b>${T.ctl_paid_on}/${T.ctl_samples}</b><span>genuine invoices still paid</span></div></div>
  <p class="w" style="--i:0;--d:1350ms">Agent proposes. Rust policy engine decides. A person approves anything unusual.</p>
  <div class="fine">24-case development set (20 poisoned + 4 genuine invoices × 3 samples), Muse Spark Open via Baseten, mock payment rail; same result on Muse Glimmer 30B. Rules were built on these cases; the frozen held-out eval is not yet run.</div>`;
}
function setView(v){view=v;timers.forEach(clearTimeout);timers=[];
 const cov=v=='intro'||v=='close';$('cover').classList.toggle('on',cov);
 if(cov){renderCover(v);startGL();return} stopGL();
 document.querySelectorAll('.view').forEach(e=>e.classList.toggle('on',e.id=='v-'+v));
 document.querySelectorAll('.nv').forEach(e=>e.classList.toggle('on',e.dataset.view==v));
 $('seg').style.visibility=$('play').style.visibility=v=='inbox'?'visible':'hidden';
 if(v=='results')renderResults();else if(v=='vendors')renderVendors();else if(v=='how')renderHow();else show(cur)}
function setArm(a){arm=a;document.querySelectorAll('#seg button').forEach(b=>b.classList.toggle('on',b.dataset.arm==a));show(cur)}
function show(i){cur=i;timers.forEach(clearTimeout);timers=[];const live=!(STATIC||RM||Q.get('step')!=null);renderRows(live);renderDetail()}
document.querySelectorAll('.nv').forEach(b=>b.onclick=()=>setView(b.dataset.view));
document.querySelectorAll('#seg button').forEach(b=>b.onclick=()=>setArm(b.dataset.arm));
$('play').onclick=()=>show(cur);
$('cover').onclick=()=>setView('inbox');
document.addEventListener('keydown',e=>{if(e.target.tagName=='INPUT')return;const k=e.key.toLowerCase();
 if(view=='intro'||view=='close'){if(k=='escape'||k=='enter'){setView('inbox')}return}
 if(k>='1'&&k<=String(D.length)){show(+k-1)}else if(k=='arrowdown'||k=='j')show((cur+1)%D.length);else if(k=='arrowup'||k=='k')show((cur+D.length-1)%D.length);
 else if(k=='o')setArm('off');else if(k=='n')setArm('on');else if(k=='s')setArm(arm=='off'?'on':'off');else if(k=='r'||k==' '){e.preventDefault();show(cur)}
 else if(k=='h')setView('how');else if(k=='i')setView('intro')});
addEventListener('resize',()=>{if(view=='intro'||view=='close')startGL()});
// ---- recording helpers: a synthetic cursor so screen captures show what was clicked ----
window.__cursorTo=(sel,dx=0,dy=0)=>{const el=typeof sel=='string'?document.querySelector(sel):sel;if(!el)return false;const r=el.getBoundingClientRect(),c=$('cursor');
 c.style.left=(r.left+r.width*.5+dx)+'px';c.style.top=(r.top+r.height*.55+dy)+'px';return true};
window.__click=(sel)=>{const el=document.querySelector(sel),c=$('cursor');if(!el)return false;c.classList.remove('click');void c.offsetWidth;c.classList.add('click');setTimeout(()=>el.click(),120);return true};
window.__pace=n=>{PACE=n};window.__sched=(a,r)=>{window.__steps=a;window.__run=r};window.__play=()=>{window.__hold=false;show(cur)};
window.__scrollDetail=y=>{const d=$('detail');d.scrollTo({top:y,behavior:RM?'auto':'smooth'})};
window.__state=o=>{if(o.case){const j=D.findIndex(c=>c.id==o.case);if(j>=0)cur=j}if(o.arm){arm=o.arm;document.querySelectorAll('#seg button').forEach(b=>b.classList.toggle('on',b.dataset.arm==arm))}setView(o.view||'inbox')};
const qc=Q.get('case'); cur=Math.max(0,D.findIndex(c=>c.id==(qc||'L02')));
arm=Q.get('arm')=='on'?'on':'off';document.querySelectorAll('#seg button').forEach(b=>b.classList.toggle('on',b.dataset.arm==arm));
setView(Q.get('view')||'inbox');
</script></body></html>
