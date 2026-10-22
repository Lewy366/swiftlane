// Shared helpers, page switching, events from Python and the job list.
const $ = id => document.getElementById(id);
const App = {
  api: null, state: null, page: null,
  handlers: {},      // event name -> [fn]
  pages: {},         // page name -> {show(), drop(files)}
  jobs: new Map(),   // job id -> row ui
};

function el(tag, cls, text) { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
function icon(name) {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); s.setAttribute('class', 'i');
  const u = document.createElementNS('http://www.w3.org/2000/svg', 'use'); u.setAttribute('href', '#i-' + name); s.append(u); return s;
}
function miniBtn(name, title, fn) { const b = el('button', 'mini'); b.title = title; b.append(icon(name)); b.onclick = e => { e.stopPropagation(); fn(); }; return b; }
function fmtSize(n) {
  const u = ['B', 'KB', 'MB', 'GB']; let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i ? n.toFixed(1) : n) + ' ' + u[i];
}
function ago(t) {
  const s = Date.now() / 1000 - t;
  if (s < 45) return 'just now';
  if (s < 3600) return Math.round(s / 60) + ' min ago';
  if (s < 86400) return Math.round(s / 3600) + ' h ago';
  return new Date(t * 1000).toLocaleDateString();
}
const KINDS = [
  ['k-img', /^(jpe?g|heic|heif|png|gif|webp|bmp|tiff?|dng|raw|avif|ico|svg)$/], ['k-vid', /^(mov|mp4|m4v|avi|mkv|webm|hevc|wmv|flv|3gp|mpe?g|ts)$/],
  ['k-pdf', /^pdf$/], ['k-zip', /^(zip|rar|7z|tar|gz)$/], ['k-aud', /^(mp3|m4a|wav|aac|flac|ogg|opus|wma|aiff?)$/],
  ['k-doc', /^(docx?|xlsx?|pptx?|txt|rtf|pages|numbers|key|csv|md)$/],
];
function kindOf(name) {
  const ext = (name.split('.').pop() || '').toLowerCase();
  const k = KINDS.find(([, re]) => re.test(ext));
  return { cls: k ? k[0] : 'k-other', label: (name.includes('.') ? ext : 'file').slice(0, 4).toUpperCase() };
}
function fileRow(name) {
  const row = el('div', 'row'), k = kindOf(name), meta = el('div', 'meta'), sub = el('div', 'sub');
  const nameEl = el('div', 'name', name); nameEl.setAttribute('translate', 'no');  // file names stay as they are
  meta.append(nameEl, sub); row.append(el('div', 'kind ' + k.cls, k.label), meta);
  return { row, meta, sub };
}
function segValue(id) { const b = $(id).querySelector('button.on'); return b ? b.dataset.v : null; }
function bindSeg(id, onChange) {
  $(id).querySelectorAll('button').forEach(b => b.onclick = () => {
    $(id).querySelectorAll('button').forEach(x => x.classList.toggle('on', x === b));
    onChange && onChange(b.dataset.v);
  });
}
let toastTimer;
function toast(text) { const t = $('toast'); t.textContent = text; t.classList.add('show'); clearTimeout(toastTimer); toastTimer = setTimeout(() => t.classList.remove('show'), 2400); }
function baseName(p) { return p.split(/[\\/]/).pop(); }

// ---------- events from Python
App.on = (event, fn) => (App.handlers[event] = App.handlers[event] || []).push(fn);
window.onCore = (event, data) => (App.handlers[event] || []).forEach(fn => fn(data));
App.on('dropped', files => {
  const page = App.pages[App.page];
  if (page && page.drop) page.drop(files); else toast('Open a tool that takes files, then drop them there.');
});
// Files from Explorer's right-click menu: open the right tool with them
function fromExplorer(r) {
  if (!r) return;
  const page = { share: 'share', send: 'transfer', convert: 'converter' }[r.cmd];
  showPage(page);
  if (r.cmd === 'share') App.pages.share.dropAll({ files: r.files, folders: r.folders });
  else if (r.files.length) App.pages[page].drop(r.files);
}
App.on('explorer', fromExplorer);
App.on('dropped-all', data => {  // folders were dropped: only the Share page takes those
  const page = App.pages[App.page];
  if (page && page.dropAll) page.dropAll(data);
  else if (data.files.length && page && page.drop) { page.drop(data.files); toast('Folders were skipped here. To share a folder, use Share links.'); }
  else toast('To share a folder, open Share links and drop it there.');
});

