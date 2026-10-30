// InvoiceShield approver page. Talks only to this origin's existing API:
//   GET  /v1/reviews                      pending REVIEW decisions (approver/operator role)
//   POST /v1/decisions/{id}/resolve       {"approve": true|false} (approver role; maker-checker)
// Untrusted text (reason details come from invoice content) is only ever set with textContent.
"use strict";
(function () {
  let key = null; // in memory only
  const $ = (id) => document.getElementById(id);
  const status = (msg, err) => { const s = $("status"); s.textContent = msg; s.className = err ? "err" : ""; };
  const money = (minor, cur) => {
    try { return new Intl.NumberFormat("en-IN", { style: "currency", currency: cur }).format(minor / 100); }
    catch (_) { return (minor / 100).toFixed(2) + " " + cur; }
  };
  const why = {
    forbidden: "This key does not hold the approver role.",
    unauthorized: "Key not recognised.",
    maker_checker_proposer_cannot_resolve: "Blocked by maker-checker: you proposed this payment, so another approver must resolve it.",
    not_a_pending_review: "Already resolved (or not a REVIEW). Refresh the queue.",
    rate_limited: "Rate limited. Wait a moment and retry.",
  };
  async function api(method, path, body) {
    const r = await fetch(path, {
      method, cache: "no-store", credentials: "omit",
      headers: Object.assign({ Authorization: "Bearer " + key }, body ? { "Content-Type": "application/json" } : {}),
      body: body ? JSON.stringify(body) : undefined,
    });
    let j = null; try { j = await r.json(); } catch (_) { /* empty body */ }
    return { status: r.status, body: j };
  }
  function li(text, cls) { const e = document.createElement("li"); if (cls) e.className = cls; e.textContent = text; return e; }
  function render(items) {
    const ol = $("items"); ol.replaceChildren();
    $("count").textContent = items.length + (items.length === 1 ? " payment waiting" : " payments waiting");
    $("empty").hidden = items.length !== 0;
    const tpl = $("item");
    for (const it of items) {
      const n = tpl.content.firstElementChild.cloneNode(true);
      const q = (c) => n.querySelector(c);
      q(".vendor").textContent = it.vendor_id;
      q(".invoice").textContent = it.invoice_key;
      q(".amount").textContent = money(it.amount_minor, it.currency);
      q(".did").textContent = it.decision_id;
      q(".po").textContent = it.po_ref;
      q(".proposer").textContent = it.proposed_by || "(legacy, unknown)";
      q(".ts").textContent = new Date(it.ts * 1000).toISOString().replace("T", " ").slice(0, 19) + " UTC";
      const p = it.decision.payee;
      q(".payee").textContent = p ? p.beneficiary_id + " · " + p.bank_name : "(none)";
      for (const r of it.decision.reasons || []) {
        const e = li("", "v-" + r.verdict);
        const c = document.createElement("code"); c.textContent = r.rule;
        e.append(c, document.createTextNode(" [" + r.verdict + ", " + r.control + "] " + r.detail));
        q(".reasons").append(e);
      }
      for (const s of it.decision.signals || []) {
        const e = li("", "v-" + s.severity);
        const c = document.createElement("code"); c.textContent = "signal:" + s.kind;
        e.append(c, document.createTextNode(" [" + s.severity + "] " + (s.detail || "")));
        q(".signals").append(e);
      }
      const res = q(".result");
      const act = async (approve) => {
        if (!confirm((approve ? "Approve and pay " : "Reject ") + money(it.amount_minor, it.currency) + " to " + it.vendor_id + "?")) return;
        q(".approve").disabled = q(".reject").disabled = true;
        const r = await api("POST", "/v1/decisions/" + encodeURIComponent(it.decision_id) + "/resolve", { approve });
        if (r.status === 200) { res.className = "result ok"; res.textContent = (approve ? "Approved" : "Rejected") + " by " + r.body.approver + "."; }
        else {
          const code = r.body && r.body.error; res.className = "result err";
          res.textContent = (why[code] || "Refused (" + r.status + (code ? " " + code : "") + ").");
          if (r.status !== 409) q(".approve").disabled = q(".reject").disabled = false;
        }
      };
      q(".approve").addEventListener("click", () => act(true));
      q(".reject").addEventListener("click", () => act(false));
      ol.append(n);
    }
  }
  async function load() {
    status("Loading…");
    const r = await api("GET", "/v1/reviews");
    if (r.status !== 200) { status(why[r.body && r.body.error] || "Could not load the queue (" + r.status + ").", true); return; }
    status("Signed in as " + r.body.identity + ".");
    $("signin").hidden = true; $("queue").hidden = false;
    render(r.body.reviews);
  }
  document.addEventListener("DOMContentLoaded", () => {
    $("keyform").addEventListener("submit", (e) => { e.preventDefault(); key = $("key").value.trim(); $("key").value = ""; if (key) load(); });
    $("refresh").addEventListener("click", load);
    $("signout").addEventListener("click", () => { key = null; $("items").replaceChildren(); $("queue").hidden = true; $("signin").hidden = false; status("Key forgotten."); });
  });
})();
