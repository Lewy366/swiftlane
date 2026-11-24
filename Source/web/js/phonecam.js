// Phone camera: photos taken with the phone page's Camera / Scan button wait here. "Use as photo" saves one,
// "Scan document" lets you drag four corners onto a page, Swiftlane (Rust) flattens it, and the pages can be
// saved as JPG or as one PDF. Corners are never applied on their own: they only move when you move them.
(() => {
  const HANDLE = 11, GRAB = 30, PAD = 26, LOUPE = 58, ZOOM = 2.6;   // big grab area for fingers and pens
  const START = [[0.04, 0.04], [0.96, 0.04], [0.96, 0.96], [0.04, 0.96]];
  let captures = [], tray = [], cur = null, sel = 0, drag = -1, pointer = null, dragFrom = -1;
  let view = null, previewTimer = null, previewSeq = 0, lastPreview = null;
  const specOf = p => ({ id: p.id, corners: p.corners, rotate: p.rotate, look: p.look });
  const accent = () => getComputedStyle(document.documentElement).getPropertyValue('--accent').trim() || '#3b9eff';

  // ---------- the waiting photos
  function renderList() {
    const grid = $('camGrid'); grid.replaceChildren();
    for (const c of captures) {
      const card = el('div', 'pdf-page'); card.style.cursor = 'default';
      const img = el('img'); img.alt = ''; if (c.thumb) img.src = c.thumb; img.style.aspectRatio = c.w && c.h ? c.w + ' / ' + c.h : '1 / 1.35'; img.style.maxHeight = '190px';
      const acts = el('div', 'pdf-acts'); acts.style.opacity = 1;
      acts.append(miniBtn('x', 'Remove this photo', async () => { captures = await App.api.cam_remove(c.id); renderList(); }));
      const cap = el('div', 'pdf-cap'); cap.append(el('span', null, c.name), el('small', null, fmtSize(c.size)));
      const row = el('div', 'pdf-bar'); row.style.marginTop = '8px';
      const use = el('button', 'btn small', 'Use as photo'), scan = el('button', 'btn primary small', 'Scan document');
      use.onclick = async () => {
        use.disabled = true; const r = await App.api.cam_use_photo(c.id); use.disabled = false;
        if (r && r.error) toast(r.error); else if (r) toast(t('Saved') + ' ' + r.saved);
      };
      scan.onclick = () => openScan(c);
      row.append(use, scan);
      card.append(img, acts, cap, row); grid.append(card);
    }
    $('camEmpty').hidden = captures.length > 0; $('camClear').hidden = !captures.length;
  }

  function renderTray() {
    const box = $('camTray'); box.replaceChildren();
    tray.forEach((p, i) => {
      const card = el('div', 'pdf-page'); card.draggable = true;
      const img = el('img'); img.alt = ''; img.src = p.preview;
      const acts = el('div', 'pdf-acts'); acts.style.opacity = 1;
      const turned = (b, deg) => { b.style.transform = `rotate(${deg}deg)`; return b; };   // the up arrow, turned to point left or right
      if (i > 0) acts.append(turned(miniBtn('up', 'Move earlier', () => moveTray(i, i - 1)), -90));
      if (i < tray.length - 1) acts.append(turned(miniBtn('up', 'Move later', () => moveTray(i, i + 1)), 90));
      acts.append(miniBtn('x', 'Remove this page', () => { tray.splice(i, 1); renderTray(); }));
      const cap = el('div', 'pdf-cap'); cap.append(el('span', null, String(i + 1)));
      card.append(img, acts, cap);
      card.ondragstart = e => { dragFrom = i; e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/x-swiftlane-scan', String(i)); };
      card.ondragover = e => { if (dragFrom >= 0) e.preventDefault(); };
      card.ondrop = e => { e.preventDefault(); e.stopPropagation(); if (dragFrom >= 0) moveTray(dragFrom, i); dragFrom = -1; };
      card.ondragend = () => { dragFrom = -1; };
      box.append(card);
    });
    $('camTrayCard').hidden = !tray.length; $('camTrayCount').textContent = tray.length || '';
  }
  function moveTray(from, to) { if (from === to) return; const [p] = tray.splice(from, 1); tray.splice(to, 0, p); renderTray(); }

  // ---------- placing the corners
  async function openScan(c) {
    toast('Opening…');
    const r = await App.api.cam_open(c.id);
    if (!r || r.error) { toast((r && r.error) || 'Swiftlane couldn\'t open this photo.'); return; }
    const img = new Image();
    img.onload = () => {
      cur = { id: r.id, img, w: r.w, h: r.h, corners: r.corners.map(p => p.slice()), rotate: 0, look: 'colour' };
      sel = 0; lastPreview = null;
      $('camListView').hidden = true; $('camEditView').hidden = false;
      $('camLook').querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === 'colour'));
      $('camResult').hidden = true; $('camResultMsg').hidden = false; $('camResultMsg').textContent = 'Placing the corners…'; $('camResultInfo').textContent = '';
      requestAnimationFrame(() => { layout(); draw(); schedulePreview(0); $('camCanvas').focus(); });
    };
    img.src = r.image;
  }
  function closeScan() { cur = null; clearTimeout(previewTimer); $('camEditView').hidden = true; $('camListView').hidden = false; }

  function layout() {
    const stage = $('camStage'), cv = $('camCanvas'), dpr = window.devicePixelRatio || 1;
    const W = stage.clientWidth, H = stage.clientHeight;
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr);
    const s = Math.min((W - 2 * PAD) / cur.w, (H - 2 * PAD) / cur.h);
    view = { W, H, dpr, s, ox: (W - cur.w * s) / 2, oy: (H - cur.h * s) / 2 };
  }
  const toScreen = c => [view.ox + c[0] * cur.w * view.s, view.oy + c[1] * cur.h * view.s];

  function draw() {
    if (!cur || !view) return;
    const cv = $('camCanvas'), g = cv.getContext('2d'), { W, H, dpr, s, ox, oy } = view, col = accent();
    g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, W, H);
    g.drawImage(cur.img, ox, oy, cur.w * s, cur.h * s);
    const pts = cur.corners.map(toScreen);
    // dim everything outside the four corners
    g.save(); g.beginPath(); g.rect(ox, oy, cur.w * s, cur.h * s);
    g.moveTo(...pts[0]); for (const p of pts.slice(1)) g.lineTo(...p); g.closePath();
    g.fillStyle = 'rgba(0,0,0,.45)'; g.fill('evenodd'); g.restore();
    g.beginPath(); g.moveTo(...pts[0]); for (const p of pts.slice(1)) g.lineTo(...p); g.closePath();
    g.lineWidth = 2; g.strokeStyle = col; g.stroke();
    pts.forEach((p, i) => {
      g.beginPath(); g.arc(p[0], p[1], HANDLE, 0, Math.PI * 2);
      g.fillStyle = i === sel ? col : 'rgba(255,255,255,.92)'; g.fill();
      g.lineWidth = 2.5; g.strokeStyle = i === sel ? '#fff' : col; g.stroke();
    });
    if (drag >= 0 && pointer) loupe(g, drag);
  }

  // A magnifier that sits away from the finger, so the corner can be placed exactly (as scanner apps do).
  function loupe(g, i) {
    const { W, s } = view;
    const left = !(pointer[0] < LOUPE * 2 + 30 && pointer[1] < LOUPE * 2 + 30);
    const cx = left ? LOUPE + 14 : W - LOUPE - 14, cy = LOUPE + 14;
    const px = s * cur.w / cur.img.naturalWidth;   // screen px per picture px
    const half = LOUPE / (ZOOM * px), sx = cur.corners[i][0] * cur.img.naturalWidth, sy = cur.corners[i][1] * cur.img.naturalHeight;
    g.save(); g.beginPath(); g.arc(cx, cy, LOUPE, 0, Math.PI * 2); g.clip();
    g.fillStyle = '#000'; g.fillRect(cx - LOUPE, cy - LOUPE, LOUPE * 2, LOUPE * 2);
    g.drawImage(cur.img, sx - half, sy - half, half * 2, half * 2, cx - LOUPE, cy - LOUPE, LOUPE * 2, LOUPE * 2);
    g.restore();
    g.beginPath(); g.arc(cx, cy, LOUPE, 0, Math.PI * 2); g.lineWidth = 3; g.strokeStyle = accent(); g.stroke();
    g.beginPath(); g.moveTo(cx - 12, cy); g.lineTo(cx + 12, cy); g.moveTo(cx, cy - 12); g.lineTo(cx, cy + 12); g.lineWidth = 1.5; g.strokeStyle = '#fff'; g.stroke();
  }

  function pos(e) { const r = $('camCanvas').getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; }
  function setCorner(i, p) {
    cur.corners[i] = [Math.min(1, Math.max(0, (p[0] - view.ox) / (cur.w * view.s))), Math.min(1, Math.max(0, (p[1] - view.oy) / (cur.h * view.s)))];
  }
  function bindCanvas() {
    const cv = $('camCanvas');
    cv.onpointerdown = e => {
      if (!cur) return;
      const p = pos(e); let best = -1, bd = GRAB;
      cur.corners.forEach((c, i) => { const q = toScreen(c), d = Math.hypot(q[0] - p[0], q[1] - p[1]); if (d < bd) { bd = d; best = i; } });
      if (best < 0) return;
      drag = best; sel = best; pointer = p; cv.setPointerCapture(e.pointerId); setCorner(best, p); draw(); e.preventDefault();
    };
    cv.onpointermove = e => {
      if (!cur) return; const p = pos(e);
      if (drag < 0) { cv.style.cursor = cur.corners.some(c => { const q = toScreen(c); return Math.hypot(q[0] - p[0], q[1] - p[1]) < GRAB; }) ? 'grab' : 'default'; return; }
      pointer = p; setCorner(drag, p); draw();
    };
    const end = () => { if (drag < 0) return; drag = -1; pointer = null; draw(); schedulePreview(150); };
    cv.onpointerup = end; cv.onpointercancel = end;
    cv.onkeydown = e => {
      if (!cur) return;
      if (/^[1-4]$/.test(e.key)) { sel = +e.key - 1; draw(); e.preventDefault(); return; }
      const d = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
      if (!d) return;
      const step = (e.shiftKey ? 10 : 1) / view.s, c = cur.corners[sel];
      c[0] = Math.min(1, Math.max(0, c[0] + d[0] * step / cur.w)); c[1] = Math.min(1, Math.max(0, c[1] + d[1] * step / cur.h));
      draw(); schedulePreview(300); e.preventDefault();
    };
    cv.title = t('Keys 1 to 4 pick a corner, the arrow keys move it (Shift for bigger steps).');
    new ResizeObserver(() => { if (cur) { layout(); draw(); } }).observe($('camStage'));
  }

  // ---------- the flattened result (drawn by Rust from the corners)
  function schedulePreview(ms) { clearTimeout(previewTimer); previewTimer = setTimeout(refreshPreview, ms); }
  async function refreshPreview() {
    if (!cur) return null;
    const seq = ++previewSeq, spec = specOf(cur);
    const r = await App.api.cam_preview(spec);
    if (seq !== previewSeq || !cur) return null;
    if (!r || r.error) {
      lastPreview = null; $('camResult').hidden = true; $('camResultMsg').hidden = false; $('camResultMsg').textContent = (r && r.error) || 'Swiftlane couldn\'t flatten this page.'; $('camResultInfo').textContent = '';
      return null;
    }
    lastPreview = r.image; $('camResult').src = r.image; $('camResult').hidden = false; $('camResultMsg').hidden = true;
    $('camResultInfo').textContent = r.w + ' × ' + r.h + ' preview';
    return r.image;
  }

  function doneSaving(r, what) {
    if (r === null || r === undefined) return;
    if (r.error) { toast(r.error); return; }
    toast(what + (r.folder ? ' · ' + r.folder : ''));
  }

  App.pages.phonecam = {
    init() {
      bindCanvas();
      bindSeg('camLook', v => { if (cur) { cur.look = v; schedulePreview(0); } });
      $('camRotL').onclick = () => { if (cur) { cur.rotate = (cur.rotate + 270) % 360; schedulePreview(0); } };
      $('camRotR').onclick = () => { if (cur) { cur.rotate = (cur.rotate + 90) % 360; schedulePreview(0); } };
      $('camReset').onclick = () => { if (cur) { cur.corners = START.map(p => p.slice()); draw(); schedulePreview(0); } };
      $('camBack').onclick = closeScan;
      $('camAddPage').onclick = async () => {
        if (!cur) return;
        const preview = await refreshPreview();
        if (!preview) { toast('Move the corners until the result looks right first.'); return; }
        tray.push({ ...specOf(cur), corners: cur.corners.map(p => p.slice()), preview });
        renderTray(); closeScan(); toast('Page added');
      };
      $('camSaveThis').onclick = async () => {
        if (!cur) return;
        const r = await App.api.cam_save_jpg([specOf(cur)]);
        doneSaving(r, t('Saved'));
      };
      $('camSavePdf').onclick = async () => doneSaving(await App.api.cam_save_pdf(tray.map(specOf), 'new'), t('PDF saved'));
      $('camAppendPdf').onclick = async () => doneSaving(await App.api.cam_save_pdf(tray.map(specOf), 'append'), t('New PDF saved'));
      $('camSaveJpgs').onclick = async () => doneSaving(await App.api.cam_save_jpg(tray.map(specOf)), t('Saved'));
      $('camTrayClear').onclick = () => { tray = []; renderTray(); };
      $('camClear').onclick = async () => { captures = await App.api.cam_clear(); renderList(); };
      $('camAddPc').onclick = async () => added(await App.api.cam_pick());
      App.on('camera-capture', e => {
        toast('New photo from your phone');
        if (!captures.some(c => c.id === e.id)) captures.push(e);
        if (App.page === 'phonecam') renderList();
      });
      window.addEventListener('resize', () => { if (cur) { layout(); draw(); } });
    },
    async show() {
      captures = await App.api.cam_list() || [];
      renderList(); renderTray();
    },
    // Pictures dropped on the window (Rust only accepts paths Windows handed it in the drop)
    async drop(files) {
      const pics = files.filter(f => f.kind === 'image');
      if (!pics.length) { toast(t('Drop pictures (JPG, PNG, WebP or HEIC) here.')); return; }
      added(await App.api.cam_add(pics.map(f => f.path)));
    },
  };

  function added(r) {
    if (!r || r.cancelled) return;
    if (Array.isArray(r.list)) { captures = r.list; renderList(); }
    if (r.errors && r.errors.length) toast(r.errors[0]);
    else if (r.added && r.added.length) toast(t('Pictures added'));
  }
})();
