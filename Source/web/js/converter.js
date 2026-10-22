// File converter page.
(() => {
  let files = [];          // [{path, name, size, kind, targets, target}]
  const DEFAULT = { image: 'jpg', video: 'mp4', audio: 'mp3', pdf: 'png' };

  function add(newFiles) {
    let skipped = 0;
    for (const f of newFiles) {
      if (!f.kind) { skipped++; continue; }
      if (files.some(x => x.path === f.path)) continue;
      const target = f.targets.includes(DEFAULT[f.kind]) && !f.name.toLowerCase().endsWith('.' + DEFAULT[f.kind]) ? DEFAULT[f.kind]
        : f.targets.find(t => !f.name.toLowerCase().endsWith('.' + t));
      files.push({ ...f, target });
    }
    if (skipped) toast(skipped + ' file(s) skipped: that type can’t be converted');
    render();
  }

  function render() {
    $('convFilesCard').hidden = !files.length;
    $('convCount').textContent = files.length || '';
    const list = $('convList'); list.replaceChildren();
    for (const f of files) {
      const ui = fileRow(f.name);
      ui.sub.textContent = f.kind[0].toUpperCase() + f.kind.slice(1) + ' · ' + fmtSize(f.size);
      const sel = el('select', 'input'); sel.style.width = '110px';
      for (const t of f.targets) { const o = el('option', null, '→ ' + t.toUpperCase()); o.value = t; sel.append(o); }
      sel.value = f.target; sel.onchange = () => { f.target = sel.value; renderOptions(); };
      ui.row.append(sel, miniBtn('x', 'Remove', () => { files = files.filter(x => x !== f); render(); }));
      list.append(ui.row);
    }
    // "Convert all to": formats every file can become
    const common = files.length ? files.map(f => f.targets).reduce((a, b) => a.filter(t => b.includes(t))) : [];
    const chips = $('convAll'); chips.replaceChildren();
    for (const t of common) {
      const c = el('button', 'chip' + (files.every(f => f.target === t) ? ' on' : ''), t.toUpperCase());
      c.onclick = () => { files.forEach(f => f.target = t); render(); };
      chips.append(c);
    }
    $('convAllWrap').hidden = common.length === 0;
    renderOptions();
  }

  function renderOptions() {
    const imagesToPdf = files.filter(f => f.kind === 'image' && f.target === 'pdf').length;
    $('convCombineWrap').hidden = imagesToPdf < 2;
    $('convGo').textContent = files.length === 1 ? 'Convert' : `Convert ${files.length} files`;
  }

  App.pages.converter = {
    init() {
      $('convDrop').onclick = async () => add(await App.api.pick_files());
      $('convClear').onclick = () => { files = []; render(); };
      bindSeg('convQuality');
      $('convGo').onclick = async () => {
        if (!files.length) return;
        const items = files.map(f => ({ path: f.path, target: f.target }));
        // Swiftlane asks where to save (file name for one file, a folder for several); Cancel keeps the list.
        const jobs = await App.api.convert(items, { quality: segValue('convQuality'), combine_pdf: $('convCombine').checked });
        if (!jobs) return;
        files = []; render();
      };
    },
    drop: add,
  };
})();
