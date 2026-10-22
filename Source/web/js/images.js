// Image editor page: live canvas preview, crop and resize with the mouse,
// rotate/flip/straighten, colour adjustments, compression preview.
// Geometry matches toolkit/images.py: rotate 90° steps -> flip -> straighten -> crop -> resize.
(() => {
  const RATIOS = [['free', 'Free'], ['orig', 'Original'], ['1:1', '1:1'], ['4:3', '4:3'], ['3:2', '3:2'], ['16:9', '16:9'], ['9:16', '9:16'], ['4:5', '4:5']];
  const HANDLE = 9;           // handle size in CSS px
  let files = [];             // [{path, name, size}]
  let cur = null;             // {path, name, img, fw, fh, pw, ph, bytes, format}
  let ops = fresh();
  let tool = 'crop', comparing = false, exact = false;
  let drag = null;            // active mouse drag
  let renderTimer = null, lastRender = null, renderSeq = 0;
  const canvas = () => $('edCanvas');
  const accent = () => getComputedStyle(document.documentElement).getPropertyValue('--violet').trim() || '#7c5cff';

  function fresh() {
    return { rotate: 0, flipH: false, flipV: false, straighten: 0, crop: null, ratio: 'free', scale: 1,
             adjust: { brightness: 1, contrast: 1, saturation: 1, sharpness: 1 }, format: 'keep', quality: 90, strip: false };
  }

  // ------------------------------------------------------------ geometry (full-resolution pixels)
  function straightenScale(deg, w, h) {
    const a = Math.abs(deg) * Math.PI / 180;
    if (!a) return 1;
    return Math.max((w * Math.cos(a) + h * Math.sin(a)) / w, (w * Math.sin(a) + h * Math.cos(a)) / h);
  }
  function geometry() {
    const rot = ops.rotate % 180 !== 0;
    const RW = rot ? cur.fh : cur.fw, RH = rot ? cur.fw : cur.fh;
    const s = straightenScale(ops.straighten, RW, RH);
    const TW = RW / s, TH = RH / s;                          // image after straighten (the "window")
    const c = ops.crop || { x: 0, y: 0, w: 1, h: 1 };
    const cropW = Math.max(1, Math.round(TW * c.w)), cropH = Math.max(1, Math.round(TH * c.h));
    const outW = Math.max(1, Math.round(cropW * ops.scale)), outH = Math.max(1, Math.round(cropH * ops.scale));
    return { RW, RH, s, TW, TH, c, cropW, cropH, outW, outH };
  }
  function ratioValue(g) {  // wanted crop ratio in pixels (w/h), or null for free
    if (ops.ratio === 'free') return null;
    if (ops.ratio === 'orig') return g.RW / g.RH;
    const [a, b] = ops.ratio.split(':').map(Number); return a / b;
  }

  // ------------------------------------------------------------ drawing
  function layout(g) {  // where the straightened image sits on the canvas (CSS px)
    const cv = canvas(), W = cv.clientWidth, H = cv.clientHeight, m = 24;
    const k = Math.min((W - 2 * m) / g.TW, (H - 2 * m) / g.TH);
    const w = g.TW * k, h = g.TH * k;
    return { k, x: (W - w) / 2, y: (H - h) / 2, w, h, W, H };
  }
  function cropRect(L, g) { return { x: L.x + g.c.x * L.w, y: L.y + g.c.y * L.h, w: g.c.w * L.w, h: g.c.h * L.h }; }

  function drawImage(ctx, L, g) {
    // full-res pixel -> canvas: scale L.k; the preview image is smaller than full res by pw/fw
    const q = cur.pw / cur.fw, k = L.k / q;
    ctx.save();
    ctx.beginPath(); ctx.rect(L.x, L.y, L.w, L.h); ctx.clip();
    ctx.translate(L.x + L.w / 2, L.y + L.h / 2);
    ctx.rotate(ops.straighten * Math.PI / 180);
    ctx.scale(ops.flipH ? -1 : 1, ops.flipV ? -1 : 1);
    ctx.rotate(ops.rotate * Math.PI / 180);
    ctx.filter = `brightness(${ops.adjust.brightness}) contrast(${ops.adjust.contrast}) saturate(${ops.adjust.saturation})`;
    ctx.drawImage(cur.img, -cur.pw * k / 2, -cur.ph * k / 2, cur.pw * k, cur.ph * k);
    ctx.restore();
  }

  function draw() {
    const cv = canvas(), dpr = window.devicePixelRatio || 1;
    cv.width = cv.clientWidth * dpr; cv.height = cv.clientHeight * dpr;
    const ctx = cv.getContext('2d'); ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cv.clientWidth, cv.clientHeight);
    if (!cur) return;
    if (comparing) {  // the untouched original, fitted
      const W = cv.clientWidth - 48, H = cv.clientHeight - 48, k = Math.min(W / cur.pw, H / cur.ph);
      ctx.drawImage(cur.img, (cv.clientWidth - cur.pw * k) / 2, (cv.clientHeight - cur.ph * k) / 2, cur.pw * k, cur.ph * k);
      label(ctx, 'Original', 16, 26);
      return;
    }
    const g = geometry(), L = layout(g), C = cropRect(L, g);
    if (tool === 'resize') return drawResize(ctx, L, g, C);
    drawImage(ctx, L, g);
    // dim everything outside the crop box
    ctx.fillStyle = 'rgba(5,5,12,.62)';
    ctx.beginPath(); ctx.rect(L.x, L.y, L.w, L.h); ctx.rect(C.x, C.y, C.w, C.h); ctx.fill('evenodd');
    // rule of thirds while dragging
    if (drag) {
      ctx.strokeStyle = 'rgba(255,255,255,.35)'; ctx.lineWidth = 1; ctx.beginPath();
      for (const t of [1 / 3, 2 / 3]) { ctx.moveTo(C.x + C.w * t, C.y); ctx.lineTo(C.x + C.w * t, C.y + C.h); ctx.moveTo(C.x, C.y + C.h * t); ctx.lineTo(C.x + C.w, C.y + C.h * t); }
      ctx.stroke();
    }
    ctx.strokeStyle = '#fff'; ctx.lineWidth = 1.5; ctx.strokeRect(C.x, C.y, C.w, C.h);
    for (const [hx, hy] of handles(C)) {
      ctx.fillStyle = '#fff'; ctx.fillRect(hx - HANDLE / 2, hy - HANDLE / 2, HANDLE, HANDLE);
      ctx.strokeStyle = accent(); ctx.strokeRect(hx - HANDLE / 2, hy - HANDLE / 2, HANDLE, HANDLE);
    }
    if (ops.crop) label(ctx, `${g.cropW} × ${g.cropH}`, C.x + 6, C.y + 20);
  }

  function resizeRect(L, g, C) {  // the cropped picture shown at its new size (fits the stage at most)
    const maxK = Math.min((L.W - 48) / C.w, (L.H - 48) / C.h);
    const k = Math.min(ops.scale, maxK);
    const w = C.w * k, h = C.h * k;
    return { x: (L.W - w) / 2, y: (L.H - h) / 2, w, h, k };
  }
  function drawResize(ctx, L, g, C) {
    const R = resizeRect(L, g, C);
    ctx.save();
    ctx.translate(R.x, R.y); ctx.scale(R.k, R.k); ctx.translate(-C.x, -C.y);
    ctx.beginPath(); ctx.rect(C.x, C.y, C.w, C.h); ctx.clip();
    drawImage(ctx, L, g);
    ctx.restore();
    ctx.strokeStyle = 'rgba(255,255,255,.8)'; ctx.setLineDash([5, 4]); ctx.strokeRect(R.x, R.y, R.w, R.h); ctx.setLineDash([]);
    const hx = R.x + R.w, hy = R.y + R.h;
    ctx.fillStyle = '#fff'; ctx.beginPath(); ctx.arc(hx, hy, 8, 0, 2 * Math.PI); ctx.fill();
    ctx.strokeStyle = accent(); ctx.lineWidth = 2; ctx.stroke();
    label(ctx, `${g.outW} × ${g.outH} px · ${Math.round(ops.scale * 100)}%`, R.x + 6, R.y + 20);
  }

  function label(ctx, text, x, y) {
    ctx.font = '600 12px "Segoe UI", sans-serif';
    const w = ctx.measureText(text).width;
    ctx.fillStyle = 'rgba(0,0,0,.6)'; ctx.fillRect(x - 4, y - 14, w + 8, 19);
    ctx.fillStyle = '#fff'; ctx.fillText(text, x, y);
  }
  function handles(C) {
    const xs = [C.x, C.x + C.w / 2, C.x + C.w], ys = [C.y, C.y + C.h / 2, C.y + C.h];
    return [[xs[0], ys[0], 'nw'], [xs[1], ys[0], 'n'], [xs[2], ys[0], 'ne'], [xs[2], ys[1], 'e'],
            [xs[2], ys[2], 'se'], [xs[1], ys[2], 's'], [xs[0], ys[2], 'sw'], [xs[0], ys[1], 'w']];
  }

  // ------------------------------------------------------------ mouse
  function pos(e) { const r = canvas().getBoundingClientRect(); return { x: e.clientX - r.left, y: e.clientY - r.top }; }
  function hit(p, L, g) {
    if (tool === 'resize') {
      const R = resizeRect(L, g, cropRect(L, g));
      return Math.hypot(p.x - (R.x + R.w), p.y - (R.y + R.h)) < 14 ? 'scale' : null;
    }
    const C = cropRect(L, g);
    for (const [hx, hy, name] of handles(C)) if (Math.abs(p.x - hx) < HANDLE + 3 && Math.abs(p.y - hy) < HANDLE + 3) return name;
    // Nothing cropped yet: dragging over the photo draws a new box (a full-size box can't move anyway).
    if (p.x > C.x && p.x < C.x + C.w && p.y > C.y && p.y < C.y + C.h) return ops.crop ? 'move' : 'new';
    if (p.x > L.x && p.x < L.x + L.w && p.y > L.y && p.y < L.y + L.h) return 'new';
    return null;
  }
  const CURSORS = { nw: 'nwse-resize', se: 'nwse-resize', ne: 'nesw-resize', sw: 'nesw-resize', n: 'ns-resize', s: 'ns-resize',
                    e: 'ew-resize', w: 'ew-resize', move: 'move', new: 'crosshair', scale: 'nwse-resize' };

  function onDown(e) {
    if (!cur || comparing) return;
    const g = geometry(), L = layout(g), p = pos(e), what = hit(p, L, g);
    if (!what) return;
    canvas().setPointerCapture(e.pointerId);
    drag = { what, start: p, crop: { ...g.c }, scale: ops.scale, L, g };
    if (what === 'new') drag.anchor = { x: (p.x - L.x) / L.w, y: (p.y - L.y) / L.h };
  }
  function onMove(e) {
    if (!cur) return;
    const p = pos(e);
    if (!drag) {
      const g = geometry(); canvas().style.cursor = CURSORS[hit(p, layout(g), g)] || 'default';
      return;
    }
    const { L, g } = drag;
    if (drag.what === 'scale') {
      // The picture is drawn centred at C × scale, so the corner's distance from the centre gives the scale.
      const C = cropRect(L, g), maxK = Math.min((L.W - 48) / C.w, (L.H - 48) / C.h);
      const f = Math.max(2 * (p.x - L.W / 2) / C.w, 2 * (p.y - L.H / 2) / C.h);
      ops.scale = clamp(f, 0.01, Math.max(maxK, 1));  // bigger than the stage: use the % slider or W/H
      syncInputs(); draw(); return;
    }
    const dx = (p.x - drag.start.x) / L.w, dy = (p.y - drag.start.y) / L.h;
    let c = { ...drag.crop };
    const ratio = ratioValue(g), fr = ratio ? ratio * g.TH / g.TW : null;  // ratio in fractions
    if (drag.what === 'move') {
      c.x = clamp(c.x + dx, 0, 1 - c.w); c.y = clamp(c.y + dy, 0, 1 - c.h);
    } else if (drag.what === 'new') {
      const a = drag.anchor, b = { x: clamp((p.x - L.x) / L.w, 0, 1), y: clamp((p.y - L.y) / L.h, 0, 1) };
      let w = Math.abs(b.x - a.x), h = Math.abs(b.y - a.y);
      if (fr) { if (w / h > fr) w = h * fr; else h = w / fr; }
      c = { x: b.x < a.x ? a.x - w : a.x, y: b.y < a.y ? a.y - h : a.y, w, h };
      c = fitInside(c);
    } else {
      const d = drag.what;
      let { x, y, w, h } = c;
      if (d.includes('w')) { x = clamp(c.x + dx, 0, c.x + c.w - 0.02); w = c.x + c.w - x; }
      if (d.includes('e')) { w = clamp(c.w + dx, 0.02, 1 - c.x); }
      if (d.includes('n')) { y = clamp(c.y + dy, 0, c.y + c.h - 0.02); h = c.y + c.h - y; }
      if (d.includes('s')) { h = clamp(c.h + dy, 0.02, 1 - c.y); }
      if (fr) {  // keep the ratio: the dragged side leads, the other follows
        if (d === 'n' || d === 's') { const nw = h * fr; x = clamp(x + (w - nw) / 2, 0, 1 - nw); w = nw; }
        else { const nh = w / fr; if (d.includes('n')) y = y + h - nh; h = nh; }
      }
      c = fitInside({ x, y, w, h });
    }
    if (c.w > 0.005 && c.h > 0.005) ops.crop = c;
    syncInputs(); draw();
  }
  function onUp() {
    if (!drag) return;
    drag = null;
    if (ops.crop && ops.crop.w > 0.999 && ops.crop.h > 0.999) ops.crop = null;
    draw(); scheduleRender();
  }
  function clamp(v, lo, hi) { return Math.min(hi, Math.max(lo, v)); }
  function fitInside(c) {
    c.w = Math.min(c.w, 1); c.h = Math.min(c.h, 1);
    c.x = clamp(c.x, 0, 1 - c.w); c.y = clamp(c.y, 0, 1 - c.h);
    return c;
  }
  function applyRatio() {  // largest centred box with the chosen ratio
    const g = geometry(), r = ratioValue(g);
    if (!r) return;
    const fr = r * g.TH / g.TW;
    ops.crop = fr > 1 ? { x: 0, y: (1 - 1 / fr) / 2, w: 1, h: 1 / fr } : { x: (1 - fr) / 2, y: 0, w: fr, h: 1 };
    if (ops.crop.w > 0.999 && ops.crop.h > 0.999) ops.crop = null;
  }

  // ------------------------------------------------------------ panel
  function syncInputs() {
    if (!cur) return;
    const g = geometry();
    if (document.activeElement !== $('edW')) $('edW').value = g.outW;
    if (document.activeElement !== $('edH')) $('edH').value = g.outH;
    $('edScale').value = Math.min(200, Math.round(ops.scale * 100));
    $('edScaleVal').textContent = Math.round(ops.scale * 100) + '%';
    info(g);
  }
  function info(g, est) {
    const parts = [`<span><b>${cur.fw} × ${cur.fh}</b> → <b>${g.outW} × ${g.outH}</b></span>`];
    const r = lastRender;
    if (r && r.width === g.outW && r.height === g.outH) parts.push(`<span>${r.format} · ≈ <b>${fmtSize(r.estimate)}</b> <span class="muted">(was ${fmtSize(cur.bytes)})</span></span>`);
    else parts.push('<span class="muted">estimating size…</span>');
    $('edInfo').innerHTML = parts.join('');
  }
  function pythonOps(forBatch) {
    const o = {
      rotate: ops.rotate, flip_h: ops.flipH, flip_v: ops.flipV, straighten: ops.straighten,
      adjust: ops.adjust, format: ops.format, quality: ops.quality, strip_metadata: ops.strip,
    };
    if (ops.crop) o.crop_box = [ops.crop.x, ops.crop.y, ops.crop.w, ops.crop.h];
    if (Math.abs(ops.scale - 1) > 1e-4) o.resize = { mode: 'percent', value: ops.scale * 100 };
    return o;
  }
  function scheduleRender() {
    clearTimeout(renderTimer);
    renderTimer = setTimeout(async () => {
      if (!cur) return;
      const seq = ++renderSeq, path = cur.path;
      const r = await App.api.image_render(path, pythonOps());
      if (seq !== renderSeq || !cur || cur.path !== path) return;   // a newer change superseded this one
      if (r.error) { $('edInfo').textContent = r.error; return; }
      lastRender = r;
      if (exact) { $('edExact').src = r.data; }
      info(geometry());
    }, 220);
  }
  function changed() { syncInputs(); draw(); scheduleRender(); }

  // ------------------------------------------------------------ files
  async function add(newFiles) {
    const imgs = newFiles.filter(f => f.kind === 'image');
    if (imgs.length < newFiles.length) toast('Only images can be edited here');
    for (const f of imgs) if (!files.some(x => x.path === f.path)) files.push(f);
    renderThumbs();
    if (!cur && files.length) select(files[0]);
  }
  async function select(f) {
    const p = await App.api.image_preview(f.path);
    if (p.error) { toast(p.error); return; }
    const img = new Image();
    img.onload = () => {
      const keepOps = cur !== null;  // switching images keeps the current edits (they apply to all)
      cur = { path: f.path, name: f.name, img, fw: p.width, fh: p.height, pw: img.naturalWidth, ph: img.naturalHeight, bytes: p.bytes, format: p.format };
      if (!keepOps) ops = fresh();
      lastRender = null;
      $('edEmpty').hidden = true;
      $('edSave').disabled = false;
      renderThumbs(); changed();
    };
    img.src = p.data;
  }
  function renderThumbs() {
    const t = $('edThumbs'); t.replaceChildren();
    for (const f of files) {
      const d = el('div', 'ed-thumb' + (cur && cur.path === f.path ? ' on' : ''));
      d.append(el('span', null, f.name), miniBtn('x', 'Remove', () => remove(f)));
      d.onclick = () => select(f);
      t.append(d);
    }
    if (files.length) {
      const addBtn = el('button', 'btn ghost small', '+ Add'); addBtn.onclick = browse; t.append(addBtn);
    }
    $('edSaveAll').hidden = files.length < 2;
    $('edSaveAll').textContent = `Apply to all ${files.length} images`;
    $('edSave').textContent = files.length > 1 ? 'Save this image' : 'Save image';
  }
  function remove(f) {
    files = files.filter(x => x !== f);
    if (cur && cur.path === f.path) {
      cur = null;
      if (files.length) select(files[0]);
      else { $('edEmpty').hidden = false; $('edSave').disabled = true; $('edInfo').textContent = ''; draw(); }
    }
    renderThumbs();
  }
  async function browse() { add(await App.api.pick_files()); }

  async function save(all) {
    if (!cur) return;
    const paths = all ? files.map(f => f.path) : [cur.path];
    // Swiftlane asks where to save (a file for one image, a folder for several).
    const job = await App.api.edit_images(paths, pythonOps(all));
    if (job) toast(all ? `Saving ${paths.length} images…` : 'Saving…');
  }

  // ------------------------------------------------------------ wiring
  App.pages.images = {
    init() {
      const cv = canvas();
      cv.addEventListener('pointerdown', onDown);
      cv.addEventListener('pointermove', onMove);
      cv.addEventListener('pointerup', onUp);
      cv.addEventListener('pointercancel', onUp);
      cv.addEventListener('dblclick', () => { if (tool === 'crop') { ops.crop = null; ops.ratio = 'free'; markRatio(); changed(); } });
      new ResizeObserver(() => draw()).observe($('edStage'));
      $('edEmpty').onclick = browse;

      bindSeg('edTool', v => { tool = v; draw(); });
      $('edRotL').onclick = () => { ops.rotate = (ops.rotate + 270) % 360; ops.crop = null; applyRatio(); changed(); };
      $('edRotR').onclick = () => { ops.rotate = (ops.rotate + 90) % 360; ops.crop = null; applyRatio(); changed(); };
      $('edFlipH').onclick = () => { ops.flipH = !ops.flipH; changed(); };
      $('edFlipV').onclick = () => { ops.flipV = !ops.flipV; changed(); };
      const cmp = $('edCompare');
      const startCompare = () => { comparing = true; draw(); }, stopCompare = () => { if (comparing) { comparing = false; draw(); } };
      cmp.addEventListener('pointerdown', startCompare); cmp.addEventListener('pointerup', stopCompare); cmp.addEventListener('pointerleave', stopCompare);
      $('edExactToggle').onchange = e => {
        exact = e.target.checked;
        $('edExact').hidden = !exact; canvas().style.visibility = exact ? 'hidden' : '';
        if (exact) { if (lastRender) $('edExact').src = lastRender.data; scheduleRender(); }
      };

      const ratios = $('edRatios');
      for (const [v, labelText] of RATIOS) {
        const c = el('button', 'chip' + (v === 'free' ? ' on' : ''), labelText); c.dataset.v = v;
        c.onclick = () => { ops.ratio = v; markRatio(); if (v === 'free') { ops.crop = ops.crop; } else applyRatio(); changed(); };
        ratios.append(c);
      }
      $('edCropReset').onclick = () => { ops.crop = null; ops.ratio = 'free'; markRatio(); changed(); };
      $('edStraighten').oninput = e => { ops.straighten = +e.target.value; $('edStraightenVal').textContent = ops.straighten + '°'; if (ops.ratio !== 'free') applyRatio(); changed(); };
      $('edStraighten').ondblclick = e => { e.target.value = 0; e.target.oninput({ target: e.target }); };

      const fromSize = (w, h) => {
        if (!cur) return;
        const g = geometry();
        ops.scale = w ? w / g.cropW : h / g.cropH;
        changed();
      };
      $('edW').onchange = e => fromSize(+e.target.value, 0);
      $('edH').onchange = e => fromSize(0, +e.target.value);
      $('edScale').oninput = e => { ops.scale = +e.target.value / 100; changed(); };

      document.querySelectorAll('[data-adj]').forEach(r => r.oninput = () => {
        ops.adjust[r.dataset.adj] = +r.value / 100;
        r.nextElementSibling.textContent = (r.value - 100 > 0 ? '+' : '') + (r.value - 100);
        changed();
      });
      $('edAdjReset').onclick = () => {
        document.querySelectorAll('[data-adj]').forEach(r => { r.value = 100; r.nextElementSibling.textContent = '0'; ops.adjust[r.dataset.adj] = 1; });
        changed();
      };
      bindSeg('edFormat', v => { ops.format = v; scheduleRender(); });
      $('edQuality').oninput = e => { ops.quality = +e.target.value; $('edQualityVal').textContent = e.target.value; scheduleRender(); };
      $('edStrip').onchange = e => { ops.strip = e.target.checked; scheduleRender(); };
      $('edSave').onclick = () => save(false);
      $('edSaveAll').onclick = () => save(true);
    },
    show() { setTimeout(draw); },
    drop: add,
  };
  function markRatio() { $('edRatios').querySelectorAll('.chip').forEach(c => c.classList.toggle('on', c.dataset.v === ops.ratio)); }
  App.imageEditor = { get ops() { return ops; }, get cur() { return cur; }, geometry: () => cur && geometry(), pythonOps, draw };  // for tests
})();
