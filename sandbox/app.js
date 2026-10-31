"use strict";
const $ = (id) => document.getElementById(id);
function tile(label, value) {
  const d = document.createElement("div"); const b = document.createElement("b");
  b.textContent = String(value); d.appendChild(b); d.appendChild(document.createTextNode(label)); return d;
}
async function counter() {
  const r = await fetch("/api/counter"); const c = await r.json(); const el = $("counter"); el.textContent = "";
  el.appendChild(tile("attempts", c.attempts)); el.appendChild(tile("paid (to the master account)", c.paid));
  el.appendChild(tile("paid to a wrong account", c.paid_to_wrong_account)); el.appendChild(tile("held for review", c.review));
  el.appendChild(tile("blocked", c.block));
  const s = $("samples"); if (!s.childElementCount) for (const n of c.samples) {
    const b = document.createElement("button"); b.textContent = n; b.onclick = () => send(null, n); s.appendChild(b);
  }
}
function show(res) {
  const o = $("out"); o.textContent = "";
  if (res.error) { o.textContent = "Error: " + res.error; return; }
  const v = document.createElement("span"); v.className = res.verdict; v.textContent = res.verdict; o.appendChild(v);
  o.appendChild(document.createTextNode("\n\n" + JSON.stringify(res, null, 1)));
}
async function send(file, sample) {
  $("out").textContent = "Working (extraction, model, gate)...";
  let r;
  if (sample) r = await fetch("/api/try?sample=" + encodeURIComponent(sample), { method: "POST" });
  else { const f = new FormData(); f.append("file", file); r = await fetch("/api/try", { method: "POST", body: f }); }
  show(await r.json()); counter();
}
$("go").onclick = () => { const f = $("file").files[0]; if (f) send(f, null); };
counter();
