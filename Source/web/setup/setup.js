// Swiftlane Setup wizard pages.
const $ = id => document.getElementById(id);
function el(tag, cls, text) { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
function fmtSize(n) { return n >= 1e9 ? (n / 1e9).toFixed(1) + ' GB' : Math.round(n / 1e6) + ' MB'; }
let toastTimer;
function toast(text) { const t = $('toast'); t.textContent = text; t.classList.add('show'); clearTimeout(toastTimer); toastTimer = setTimeout(() => t.classList.remove('show'), 2400); }

const ACCENTS = {
  cyan: ['#14b8e0', '#7fdcf2'], aurora: ['#7c5cff', '#00c8ff'], ocean: ['#2f6bff', '#00d4c8'], forest: ['#22c55e', '#a3e635'],
  sunset: ['#ff6a3d', '#ff3d9a'], rose: ['#ec4899', '#a855f7'], ruby: ['#ef4444', '#f59e0b'], mono: ['#9ca3af', '#e5e7eb'],
};
const FEATURES = [
  ['transfer', 'Phone transfer', 'Send and receive files with your phone over your network'],
  ['share', 'Share links', 'Send files to anyone with a link, through Tailscale'],
  ['remote', 'PC remote', 'Control volume, music, slides and more from your paired phone'],
  ['contacts', 'Send to contacts', 'Straight to another Swiftlane, no link needed'],
  ['pdf', 'PDF tools', 'Merge, split, reorder, rotate, sign, make smaller'],
  ['trimmer', 'Video trimmer', 'Cut clips, shrink them to send, keep the sound'],
  ['vault', 'Vault', 'Files only your password opens'],
  ['diskmap', 'Disk space', 'See what fills up your drives'],
  ['netscan', 'Network devices', 'Who is on your home network'],
  ['exposure', 'Is my PC exposed?', 'Firewall, open ports, remote access, router'],
  ['converter', 'File converter', 'Images, video, audio and PDF'],
  ['images', 'Image editor', 'Crop, resize, straighten, adjust colours, compress'],
  ['downloader', 'Video downloader', 'YouTube and other sites'],
  ['currency', 'Currency converter', 'Live exchange rates'],
  ['calculator', 'Calculator', 'Scientific, ClassWiz style'],
  ['speedtest', 'Speed test', 'Download, upload and ping'],
  ['adblock', 'Ad blocker test', 'Are ads and trackers blocked?'],
];

let api, info, c;  // c = the choices being built
const STEPS = [
  ['welcome', 'Welcome'], ['location', 'Location'], ['features', 'Features'], ['files', 'Your files'],
  ['phone', 'Phone & network'], ['look', 'Personalise'], ['ready', 'Ready'], ['installing', 'Installing'], ['done', 'Finish'],
];
let step = 0;

function applyAccent() {
  const [a, b] = c.theme.accent === 'custom' ? [c.theme.custom, `color-mix(in oklab, ${c.theme.custom} 60%, white)`] : ACCENTS[c.theme.accent] || ACCENTS.cyan;
  document.documentElement.style.setProperty('--violet', a); document.documentElement.style.setProperty('--cyan', b);
}
function visibleSteps() { return STEPS.filter(([k]) => k !== 'phone' || c.features.transfer); }

function renderStepList() {
  const list = $('stepList'); list.replaceChildren();
  visibleSteps().forEach(([key, label], i) => {
    const cur = visibleSteps()[step][0];
    const idx = visibleSteps().findIndex(s => s[0] === cur);
    const d = el('div', 'step-item' + (i === idx ? ' on' : i < idx ? ' done' : ''));
    const n = el('i', null, i < idx ? '✓' : String(i + 1));
    d.append(n, el('span', null, label)); list.append(d);
  });
}

function opt(title, desc, input) {
  const row = el('label', 'opt'), text = el('div');
  text.append(el('b', null, title)); if (desc) text.append(el('small', null, desc));
  row.append(input, text); return row;
}
function checkbox(checked, onChange) {
  const i = el('input'); i.type = 'checkbox'; i.checked = !!checked; i.style.cssText = 'width:18px;height:18px;margin-top:2px;accent-color:var(--violet)';
  i.onchange = () => onChange(i.checked); return i;
}
function switchInput(checked, onChange) {
  const sw = el('label', 'switch'), i = el('input'); i.type = 'checkbox'; i.checked = !!checked; sw.append(i, el('span'));
  i.onchange = () => onChange(i.checked); return sw;
}
function pathRow(value, onPick) {
  const row = el('div', 'path-row'), input = el('input', 'input'); input.value = value; input.setAttribute('translate', 'no');
  input.onchange = () => onPick(input.value);
  const b = el('button', 'btn', 'Change…'); b.onclick = async () => { const p = await api.pick_folder(input.value); if (p) { input.value = p; onPick(p); } };
  row.append(input, b); return row;
}
function head(title, lead) { const f = document.createDocumentFragment(); f.append(el('h1', null, title)); if (lead) f.append(el('p', 'lead', lead)); return f; }

// ------------------------------------------------------------ pages
const PAGES = {
  welcome(p) {
    const hero = el('div', 'hero'), img = el('img'); img.src = 'icon-180.png';
    hero.append(img, el('h1', null, 'Welcome to Swiftlane'),
      el('p', 'lead', 'Send files between your phone and this PC, plus a converter, image editor, video downloader, currency converter, calculator, speed test and ad blocker test.'));
    p.append(hero);
    if (info.existing) {
      const card = el('div', 'card'); card.style.marginTop = '20px';
      card.append(el('h2', null, `Swiftlane ${info.existing.version} is already installed`),
        el('p', 'muted small', 'Updating keeps your settings, pairing code and files.'));
      const row = el('div'); row.style.cssText = 'display:flex; gap:10px; margin-top:6px';
      const upd = el('button', 'btn primary', info.existing.version === info.version ? 'Repair' : `Update to ${info.version}`); upd.onclick = () => go(1);
      const un = el('button', 'btn danger', 'Uninstall');
      un.onclick = async () => { if (!await api.uninstall_existing()) toast('Couldn’t find the installed Swiftlane.'); };
      row.append(upd, un); card.append(row); p.append(card);
    }
  },
  location(p) {
    p.append(head('Where should Swiftlane be installed?', 'Program Files is recommended: only administrators can change it, which keeps the firewall rule safe.'));
    p.append(pathRow(c.install_dir, v => c.install_dir = v));
    p.append(el('p', 'muted small', `Needs ${fmtSize(info.needed)} · ${fmtSize(info.free)} free on this drive`));
    const card = el('div', 'card'); card.style.marginTop = '18px';
    card.append(el('h2', null, 'Shortcuts'),
      opt('Start menu', 'Also what Windows search finds', checkbox(c.start_menu, v => c.start_menu = v)),
      opt('Desktop icon', null, checkbox(c.desktop, v => c.desktop = v)),
      opt('Start with Windows', 'Opens minimized when you sign in', checkbox(c.autostart, v => c.autostart = v)));
    p.append(card);
  },
  features(p) {
    p.append(head('Which tools do you want?', 'You can switch them on or off later in Settings.'));
    const grid = el('div', 'feature-grid');
    for (const [key, name, desc] of FEATURES) grid.append(opt(name, desc, checkbox(c.features[key], v => { c.features[key] = v; renderStepList(); render(); })));
    p.append(grid);
    if (c.features.downloader && !info.node) {
      const n = el('div', 'notice'); n.style.marginTop = '16px';
      n.innerHTML = '<svg class="i"><use href="#i-info"/></svg>';
      const s = el('span', null, 'Node.js isn’t installed. The video downloader works without it, but YouTube may only offer some formats. ');
      const a = el('a', null, 'Get Node.js'); a.href = '#'; a.style.color = 'var(--cyan)'; a.onclick = e => { e.preventDefault(); api.open_url('https://nodejs.org/en/download'); };
      n.append(s, a); p.append(n);
    }
  },
  files(p) {
    p.append(head('Where should received files go?', 'Files you send from your phone are saved here.'));
    p.append(pathRow(c.save_dir, v => { c.save_dir = v; render(); }));
    const card = el('div', 'card'); card.style.marginTop = '18px';
    const sortRow = opt('Sort into folders by type', 'Keeps photos, videos and documents apart', switchInput(c.sort_files, v => { c.sort_files = v; render(); }));
    card.append(sortRow);
    const tree = el('div', 'tree'); tree.setAttribute('translate', 'no');
    const base = c.save_dir.split(/[\\/]/).pop();
    tree.textContent = c.sort_files ? `${base}\\\n   Photos\\   Videos\\   Audio\\\n   Documents\\   Archives\\   Other\\` : `${base}\\  (everything in one folder)`;
    tree.style.whiteSpace = 'pre';
    card.append(tree); p.append(card);
    const dl = el('div', 'card');
    dl.append(el('h2', null, 'Video downloads'), el('p', 'muted small', 'Where the downloader suggests saving. Leave empty to use the Videos/Audio folders above.'),
      pathRow(c.download_dir, v => c.download_dir = v));
    p.append(dl);
  },
  phone(p) {
    p.append(head('Phone & network', 'So your phone can reach Swiftlane on your home network, and nothing else can.'));
    const card = el('div', 'card');
    card.append(opt('Allow Swiftlane through the firewall', 'Only Swiftlane.exe, only port 8765, only on private (home) networks.', checkbox(c.firewall, v => c.firewall = v)));
    p.append(card);
    const nets = el('div', 'card');
    nets.append(el('h2', null, 'Your networks'), el('p', 'muted small', 'Windows only lets other devices in on networks marked Private. Mark your home network as Private; leave café and hotel networks Public.'));
    for (const n of info.networks) {
      const label = `${n.name}  ·  ${n.alias}`;
      const isPrivate = n.category === 'Private';
      const cb = checkbox(isPrivate || c.private_networks.includes(n.alias), v => {
        c.private_networks = c.private_networks.filter(a => a !== n.alias); if (v) c.private_networks.push(n.alias);
      });
      cb.disabled = isPrivate;
      nets.append(opt(label, isPrivate ? 'Already private' : 'Currently public · tick to make it private', cb));
    }
    if (!info.networks.length) nets.append(el('p', 'muted', 'No connected networks found.'));
    p.append(nets);
  },
  look(p) {
    p.append(head('Make it yours', 'All of this can be changed later in Settings.'));
    const card = el('div', 'card');
    card.append(el('h2', null, 'Accent colour'));
    const sw = el('div', 'swatches');
    for (const [key, [a, b]] of Object.entries(ACCENTS)) {
      const s = el('button', 'swatch' + (c.theme.accent === key ? ' on' : '')); s.style.background = `linear-gradient(135deg, ${a}, ${b})`;
      s.onclick = () => { c.theme.accent = key; applyAccent(); render(); }; sw.append(s);
    }
    const custom = el('label', 'swatch custom' + (c.theme.accent === 'custom' ? ' on' : ''));
    custom.style.background = c.theme.accent === 'custom' ? c.theme.custom : 'conic-gradient(#f44, #fd0, #4f4, #0df, #44f, #f4f, #f44)';
    const picker = el('input'); picker.type = 'color'; picker.value = c.theme.custom;
    picker.oninput = () => { c.theme.custom = picker.value; c.theme.accent = 'custom'; applyAccent(); custom.style.background = picker.value; };
    picker.onchange = () => render();
    custom.append(picker, el('span', null, '+')); sw.append(custom);
    card.append(sw); p.append(card);
    const more = el('div', 'card');
    const angle = el('div', 'seg');
    for (const [v, l] of [['deg', 'Deg'], ['rad', 'Rad'], ['gra', 'Gra']]) {
      const b = el('button', c.calc_angle === v ? 'on' : '', l); b.onclick = () => { c.calc_angle = v; render(); }; angle.append(b);
    }
    more.append(opt('Calculator starts in', 'Angle unit for sin, cos, tan', angle),
      opt('Notifications', 'When files arrive or a job finishes', switchInput(c.notify, v => c.notify = v)));
    p.append(more);
  },
  ready(p) {
    p.append(head('Ready to install', 'Check your choices, then click Install.'));
    const dl = el('dl', 'summary');
    const on = FEATURES.filter(([k]) => c.features[k]).map(([, n]) => t(n));
    const rows = [
      ['Install to', c.install_dir], ['Tools', on.join(', ') || '—'],
      ['Received files', c.save_dir + (c.sort_files ? '  (' + t('sorted into folders') + ')' : '')],
      ['Shortcuts', [c.start_menu && t('Start menu'), c.desktop && t('Desktop icon'), c.autostart && t('Start with Windows')].filter(Boolean).join(', ') || '—'],
    ];
    if (c.features.transfer) rows.push(['Firewall', c.firewall ? t('Swiftlane only, port 8765, private networks') : t('Not changed')],
                                       ['Make private', c.private_networks.join(', ') || '—']);
    rows.push(['Language', $('language').selectedOptions[0].textContent]);
    for (const [k, v] of rows) { const dt = el('dt', null, k), dd = el('dd', null, v); dd.setAttribute('translate', 'no'); dl.append(dt, dd); }
    p.append(dl);
    const n = el('div', 'notice'); n.style.marginTop = '20px';
    n.innerHTML = '<svg class="i"><use href="#i-info"/></svg>';
    n.append(el('span', null, 'If anything goes wrong while installing, Setup undoes every change and leaves your PC exactly as it was.'));
    p.append(n);
  },
  installing(p) {
    if (info.update) p.append(head(`Updating Swiftlane to ${info.version}…`, 'Your settings, files and pairing code stay exactly as they are.'));
    else p.append(head('Installing Swiftlane…', null));
    const bar = el('div', 'big-progress'), fill = el('i'); fill.id = 'progFill'; bar.append(fill);
    const text = el('p', 'muted'); text.id = 'progText'; text.textContent = 'Starting…';
    p.append(bar, text);
  },
  done(p) {
    if (c._error) {
      p.append(el('div', 'result-icon bad', '✕'), head('Installation didn’t finish', null));
      p.append(el('p', 'lead', 'Nothing was changed: Setup undid every step, so your PC is exactly as it was before.'));
      const err = el('div', 'notice'); err.append(el('span', null, c._error)); err.firstChild.setAttribute('translate', 'no'); p.append(err);
      const logBtn = el('button', 'btn', 'Open log'); logBtn.style.marginTop = '14px'; logBtn.onclick = () => api.open_log();
      p.append(logBtn);
      return;
    }
    if (info.update) { p.append(el('div', 'result-icon ok', '✓'), head(`Swiftlane is updated to ${info.version}`, 'Opening it now…')); return; }
    p.append(el('div', 'result-icon ok', '✓'), head('Swiftlane is installed', 'Find it in the Start menu or with Windows search.'));
    const card = el('div', 'card');
    card.append(opt('Open Swiftlane now', null, checkbox(c._launch, v => c._launch = v)));
    if (c.features.transfer) card.append(opt('Pair my phone now', 'Opens the “Connect phone” page with the QR code', checkbox(c._pair, v => c._pair = v)));
    p.append(card);
  },
};

function render() {
  const key = visibleSteps()[step][0];
  const pane = $('pane'); pane.replaceChildren();
  PAGES[key](pane);
  renderStepList();
  const back = $('backBtn'), next = $('nextBtn'), cancel = $('cancelBtn');
  back.hidden = step === 0 || ['installing', 'done'].includes(key);
  cancel.hidden = ['installing', 'done'].includes(key);
  next.hidden = key === 'installing';
  next.disabled = false;
  next.textContent = key === 'ready' ? 'Install' : key === 'done' ? (c._error ? 'Close' : 'Finish') : key === 'welcome' && info.existing ? 'Continue' : 'Next';
  pane.scrollTop = 0;
}
function go(delta) {
  const key = visibleSteps()[step][0];
  if (key === 'ready' && delta > 0) return startInstall();
  if (key === 'done') return api.finish(!c._error && c._launch, !c._error && c._pair);
  step = Math.max(0, Math.min(visibleSteps().length - 1, step + delta));
  render();
}
function startInstall() {
  step = visibleSteps().findIndex(s => s[0] === 'installing'); render();
  const choices = { ...c, language: $('language').value };
  for (const k of Object.keys(choices)) if (k.startsWith('_')) delete choices[k];
  api.install(choices);
}
window.onSetup = (event, data) => {
  if (event === 'progress') {
    const f = $('progFill'), tx = $('progText');
    if (f) f.style.width = Math.round(data.fraction * 100) + '%';
    if (tx) tx.textContent = data.text;
  } else if (event === 'installed' || event === 'failed') {
    c._error = event === 'failed' ? data.error : null;
    step = visibleSteps().findIndex(s => s[0] === 'done'); render();
    if (info.update && !c._error) setTimeout(() => api.finish(true, false), 1600);  // reopen Swiftlane by itself
  }
};

$('backBtn').onclick = () => go(-1);
$('nextBtn').onclick = () => go(1);
$('cancelBtn').onclick = () => { if (confirm(t('Cancel Swiftlane Setup? Nothing has been changed yet.'))) api.close(); };
$('language').onchange = e => setLanguage(e.target.value);

window.addEventListener('pywebviewready', async () => {
  api = window.pywebview.api;
  info = await api.info();
  c = {
    install_dir: info.install_dir, start_menu: true, desktop: info.desktop, autostart: info.autostart,
    features: info.features, save_dir: info.save_dir, sort_files: info.sort_files, download_dir: info.download_dir,
    firewall: true, private_networks: info.networks.filter(n => n.category !== 'Private' && /ethernet|wi-?fi|wlan/i.test(n.alias)).map(n => n.alias),
    theme: { accent: info.theme.accent || 'aurora', custom: info.theme.custom || '#7c5cff' }, calc_angle: info.calc_angle, notify: info.notify,
    _launch: true, _pair: !info.existing,
  };
  $('ver').textContent = 'Setup ' + info.version;
  if (info.update) {  // Update button in Swiftlane: same choices as before, straight to the progress page
    c.start_menu = info.start_menu; c.private_networks = [];
    $('stepList').hidden = true;
    $('language').value = info.language; setLanguage(info.language);
    applyAccent();
    return startInstall();
  }
  $('language').value = info.language;
  applyAccent();
  setLanguage(info.language);
  render();
});
