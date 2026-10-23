// Sidebar with pinned tools and folders (drag to rearrange), the All tools page, and Ctrl+K.
// Folder names are typed by the user: always shown as text (textContent), never as HTML.
const TOOLS = [
  { id: 'transfer', name: 'Phone transfer', icon: 'transfer', feature: 'transfer', desc: 'Send and receive files with your phone over your network', keys: 'phone iphone android send receive' },
  { id: 'share', name: 'Share links', icon: 'link', feature: 'share', desc: 'Send files to anyone with a link, through Tailscale', keys: 'link send upload friend' },
  { id: 'contacts', name: 'Send to contacts', icon: 'people', feature: 'contacts', desc: 'Straight to another Swiftlane, no link needed', keys: 'send friend contact p2p peer computer pc another swiftlane', pin: true },
  { id: 'converter', name: 'File converter', icon: 'convert', feature: 'converter', desc: 'Images, video, audio and PDF', keys: 'convert jpg png mp4 mp3 heic' },
  { id: 'pdf', name: 'PDF tools', icon: 'pdf', feature: 'pdf', desc: 'Merge, split, reorder, rotate, sign, make smaller', keys: 'pdf merge split sign compress pages', home: 'Files & media' },
  { id: 'trimmer', name: 'Video trimmer', icon: 'scissors', feature: 'trimmer', desc: 'Cut clips, shrink them to send, keep the sound', keys: 'video cut trim clip discord whatsapp mp3', home: 'Files & media' },
  { id: 'vault', name: 'Vault', icon: 'lock', feature: 'vault', desc: 'Files only your password opens', keys: 'vault encrypt password private safe secret lock', home: 'Security' },
  { id: 'diskmap', name: 'Disk space', icon: 'disk', feature: 'diskmap', desc: 'See what fills up your drives', keys: 'disk space storage full drive clean big files treemap', home: 'Files & media' },
  { id: 'netscan', name: 'Network devices', icon: 'wifi', feature: 'netscan', desc: 'Who is on your home network', keys: 'network devices wifi scan lan who router intruder', home: 'Security' },
  { id: 'exposure', name: 'Is my PC exposed?', icon: 'radar', feature: 'exposure', desc: 'Firewall, open ports, remote access, router', keys: 'security exposed firewall ports upnp remote desktop safe check hacker', home: 'Security' },
  { id: 'remote', name: 'PC remote', icon: 'remote', desc: 'Music, volume and slides from your phone', keys: 'remote control phone volume music play pause slides presentation', home: 'Everyday' },
  { id: 'images', name: 'Image editor', icon: 'image', feature: 'images', desc: 'Crop, resize, straighten, adjust colours, compress', keys: 'photo crop resize edit picture' },
  { id: 'downloader', name: 'Video downloader', icon: 'download', feature: 'downloader', desc: 'YouTube and other sites', keys: 'youtube video mp3 download' },
  { id: 'currency', name: 'Currency', icon: 'currency', feature: 'currency', desc: 'Live exchange rates', keys: 'money euro dollar zloty exchange' },
  { id: 'calculator', name: 'Calculator', icon: 'calc', feature: 'calculator', desc: 'Scientific, ClassWiz style', keys: 'math casio maths' },
  { id: 'speedtest', name: 'Speed test', icon: 'gauge', feature: 'speedtest', desc: 'Download, upload and ping', keys: 'internet speed mbps ping' },
  { id: 'adblock', name: 'Ad blocker test', icon: 'shield', feature: 'adblock', desc: 'Are ads and trackers blocked?', keys: 'ads tracker dns' },
];
const DEFAULT_LAYOUT = {
  pinned: ['transfer', 'share', 'contacts'],
  folders: [
    { name: 'Files & media', items: ['converter', 'images', 'pdf', 'trimmer', 'diskmap', 'downloader'], open: true },
    { name: 'Everyday', items: ['remote', 'currency', 'calculator'], open: true },
    { name: 'Network', items: ['speedtest', 'adblock'], open: true },
    { name: 'Security', items: ['vault', 'netscan', 'exposure'], open: true },
  ],
};