// ---------- drag highlight (Python receives the actual files)
let dragDepth = 0;
document.addEventListener('dragenter', e => { e.preventDefault(); dragDepth++; document.body.classList.add('dragging'); });
document.addEventListener('dragleave', () => { if (--dragDepth <= 0) { dragDepth = 0; document.body.classList.remove('dragging'); } });
document.addEventListener('dragover', e => e.preventDefault());
document.addEventListener('drop', e => { e.preventDefault(); dragDepth = 0; document.body.classList.remove('dragging'); });

// ---------- pages
function showPage(name) {
  const f = App.state.features;
  const navBtn = document.querySelector(`.nav[data-page="${name}"]`);
  if (navBtn && navBtn.dataset.feature && !f[navBtn.dataset.feature]) name = firstPage();
  App.page = name;
  document.querySelectorAll('.page').forEach(p => p.hidden = p.id !== 'page-' + name);
  document.querySelectorAll('.nav').forEach(n => n.classList.toggle('on', n.dataset.page === name));
  try { localStorage.setItem('swiftlane-page', name); } catch (e) {}
  const page = App.pages[name];
  if (page && page.show) page.show();
  $('main').scrollTop = 0;
}
function firstPage() {
  const btn = [...document.querySelectorAll('.nav[data-page]')].find(n => !n.hidden);
  return btn ? btn.dataset.page : 'settings';
}
document.querySelectorAll('.nav[data-page]').forEach(n => n.onclick = () => showPage(n.dataset.page));

// Ctrl+1 … Ctrl+9 jump to the visible pages in sidebar order (shown next to each page's name).
function numberNav() {
  const visible = [...document.querySelectorAll('.nav[data-page]')].filter(n => !n.hidden);
  document.querySelectorAll('.nav kbd').forEach(k => k.remove());
  visible.slice(0, 9).forEach((n, i) => { const k = el('kbd', null, '^' + (i + 1)); k.setAttribute('translate', 'no'); n.append(k); });
  return visible;
}
document.addEventListener('keydown', e => {
  if (!e.ctrlKey || e.altKey || e.shiftKey || !/^[1-9]$/.test(e.key) || !App.state) return;
  const target = numberNav()[+e.key - 1];
  if (target) { e.preventDefault(); showPage(target.dataset.page); }
});

function applyFeatures() {
  const f = App.state.features;
  if (App.renderSidebar) App.renderSidebar();  // builds the tools (pinned + folders) for the features that are on
  document.querySelectorAll('.nav[data-feature]').forEach(n => n.hidden = !f[n.dataset.feature]);
  numberNav();
  const dot = $('statusDot'), text = $('statusText');
  if (!f.transfer) { dot.className = 'dot off'; text.textContent = 'Phone transfer off'; }
  else if (App.state.server_error) { dot.className = 'dot err'; text.textContent = 'Phone transfer unavailable'; }
  else { dot.className = 'dot'; text.textContent = 'Ready · ' + (App.state.addresses[0] ? App.state.addresses[0].host : ''); }
}

