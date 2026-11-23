// Full-bleed Home / Greeting screen: recent tools, recent files, live status and updates.
(() => {
  const TOOLS_KEY = 'swiftlane-recent-tools';
  const FILES_KEY = 'swiftlane-recent-files';
  const IGNORED_PAGES = new Set(['home', 'alltools', 'settings', 'connect']);
  const DEFAULT_TOOLS = ['findfiles', 'qrstudio', 'duplicates', 'transfer'];
  const JOB_SOURCES = {
    convert: 'File converter',
    images: 'Image editor',
    download: 'Video downloader',
    trimmer: 'Video trimmer',
    trim: 'Video trimmer',
    shrink: 'Shrink to size',
    pdf: 'PDF tools',
  };
  const CURRENT_NOTES = [
    'Full-bleed Home screen with Recent tools, Recent files, and live Patch notes',
    'Storage & Cleanup: 5-10x faster parallel drive map + optional 2-5s NTFS MFT scan',
    'Cleanup advisor sorting findings into Safe to remove, Consider, and Should stay',
    'Built-in Quick clean (caches & Steam leftovers), Downloads tidy, and Steam games',
    'Find files, Duplicate finder, Startup check, and offline QR Studio',
  ];

  let receivedLive = [];
  let filesSeq = 0;
  let statusObserver = null;

  const tr = s => (typeof t === 'function' ? t(s) : s);

  function readStore(key) {
    try {
      const raw = localStorage.getItem(key);
      const parsed = raw ? JSON.parse(raw) : [];
      return Array.isArray(parsed) ? parsed : [];
    } catch (e) {
      return [];
    }
  }

  function writeStore(key, list) {
    try { localStorage.setItem(key, JSON.stringify(list)); } catch (e) {}
  }

  function normTs(v) {
    const n = Number(v);
    if (!Number.isFinite(n) || n <= 0) return 0;
    return n > 1e12 ? n : Math.round(n * 1000);
  }

  function toolMeta(id) {
    return typeof TOOLS !== 'undefined' ? TOOLS.find(x => x.id === id) : null;
  }

  function toolEnabled(tl) {
    if (!tl) return false;
    if (!tl.feature) return true;
    return !(App.state && App.state.features && App.state.features[tl.feature] === false);
  }

  function shortAge(ts) {
    if (!ts) return '';
    const ms = normTs(ts);
    if (!ms) return '';
    const sec = Math.max(0, (Date.now() - ms) / 1000);
    if (sec < 60) return tr('Now');
    if (sec < 3600) return Math.floor(sec / 60) + 'm';
    if (sec < 21600) return Math.floor(sec / 3600) + 'h';
    const d = new Date(ms);
    const now = new Date();
    const sameDay = d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth() && d.getDate() === now.getDate();
    if (sameDay) return tr('Today');
    const yest = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1);
    if (d.getFullYear() === yest.getFullYear() && d.getMonth() === yest.getMonth() && d.getDate() === yest.getDate()) return tr('Yesterday');
    if (sec < 7 * 86400) return Math.floor(sec / 86400) + 'd';
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  function greetingPhrase() {
    const h = new Date().getHours();
    if (h >= 5 && h < 12) return 'Good morning';
    if (h >= 12 && h < 18) return 'Good afternoon';
    return 'Good evening';
  }

  function renderGreeting() {
    const g = $('homeGreeting');
    if (!g) return;
    const ver = App.state && App.state.version ? ' · v' + App.state.version : '';
    g.textContent = tr(greetingPhrase()) + ver;
  }

  function syncStatus() {
    const srcDot = $('statusDot'), srcText = $('statusText');
    const dstDot = $('homeStatusDot'), dstText = $('homeStatusText');
    if (srcDot && dstDot) dstDot.className = srcDot.className;
    if (srcText && dstText) dstText.textContent = srcText.textContent;
  }

  // ---------- recent tools
  function recordToolVisit(id) {
    if (!id || IGNORED_PAGES.has(id) || !toolMeta(id)) return;
    const list = readStore(TOOLS_KEY).filter(x => x && x.id && x.id !== id && !IGNORED_PAGES.has(x.id));
    list.unshift({ id, ts: Date.now() });
    writeStore(TOOLS_KEY, list.slice(0, 12));
  }
  App.recordToolVisit = recordToolVisit;

  function renderRecentTools() {
    const box = $('homeRecentTools');
    if (!box) return;
    box.replaceChildren();

    const stored = readStore(TOOLS_KEY)
      .filter(x => x && x.id && !IGNORED_PAGES.has(x.id))
      .map(x => ({ tl: toolMeta(x.id), ts: normTs(x.ts) }))
      .filter(x => toolEnabled(x.tl));

    const seen = new Set();
    let picks = [];
    for (const item of stored) {
      if (seen.has(item.tl.id)) continue;
      seen.add(item.tl.id);
      picks.push(item);
      if (picks.length >= 4) break;
    }

    if (!picks.length) {
      picks = DEFAULT_TOOLS
        .map(id => ({ tl: toolMeta(id), ts: 0 }))
        .filter(x => toolEnabled(x.tl))
        .slice(0, 4);
    }

    for (const { tl, ts } of picks) {
      const row = el('button', 'home-tool-row');
      row.type = 'button';
      const ico = el('span', 'home-tool-ico cat-' + (tl.cat || 'files'));
      ico.append(icon(tl.icon));
      const name = el('span', 'home-tool-name', tl.name);
      row.append(ico, name);
      const age = shortAge(ts);
      if (age) row.append(el('span', 'home-tool-age', age));
      row.onclick = () => showPage(tl.id);
      box.append(row);
    }
  }

  // ---------- recent files (Phone transfer + completed non-Vault jobs)
  function recordJobOutputs(job) {
    if (!job || job.status !== 'done' || job.kind === 'vault') return;
    if (!Array.isArray(job.outputs) || !job.outputs.length) return;
    const source = JOB_SOURCES[job.kind] || 'Swiftlane';
    const now = Date.now();
    const list = readStore(FILES_KEY).filter(x => x && x.path && x.kind !== 'vault' && x.source !== 'Vault');
    const seen = new Set();
    const added = [];
    for (const out of job.outputs) {
      const path = typeof out === 'string' ? out : (out && out.path);
      if (!path) continue;
      const key = path.toLowerCase();
      if (seen.has(key)) continue;
      seen.add(key);
      const name = (typeof out === 'object' && out && out.name) || baseName(path);
      const size = (typeof out === 'object' && out && Number(out.size)) || 0;
      added.push({ name, path, size, ts: now, source });
    }
    if (!added.length) return;
    const merged = [...added, ...list.filter(x => !seen.has(String(x.path).toLowerCase()))].slice(0, 20);
    writeStore(FILES_KEY, merged);
    if (App.page === 'home') renderRecentFiles();
  }

  async function fetchReceived() {
    let raw = null;
    if (App.api && typeof App.api.get_received === 'function') {
      try {
        const res = await App.api.get_received();
        if (Array.isArray(res)) raw = res;
      } catch (e) {}
    }
    if (!raw) raw = receivedLive;
    return raw
      .filter(e => e && e.path)
      .slice(0, 5)
      .map(e => ({
        name: e.name || baseName(e.path),
        path: e.path,
        size: Number(e.size) || 0,
        ts: normTs(e.time != null ? e.time : e.ts),
        source: 'Phone transfer',
      }));
  }

  async function renderRecentFiles() {
    const box = $('homeRecentFiles');
    if (!box) return;
    const seq = ++filesSeq;

    const received = await fetchReceived();
    if (seq !== filesSeq) return;

    const stored = readStore(FILES_KEY)
      .filter(x => x && x.path && x.kind !== 'vault' && x.source !== 'Vault')
      .map(x => ({
        name: x.name || baseName(x.path),
        path: x.path,
        size: Number(x.size) || 0,
        ts: normTs(x.ts),
        source: x.source || 'Swiftlane',
      }));

    const combined = [...received, ...stored].sort((a, b) => b.ts - a.ts);
    const seen = new Set();
    const top = [];
    for (const item of combined) {
      const key = String(item.path).toLowerCase();
      if (seen.has(key)) continue;
      seen.add(key);
      top.push(item);
      if (top.length >= 5) break;
    }

    box.replaceChildren();
    if (!top.length) {
      box.append(el('div', 'empty', 'Files you receive or convert show up here.'));
      return;
    }

    for (const item of top) {
      const row = el('div', 'home-file-row');
      row.setAttribute('role', 'button');
      row.tabIndex = 0;
      row.title = item.path;

      const k = kindOf(item.name);
      const badge = el('div', 'kind ' + k.cls, k.label);
      const meta = el('div', 'meta');
      const nameEl = el('div', 'name', item.name);
      nameEl.setAttribute('translate', 'no');
      const subText = item.size > 0 ? tr(item.source) + ' · ' + fmtSize(item.size) : tr(item.source);
      const subEl = el('div', 'sub', subText);
      meta.append(nameEl, subEl);

      const reveal = miniBtn('folder', tr('Show in folder'), () => App.api.reveal_path(item.path));
      reveal.type = 'button';

      const open = () => App.api.open_path(item.path);
      row.onclick = open;
      row.onkeydown = e => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          open();
        }
      };

      row.append(badge, meta, reveal);
      box.append(row);
    }
  }

  // ---------- updates & patch notes card
  function parseBullets(notes, max) {
    const out = [];
    for (const raw of String(notes || '').split(/\r?\n/)) {
      const clean = raw.trim().replace(/^[-*•]+\s+/, '').replace(/^#+\s+/, '').trim();
      if (!clean || /^[-=_*]{3,}$/.test(clean)) continue;
      out.push(clean);
      if (out.length >= max) break;
    }
    return out;
  }

  function fillNotesList(listEl, lines) {
    listEl.replaceChildren();
    for (const line of lines) listEl.append(el('li', null, line));
  }

  function openUpdateAction(u) {
    const modal = $('updModal');
    if (modal) {
      if (u && $('umTitle') && !$('umTitle').textContent.trim()) {
        $('umTitle').textContent = tr('Update available') + ': Swiftlane ' + u.version;
        if ($('umMeta')) {
          $('umMeta').textContent = tr('You have') + ' ' + (App.state ? App.state.version : '')
            + (u.size ? ' · ' + fmtSize(u.size) : '');
        }
        if ($('umNotes') && !$('umNotes').children.length) {
          const ul = el('ul');
          fillNotesList(ul, parseBullets(u.notes, 6));
          $('umNotes').replaceChildren(ul);
        }
      }
      modal.hidden = false;
      if ($('umGo')) $('umGo').focus();
      return;
    }
    if ($('updGo')) { $('updGo').click(); return; }
    showPage('settings');
  }

  async function checkUpdatesFromHome() {
    const btn = $('homeUpdBtn');
    if (!btn) return;
    btn.disabled = true;
    btn.textContent = 'Checking…';
    try {
      const r = await App.api.check_update();
      if (!r || r.error) {
        toast(r && r.error ? r.error : 'Could not check for updates');
        renderUpdate();
        return;
      }
      App.state.update = r.update || null;
      renderUpdate();
      if (!r.update) toast('Swiftlane is up to date');
    } catch (e) {
      toast('Could not check for updates');
      renderUpdate();
    }
  }

  function renderUpdate() {
    const card = $('homeUpdCard');
    const title = $('homeUpdTitle');
    const sub = $('homeUpdSub');
    const dot = $('homeUpdDot');
    const head = $('homeUpdHead');
    const notes = $('homeUpdNotes');
    const btn = $('homeUpdBtn');
    if (!card || !title || !sub || !dot || !head || !notes || !btn) return;

    const u = App.state && App.state.update;
    if (u) {
      card.classList.add('home-upd-avail');
      title.textContent = 'Update available';
      sub.textContent = 'v' + u.version + (u.size ? ' · ' + fmtSize(u.size) : '');
      dot.className = 'home-upd-dot avail';
      head.textContent = "What's new in " + u.version;
      const bullets = parseBullets(u.notes, 6);
      fillNotesList(notes, bullets.length ? bullets : ['Improvements and fixes in this release.']);
      btn.className = 'btn primary';
      btn.disabled = false;
      btn.textContent = 'Update now';
      btn.onclick = () => openUpdateAction(u);
    } else {
      card.classList.remove('home-upd-avail');
      title.textContent = 'Up to date';
      sub.textContent = App.state && App.state.version ? 'v' + App.state.version : '';
      dot.className = 'home-upd-dot ok';
      head.textContent = 'Patch notes';
      const updatedNotes = App.state && App.state.updated && App.state.updated.notes
        ? parseBullets(App.state.updated.notes, 6)
        : [];
      fillNotesList(notes, updatedNotes.length ? updatedNotes : CURRENT_NOTES);
      btn.className = 'btn';
      btn.disabled = false;
      btn.textContent = 'Check for updates';
      btn.onclick = checkUpdatesFromHome;
    }
  }

  // ---------- live events
  App.on('job', recordJobOutputs);

  App.on('received', data => {
    if (data && data.entry && data.entry.path) {
      const key = String(data.entry.path).toLowerCase();
      receivedLive = [data.entry, ...receivedLive.filter(e => e && String(e.path).toLowerCase() !== key)].slice(0, 20);
      if (App.page === 'home') renderRecentFiles();
    }
  });

  App.on('update-available', u => {
    if (App.state) App.state.update = u;
    renderUpdate();
  });

  // ---------- page lifecycle
  App.pages.home = {
    init() {
      receivedLive = Array.isArray(App.state && App.state.received) ? [...App.state.received] : [];

      const menuBtn = $('homeMenuBtn');
      if (menuBtn) menuBtn.onclick = () => document.body.classList.toggle('nav-open');

      const searchBtn = $('homeSearchBtn');
      if (searchBtn) searchBtn.onclick = () => { const s = $('searchBtn'); if (s) s.click(); };

      const allTools = $('homeAllTools');
      if (allTools) allTools.onclick = () => showPage('alltools');

      if (!statusObserver && typeof MutationObserver !== 'undefined') {
        statusObserver = new MutationObserver(syncStatus);
        if ($('statusDot')) statusObserver.observe($('statusDot'), { attributes: true, attributeFilter: ['class'] });
        if ($('statusText')) statusObserver.observe($('statusText'), { childList: true, characterData: true, subtree: true });
      }

      renderGreeting();
      syncStatus();
      renderRecentTools();
      renderRecentFiles();
      renderUpdate();

      setInterval(() => {
        if (App.page === 'home') {
          renderGreeting();
          renderRecentTools();
        }
      }, 60000);
    },

    show() {
      renderGreeting();
      syncStatus();
      renderRecentTools();
      renderRecentFiles();
      renderUpdate();
    },

    relang() {
      renderGreeting();
      renderRecentTools();
      renderRecentFiles();
    },
  };
})();
