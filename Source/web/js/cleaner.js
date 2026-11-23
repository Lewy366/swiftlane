// Storage & Cleanup: Quick clean, Downloads, and Steam Games tabs inside #page-diskmap.
// Pairs with Rust/src/tools/cleaner.rs (clean_scan, clean_run, clean_open, clean_reveal).
(() => {
  let initialized = false;
  let mode = 'map';
  let data = null;
  let loading = false;
  let cleaning = false;
  const selected = new Set();
  const rowRefs = new Map(); // id -> { row, cb, item }

  const tr = s => (typeof t === 'function' ? t(s) : s);

  function fmtDate(sec) {
    if (!sec) return '';
    try {
      return new Date(sec * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
    } catch (_) {
      return new Date(sec * 1000).toLocaleDateString();
    }
  }

  function cleanableItems() {
    const out = [];
    if (!data) return out;
    for (const q of (data.quick || [])) {
      if (q.kind === 'empty_cache' || q.kind === 'recycle') out.push(q);
    }
    for (const d of (data.downloads || [])) {
      out.push({ ...d, _source: 'downloads', kind_clean: 'recycle' });
    }
    return out;
  }

  function pruneSelection() {
    const valid = new Set(cleanableItems().map(i => i.id));
    for (const id of [...selected]) {
      if (!valid.has(id)) selected.delete(id);
    }
  }

  function selectionStats() {
    let count = 0;
    let bytes = 0;
    let recycleCount = 0;
    let cacheCount = 0;
    for (const item of cleanableItems()) {
      if (!selected.has(item.id)) continue;
      count++;
      bytes += item.size || 0;
      if (item.kind === 'empty_cache') cacheCount++;
      else recycleCount++;
    }
    return { count, bytes, recycleCount, cacheCount };
  }

  function updateSpaceBar() {
    const freeEl = $('clFreeText');
    const afterEl = $('clAfterText');
    const barUsed = $('clBarUsed');
    const barSel = $('clBarSel');
    const barFree = $('clBarFree');
    const removeBtn = $('clRemoveBtn');
    const refreshBtn = $('clRefresh');

    if (refreshBtn) refreshBtn.disabled = loading || cleaning;

    const { count, bytes } = selectionStats();
    if (removeBtn) {
      removeBtn.disabled = loading || cleaning || count === 0;
      removeBtn.textContent = cleaning
        ? tr('Removing…')
        : count > 0
          ? `${tr('Remove ticked')} (${count.toLocaleString()} · ${fmtSize(bytes)})`
          : tr('Remove ticked');
    }

    if (!data || !data.drive) {
      if (freeEl) freeEl.textContent = loading ? tr('Measuring storage…') : '';
      if (afterEl) { afterEl.textContent = ''; afterEl.hidden = true; }
      if (barUsed) barUsed.style.flexBasis = '0%';
      if (barSel) barSel.style.flexBasis = '0%';
      if (barFree) barFree.style.flexBasis = '100%';
      return;
    }

    const d = data.drive;
    const label = (d.path || 'C:').replace(/\\+$/, '');
    const total = d.total || 0;
    const free = d.free || 0;
    const used = d.used != null ? d.used : Math.max(0, total - free);

    if (freeEl) {
      freeEl.replaceChildren();
      const drv = el('b', null, label);
      drv.setAttribute('translate', 'no');
      freeEl.append(
        drv,
        document.createTextNode(` · ${fmtSize(free)} ${tr('free of')} ${fmtSize(total)}`)
      );
    }

    const clampedSel = Math.min(used, bytes);
    const remUsed = Math.max(0, used - clampedSel);
    const pUsed = total > 0 ? (remUsed / total) * 100 : 0;
    const pSel = total > 0 ? (clampedSel / total) * 100 : 0;
    const pFree = total > 0 ? Math.max(0, 100 - pUsed - pSel) : 100;

    if (barUsed) barUsed.style.flexBasis = pUsed.toFixed(2) + '%';
    if (barSel) barSel.style.flexBasis = pSel.toFixed(2) + '%';
    if (barFree) barFree.style.flexBasis = pFree.toFixed(2) + '%';

    if (afterEl) {
      if (count > 0 && bytes > 0) {
        afterEl.hidden = false;
        afterEl.textContent = `${tr('After removing')}: ${fmtSize(free + bytes)} ${tr('free')} (+${fmtSize(bytes)})`;
      } else {
        afterEl.hidden = true;
        afterEl.textContent = '';
      }
    }
  }

  function setChecked(id, checked) {
    if (checked) selected.add(id);
    else selected.delete(id);
    const ref = rowRefs.get(id);
    if (ref) {
      if (ref.cb) ref.cb.checked = checked;
      ref.row.classList.toggle('cl-selected', checked);
    }
    updateSpaceBar();
  }

  function renderQuick() {
    const list = $('clQuickList');
    if (!list) return;
    list.replaceChildren();

    if (loading && !data) {
      const box = el('div', 'empty');
      box.append(el('span', 'spinner'), document.createTextNode(' ' + tr('Measuring caches and leftover folders…')));
      list.append(box);
      return;
    }

    const items = (data && data.quick) || [];
    if (!items.length) {
      list.append(el('div', 'empty', tr('Nothing found to clean right now.')));
      return;
    }

    const groups = [
      { key: 'safe', title: tr('Safe to clear'), badgeCls: 'dm-adv-safe', badge: tr('Safe') },
      { key: 'consider', title: tr('Consider clearing'), badgeCls: 'dm-adv-consider', badge: tr('Review') },
    ];

    for (const grp of groups) {
      const members = items.filter(i => i.group === grp.key);
      if (!members.length) continue;

      const head = el('div', 'cl-group-head');
      const title = el('span', 'label', grp.title);
      title.style.margin = '0';
      const badge = el('span', 'dm-adv-tag ' + grp.badgeCls, grp.badge);
      head.append(title, badge);
      list.append(head);

      for (const item of members) {
        const cleanable = item.kind === 'empty_cache' || item.kind === 'recycle';
        const row = el('div', 'row cl-row' + (cleanable && selected.has(item.id) ? ' cl-selected' : ''));
        let cb = null;

        if (cleanable) {
          cb = el('input', 'cl-check');
          cb.type = 'checkbox';
          cb.checked = selected.has(item.id);
          cb.setAttribute('aria-label', item.name);
          cb.onclick = e => e.stopPropagation();
          cb.onchange = () => setChecked(item.id, cb.checked);
          row.append(cb);
          row.style.cursor = 'pointer';
          row.onclick = () => setChecked(item.id, !selected.has(item.id));
        }

        const kindBox = el('div', 'kind ' + (item.kind === 'recycle' ? 'k-zip' : item.kind === 'open' ? 'k-doc' : 'k-other'));
        kindBox.append(icon(item.kind === 'recycle' ? 'folder' : item.kind === 'open' ? 'open' : 'disk'));

        const meta = el('div', 'meta');
        const nameEl = el('div', 'name');
        if (item.id && item.id.startsWith('steamleft-')) {
          const prefix = 'Leftover game folder: ';
          if (item.name.startsWith(prefix)) {
            const folderName = el('span', null, item.name.slice(prefix.length));
            folderName.setAttribute('translate', 'no');
            nameEl.append(document.createTextNode(tr('Leftover game folder') + ': '), folderName);
          } else {
            nameEl.textContent = item.name;
            nameEl.setAttribute('translate', 'no');
          }
        } else {
          nameEl.textContent = tr(item.name);
        }

        const sub = el('div', 'sub cl-detail', tr(item.detail || ''));
        meta.append(nameEl, sub);
        row.append(kindBox, meta);

        const acts = el('div', 'acts cl-acts');
        if (cleanable) {
          const sz = el('span', 'cl-size', fmtSize(item.size || 0));
          sz.setAttribute('translate', 'no');
          acts.append(sz);
          if (item.path) {
            row.title = item.path;
            acts.append(miniBtn('folder', tr('Show in File Explorer'), () => App.api.clean_reveal(item.id)));
          }
        } else if (item.kind === 'open') {
          const btn = el('button', 'btn small', tr(item.button || 'Open'));
          btn.onclick = e => {
            e.stopPropagation();
            App.api.clean_open(item.id, 'primary');
          };
          acts.append(btn);
        }
        row.append(acts);
        list.append(row);

        if (cleanable) rowRefs.set(item.id, { row, cb, item });
      }
    }
  }

  function downloadKindLabel(kind) {
    if (kind === 'archive') return tr('Archive');
    if (kind === 'installer') return tr('Installer');
    if (kind === 'folder') return tr('Folder');
    return tr('File');
  }

  function renderDownloads() {
    const list = $('clDlList');
    if (!list) return;
    list.replaceChildren();

    if (loading && !data) {
      const box = el('div', 'empty');
      box.append(el('span', 'spinner'), document.createTextNode(' ' + tr('Scanning Downloads…')));
      list.append(box);
      return;
    }

    const items = (data && data.downloads) || [];
    if (!items.length) {
      list.append(el('div', 'empty', tr('Your Downloads folder is empty.')));
      return;
    }

    for (const item of items) {
      const { row, sub } = fileRow(item.name);
      row.classList.add('cl-row');
      if (selected.has(item.id)) row.classList.add('cl-selected');
      if (item.kind === 'folder') {
        const k = row.querySelector('.kind');
        if (k) { k.className = 'kind k-other'; k.replaceChildren(icon('folder')); }
      }

      const cb = el('input', 'cl-check');
      cb.type = 'checkbox';
      cb.checked = selected.has(item.id);
      cb.setAttribute('aria-label', item.name);
      cb.onclick = e => e.stopPropagation();
      cb.onchange = () => setChecked(item.id, cb.checked);
      row.prepend(cb);

      const dateStr = fmtDate(item.modified);
      const szText = el('span', null, fmtSize(item.size || 0));
      szText.setAttribute('translate', 'no');
      sub.replaceChildren(szText);
      if (dateStr) {
        const dt = el('span', null, ' · ' + dateStr);
        dt.setAttribute('translate', 'no');
        sub.append(dt);
      }

      const acts = el('div', 'acts cl-acts');
      const badge = el('span', 'tag cl-tag cl-tag-' + item.kind, downloadKindLabel(item.kind));
      acts.append(badge, miniBtn('open', tr('Show in File Explorer'), () => App.api.clean_reveal(item.id)));
      row.append(acts);

      if (item.path) row.title = item.path;
      row.style.cursor = 'pointer';
      row.onclick = () => setChecked(item.id, !selected.has(item.id));
      row.ondblclick = e => {
        e.stopPropagation();
        App.api.clean_reveal(item.id);
      };

      list.append(row);
      rowRefs.set(item.id, { row, cb, item });
    }
  }

  function renderGames() {
    const list = $('clGamesList');
    const empty = $('clGamesEmpty');
    if (!list) return;
    list.replaceChildren();

    if (loading && !data) {
      if (empty) empty.hidden = true;
      const box = el('div', 'empty');
      box.append(el('span', 'spinner'), document.createTextNode(' ' + tr('Scanning Steam libraries…')));
      list.append(box);
      return;
    }

    const games = (data && data.games) || [];
    if (empty) empty.hidden = games.length > 0;
    if (!games.length) return;

    for (const g of games) {
      const row = el('div', 'row cl-row');

      const kindBox = el('div', 'kind k-other');
      kindBox.append(icon('grid'));

      const meta = el('div', 'meta');
      const nameEl = el('div', 'name', g.name);
      nameEl.setAttribute('translate', 'no');

      const sub = el('div', 'sub');
      const sz = el('span', null, fmtSize(g.size || 0));
      sz.setAttribute('translate', 'no');
      const played = g.last_played ? fmtDate(g.last_played) : tr('Never');
      const playedEl = el('span', null, played);
      if (g.last_played) playedEl.setAttribute('translate', 'no');
      sub.append(sz, document.createTextNode(' · ' + tr('Last played') + ': '), playedEl);
      meta.append(nameEl, sub);

      const acts = el('div', 'acts cl-acts');
      if (g.drive) {
        const drv = el('span', 'tag cl-drive', g.drive);
        drv.setAttribute('translate', 'no');
        acts.append(drv);
      }
      if (g.path) {
        row.title = g.path;
        acts.append(miniBtn('folder', tr('Show in File Explorer'), () => App.api.clean_reveal(g.id)));
      }

      const uninstallBtn = el('button', 'btn small danger', tr('Uninstall…'));
      uninstallBtn.onclick = e => {
        e.stopPropagation();
        App.api.clean_open(g.id, 'primary');
      };
      const steamBtn = el('button', 'btn small ghost', tr('Open in Steam'));
      steamBtn.onclick = e => {
        e.stopPropagation();
        App.api.clean_open(g.id, 'secondary');
      };
      acts.append(uninstallBtn, steamBtn);

      row.append(kindBox, meta, acts);
      list.append(row);
    }
  }

  function renderAll() {
    rowRefs.clear();
    pruneSelection();
    renderQuick();
    renderDownloads();
    renderGames();
    updateSpaceBar();
  }

  async function scan() {
    if (loading || !App.api || !App.api.clean_scan) return;
    loading = true;
    renderAll();
    try {
      const res = await App.api.clean_scan();
      if (res && res.error) toast(res.error);
      else if (res) data = res;
    } catch (e) {
      toast(tr('Could not measure storage right now.'));
    } finally {
      loading = false;
      renderAll();
    }
  }

  async function runClean() {
    if (cleaning || loading || !App.api || !App.api.clean_run) return;
    const ids = [...selected];
    const { count, bytes, recycleCount, cacheCount } = selectionStats();
    if (!count) return;

    const lines = [
      `${tr('Remove')} ${count.toLocaleString()} ${count === 1 ? tr('ticked item') : tr('ticked items')} (~${fmtSize(bytes)})?`,
    ];
    if (recycleCount > 0) {
      lines.push(tr('Downloads and leftover game folders are moved to the Recycle Bin so you can restore them later.'));
    }
    if (cacheCount > 0) {
      lines.push(tr('Cache folders are cleared safely in place; any files currently in use are skipped.'));
    }
    if (!confirm(lines.join('\n\n'))) return;

    cleaning = true;
    updateSpaceBar();
    try {
      const res = await App.api.clean_run(ids);
      if (!res || res.error) {
        toast((res && res.error) || tr('Cleanup failed.'));
        return;
      }
      selected.clear();
      if (res.scan) data = res.scan;
      renderAll();

      const freed = res.freed || 0;
      const failed = res.failed || 0;
      if (freed > 0 && failed > 0) {
        toast(`${tr('Freed')} ${fmtSize(freed)} (${failed.toLocaleString()} ${tr('skipped or in use')})`);
      } else if (freed > 0) {
        toast(`${tr('Freed')} ${fmtSize(freed)}`);
      } else if (failed > 0) {
        toast(res.first_error || tr('Some items could not be removed because they are in use.'));
      } else {
        toast(tr('Nothing needed removing.'));
      }
    } catch (e) {
      toast(tr('Cleanup failed.'));
    } finally {
      cleaning = false;
      updateSpaceBar();
    }
  }

  function applyMode(next) {
    mode = next || 'map';
    const panels = {
      map: $('diskTabMap'),
      quick: $('diskTabQuick'),
      downloads: $('diskTabDownloads'),
      games: $('diskTabGames'),
    };
    for (const [k, node] of Object.entries(panels)) {
      if (node) node.hidden = k !== mode;
    }
    const card = $('clSpaceCard');
    if (card) card.hidden = mode === 'map';

    if (mode !== 'map' && !data && !loading) {
      scan();
    } else if (mode !== 'map') {
      updateSpaceBar();
    }
  }

  function tickQuickSafe() {
    if (!data || !data.quick) return;
    for (const item of data.quick) {
      if (item.group === 'safe' && (item.kind === 'empty_cache' || item.kind === 'recycle')) {
        setChecked(item.id, true);
      }
    }
  }

  function untickQuick() {
    if (!data || !data.quick) return;
    for (const item of data.quick) setChecked(item.id, false);
  }

  function tickDownloadsKind(kind) {
    if (!data || !data.downloads) return;
    for (const item of data.downloads) {
      if (item.kind === kind) setChecked(item.id, true);
    }
  }

  function untickDownloads() {
    if (!data || !data.downloads) return;
    for (const item of data.downloads) setChecked(item.id, false);
  }

  function initCleaner() {
    if (initialized) return;
    const tabs = $('stModeTabs');
    if (!tabs) return;
    initialized = true;

    bindSeg('stModeTabs', v => applyMode(v));

    const refreshBtn = $('clRefresh');
    if (refreshBtn) refreshBtn.onclick = () => scan();

    const removeBtn = $('clRemoveBtn');
    if (removeBtn) removeBtn.onclick = () => runClean();

    const selSafe = $('clSelSafe');
    if (selSafe) selSafe.onclick = () => tickQuickSafe();

    const selNoneQ = $('clSelNoneQ');
    if (selNoneQ) selNoneQ.onclick = () => untickQuick();

    const selArch = $('clSelArch');
    if (selArch) selArch.onclick = () => tickDownloadsKind('archive');

    const selInst = $('clSelInst');
    if (selInst) selInst.onclick = () => tickDownloadsKind('installer');

    const selNoneD = $('clSelNoneD');
    if (selNoneD) selNoneD.onclick = () => untickDownloads();

    const activeBtn = tabs.querySelector('button.on');
    applyMode(activeBtn ? activeBtn.dataset.v : 'map');
  }

  App.initCleaner = initCleaner;

  // Hook into App.pages.diskmap if already registered so init/show stay seamless.
  if (App.pages && App.pages.diskmap) {
    const prevInit = App.pages.diskmap.init;
    App.pages.diskmap.init = function () {
      if (prevInit) prevInit.apply(this, arguments);
      initCleaner();
    };
    const prevShow = App.pages.diskmap.show;
    App.pages.diskmap.show = function () {
      if (prevShow) prevShow.apply(this, arguments);
      initCleaner();
      if (mode !== 'map' && !data && !loading) scan();
    };
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initCleaner);
  } else {
    initCleaner();
  }
})();
