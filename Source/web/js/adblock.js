// Ad blocker test page.
(() => {
  async function run() {
    const btn = $('abRun');
    btn.disabled = true; btn.textContent = 'Testing…';
    $('abHeadline').textContent = 'Testing…';
    $('abDetail').textContent = '';
    try {
      render(await App.api.adblock_test());
    } finally { btn.disabled = false; btn.textContent = 'Run again'; }
  }

  function render(r) {
    const ring = $('abRing'), pct = r.score;
    ring.style.setProperty('--p', pct);
    ring.style.setProperty('--ring', pct >= 80 ? 'var(--ok)' : pct >= 30 ? 'var(--warn)' : 'var(--err)');
    $('abScore').textContent = pct + '%';
    $('abHeadline').textContent = pct >= 80 ? 'Ads are blocked' : pct >= 30 ? 'Partly blocked' : 'Ads are not blocked';
    $('abDetail').textContent = `${r.blocked} of ${r.total} ad and tracker domains are blocked on this PC.`;
    const dns = $('abDns'); dns.hidden = false; dns.replaceChildren();
    const named = r.dns.filter(d => d.name);
    dns.append(document.createTextNode('DNS servers: '));
    r.dns.forEach((d, i) => { const c = el('code', null, d.ip + (d.name ? ` (${d.name})` : '')); dns.append(c, document.createTextNode(i < r.dns.length - 1 ? ', ' : '')); });
    if (r.hosts_count) dns.append(document.createTextNode(` · hosts file blocks ${r.hosts_count} names`));
    if (!named.length && pct < 30) dns.append(el('div', 'small', 'Tip: an ad-blocking DNS such as AdGuard DNS (94.140.14.14) blocks ads for every app on this PC.'));

    const grid = $('abCats'); grid.replaceChildren();
    for (const [cat, rows] of Object.entries(r.categories)) {
      const c = el('section', 'card'), blocked = rows.filter(x => x.blocked).length;
      const h = el('h2', null, cat); h.append(el('span', 'spacer'), el('span', blocked === rows.length ? 'pill-ok' : blocked ? 'pill-bad' : 'pill-bad', `${blocked}/${rows.length}`));
      c.append(h);
      for (const x of rows) {
        const row = el('div', 'ab-row'), name = el('code', null, x.domain); name.setAttribute('translate', 'no');
        const pill = el('span', x.blocked ? 'pill-ok' : 'pill-bad', x.blocked ? 'Blocked' : 'Reachable');
        if (x.how) pill.title = x.how;
        row.append(name, pill); c.append(row);
      }
      grid.append(c);
    }
  }

  App.pages.adblock = { init() { $('abRun').onclick = run; } };
})();