(() => {
  const tool = id => TOOLS.find(t => t.id === id);
  let layout = null;

  // Keep the layout valid: every known tool appears exactly once; new tools land in "More".
  function normalise(raw) {
    const l = { pinned: [], folders: [] }, seen = new Set();
    const take = ids => (Array.isArray(ids) ? ids : []).filter(id => tool(id) && !seen.has(id) && seen.add(id));
    l.pinned = take(raw && raw.pinned);
    for (const f of (raw && Array.isArray(raw.folders) ? raw.folders : []).slice(0, 20))
      l.folders.push({ name: String(f.name || 'Folder').slice(0, 40), items: take(f.items), open: f.open !== false });
    let rest = TOOLS.map(t => t.id).filter(id => !seen.has(id));
    for (const id of [...rest]) if (tool(id).pin) { l.pinned.push(id); rest = rest.filter(x => x !== id); }  // new main tools join the pinned ones
    for (const id of [...rest]) {  // new tools go to their usual folder if you still have it
      let home = l.folders.find(f => f.name === tool(id).home);
      if (!home && DEFAULT_LAYOUT.folders.some(f => f.name === tool(id).home)) { home = { name: tool(id).home, items: [], open: true }; l.folders.push(home); }
      if (home) { home.items.push(id); rest = rest.filter(x => x !== id); }
    }
    if (rest.length) {
      let more = l.folders.find(f => f.name === 'More');
      if (!more) { more = { name: 'More', items: [], open: true }; l.folders.push(more); }
      more.items.push(...rest);
    }
    return l;
  }
  function save() { App.api.set_sidebar(layout); }
  const enabled = t => !t.feature || App.state.features[t.feature] !== false;

  // Fade the list's edge where more tools are hidden above or below.
  function fadeEdges() {
    const n = $('navTools'); if (!n) return;
    const more = n.scrollHeight > n.clientHeight + 1;
    n.classList.toggle('can-scroll', more);
    n.style.setProperty('--fade-top', more && n.scrollTop > 2 ? '18px' : '0px');
    n.style.setProperty('--fade-bottom', more && n.scrollTop + n.clientHeight < n.scrollHeight - 2 ? '22px' : '0px');
  }
  document.addEventListener('DOMContentLoaded', () => {
    const n = $('navTools'); if (!n) return;
    n.addEventListener('scroll', fadeEdges, { passive: true });
    new ResizeObserver(fadeEdges).observe(n);
    new MutationObserver(fadeEdges).observe(n, { childList: true, subtree: true });
  });

  function navButton(t) {
    const b = el('button', 'nav'); b.dataset.page = t.id; b.dataset.tool = t.id; b.draggable = true;
    if (t.feature) b.dataset.feature = t.feature;
    b.append(icon(t.icon), document.createTextNode(t.name));
    if (t.id === 'transfer' || t.id === 'share') {
      const badge = el('span', 'badge'); badge.id = t.id === 'transfer' ? 'navTransferBadge' : 'navShareBadge'; badge.hidden = true; b.append(badge);
    }
    b.onclick = () => showPage(t.id);
    b.ondragstart = e => { e.dataTransfer.setData('text/x-swiftlane-tool', t.id); e.dataTransfer.effectAllowed = 'move'; b.classList.add('dragging-tool'); };
    b.ondragend = () => b.classList.remove('dragging-tool');
    dropTarget(b, id => moveTool(id, place(t.id)));
    return b;
  }
  function place(id) {  // where a tool sits: {list, index}
    let i = layout.pinned.indexOf(id); if (i >= 0) return { list: layout.pinned, index: i };
    for (const f of layout.folders) { i = f.items.indexOf(id); if (i >= 0) return { list: f.items, index: i }; }
    return null;
  }
  function moveTool(id, to) {
    if (!to) return;
    const from = place(id); if (!from) return;
    from.list.splice(from.index, 1);
    const index = to.list === from.list && from.index < to.index ? to.index - 1 : to.index;
    to.list.splice(Math.max(0, Math.min(index, to.list.length)), 0, id);
    save(); render();
  }
  function dropTarget(node, onDrop) {
    node.addEventListener('dragover', e => { if (e.dataTransfer.types.includes('text/x-swiftlane-tool')) { e.preventDefault(); e.stopPropagation(); node.classList.add('drop-here'); } });
    node.addEventListener('dragleave', () => node.classList.remove('drop-here'));
    node.addEventListener('drop', e => {
      const id = e.dataTransfer.getData('text/x-swiftlane-tool'); node.classList.remove('drop-here');
      if (id) { e.preventDefault(); e.stopPropagation(); onDrop(id); }
    });
  }

  function render() {
    if (!App.state) return;
    layout = layout || normalise(App.state.sidebar || DEFAULT_LAYOUT);
    const box = $('navTools'); box.replaceChildren();
    const pinned = el('div', 'nav-pinned');
    for (const id of layout.pinned) { const t = tool(id); if (enabled(t)) pinned.append(navButton(t)); }
    dropTarget(pinned, id => moveTool(id, { list: layout.pinned, index: layout.pinned.length }));
    box.append(pinned);
    layout.folders.forEach((f, fi) => {
      const visible = f.items.map(tool).filter(enabled);
      const wrap = el('div', 'folder' + (f.open ? ' open' : ''));
      const head = el('button', 'folder-head');
      const chev = el('span', 'chev', '›'), name = el('span', 'folder-name', f.name); name.setAttribute('translate', f.name === 'More' || DEFAULT_LAYOUT.folders.some(d => d.name === f.name) ? 'yes' : 'no');
      head.append(chev, name, el('span', 'folder-count', visible.length || ''));
      head.title = 'Click to open or close · right-click to rename or delete';
      head.onclick = () => { f.open = !f.open; save(); render(); numberNav(); };
      head.oncontextmenu = e => { e.preventDefault(); folderMenu(fi); };
      dropTarget(head, id => { f.open = true; moveTool(id, { list: f.items, index: f.items.length }); });
      const items = el('div', 'folder-items');
      for (const t of visible) items.append(navButton(t));
      wrap.append(head, items);
      if (!visible.length && f.items.length) wrap.hidden = true;  // only switched-off tools inside
      box.append(wrap);
    });
    const add = el('button', 'folder-add', '+ New folder');
    add.onclick = () => {
      const n = prompt(t('Folder name:'), ''); if (!n || !n.trim()) return;
      layout.folders.push({ name: n.trim().slice(0, 40), items: [], open: true }); save(); render();
      toast('Folder made · drag tools onto it');
    };
    box.append(add);
    numberNav();
    if (App.page) document.querySelectorAll('.nav').forEach(n => n.classList.toggle('on', n.dataset.page === App.page));
    if (App.pages.alltools && App.page === 'alltools') renderAllTools();
  }
  function folderMenu(fi) {
    const f = layout.folders[fi];
    const choice = prompt(t('Rename this folder, or leave it empty to delete it (its tools move to the top):'), f.name);
    if (choice === null) return;
    if (!choice.trim()) { layout.pinned.push(...f.items); layout.folders.splice(fi, 1); toast('Folder deleted'); }
    else f.name = choice.trim().slice(0, 40);
    save(); render();
  }
  function togglePin(id) {
    const p = place(id);
    if (p.list === layout.pinned) {
      p.list.splice(p.index, 1);
      let more = layout.folders.find(f => f.name === 'More') || layout.folders[0];
      if (!more) { more = { name: 'More', items: [], open: true }; layout.folders.push(more); }
      more.items.push(id);
    } else { p.list.splice(p.index, 1); layout.pinned.push(id); }
    save(); render();
  }
  App.renderSidebar = render;

  // ---------- All tools
  function renderAllTools() {
    const q = $('atSearch').value.trim().toLowerCase(), grid = $('atGrid'); grid.replaceChildren();
    for (const tl of TOOLS) {
      if (q && !`${t(tl.name)} ${tl.name} ${t(tl.desc)} ${tl.keys}`.toLowerCase().includes(q)) continue;
      const on = enabled(tl), tile = el('div', 'tool-tile card' + (on ? '' : ' off'));
      const top = el('div', 'tt-top'), ic = el('div', 'tt-icon'); ic.append(icon(tl.icon));
      const pin = el('button', 'mini tt-pin' + (layout.pinned.includes(tl.id) ? ' pinned' : ''));
      pin.title = layout.pinned.includes(tl.id) ? 'Unpin from the top of the sidebar' : 'Pin to the top of the sidebar';
      pin.append(icon('star')); pin.onclick = e => { e.stopPropagation(); togglePin(tl.id); renderAllTools(); };
      top.append(ic, pin);
      tile.append(top, el('b', null, tl.name), el('small', null, on ? tl.desc : 'Switched off in Settings'));
      tile.onclick = () => on ? showPage(tl.id) : showPage('settings');
      grid.append(tile);
    }
    $('atEmpty').hidden = grid.children.length > 0;
  }
  App.pages.alltools = {
    init() { $('atSearch').oninput = renderAllTools; },
    show() { $('atSearch').value = ''; renderAllTools(); setTimeout(() => $('atSearch').focus(), 50); },
  };

  // ---------- Ctrl+K: jump to any tool or action by typing
  const ACTIONS = [
    { name: 'All tools', icon: 'grid', run: () => showPage('alltools'), keys: 'apps everything' },
    { name: 'Settings', icon: 'gear', run: () => showPage('settings'), keys: 'options preferences appearance language' },
    { name: 'Connect phone', icon: 'phone', run: () => showPage('connect'), keys: 'qr pair pairing code', feature: 'transfer' },
    { name: 'Check for updates', icon: 'refresh', run: () => { showPage('settings'); $('updCheck').click(); }, keys: 'update new version' },
    { name: 'Change background', icon: 'image', run: () => { showPage('settings'); $('bgChoose').scrollIntoView({ block: 'center' }); }, keys: 'wallpaper picture gif video' },
  ];
  let results = [], sel = 0;
  function openPalette() {
    $('palette').hidden = false; $('palInput').value = ''; filter(); $('palInput').focus();
  }
  function closePalette() { $('palette').hidden = true; }
  function filter() {
    const q = $('palInput').value.trim().toLowerCase();
    const all = [...TOOLS.filter(enabled).map(tl => ({ name: tl.name, icon: tl.icon, desc: tl.desc, keys: tl.keys, run: () => showPage(tl.id) })),
                 ...ACTIONS.filter(a => !a.feature || App.state.features[a.feature] !== false)];
    results = all.filter(r => !q || `${t(r.name)} ${r.name} ${r.desc ? t(r.desc) : ''} ${r.keys || ''}`.toLowerCase().includes(q))
                 .sort((a, b) => (t(b.name).toLowerCase().startsWith(q) ? 1 : 0) - (t(a.name).toLowerCase().startsWith(q) ? 1 : 0))
                 .slice(0, 9);
    sel = 0; draw();
  }
  function draw() {
    const list = $('palList'); list.replaceChildren();
    results.forEach((r, i) => {
      const row = el('button', 'pal-row' + (i === sel ? ' on' : ''));
      row.append(icon(r.icon), el('span', null, r.name)); if (r.desc) row.append(el('small', null, r.desc));
      row.onmouseenter = () => { sel = i; draw(); };
      row.onclick = () => { closePalette(); r.run(); };
      list.append(row);
    });
    $('palEmpty').hidden = results.length > 0;
  }
  document.addEventListener('keydown', e => {
    if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'k' && App.state) {
      e.preventDefault(); $('palette').hidden ? openPalette() : closePalette(); return;
    }
    if ($('palette').hidden) return;
    if (e.key === 'Escape') { closePalette(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); sel = Math.min(results.length - 1, sel + 1); draw(); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); sel = Math.max(0, sel - 1); draw(); }
    else if (e.key === 'Enter' && results[sel]) { e.preventDefault(); const r = results[sel]; closePalette(); r.run(); }
  });
  document.addEventListener('DOMContentLoaded', () => {
    $('palInput').oninput = filter;
    $('palette').onclick = e => { if (e.target === $('palette')) closePalette(); };
    $('searchBtn').onclick = openPalette;
  });
})();
