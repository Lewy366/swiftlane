// PDF tools: pages of one or more PDFs on a board; reorder by dragging, rotate, delete, sign, then save/split/shrink.
(() => {
  let docs = [];       // [{path, name, pages:[{thumb,w,h}]}]
  let board = [];      // [{src, page, rotate, key}] in the new order
  let sigs = [];       // [{key, png, x, y, w}]
  const selected = new Set();
  let dragKey = null;

  async function add(paths) {
    paths = paths.filter(p => /\.pdf$/i.test(p));
    if (!paths.length) { toast('Only PDF files go here'); return; }
    toast('Opening…');
    for (const d of await App.api.pdf_open(paths)) {
      if (d.error) { toast(d.name + ': ' + d.error); continue; }
      const src = docs.length; docs.push(d);
      d.pages.forEach((p, i) => board.push({ src, page: i, rotate: 0, key: src + ':' + i + ':' + Math.random().toString(36).slice(2, 7) }));
    }
    render();
  }

  function render() {
    $('pdfEmpty').hidden = board.length > 0; $('pdfWork').hidden = !board.length;
    $('pdfCount').textContent = board.length ? `${board.length} pages · ${docs.length} PDF${docs.length > 1 ? 's' : ''}` : '';
    const grid = $('pdfBoard'); grid.replaceChildren();
    board.forEach((b, i) => {
      const d = docs[b.src], p = d.pages[b.page];
      const tile = el('div', 'pdf-page' + (selected.has(b.key) ? ' on' : '')); tile.draggable = true;
      const img = el('img'); img.src = p.thumb || ''; img.style.transform = `rotate(${b.rotate}deg)`; img.alt = '';
      const cap = el('div', 'pdf-cap'); const nm = el('span', null, d.name); nm.setAttribute('translate', 'no');
      cap.append(el('b', null, String(i + 1)), nm, el('small', null, 'p. ' + (b.page + 1)));
      const acts = el('div', 'pdf-acts');
      acts.append(miniBtn('refresh', 'Rotate', () => { b.rotate = (b.rotate + 90) % 360; render(); }),
                  miniBtn('x', 'Remove this page', () => { board.splice(board.indexOf(b), 1); sigs = sigs.filter(s => s.key !== b.key); render(); }));
      tile.append(img, cap, acts);
      if (sigs.some(s => s.key === b.key)) tile.append(el('span', 'pdf-sig', 'Signed'));
      tile.onclick = e => { if (e.target.closest('.pdf-acts')) return; selected.has(b.key) ? selected.delete(b.key) : selected.add(b.key); render(); };
      tile.ondragstart = () => { dragKey = b.key; tile.classList.add('dragging-tool'); };
      tile.ondragend = () => { dragKey = null; };
      tile.ondragover = e => { if (dragKey) { e.preventDefault(); tile.classList.add('drop-here'); } };
      tile.ondragleave = () => tile.classList.remove('drop-here');
      tile.ondrop = e => {
        e.preventDefault(); e.stopPropagation(); tile.classList.remove('drop-here');
        const from = board.findIndex(x => x.key === dragKey); if (from < 0) return;
        const [m] = board.splice(from, 1); board.splice(board.indexOf(b) + (from <= i ? 1 : 0), 0, m); render();
      };
      grid.append(tile);
    });
    $('pdfSelInfo').textContent = selected.size ? selected.size + ' selected' : 'Click pages to select them';
  }

  const plan = () => board.map(b => ({ src: b.src, page: b.page, rotate: b.rotate }));
  const sigPlan = () => sigs.map(s => ({ at: board.findIndex(b => b.key === s.key), png: s.png, x: s.x, y: s.y, w: s.w })).filter(s => s.at >= 0);

  // ---------- signature: draw (or pick a picture), then click where it goes on the page
  let pad, padCtx, drawing = false, sigPng = null, sigTarget = null, sigPos = null;
  function openSign() {
    const key = [...selected][0] || (board[board.length - 1] || {}).key;
    sigTarget = board.find(b => b.key === key); if (!sigTarget) return;
    $('sigModal').hidden = false; $('sigStep2').hidden = true; $('sigStep1').hidden = false; sigPng = null;
    pad = $('sigPad'); padCtx = pad.getContext('2d'); padCtx.clearRect(0, 0, pad.width, pad.height);
    padCtx.lineWidth = 4; padCtx.lineCap = 'round'; padCtx.strokeStyle = '#111';
  }
  function padXY(e) { const r = pad.getBoundingClientRect(); return [(e.clientX - r.left) * pad.width / r.width, (e.clientY - r.top) * pad.height / r.height]; }
  async function toPlacing(png) {
    sigPng = png; $('sigStep1').hidden = true; $('sigStep2').hidden = false;
    $('sigPage').src = await App.api.pdf_page(docs[sigTarget.src].path, sigTarget.page) || '';
    $('sigPreview').src = png; sigPos = { x: .55, y: .8 }; placePreview();
  }
  function placePreview() {
    const w = +$('sigSize').value / 100, prev = $('sigPreview');
    prev.style.left = sigPos.x * 100 + '%'; prev.style.top = sigPos.y * 100 + '%'; prev.style.width = w * 100 + '%';
  }

  App.pages.pdf = {
    init() {
      $('pdfDrop').onclick = async () => add(await App.api.pdf_pick());
      $('pdfAdd').onclick = async () => add(await App.api.pdf_pick());
      $('pdfClear').onclick = () => { docs = []; board = []; sigs = []; selected.clear(); render(); };
      $('pdfRotSel').onclick = () => { board.forEach(b => { if (selected.has(b.key)) b.rotate = (b.rotate + 90) % 360; }); render(); };
      $('pdfDelSel').onclick = () => { board = board.filter(b => !selected.has(b.key)); sigs = sigs.filter(s => board.some(b => b.key === s.key)); selected.clear(); render(); };
      $('pdfSave').onclick = async () => { const r = await App.api.pdf_save(docs.map(d => d.path), plan(), sigPlan()); if (r && r.error) toast(r.error); };
      $('pdfSplit').onclick = async () => {
        if (docs.length !== 1) { toast('Split works on one PDF at a time'); return; }
        const cuts = board.map((b, i) => selected.has(b.key) ? i : -1).filter(i => i > 0);
        const groups = []; let start = 0;
        if (!cuts.length) board.forEach(b => groups.push([b.page]));        // nothing selected: every page on its own
        else { for (const c of [...cuts, board.length]) { groups.push(board.slice(start, c).map(b => b.page)); start = c; } }
        await App.api.pdf_split(docs[0].path, groups);
      };
      $('pdfSmaller').onclick = async () => {
        if (docs.length !== 1) { toast('Make smaller works on one PDF at a time'); return; }
        if (!confirm(t('This turns every page into a picture: the file gets smaller, but text can no longer be selected or searched. Continue?'))) return;
        await App.api.pdf_smaller(docs[0].path);
      };
      $('pdfSign').onclick = openSign;
      $('sigCancel').onclick = $('sigCancel2').onclick = () => $('sigModal').hidden = true;
      $('sigClearPad').onclick = () => padCtx.clearRect(0, 0, pad.width, pad.height);
      $('sigPad').onpointerdown = e => { drawing = true; padCtx.beginPath(); padCtx.moveTo(...padXY(e)); };
      $('sigPad').onpointermove = e => { if (drawing) { padCtx.lineTo(...padXY(e)); padCtx.stroke(); } };
      window.addEventListener('pointerup', () => drawing = false);
      $('sigUseDrawn').onclick = () => toPlacing(pad.toDataURL('image/png'));
      $('sigUsePicture').onclick = async () => { const r = await App.api.pick_signature(); if (r && r.error) toast(r.error); else if (r) toPlacing(r); };
      $('sigSize').oninput = placePreview;
      $('sigPlace').onclick = e => { const r = $('sigPlace').getBoundingClientRect(); sigPos = { x: (e.clientX - r.left) / r.width, y: (e.clientY - r.top) / r.height }; placePreview(); };
      $('sigAdd').onclick = () => {
        sigs.push({ key: sigTarget.key, png: sigPng, x: sigPos.x, y: sigPos.y, w: +$('sigSize').value / 100 });
        $('sigModal').hidden = true; render(); toast('Signature added · save the PDF to keep it');
      };
    },
    drop(files) { add(files.map(f => f.path)); },
  };
})();
