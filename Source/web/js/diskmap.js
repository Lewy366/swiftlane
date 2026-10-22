// Disk space map: pick a drive or folder, scan it, then click through a treemap of what takes the room.
(() => {
  let view = null, scanning = null;

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
    if (r) { scanning = r; $('diskStatus').hidden = false; $('diskStatusText').textContent = t('Starting…'); }
  }

  async function loadDrives() {
    const ds = await App.api.disk_drives();
    $('diskDrives').replaceChildren(...ds.map(d => {
      const c = el('button', 'dm-drive');
      const used = d.used / d.total;
      const head = el('div', 'dm-drive-head'); head.append(el('b', null, d.path.replace('\\', '')), el('span', null, d.label));
      const bar = el('div', 'dm-bar big' + (used > .9 ? ' full' : '')); bar.style.setProperty('--p', used);
      c.append(head, bar, el('small', null, `${fmtSize(d.free)} ${t('free of')} ${fmtSize(d.total)}`));
      c.onclick = () => scan(d.path);
      return c;
    }));
  }

  App.on('disk', render);
  App.on('job', job => {
    if (job.kind !== 'disk' || job.id !== scanning) return;
    if (job.status === 'running') $('diskStatusText').textContent = job.text || '';
    else { scanning = null; $('diskStatus').hidden = true; if (job.status === 'error') toast(job.text); }
  });

  App.pages.diskmap = {
    init() {
      $('diskFolder').onclick = async () => { const f = await App.api.disk_pick(); if (f) scan(f); };
      $('diskCancel').onclick = () => scanning && App.api.cancel_job(scanning);
      $('diskUp').onclick = () => view && view.crumbs.length > 1 && open(view.crumbs[view.crumbs.length - 2].path);
      $('diskRescan').onclick = () => view && scan(view.root);
      new ResizeObserver(() => drawMap()).observe($('diskMap'));
    },
    show() { loadDrives(); },
    drop(files) { toast(t('Drop a folder, or pick a drive above')); },
    dropAll(data) { if (data.folders && data.folders.length) scan(data.folders[0]); },
  };
})();
