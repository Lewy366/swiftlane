// Speed test page: live gauge, ping/jitter/download/upload tiles, results of this session.
(() => {
  const MAX = 1000, SWEEP = 240, R = 120, CX = 150, CY = 150;   // gauge geometry
  const TICKS = [0, 5, 10, 50, 100, 250, 500, 1000];
  let jobId = null, running = false;
  const history = [];

  // logarithmic scale so 5 Mbps and 900 Mbps both read well
  const frac = v => Math.min(1, Math.log10(1 + Math.max(0, v)) / Math.log10(1 + MAX));
  function point(f) {
    const a = (-90 - SWEEP / 2 + SWEEP * f) * Math.PI / 180;
    return [CX + R * Math.cos(a), CY + R * Math.sin(a)];
  }
  function arc(f) {
    const [x0, y0] = point(0), [x1, y1] = point(Math.max(0.0001, f));
    return `M ${x0} ${y0} A ${R} ${R} 0 ${SWEEP * f > 180 ? 1 : 0} 1 ${x1} ${y1}`;
  }
  function setGauge(value, phase) {
    $('gaugeFg').setAttribute('d', arc(value == null ? 0 : frac(value)));
    $('gaugeValue').textContent = value == null ? '–' : value >= 100 ? Math.round(value) : value.toFixed(value >= 10 ? 1 : 2);
    $('gaugePhase').textContent = phase;
  }
  function drawScale() {
    const svg = document.querySelector('.gauge');
    if (!svg.querySelector('defs')) {
      svg.insertAdjacentHTML('afterbegin', '<defs><linearGradient id="gaugeGrad" x1="0" x2="1"><stop offset="0" stop-color="var(--violet)"/><stop offset="1" stop-color="var(--cyan)"/></linearGradient></defs>');
    }
    document.querySelector('.gauge-bg').setAttribute('d', arc(1));
    const g = $('gaugeTicks'); g.innerHTML = '';
    for (const v of TICKS) {
      const a = (-90 - SWEEP / 2 + SWEEP * frac(v)) * Math.PI / 180;
      const t = document.createElementNS('http://www.w3.org/2000/svg', 'text');
      t.setAttribute('x', CX + (R + 24) * Math.cos(a)); t.setAttribute('y', CY + (R + 24) * Math.sin(a) + 4);
      t.textContent = v; g.append(t);
    }
  }
  function fmt(v) { return v == null ? '–' : v >= 100 ? Math.round(v) : v.toFixed(1); }

  function renderHistory() {
    const list = $('spHistory'); list.replaceChildren();
    for (const h of history) {
      const row = el('div', 'row'), meta = el('div', 'meta');
      meta.append(el('div', 'name', `↓ ${fmt(h.download)} Mbps   ↑ ${fmt(h.upload)} Mbps   ·   ${fmt(h.ping)} ms`),
                  el('div', 'sub', `${h.time.toLocaleTimeString()}${h.server && h.server.city ? ' · Cloudflare ' + h.server.city : ''}`));
      row.append(meta); list.append(row);
    }
    $('spEmpty').hidden = history.length > 0;
  }

  async function start() {
    if (running) { App.api.cancel_job(jobId); return; }
    running = true;
    ['spPing', 'spJitter', 'spDown', 'spUp'].forEach(id => $(id).textContent = '–');
    $('spError').hidden = true; $('spServer').textContent = '';
    $('spStart').textContent = 'Cancel';
    setGauge(null, 'Measuring ping');
    jobId = await App.api.speed_test();
  }
  function finish() { running = false; $('spStart').textContent = 'Start again'; }

  App.on('speed', d => {
    if (d.phase === 'ping') {
      if (d.value != null) { $('spPing').textContent = fmt(d.value); }
      setGauge(null, 'Measuring ping');
    } else {
      setGauge(d.value, d.phase === 'down' ? 'Measuring download' : 'Measuring upload');
      $(d.phase === 'down' ? 'spDown' : 'spUp').textContent = fmt(d.value);
    }
  });
  App.on('speed-result', r => {
    $('spPing').textContent = fmt(r.ping); $('spJitter').textContent = fmt(r.jitter);
    $('spDown').textContent = fmt(r.download); $('spUp').textContent = fmt(r.upload);
    setGauge(r.download, 'Done');
    const s = r.server || {};
    $('spServer').textContent = s.city ? `Cloudflare ${s.city}${s.country ? ' (' + s.country + ')' : ''} · ${s.ip_version}` : '';
    history.unshift({ ...r, time: new Date() }); renderHistory();
  });
  App.on('job', job => {
    if (job.kind !== 'speed' || job.id !== jobId || job.status === 'running') return;
    finish();
    if (job.status === 'cancelled') setGauge(null, 'Cancelled');
    if (job.status === 'error') { $('spError').textContent = job.text; $('spError').hidden = false; setGauge(null, 'Failed'); }
  });

  App.pages.speedtest = {
    init() { drawScale(); setGauge(null, 'Ready'); $('spStart').onclick = start; renderHistory(); },
  };
})();
