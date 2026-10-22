// "Is my PC exposed?": runs the read-only check, explains each finding, offers fixes only as buttons.
(() => {
  let result = null, running = null, showAll = false;
  const VERDICT = {
    ok: ['Looks good', 'Nothing here lets strangers in. The notes below are worth knowing, not worrying about.'],
    warn: ['A few things to look at', 'Nothing is wide open, but some doors could be closed.'],
    bad: ['Something needs fixing', 'This PC is more open than it should be. Fix the red items first.'],
  };

  function finding(f) {
    const row = el('div', 'xp-item xp-' + f.level);
    const dot = el('span', 'xp-dot'); const body = el('div', 'meta');
    body.append(el('div', 'name', f.title), el('div', 'sub', f.detail));
    row.append(dot, body);
    if (f.fix) {
      const b = el('button', 'btn small' + (f.level === 'bad' ? ' primary' : ''), f.fix.label);
      if (f.fix.admin) b.title = t('Windows will ask for permission');
      b.onclick = async () => {
        if (f.fix.action === 'page') { showPage(f.fix.args[0]); return; }
        const r = await App.api.exposure_fix(f.fix.action, f.fix.args);
        if (r && r.error) toast(r.error); else { toast(t('Done. Checking again…')); setTimeout(run, 1500); }
      };
      row.append(b);
    }
    return row;
  }

  function portRow(p) {
    const { row, sub } = fileRow(p.what || p.program);
    const k = row.querySelector('.kind'); k.className = 'kind xp-port'; k.textContent = p.port;
    sub.textContent = (p.what ? p.program + ' · ' : '') + (p.network ? t('reachable from the network') : t('only this PC')) + (p.why ? ' · ' + p.why : '');
    sub.setAttribute('translate', 'no');
    if (!p.network) row.classList.add('xp-local');
    return row;
  }

  function render(r) {
    if (r !== undefined) result = r;
    $('xpResult').hidden = !result;
    if (!result) return;
    const [title, text] = VERDICT[result.verdict];
    $('xpVerdict').className = 'card xp-verdict xp-' + result.verdict;
    $('xpVerdictTitle').textContent = t(title); $('xpVerdictText').textContent = t(text);
    $('xpFindings').replaceChildren(...result.findings.map(finding));
    const ports = result.ports.filter(p => showAll || p.network);
    $('xpPorts').replaceChildren(...ports.map(portRow));
    $('xpShowAll').textContent = showAll ? t('Only show network ones') : t('Also show local-only ones');
    const fw = result.upnp && result.upnp.forwards || [];
    $('xpUpnpCard').hidden = !fw.length;
    $('xpUpnp').replaceChildren(...fw.map(f => {
      const { row, sub } = fileRow(f.name || t('Unnamed forward'));
      const k = row.querySelector('.kind'); k.className = 'kind xp-port'; k.textContent = f.external;
      sub.textContent = `${t('internet port')} ${f.external}/${f.protocol} → ${f.client}:${f.internal}` + (f.client === result.ip ? ' (' + t('this PC') + ')' : '');
      const acts = el('div', 'acts');
      const b = el('button', 'btn small ghost', t('Close'));
      b.onclick = async () => {
        if (!confirm(t('Ask the router to close this port? The program that opened it may stop working from outside.'))) return;
        const r = await App.api.exposure_unforward(f.external, f.protocol);
        if (r && r.error) toast(r.error); else render(r);
      };
      acts.append(b); row.append(acts);
      return row;
    }));
  }

  async function run() {
    if (running) return;
    $('xpRun').disabled = true; $('xpRun').textContent = t('Checking…');
    running = await App.api.exposure_check();
  }

  App.on('exposure', render);
  App.on('job', job => {
    if (job.kind !== 'exposure' || job.id !== running || job.status === 'running') return;
    running = null; $('xpRun').disabled = false; $('xpRun').textContent = t('Check again');
    if (job.status === 'error') toast(job.text);
  });

  App.pages.exposure = {
    init() {
      $('xpRun').onclick = run;
      $('xpShowAll').onclick = () => { showAll = !showAll; render(); };
    },
    async show() { const r = await App.api.exposure_state(); render(r); if (!r) run(); },
  };
})();