// ---------- appearance
// One solid accent colour per theme, plus a lighter tone of it for text on dark.
const ACCENTS = {
  cyan: ['#14b8e0', '#7fdcf2'], sunset: ['#ff8a3d', '#ffb07a'], aurora: ['#8b7cf6', '#b6abff'], ocean: ['#4c8dff', '#8db6ff'], forest: ['#3fbf7f', '#7fdcab'],
  rose: ['#f2639b', '#f79bbf'], ruby: ['#f05252', '#f68b8b'], mono: ['#d6d3cc', '#ecebe7'],
};
function accentPair(theme) {
  if (theme.accent === 'custom') return [theme.custom, `color-mix(in oklab, ${theme.custom} 65%, white)`];
  return ACCENTS[theme.accent] || ACCENTS.cyan;
}
// Light or dark: chosen in Settings, or "auto" follows Windows.
const darkQuery = matchMedia('(prefers-color-scheme: dark)');
function applyMode(mode) {
  App.mode = mode || App.mode || 'dark';
  document.body.classList.toggle('light', App.mode === 'light' || (App.mode === 'auto' && !darkQuery.matches));
}
darkQuery.addEventListener('change', () => { applyMode(); checkReadable(); });
function inkOn(hex) {  // white or near-black text, whichever reads better on this colour
  const m = /^#?([0-9a-f]{6})$/i.exec(hex || ''); if (!m) return '#fff';
  const [r, g, b] = [0, 2, 4].map(i => parseInt(m[1].slice(i, i + 2), 16) / 255).map(c => c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4);
  return .2126 * r + .7152 * g + .0722 * b > .3 ? '#0b1218' : '#fff';
}
function applyTheme(theme, background) {
  const [a, b] = accentPair(theme), root = document.documentElement.style;
  applyMode(theme.mode);
  root.setProperty('--violet', a); root.setProperty('--cyan', b); root.setProperty('--on-accent', inkOn(a));
  if (background !== undefined) setBackground(background);
  document.body.classList.toggle('has-bg', !!App.hasBg);
  $('bgImage').hidden = !App.hasBg;
  const blur = theme.blur_on === false ? 0 : theme.blur;  // the glass panes blur regardless; this blurs the whole background
  $('bgPic').style.filter = $('bgVid').style.filter = blur ? `blur(${blur}px)` : '';
  $('bgDim2').style.opacity = theme.dim / 100;
  App.theme = theme;
  checkReadable();
}

// ---------- your background: a picture, an animated GIF/WebP, or a looping video
let bgUrl = null;
function setBackground(data) {
  if (bgUrl) { URL.revokeObjectURL(bgUrl); bgUrl = null; }
  const vid = $('bgVid'), pic = $('bgPic');
  App.hasBg = !!data;
  App.bgKind = !data ? null : data.startsWith('data:video') ? 'video' : /^data:image\/(gif|webp)/.test(data) ? 'anim' : 'image';
  vid.hidden = App.bgKind !== 'video'; pic.hidden = App.bgKind === 'video';
  document.body.classList.toggle('anim-bg', App.bgKind === 'video' || App.bgKind === 'anim');
  if (App.bgKind === 'video') {
    pic.style.backgroundImage = ''; App.bgSrc = null;
    fetch(data).then(r => r.blob()).then(b => { bgUrl = URL.createObjectURL(b); vid.src = bgUrl; App.bgSrc = bgUrl; vid.play().catch(() => {}); checkReadable(); });
  } else {
    vid.removeAttribute('src'); vid.load();
    pic.style.backgroundImage = data ? `url("${data}")` : ''; App.bgSrc = data || null;
  }
}
document.addEventListener('visibilitychange', () => {  // a moving background pauses while the window is hidden
  const v = $('bgVid'); if (v.hidden || !v.src) return;
  document.hidden ? v.pause() : v.play().catch(() => {});
});

