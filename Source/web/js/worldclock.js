// World clock: a live map with the night side shaded from the sun's real position, analog city clocks,
// and a planner strip that "time travels" every clock to a chosen moment. Everything is computed here, offline.
(() => {
  const TOP = 6, BOTTOM = 148;              // map shows latitude 84°N .. 58°S (y = 90 - lat)
  const HERE = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const city = id => CITIES.find(c => c[0] === id);
  const DEFAULT = ['newyork', 'london', 'tokyo', 'sydney'];
  let ids = [], shown = null /* null = now, else a Date */, timers = [], land = null, shade = { at: 0, img: null };

  // ---------- time maths (Intl knows every zone's rules, including daylight saving)
  const partsFmt = {};
  function parts(tz, date) {
    const f = partsFmt[tz] || (partsFmt[tz] = new Intl.DateTimeFormat('en-US', { timeZone: tz, hourCycle: 'h23', year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric' }));
    const o = {}; for (const p of f.formatToParts(date)) if (p.type !== 'literal') o[p.type] = +p.value;
    return o;
  }
  function offsetMin(tz, date) { const p = parts(tz, date); return Math.round((Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second) - Math.floor(date / 1000) * 1000) / 60000); }
  const lang = () => ({ de: 'de-DE', pl: 'pl-PL', it: 'it-IT' }[App.state && App.state.language] || undefined);
  const timeStr = (tz, date) => new Intl.DateTimeFormat(lang(), { timeZone: tz, hour: '2-digit', minute: '2-digit' }).format(date);
  function relLabel(tz, date) {
    const a = parts(HERE, date), b = parts(tz, date);
    const dayDiff = Math.round((Date.UTC(b.year, b.month - 1, b.day) - Date.UTC(a.year, a.month - 1, a.day)) / 864e5);
    const day = dayDiff === 0 ? t('Today') : dayDiff === 1 ? t('Tomorrow') : dayDiff === -1 ? t('Yesterday') : '';
    const diff = offsetMin(tz, date) - offsetMin(HERE, date);
    if (!diff) return `${day}, ${t('same time')}`;
    const h = Math.trunc(Math.abs(diff) / 60), m = Math.abs(diff) % 60;
    return `${day}, ${diff > 0 ? '+' : '−'}${h}${m ? ':' + String(m).padStart(2, '0') : ''} ${t(h === 1 && !m ? 'hr' : 'hrs')}`;
  }
  const isNight = (tz, date) => { const h = parts(tz, date).hour; return h < 6 || h >= 18; };

  // ---------- the sun: where it's overhead right now (accurate to a fraction of a degree)
  function subsolar(date) {
    const d = date / 864e5 + 2440587.5 - 2451545.0, rad = Math.PI / 180;
    const g = (357.529 + 0.98560028 * d) * rad, q = 280.459 + 0.98564736 * d;
    const L = (q + 1.915 * Math.sin(g) + 0.020 * Math.sin(2 * g)) * rad, e = (23.439 - 0.00000036 * d) * rad;
    const ra = Math.atan2(Math.cos(e) * Math.sin(L), Math.cos(L)) / rad / 15;
    const decl = Math.asin(Math.sin(e) * Math.sin(L)) / rad;
    let eqt = q / 15 - ra; eqt = ((eqt + 12) % 24 + 24) % 24 - 12;
    const utcH = (date % 864e5) / 36e5;
    let lon = -15 * (utcH - 12 + eqt); lon = ((lon + 540) % 360) - 180;
    return { lat: decl, lon };
  }
  // night shading, one cell per degree: dark below -12° sun altitude, fading through twilight to day
  function nightImage(date) {
    const W = 360, H = BOTTOM - TOP, img = new ImageData(W, H), s = subsolar(date), rad = Math.PI / 180;
    const sd = Math.sin(s.lat * rad), cd = Math.cos(s.lat * rad), light = document.body.classList.contains('light');
    const [r, g, b, max] = light ? [16, 26, 56, 0.46] : [1, 3, 10, 0.74];
    for (let y = 0; y < H; y++) {
      const lat = (90 - (y + TOP + 0.5)) * rad, sl = Math.sin(lat), cl = Math.cos(lat);
      for (let x = 0; x < W; x++) {
        const lon = (x + 0.5 - 180) * rad;
        const alt = Math.asin(sl * sd + cl * cd * Math.cos(lon - s.lon * rad)) / rad;
        const k = alt >= 0.8 ? 0 : alt <= -12 ? 1 : (0.8 - alt) / 12.8;
        const i = (y * W + x) * 4;
        img.data[i] = r; img.data[i + 1] = g; img.data[i + 2] = b; img.data[i + 3] = Math.round(255 * max * k * k * (3 - 2 * k));
      }
    }
    return { img, sun: s };
  }

  // ---------- map
  function drawMap() {
    const box = $('wcMap'), cv = $('wcCanvas'), w = box.clientWidth;
    if (!w) return;
    const h = Math.round(w * (BOTTOM - TOP) / 360), dpr = window.devicePixelRatio || 1, now = shown || new Date();
    if (cv.width !== Math.round(w * dpr)) { cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr); cv.style.height = h + 'px'; }
    const ctx = cv.getContext('2d'), sx = cv.width / 360, light = document.body.classList.contains('light');
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = light ? '#cfdcec' : '#14284a'; ctx.fillRect(0, 0, cv.width, cv.height);
    land = land || new Path2D(WORLD_LAND);
    ctx.setTransform(sx, 0, 0, sx, 0, -TOP * sx);
    ctx.fillStyle = light ? '#fbfcfe' : '#56698c'; ctx.fill(land);
    ctx.lineWidth = 0.25; ctx.strokeStyle = light ? 'rgba(40,60,90,.18)' : 'rgba(255,255,255,.16)'; ctx.stroke(land);
    if (Math.abs(now - shade.at) > 60000 || shade.light !== light) { shade = { at: +now, light, ...nightImage(now) }; }
    const off = new OffscreenCanvas(360, BOTTOM - TOP); off.getContext('2d').putImageData(shade.img, 0, 0);
    ctx.setTransform(1, 0, 0, 1, 0, 0); ctx.imageSmoothingEnabled = true; ctx.imageSmoothingQuality = 'high';
    ctx.drawImage(off, 0, 0, cv.width, cv.height);
    // the sun
    const sx2 = (shade.sun.lon + 180) / 360 * w, sy2 = (90 - shade.sun.lat - TOP) / (BOTTOM - TOP) * h;
    const sun = $('wcSun'); sun.style.left = sx2 + 'px'; sun.style.top = sy2 + 'px';
    drawPins(w, h, now);
  }
  function drawPins(w, h, now) {
    const layer = $('wcPins'); layer.replaceChildren();
    const list = [here(), ...ids.map(city).filter(Boolean)];
    // labels: try right, left, then below/above on each side; never on top of another label or another pin
    const boxes = list.map(c => ({ x: (c[5] + 180) / 360 * w, y: (90 - c[4] - TOP) / (BOTTOM - TOP) * h }));
    const taken = boxes.map(p => ({ l: p.x - 6, r: p.x + 6, t: p.y - 6, b: p.y + 6 }));  // the pins themselves
    const hit = a => taken.some(b => a.l < b.r && a.r > b.l && a.t < b.b && a.b > b.t);
    list.forEach((c, i) => {
      const { x, y } = boxes[i], text = timeStr(c[3], now), wPill = 34 + (c[1].length + text.length) * 6.6, hPill = 22;
      const options = [['', 9, -hPill / 2], ['left', -9 - wPill, -hPill / 2], ['below', 9, 12], ['below left', -9 - wPill, 12], ['above', 9, -12 - hPill], ['above left', -9 - wPill, -12 - hPill]];
      let pick = options.find(([, dx, dy]) => { const a = { l: x + dx, r: x + dx + wPill, t: y + dy, b: y + dy + hPill }; return a.l > 0 && a.r < w && a.t > 0 && a.b < h && !hit(a); }) || options[0];
      const [cls, dx, dy] = pick;
      taken.push({ l: x + dx, r: x + dx + wPill, t: y + dy, b: y + dy + hPill });
      const pin = el('div', 'wc-pin' + (c[0] === 'here' ? ' here' : ''));
      pin.style.left = x + 'px'; pin.style.top = y + 'px';
      const label = el('span', 'wc-pill' + (cls ? ' ' + cls : '')); const nm = el('b', null, c[1]); nm.setAttribute('translate', 'no');
      label.style.left = dx + 'px'; label.style.top = dy + 'px';
      label.append(nm, el('span', null, text));
      pin.append(el('i'), label); layer.append(pin);
    });
  }

  // "here": the city matching this PC's time zone, or the zone's own name
  function here() {
    const match = CITIES.find(c => c[3] === HERE);
    const name = match ? match[1] : HERE.split('/').pop().replace(/_/g, ' ');
    return ['here', name, t('This PC'), HERE, match ? match[4] : 0, match ? match[5] : 0];
  }

  // ---------- analog clock faces
  function face() {
    const NS = 'http://www.w3.org/2000/svg', svg = document.createElementNS(NS, 'svg');
    svg.setAttribute('viewBox', '-50 -50 100 100'); svg.classList.add('wc-face');
    const add = (tag, attrs) => { const e = document.createElementNS(NS, tag); for (const k in attrs) e.setAttribute(k, attrs[k]); svg.append(e); return e; };
    add('circle', { r: 48, class: 'dial' });
    for (let i = 0; i < 60; i++) {
      const a = i * 6 * Math.PI / 180, big = i % 5 === 0, r1 = big ? 40.5 : 42.5;
      add('line', { x1: Math.sin(a) * r1, y1: -Math.cos(a) * r1, x2: Math.sin(a) * 45, y2: -Math.cos(a) * 45, class: big ? 'tick big' : 'tick' });
    }
    for (let n = 1; n <= 12; n++) {
      const a = n * 30 * Math.PI / 180, tx = add('text', { x: Math.sin(a) * 32, y: -Math.cos(a) * 32 + 3.6, class: 'num' }); tx.textContent = n;
    }
    const hh = add('line', { x1: 0, y1: 4, x2: 0, y2: -22, class: 'hand hour' });
    const mm = add('line', { x1: 0, y1: 5, x2: 0, y2: -36, class: 'hand min' });
    add('circle', { r: 2.6, class: 'pivot' });
    const ss = add('line', { x1: 0, y1: 9, x2: 0, y2: -40, class: 'hand sec' });
    add('circle', { r: 1.4, class: 'pivot2' });
    return { svg, set(p) {
      const sec = p.second, min = p.minute + sec / 60, hr = (p.hour % 12) + min / 60;
      hh.setAttribute('transform', `rotate(${hr * 30})`); mm.setAttribute('transform', `rotate(${min * 6})`); ss.setAttribute('transform', `rotate(${sec * 6})`);
    } };
  }

  // ---------- city cards
  let cards = [];
  function buildCards() {
    const grid = $('wcCards'); grid.replaceChildren(); cards = [];
    for (const c of [here(), ...ids.map(city).filter(Boolean)]) {
      const card = el('div', 'wc-card'), f = face(), info = el('div', 'wc-info');
      const nm = el('div', 'wc-name', c[1]); nm.setAttribute('translate', 'no');
      const rel = el('div', 'wc-rel'), digits = el('div', 'wc-time');
      info.append(rel, nm, digits);
      card.append(f.svg, info);
      if (c[0] !== 'here') card.append(miniBtn('x', 'Remove', () => { ids = ids.filter(x => x !== c[0]); save(); buildCards(); drawMap(); }));
      else card.classList.add('here');
      grid.append(card);
      cards.push({ c, f, rel, digits, card });
    }
    tick();
  }
  function tick() {
    const now = shown || new Date();
    for (const k of cards) {
      k.f.set(parts(k.c[3], now));
      k.digits.textContent = timeStr(k.c[3], now);
      k.rel.textContent = k.c[0] === 'here' ? `${t('Here')} · ${k.c[2]}` : relLabel(k.c[3], now);
      k.card.classList.toggle('night', isNight(k.c[3], now));
    }
  }

  // ---------- add a city
  function renderSearch() {
    const q = $('wcSearch').value.trim().toLowerCase();
    const list = CITIES.filter(c => !ids.includes(c[0]) && (!q || (c[1] + ' ' + c[2]).toLowerCase().includes(q))).slice(0, 40);
    $('wcResults').replaceChildren(...list.map(c => {
      const b = el('button', 'wc-result'), nm = el('b', null, c[1]); nm.setAttribute('translate', 'no');
      b.append(nm, el('span', null, `${c[2]} · ${timeStr(c[3], new Date())}`));
      b.onclick = () => { ids.push(c[0]); save(); $('wcAdd').hidden = true; buildCards(); drawMap(); planner(); };
      return b;
    }));
  }
  const save = () => App.api.set_clocks(ids);

  // ---------- planner: drag along your day and every city follows
  function planner() {
    const rows = $('wcPlan'); rows.replaceChildren();
    const base = new Date(shown || Date.now()); base.setHours(0, 0, 0, 0);  // your midnight
    for (const c of [here(), ...ids.map(city).filter(Boolean)]) {
      const row = el('div', 'wc-row'), name = el('div', 'wc-rname', c[1]); name.setAttribute('translate', 'no');
      const strip = el('div', 'wc-strip');
      for (let i = 0; i < 24; i++) {
        const at = new Date(+base + i * 36e5), p = parts(c[3], at), hr = p.hour;
        const cell = el('div', 'wc-hr ' + (hr >= 8 && hr < 18 ? 'work' : hr >= 6 && hr < 22 ? 'awake' : 'sleep') + (hr === 0 ? ' midnight' : ''));
        cell.textContent = hr;
        if (p.minute) { cell.append(el('sup', null, String(p.minute).padStart(2, '0'))); cell.title = `${hr}:${String(p.minute).padStart(2, '0')}`; }
        strip.append(cell);
      }
      row.append(name, strip, el('div', 'wc-at')); rows.append(row);
    }
    placeCursor();
  }
  function placeCursor() {
    const strip = document.querySelector('#wcPlan .wc-strip'); if (!strip) return;
    const now = shown || new Date(), base = new Date(now); base.setHours(0, 0, 0, 0);
    const frac = (now - base) / 864e5, r = strip.getBoundingClientRect(), box = $('wcPlanWrap').getBoundingClientRect();
    const cur = $('wcCursor'); cur.style.left = (r.left - box.left + frac * r.width) + 'px';
    const list = [here(), ...ids.map(city).filter(Boolean)];
    document.querySelectorAll('#wcPlan .wc-at').forEach((e, i) => { const c = list[i]; e.textContent = timeStr(c[3], now) + (c[0] === 'here' ? '' : ' · ' + relLabel(c[3], now).split(',')[0]); });
    $('wcNow').hidden = !shown;
    $('wcShowing').textContent = shown ? `${t('Showing')} ${timeStr(HERE, shown)} ${t('your time')}` : t('Live');
  }
  function pickAt(clientX) {
    const strip = document.querySelector('#wcPlan .wc-strip'); if (!strip) return;
    const r = strip.getBoundingClientRect(), frac = Math.min(0.9999, Math.max(0, (clientX - r.left) / r.width));
    const base = new Date(shown || Date.now()); base.setHours(0, 0, 0, 0);
    shown = new Date(+base + Math.round(frac * 96) * 9e5);  // snaps to 15 minutes
    tick(); placeCursor(); drawMap();
  }

  App.pages.worldclock = {
    init() {
      const saved = (App.state.clocks || DEFAULT).filter(city);
      ids = saved.length || App.state.clocks ? saved : DEFAULT;
      $('wcAddBtn').onclick = () => { $('wcAdd').hidden = !$('wcAdd').hidden; if (!$('wcAdd').hidden) { $('wcSearch').value = ''; renderSearch(); $('wcSearch').focus(); } };
      $('wcSearch').oninput = renderSearch;
      $('wcSearch').onkeydown = e => { if (e.key === 'Escape') $('wcAdd').hidden = true; if (e.key === 'Enter') { const b = document.querySelector('#wcResults .wc-result'); if (b) b.click(); } };
      $('wcNow').onclick = () => { shown = null; tick(); placeCursor(); drawMap(); };
      const plan = $('wcPlan');
      let dragging = false;
      plan.addEventListener('pointerdown', e => { if (!e.target.closest('.wc-strip')) return; dragging = true; plan.setPointerCapture(e.pointerId); pickAt(e.clientX); });
      plan.addEventListener('pointermove', e => { if (dragging) pickAt(e.clientX); });
      plan.addEventListener('pointerup', () => dragging = false);
      new ResizeObserver(() => { if (App.page === 'worldclock') { drawMap(); placeCursor(); } }).observe($('wcMap'));
    },
    show() {
      buildCards(); planner(); drawMap();
      timers.forEach(clearInterval);
      timers = [setInterval(() => { if (App.page !== 'worldclock') return; if (!shown) { tick(); placeCursor(); } }, 1000),
                setInterval(() => { if (App.page === 'worldclock' && !shown) drawMap(); }, 30000)];
    },
  };
})();
