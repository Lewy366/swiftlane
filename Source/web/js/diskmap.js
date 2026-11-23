// Disk space map: pick a drive or folder, scan it, then click through a treemap of what takes the room.
(() => {
  const ADV_LABELS = {
    safe: 'Safe to remove',
    consider: 'Consider removing',
    keep: 'Should stay',
  };
  const ADV_BADGES = {
    safe: 'Safe',
    consider: 'Consider',
    keep: 'Keep',
  };

  let view = null, scanning = null, advTab = 'safe';

  function setBusy(jobId) {
    scanning = jobId || null;
    const busy = !!scanning;
    const status = $('diskStatus');
    if (status) status.hidden = !busy;
    const folderBtn = $('diskFolder');
    if (folderBtn) folderBtn.disabled = busy;
    const mftBtn = $('diskMftBtn');
    const mftSel = $('diskMftDrive');
    if (mftBtn) mftBtn.disabled = busy || !(mftSel && mftSel.options.length);
    if (busy && $('diskStatusText')) $('diskStatusText').textContent = t('Starting…');
  }

  // Squarified treemap: lays out sizes (biggest first) in a w×h box, rows that keep tiles close to square.
  function squarify(items, x, y, w, h) {
    const total = items.reduce((a, i) => a + i.size, 0), out = [];
    if (!total || w <= 0 || h <= 0) return out;
    const scale = w * h / total;
    let rest = items.map(i => ({ item: i, area: i.size * scale }));
    while (rest.length) {
      const side = Math.min(w, h);
      let row = [rest[0]], worst = ratio(row, side), i = 1;
      while (i < rest.length) {
        const next = ratio([...row, rest[i]], side);
        if (next > worst) break;
        row.push(rest[i]); worst = next; i++;
      }
      const sum = row.reduce((a, r) => a + r.area, 0), thick = sum / side;
      let off = 0;
      for (const r of row) {
        const len = r.area / thick;
        out.push(w >= h ? { item: r.item, x, y: y + off, w: thick, h: len } : { item: r.item, x: x + off, y, w: len, h: thick });
        off += len;
      }
      if (w >= h) { x += thick; w -= thick; } else { y += thick; h -= thick; }
      rest = rest.slice(i);
    }
    return out;
  }
  function ratio(row, side) {
    const sum = row.reduce((a, r) => a + r.area, 0), max = Math.max(...row.map(r => r.area)), min = Math.min(...row.map(r => r.area));
    return Math.max(side * side * max / (sum * sum), sum * sum / (side * side * min));
  }

  function drawMap() {
    const box = $('diskMap'); box.replaceChildren();
    if (!view) return;
    const W = box.clientWidth, H = box.clientHeight;
    const items = view.items.filter(i => i.size > 0);
    for (const t of squarify(items, 0, 0, W, H)) {
      const i = t.item, tile = el('div', 'dm-tile dm-' + (i.kind === 'file' ? kindOf(i.name).cls : i.kind));
      Object.assign(tile.style, { left: t.x + 'px', top: t.y + 'px', width: Math.max(0, t.w - 3) + 'px', height: Math.max(0, t.h - 3) + 'px' });
      if (t.w > 70 && t.h > 34) {
        const nm = el('b', null, i.kind === 'small' ? t_(i) : i.name); nm.setAttribute('translate', i.kind === 'small' ? 'yes' : 'no');
        tile.append(nm, el('span', null, fmtSize(i.size)));
      }
      tile.title = `${i.kind === 'small' ? t_(i) : i.name} · ${fmtSize(i.size)}`;
      if (i.kind === 'dir') tile.onclick = () => open(i.path);
      else if (i.kind === 'file') tile.onclick = () => App.api.disk_reveal(i.path);
      box.append(tile);
    }
  }
  const t_ = i => i.kind === 'small' ? `${i.name} (${i.count})` : i.name;

  function itemRow(i, total) {
    const { row, sub } = fileRow(i.kind === 'small' ? t_(i) : i.name);
    if (i.kind === 'dir') row.querySelector('.kind').replaceChildren(icon('folder'));
    sub.textContent = fmtSize(i.size) + (total ? ` · ${Math.round(i.size / total * 100)}%` : '') + (i.kind === 'dir' ? ` · ${i.count.toLocaleString()} ${t('files')}` : '');
    if (i.denied) sub.textContent += ' · ' + t('some contents not readable');
    const bar = el('div', 'dm-bar'); bar.style.setProperty('--p', total ? i.size / total : 0); row.querySelector('.meta').append(bar);
    if (i.path) {
      const acts = el('div', 'acts');
      acts.append(miniBtn('open', 'Show in File Explorer', () => App.api.disk_reveal(i.path)));
      if (!i.protected) acts.append(miniBtn('trash', 'Move to the Recycle Bin', () => trash(i)));
      row.append(acts);
      if (i.kind === 'dir') { row.style.cursor = 'pointer'; row.onclick = () => open(i.path); }
    }
    return row;
  }

  function advisorRow(item, tier) {
    const { row, sub } = fileRow(item.name);
    if (item.kind === 'dir') row.querySelector('.kind').replaceChildren(icon('folder'));
    const meta = row.querySelector('.meta');
    if (meta) { meta.style.flex = '1'; meta.style.minWidth = '0'; }
    sub.textContent = `${fmtSize(item.size)} · ${t(item.reason)}`;
    sub.title = item.path || '';
    row.title = item.path || '';

    const acts = el('div', 'acts');
    Object.assign(acts.style, { display: 'flex', alignItems: 'center', gap: '6px', marginLeft: 'auto' });
    acts.append(el('span', `dm-adv-tag dm-adv-${tier}`, t(ADV_BADGES[tier] || tier)));
    if (item.path) {
      acts.append(miniBtn('open', 'Show in File Explorer', () => App.api.disk_reveal(item.path)));
      // Never offer a Recycle Bin button on `keep` items or any protected path.
      if (tier !== 'keep' && !item.protected) {
        acts.append(miniBtn('trash', 'Move to the Recycle Bin', () => trash(item)));
      }
      if (item.kind === 'dir') {
        row.style.cursor = 'pointer';
        row.onclick = () => open(item.path);
      }
    }
    row.append(acts);
    return row;
  }

  function renderAdvisor() {
    const card = $('diskAdvisorCard');
    if (!card) return;
    const advice = view && view.advice;
    const safe = (advice && Array.isArray(advice.safe)) ? advice.safe : [];
    const consider = (advice && Array.isArray(advice.consider)) ? advice.consider : [];
    const keep = (advice && Array.isArray(advice.keep)) ? advice.keep : [];
    const tiers = { safe, consider, keep };

    if (!safe.length && !consider.length && !keep.length) {
      card.hidden = true;
      return;
    }
    card.hidden = false;

    const tabs = $('diskAdvTabs');
    if (tabs) {
      tabs.querySelectorAll('button[data-v]').forEach(btn => {
        const key = btn.dataset.v;
        const list = tiers[key] || [];
        const totalBytes = list.reduce((a, x) => a + (x.size || 0), 0);
        const base = t(ADV_LABELS[key] || key);
        btn.textContent = list.length ? `${base} (${list.length} · ${fmtSize(totalBytes)})` : `${base} (0)`;
        btn.classList.toggle('on', key === advTab);
      });
    }

    const active = tiers[advTab] || [];
    const empty = $('diskAdvEmpty');
    if (empty) empty.hidden = active.length > 0;
    const listEl = $('diskAdvList');
    if (listEl) listEl.replaceChildren(...active.map(item => advisorRow(item, advTab)));
  }

  function render(v) {
    if (v !== undefined) view = v;
    $('diskResult').hidden = !view;
    if (!view) return;
    const crumbs = $('diskCrumbs'); crumbs.replaceChildren();
    view.crumbs.forEach((c, n) => {
      if (n) crumbs.append(el('span', 'sep', '›'));
      const b = el('button', 'crumb', n ? c.name : c.path); b.setAttribute('translate', 'no'); b.onclick = () => open(c.path); crumbs.append(b);
    });
    $('diskTotal').textContent = `${fmtSize(view.size)} · ${view.files.toLocaleString()} ${t('files')}`;
    $('diskUp').disabled = view.crumbs.length < 2;
    drawMap();
    renderAdvisor();
    $('diskList').replaceChildren(...view.items.slice(0, 40).map(i => itemRow(i, view.size)));
    $('diskTopCard').hidden = !view.top.length;
    $('diskTop').replaceChildren(...view.top.map(i => itemRow({ ...i, kind: 'file' }, 0)));
  }

  async function open(path) { const v = await App.api.disk_view(path); if (v) render(v); }
  async function trash(i) {
    const what = i.kind === 'dir' ? t('the folder') + ` "${i.name}" (${fmtSize(i.size)})` : `"${i.name}" (${fmtSize(i.size)})`;
    if (!confirm(t('Move') + ' ' + what + ' ' + t('to the Recycle Bin? You can restore it from there.'))) return;
    const r = await App.api.disk_trash(i.path);
    if (r && r.error) toast(r.error); else if (r) { render(r); toast(t('Moved to the Recycle Bin')); }
  }
  async function scan(path) {
    if (scanning) return;
    const r = await App.api.disk_scan(path);
    if (r && r.error) { toast(r.error); return; }
    if (r) setBusy(r);
  }
  async function scanMft() {
    if (scanning) return;
    const sel = $('diskMftDrive');
    const drive = sel ? sel.value : '';
    if (!drive) { toast(t('No NTFS drive selected')); return; }
    const r = await App.api.disk_scan_mft(drive);
    if (r && r.error) { toast(r.error); return; }
    if (r) setBusy(r);
  }

  function populateMftDrives(ds) {
    const sel = $('diskMftDrive');
    if (!sel) return;
    const prev = sel.value;
    const fixed = ds.filter(d => !d.removable);
    const choices = fixed.length ? fixed : ds;
    sel.replaceChildren();
    for (const d of choices) {
      const opt = el('option');
      const letter = String(d.path || '').replace(/[\\/]+$/, '');
      opt.value = d.path || letter;
      opt.setAttribute('translate', 'no');
      opt.textContent = d.label ? `${letter} · ${d.label}` : letter;
      sel.append(opt);
    }
    if (prev && [...sel.options].some(o => o.value === prev)) sel.value = prev;
    const mftBtn = $('diskMftBtn');
    if (mftBtn && !scanning) mftBtn.disabled = !sel.options.length;
  }

  async function loadDrives() {
    const raw = await App.api.disk_drives();
    const ds = Array.isArray(raw) ? raw : [];
    $('diskDrives').replaceChildren(...ds.map(d => {
      const c = el('button', 'dm-drive');
      const used = d.total ? d.used / d.total : 0;
      const head = el('div', 'dm-drive-head'); head.append(el('b', null, d.path.replace('\\', '')), el('span', null, d.label));
      const bar = el('div', 'dm-bar big' + (used > .9 ? ' full' : '')); bar.style.setProperty('--p', used);
      c.append(head, bar, el('small', null, `${fmtSize(d.free)} ${t('free of')} ${fmtSize(d.total)}`));
      c.onclick = () => scan(d.path);
      return c;
    }));
    populateMftDrives(ds);
  }

  App.on('disk', render);
  App.on('job', job => {
    if (job.kind !== 'disk' || job.id !== scanning) return;
    if (job.status === 'running') $('diskStatusText').textContent = job.text || '';
    else { setBusy(null); if (job.status === 'error') toast(job.text); }
  });

  App.pages.diskmap = {
    init() {
      $('diskFolder').onclick = async () => { const f = await App.api.disk_pick(); if (f) scan(f); };
      if ($('diskMftBtn')) $('diskMftBtn').onclick = scanMft;
      $('diskCancel').onclick = () => scanning && App.api.cancel_job(scanning);
      $('diskUp').onclick = () => view && view.crumbs.length > 1 && open(view.crumbs[view.crumbs.length - 2].path);
      $('diskRescan').onclick = () => view && scan(view.root);
      const tabs = $('diskAdvTabs');
      if (tabs) {
        tabs.querySelectorAll('button[data-v]').forEach(btn => {
          btn.onclick = () => {
            advTab = btn.dataset.v || 'safe';
            renderAdvisor();
          };
        });
      }
      new ResizeObserver(() => drawMap()).observe($('diskMap'));
    },
    show() { loadDrives(); },
    drop(files) { toast(t('Drop a folder, or pick a drive above')); },
    dropAll(data) { if (data.folders && data.folders.length) scan(data.folders[0]); },
  };
})();