// ---------- readable on any background, without you doing anything:
// measure how bright the background is (several frames for animations), then tone down the
// background behind the page text and make the glass more solid, just enough for 4.5:1 contrast.
const lumaCache = {};
async function sampleLuma(src, kind) {
  if (lumaCache.src === src) return lumaCache.out;
  const c = document.createElement('canvas'); c.width = 48; c.height = 30;
  const g = c.getContext('2d', { willReadFrequently: true }), out = [];
  const lin = v => (v /= 255) <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4;
  const read = () => { const d = g.getImageData(0, 0, 48, 30).data;
    for (let i = 0; i < d.length; i += 4) out.push(.2126 * lin(d[i]) + .7152 * lin(d[i + 1]) + .0722 * lin(d[i + 2])); };
  try {
    if (kind === 'video') {
      const v = document.createElement('video'); v.muted = true; v.src = src;
      await new Promise(r => { v.onloadeddata = r; v.onerror = r; setTimeout(r, 4000); });
      for (const k of [.05, .3, .55, .8]) {
        v.currentTime = (v.duration || 0) * k;
        await new Promise(r => { v.onseeked = r; setTimeout(r, 800); });
        g.drawImage(v, 0, 0, 48, 30); read();
      }
    } else { const im = new Image(); im.src = src; await im.decode(); g.drawImage(im, 0, 0, 48, 30); read(); }
  } catch (e) {}
  out.sort((a, b) => a - b); if (out.length) { lumaCache.src = src; lumaCache.out = out; }
  return out;
}
async function checkReadable() {
  const root = document.documentElement.style, note = $('bgReadable');
  if (!App.hasBg || !App.bgSrc) {
    ['--tint', '--tint-side', '--scrim'].forEach(p => root.removeProperty(p));
    App.readability = null; if (note) note.hidden = true; return;
  }
  const lums = await sampleLuma(App.bgSrc, App.bgKind); if (!lums.length) return;
  const light = document.body.classList.contains('light'), dim = ((App.theme && App.theme.dim) ?? 30) / 100;
  const at = q => lums[Math.min(lums.length - 1, Math.floor(q * lums.length))];
  let scrim = 0, glass = 0, maxed = false;
  if (!light) {  // white text: what's behind it may be at most 0.183 bright
    const worst = at(.9) * (1 - dim);
    if (worst > .183) { const need = 1 - .183 / worst; scrim = Math.min(.6, need); maxed = need > .6; }
    const under = worst * (1 - scrim);
    glass = under > .183 ? (under - .183) / (under - .012) : 0;
  } else {       // dark text: what's behind it must be at least 0.202 bright
    const worst = at(.1) * (1 - dim) + dim;
    if (worst < .202) { const need = (.202 - worst) / (1 - worst); scrim = Math.min(.6, need); maxed = need > .6; }
    const under = worst + (1 - worst) * scrim;
    glass = under < .202 ? (.202 - under) / (1 - under) : 0;
  }
  const tint = Math.min(.9, Math.max(.34, glass + .12)), side = Math.min(.9, Math.max(.26, glass + .1));
  root.setProperty('--tint', tint.toFixed(2)); root.setProperty('--tint-side', side.toFixed(2)); root.setProperty('--scrim', scrim.toFixed(2));
  App.readability = { adjusted: scrim > .02 || tint > .35, maxed, scrim, tint };
  if (note) {
    note.hidden = !App.readability.adjusted; note.className = 'small ' + (maxed ? 'warn' : 'muted');
    note.textContent = maxed
      ? (light ? 'This background is very dark, so some text may still be hard to read. Turn up “Darken” or pick a calmer background.'
               : 'This background is very bright, so some text may still be hard to read. Turn up “Darken” or pick a calmer background.')
      : 'Swiftlane toned down the background behind the text so everything stays readable.';
  }
}

// ---------- the glass reacts: light follows the mouse, a press ripples and bends the glass for a moment
const GLASS = '.sidebar, .card, .drop';
const calm = matchMedia('(prefers-reduced-motion: reduce)');  // Windows: Settings > Accessibility > Visual effects > Animation effects
// Hovering tilts the glass a little toward the pointer (less for big panes) and slides its reflection.
let tilted = null;
function untilt(g) {
  if (!g) return;
  g.classList.remove('tilting'); ['--rx', '--ry', '--sx'].forEach(p => g.style.removeProperty(p));
}
document.addEventListener('pointermove', e => {
  const g = e.target.closest && e.target.closest(GLASS);
  if (g !== tilted) { untilt(tilted); tilted = g; }
  if (!g || calm.matches) return;
  const r = g.getBoundingClientRect(), x = (e.clientX - r.left) / r.width - .5, y = (e.clientY - r.top) / r.height - .5;
  const max = Math.min(3.5, 520 / Math.max(r.width, r.height) * 2);  // degrees: small panes tilt more than big ones
  g.classList.add('tilting');
  g.style.setProperty('--rx', (-y * max).toFixed(2) + 'deg'); g.style.setProperty('--ry', (x * max).toFixed(2) + 'deg');
  g.style.setProperty('--sx', (50 - x * 60).toFixed(1) + '%');
}, { passive: true });
document.addEventListener('pointerleave', () => { untilt(tilted); tilted = null; });
let lensAnim = 0;
document.addEventListener('pointerdown', e => {
  if (calm.matches || e.button !== 0) return;
  const g = e.target.closest && e.target.closest(GLASS); if (!g) return;
  const r = g.getBoundingClientRect();
  let fx = g.querySelector(':scope > .fx'); if (!fx) { fx = el('span', 'fx'); g.append(fx); }
  const d = el('i', 'ripple'), size = Math.max(r.width, r.height) * 1.1;
  Object.assign(d.style, { left: (e.clientX - r.left) + 'px', top: (e.clientY - r.top) + 'px', width: size + 'px', height: size + 'px' });
  fx.append(d); setTimeout(() => d.remove(), 750);
  if (e.target.closest('input, textarea, select, .key, canvas')) return;  // no bending while typing or editing
  const map = document.querySelector('#lens feDisplacementMap'); if (!map) return;
  const base = 38, peak = base + 70, t0 = performance.now(), id = ++lensAnim;
  const step = t => { if (id !== lensAnim) return; const k = Math.min(1, (t - t0) / 600);
    map.setAttribute('scale', k < 1 ? (peak + (base - peak) * (1 - Math.exp(-6 * k) * Math.cos(10 * k))).toFixed(1) : base);
    if (k < 1) requestAnimationFrame(step); };
  requestAnimationFrame(step);
});

