// Currency converter page.
(() => {
  const QUICK = ['USD', 'EUR', 'GBP', 'CHF', 'JPY', 'CAD', 'AUD', 'CNY', 'PLN', 'SEK', 'TRY'];  // 10 tiles = two full rows
  let loaded = false;

  function fmtMoney(v, code) {
    try { return new Intl.NumberFormat(undefined, { style: 'currency', currency: code, maximumFractionDigits: v < 1 ? 4 : 2 }).format(v); }
    catch (e) { return v.toFixed(2) + ' ' + code; }
  }

  function fill(status) {
    for (const id of ['curFrom', 'curTo']) {
      const sel = $(id), keep = sel.value;
      sel.replaceChildren(...status.currencies.map(c => { const o = el('option', null, `${c.code} · ${c.name}`); o.value = c.code; return o; }));
      if (keep) sel.value = keep;
    }
    if (!$('curFrom').value || !loaded) { $('curFrom').value = 'EUR'; $('curTo').value = 'USD'; }
    loaded = status.currencies.length > 0;
    $('curDate').textContent = status.date ? 'Rates of ' + new Date(status.date).toLocaleDateString() : '';
    const err = $('curError'); err.hidden = !status.error; err.querySelector('span').textContent = status.error || '';
  }

  async function update() {
    if (!loaded) return;
    const amount = parseFloat($('curAmount').value), from = $('curFrom').value, to = $('curTo').value;
    if (isNaN(amount)) { $('curResult').textContent = '—'; return; }
    const r = await App.api.currency_convert(amount, from, to);
    if (r.error) { $('curResult').textContent = '—'; return; }
    $('curResult').textContent = fmtMoney(r.value, to);
    $('curRate').textContent = `1 ${from} = ${r.rate.toPrecision(6)} ${to}`;
    const quick = $('curQuick'); quick.replaceChildren();
    $('curQuickTitle').textContent = `${fmtMoney(amount, from)} in other currencies`;
    for (const code of QUICK.filter(c => c !== from)) {
      const q = await App.api.currency_convert(amount, from, code);
      if (q.error) continue;
      const d = el('div'); d.append(el('b', null, fmtMoney(q.value, code)), el('small', null, code));
      d.style.cursor = 'pointer'; d.onclick = () => { $('curTo').value = code; update(); };
      quick.append(d);
    }
  }

  async function load(force) {
    $('curRefresh').disabled = true;
    try { fill(await App.api.currency_status(!!force)); } finally { $('curRefresh').disabled = false; }
    update();
  }

  App.pages.currency = {
    init() {
      let t;
      $('curAmount').oninput = () => { clearTimeout(t); t = setTimeout(update, 150); };
      $('curFrom').onchange = $('curTo').onchange = update;
      $('curSwap').onclick = () => { const a = $('curFrom').value; $('curFrom').value = $('curTo').value; $('curTo').value = a; update(); };
      $('curRefresh').onclick = () => load(true);
    },
    show() { load(false); },
  };
})();
