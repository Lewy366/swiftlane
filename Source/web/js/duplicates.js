// Duplicate file finder: scan a drive or folder for identical files, pick which copies to keep,
// and move only the selected extras to the Recycle Bin.
(() => {
  const PAGE_SIZE = 100;
  let view = null;
  let scanning = null;
  let trashing = false;
  let shownGroups = PAGE_SIZE;
  const selected = new Set();
  const rowRefs = new Map(); // path -> { row, cb, badge, file }

  function fmtModified(sec) {
    if (!sec) return '';
    try {
      return new Date(sec * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
    } catch (_) {
      return new Date(sec * 1000).toLocaleString();
    }
  }

  function validPaths() {
    const out = new Set();
    if (!view || !view.groups) return out;
    for (const g of view.groups) {
      for (const f of g.files) {
        if (!f.protected) out.add(f.path);
      }
    }
    return out;
  }

  function pruneSelection() {
    const allowed = validPaths();
    for (const p of [...selected]) {
      if (!allowed.has(p)) selected.delete(p);
    }
  }

  function selectionStats() {
    let count = 0;
    let bytes = 0;
    let allSelectedGroups = 0;
    if (view && view.groups) {
      for (const g of view.groups) {
        let inGroup = 0;
        for (const f of g.files) {
          if (!f.protected && selected.has(f.path)) {
            count++;
            bytes += g.size;
            inGroup++;
          }
        }
        if (inGroup > 0 && inGroup === g.files.length) allSelectedGroups++;
      }
    }
    return { count, bytes, allSelectedGroups };
  }

  function updateRowState(ref) {
    const isSel = !ref.file.protected && selected.has(ref.file.path);
    ref.cb.checked = isSel;
    ref.row.classList.toggle('dup-selected', isSel);
    ref.row.classList.toggle('dup-kept', !isSel && !ref.file.protected);
    ref.row.classList.toggle('dup-protected', !!ref.file.protected);
    if (ref.file.protected) {
      ref.badge.className = 'tag dup-badge dup-prot';
      ref.badge.textContent = t('Protected');
    } else if (isSel) {
      ref.badge.className = 'tag dup-badge dup-del';
      ref.badge.textContent = t('Recycle Bin');
    } else {
      ref.badge.className = 'tag dup-badge dup-keep';
      ref.badge.textContent = t('KEEP');
    }
  }

  function updateActionBar() {
    const btn = $('dupTrashBtn');
    const clearBtn = $('dupClearSel');
    const hasGroups = !!(view && view.groups && view.groups.length);
    const { count, bytes } = selectionStats();
    btn.disabled = trashing || count === 0;
    btn.textContent = count > 0
      ? `${t('Move to Recycle Bin')} (${count.toLocaleString()} · ${fmtSize(bytes)})`
      : t('Move to Recycle Bin');
    clearBtn.disabled = count === 0;
    $('dupKeepNewest').disabled = !hasGroups;
    $('dupKeepOldest').disabled = !hasGroups;
    $('dupKeepShortest').disabled = !hasGroups;
  }

  function togglePath(path, checked) {
    const ref = rowRefs.get(path);
    if (ref && ref.file.protected) return;
    if (checked) selected.add(path);
    else selected.delete(path);
    if (ref) updateRowState(ref);
    updateActionBar();
  }

  function pickKeeper(files, mode) {
    return files.reduce((best, cur) => {
      if (!best) return cur;
      if (mode === 'newest') {
        if (cur.modified !== best.modified) return cur.modified > best.modified ? cur : best;
      } else if (mode === 'oldest') {
        const cm = cur.modified || Number.MAX_SAFE_INTEGER;
        const bm = best.modified || Number.MAX_SAFE_INTEGER;
        if (cm !== bm) return cm < bm ? cur : best;
      } else if (mode === 'shortest') {
        if (cur.path.length !== best.path.length) return cur.path.length < best.path.length ? cur : best;
      }
      if (cur.protected !== best.protected) return cur.protected ? cur : best;
      if (cur.path.length !== best.path.length) return cur.path.length < best.path.length ? cur : best;
      return cur.path < best.path ? cur : best;
    }, null);
  }

  function autoSelect(mode) {
    if (!view || !view.groups || !view.groups.length) return;
    selected.clear();
    for (const g of view.groups) {
      const keeper = pickKeeper(g.files, mode);
      for (const f of g.files) {
        if (f !== keeper && !f.protected) selected.add(f.path);
      }
    }
    for (const ref of rowRefs.values()) updateRowState(ref);
    updateActionBar();
  }

  function renderFileRow(f) {
    const { row, sub } = fileRow(f.name);
    row.classList.add('dup-file');

    const cb = el('input', 'dup-check');
    cb.type = 'checkbox';
    cb.disabled = !!f.protected;
    cb.setAttribute('aria-label', f.name);
    cb.onclick = e => e.stopPropagation();
    cb.onchange = () => togglePath(f.path, cb.checked);
    row.prepend(cb);

    const dirSpan = el('span', 'dup-dir', f.dir);
    dirSpan.setAttribute('translate', 'no');
    sub.append(dirSpan);
    const modText = fmtModified(f.modified);
    if (modText) {
      sub.append(el('span', 'dup-mod', ` · ${modText}`));
    }

    const badge = el('span', 'tag dup-badge');
    const acts = el('div', 'acts');
    acts.append(badge, miniBtn('open', t('Show in File Explorer'), () => App.api.dup_reveal(f.path)));
    row.append(acts);

    if (!f.protected) {
      row.style.cursor = 'pointer';
      row.onclick = () => togglePath(f.path, !selected.has(f.path));
    } else {
      row.title = t('Protected system or user folder file');
    }

    const ref = { row, cb, badge, file: f };
    rowRefs.set(f.path, ref);
    updateRowState(ref);
    return row;
  }

  function renderGroup(g) {
    const wrap = el('div', 'dup-group');
    const head = el('div', 'dup-group-head');

    const title = el('b', 'dup-group-name', g.files[0] ? g.files[0].name : '');
    title.setAttribute('translate', 'no');

    const meta = el('span', 'muted small dup-group-meta');
    meta.textContent = `${g.files.length} ${t('copies')} · ${fmtSize(g.size)} ${t('each')} · ${fmtSize(g.reclaimable)} ${t('reclaimable')}`;

    const hash = el('code', 'dup-hash', (g.sha256 || '').slice(0, 8));
    hash.setAttribute('translate', 'no');
    hash.title = g.sha256 || '';

    head.append(title, meta, el('span', 'spacer'), hash);

    const list = el('div', 'list dup-files');
    for (const f of g.files) list.append(renderFileRow(f));

    wrap.append(head, list);
    return wrap;
  }

  function renderGroups() {
    const box = $('dupGroups');
    box.replaceChildren();
    rowRefs.clear();

    if (!view || !view.groups || !view.groups.length) {
      const empty = el('div', 'empty');
      empty.append(icon('shield'));
      empty.append(el('b', null, t('No duplicate files found')));
      const info = el('span');
      const count = view ? (view.scanned_files || 0).toLocaleString() : '0';
      info.textContent = `${t('Scanned')} ${count} ${t('files')}`;
      empty.append(info);
      box.append(empty);
      return;
    }

    const frag = document.createDocumentFragment();
    const visible = view.groups.slice(0, shownGroups);
    for (const g of visible) frag.append(renderGroup(g));

    if (view.groups.length > shownGroups) {
      const remaining = view.groups.length - shownGroups;
      const moreWrap = el('div', 'dup-more');
      const moreBtn = el('button', 'btn small ghost', `${t('Show more groups')} (${remaining.toLocaleString()} ${t('remaining')})`);
      moreBtn.onclick = () => {
        shownGroups += PAGE_SIZE;
        renderGroups();
      };
      moreWrap.append(moreBtn);
      frag.append(moreWrap);
    }

    box.append(frag);
  }

  function render(v, opts = {}) {
    if (v !== undefined) view = v;
    $('dupResult').hidden = !view;
    if (!view) {
      selected.clear();
      rowRefs.clear();
      $('dupGroups').replaceChildren();
      return;
    }
    if (opts.resetSelection) {
      selected.clear();
      shownGroups = PAGE_SIZE;
    } else {
      pruneSelection();
    }
    $('dupReclaimable').textContent = fmtSize(view.reclaimable_bytes || 0);
    $('dupGroupCount').textContent = (view.total_groups || 0).toLocaleString();
    $('dupExtraCount').textContent = (view.extra_files || 0).toLocaleString();
    renderGroups();
    updateActionBar();
  }

  async function trashSelected() {
    if (trashing || !view) return;
    const paths = [...selected].filter(p => validPaths().has(p));
    if (!paths.length) return;
    const { count, bytes, allSelectedGroups } = selectionStats();
    let msg = `${t('Move')} ${count.toLocaleString()} ${count === 1 ? t('file') : t('files')} (${fmtSize(bytes)}) ${t('to the Recycle Bin? You can restore them from there.')}`;
    if (allSelectedGroups > 0) {
      msg = `${t('Warning: every copy is selected in')} ${allSelectedGroups.toLocaleString()} ${allSelectedGroups === 1 ? t('group') : t('groups')}.\n\n${msg}`;
    }
    if (!confirm(msg)) return;

    trashing = true;
    updateActionBar();
    try {
      const r = await App.api.dup_trash(paths);
      if (!r) return;
      if (r.error) {
        toast(r.error);
        return;
      }
      const trashed = Array.isArray(r.trashed) ? r.trashed : [];
      const errors = Array.isArray(r.errors) ? r.errors : [];
      for (const p of trashed) selected.delete(p);
      if (r.view !== undefined) render(r.view, { resetSelection: false });
      if (trashed.length && !errors.length) {
        toast(`${t('Moved')} ${trashed.length.toLocaleString()} ${trashed.length === 1 ? t('file') : t('files')} ${t('to the Recycle Bin')}`);
      } else if (trashed.length && errors.length) {
        const firstErr = errors[0] && errors[0].error ? errors[0].error : t('Some files could not be moved');
        toast(`${t('Moved')} ${trashed.length.toLocaleString()}; ${errors.length.toLocaleString()} ${t('failed')}: ${firstErr}`);
      } else if (errors.length) {
        toast(errors[0] && errors[0].error ? errors[0].error : t('Could not move files to the Recycle Bin'));
      }
    } finally {
      trashing = false;
      updateActionBar();
    }
  }

  async function scan(path) {
    if (scanning) return;
    const minEl = $('dupMinSize');
    const minBytes = minEl ? (Number(minEl.value) || 1) : 1;
    const r = await App.api.dup_scan(path, minBytes);
    if (r && r.error) {
      toast(r.error);
      return;
    }
    if (r) {
      scanning = r;
      $('dupStatus').hidden = false;
      $('dupStatusText').textContent = t('Starting…');
    }
  }

  async function loadDrives() {
    const ds = await App.api.dup_drives();
    if (!Array.isArray(ds)) return;
    $('dupDrives').replaceChildren(...ds.map(d => {
      const c = el('button', 'dm-drive');
      const used = d.total ? d.used / d.total : 0;
      const head = el('div', 'dm-drive-head');
      const drv = el('b', null, d.path.replace('\\', ''));
      drv.setAttribute('translate', 'no');
      const lbl = el('span', null, d.label);
      lbl.setAttribute('translate', 'no');
      head.append(drv, lbl);
      const bar = el('div', 'dm-bar big' + (used > 0.9 ? ' full' : ''));
      bar.style.setProperty('--p', used);
      c.append(head, bar, el('small', null, `${fmtSize(d.free)} ${t('free of')} ${fmtSize(d.total)}`));
      c.onclick = () => scan(d.path);
      return c;
    }));
  }

  App.on('duplicates', v => render(v, { resetSelection: true }));
  App.on('job', job => {
    if (job.kind !== 'duplicates' || job.id !== scanning) return;
    if (job.status === 'running') {
      $('dupStatusText').textContent = job.text || '';
    } else {
      scanning = null;
      $('dupStatus').hidden = true;
      if (job.status === 'error') toast(job.text);
      else if (job.status === 'cancelled') toast(t('Cancelled'));
    }
  });

  App.pages.duplicates = {
    init() {
      $('dupPick').onclick = async () => {
        const f = await App.api.dup_pick();
        if (f) scan(f);
      };
      $('dupCancel').onclick = () => scanning && App.api.cancel_job(scanning);
      $('dupKeepNewest').onclick = () => autoSelect('newest');
      $('dupKeepOldest').onclick = () => autoSelect('oldest');
      $('dupKeepShortest').onclick = () => autoSelect('shortest');
      $('dupClearSel').onclick = () => {
        selected.clear();
        for (const ref of rowRefs.values()) updateRowState(ref);
        updateActionBar();
      };
      $('dupTrashBtn').onclick = () => trashSelected();
    },
    async show() {
      loadDrives();
      if (!view && !scanning) {
        const existing = await App.api.dup_view();
        if (existing) render(existing, { resetSelection: false });
      }
    },
    drop() { toast(t('Drop a folder, or pick a drive above')); },
    dropAll(data) {
      if (data && data.folders && data.folders.length) scan(data.folders[0]);
      else toast(t('Drop a folder, or pick a drive above'));
    },
  };
})();