// ---------- jobs (conversions, edits, downloads)
function jobRow(job) {
  const ui = fileRow(job.title.split(' → ')[0].replace(/^Download · /, '') || job.title);
  const title = ui.row.querySelector('.name');
  title.removeAttribute('translate');  // job titles ("Download · 720p video") are app text
  title.textContent = job.title;
  ui.bar = el('div', 'bar'); ui.fill = el('i'); ui.bar.append(ui.fill); ui.meta.append(ui.bar);
  ui.acts = el('div', 'acts'); ui.row.append(ui.acts);
  ui.cancel = miniBtn('x', 'Cancel', () => App.api.cancel_job(job.id)); ui.acts.append(ui.cancel);
  const list = $('jobs-' + job.kind);
  if (!list) return null;  // tools with their own progress display (speed test)
  list.prepend(ui.row);
  updateJobsEmpty(job.kind);
  return ui;
}
function updateJobsEmpty(kind) {
  const empty = document.querySelector(`[data-jobs-empty="${kind}"]`);
  if (empty) empty.hidden = $('jobs-' + kind).children.length > 0;
}
App.on('job', job => {
  let ui = App.jobs.get(job.id);
  if (!ui) { ui = jobRow(job); if (!ui) return; App.jobs.set(job.id, ui); }
  if (job.status === 'running') {
    ui.bar.classList.toggle('indeterminate', job.progress == null);
    if (job.progress != null) ui.fill.style.width = Math.round(job.progress * 100) + '%';
    if (job.text) ui.sub.textContent = job.text;
    else if (job.progress != null) ui.sub.textContent = Math.round(job.progress * 100) + '%';
    return;
  }
  ui.bar.remove(); ui.cancel.remove();
  if (job.status === 'done') {
    const outs = job.outputs || [];
    ui.sub.innerHTML = '';
    const ok = el('span', 'ok', outs.length > 1 ? `Done · ${outs.length} files` : 'Done');
    ui.sub.append(ok, document.createTextNode(outs.length ? ' · ' + outs.map(baseName).slice(0, 3).join(', ') + (outs.length > 3 ? '…' : '') : ''));
    if (outs.length) {
      ui.acts.append(miniBtn('open', 'Open', () => App.api.open_path(outs[0])), miniBtn('folder', 'Show in folder', () => App.api.reveal_path(outs[0])));
      ui.row.ondblclick = () => App.api.open_path(outs[0]);
    }
  } else if (job.status === 'cancelled') {
    ui.sub.innerHTML = ''; ui.sub.append(el('span', 'warn', 'Cancelled'));
  } else {
    ui.sub.innerHTML = ''; ui.sub.append(el('span', 'err', job.text || 'Failed'));
    ui.sub.title = job.text || '';
  }
});
document.querySelectorAll('[data-clear-jobs]').forEach(b => b.onclick = () => {
  const kind = b.dataset.clearJobs;
  [...$('jobs-' + kind).children].forEach(row => { if (!row.querySelector('.bar')) row.remove(); });
  updateJobsEmpty(kind);
});

// ---------- start
async function init() {
  App.api = window.pywebview.api;
  App.state = await App.api.get_state();
  applyTheme(App.state.theme, await App.api.get_background());
  setLanguage(App.state.language);
  $('version').textContent = 'v' + App.state.version;
  applyFeatures();
  for (const page of Object.values(App.pages)) if (page.init) page.init();
  let start = App.state.start_page;
  try { start = start || localStorage.getItem('swiftlane-page'); } catch (e) {}
  showPage(start || firstPage());
  fromExplorer(App.state.explorer);
}
window.addEventListener('pywebviewready', init);
