// Shrink to size: pick a limit (Discord, WhatsApp, email or your own), see what the quality will be
// before starting, then get a file on the clipboard that is guaranteed to be under the limit.
(() => {
  let file = null, presets = [], plan = null, running = false, jobId = null, result = null, planSeq = 0;
  const mbText = n => (n / 1e6).toFixed(n < 1e7 ? 2 : 1).replace(/\.?0+$/, '') + ' MB';  // MB = 1,000,000 bytes, like the limits themselves
  const lenText = s => { s = Math.round(s); return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0'); };
  const limitMB = () => +$('shrinkMB').value || 0;

  async function open(path) {
    const r = await App.api.shrink_open(path);
    if (r.error) { toast(r.error); return; }
    file = r; result = null;
    $('shrinkEmpty').hidden = true; $('shrinkWork').hidden = false; $('shrinkResult').hidden = true;
    $('shrinkName').textContent = r.name;
    $('shrinkMeta').textContent = [r.kind === 'video' ? lenText(r.duration) : '', r.width ? r.width + '×' + r.height : '', mbText(r.bytes)].filter(Boolean).join(' · ');
    refresh();
  }

  async function refresh() {
    if (!file) return;
    const seq = ++planSeq;
    const p = await App.api.shrink_plan(file, limitMB());
    if (seq !== planSeq) return;
    plan = p;
    const info = $('shrinkPlan'), warn = $('shrinkWarn');
    warn.hidden = true; $('shrinkGo').hidden = running; $('shrinkGo').disabled = !!p.error;
    if (p.error) { info.textContent = p.error; return; }
    if (p.fits) { info.textContent = `Already under ${mbText(p.target_bytes)}, so Swiftlane passes it on unchanged.`; return; }
    if (file.kind === 'image') { info.textContent = 'Saved as a JPEG. Swiftlane lowers the quality first and the size only if it has to.'; return; }
    info.textContent = p.downscaled
      ? `Video at about ${p.video_kbps} kbps, ${p.out_width}×${p.out_height} (smaller than the original so it keeps looking sharp).`
      : `Video at about ${p.video_kbps} kbps, ${p.out_width}×${p.out_height}.`;
    if (p.quality === 'poor' || p.quality === 'bad') {
      warn.hidden = false; warn.dataset.level = p.quality; $('shrinkGo').hidden = true;
      $('shrinkWarnText').replaceChildren(el('div', null, `At ${mbText(p.target_bytes)} this ${lenText(file.duration)} video will look ${p.quality === 'bad' ? 'very rough' : 'soft'}.`),
        ...(p.good_seconds ? [el('div', 'muted small', `Trimming it to about ${lenText(p.good_seconds)} would keep it sharp.`)] : []));
    }
  }

  function setRunning(on) {
    running = on;
    $('shrinkGo').hidden = on || !$('shrinkWarn').hidden; $('shrinkCancel').hidden = !on;
    $('shrinkProg').hidden = !on; $('shrinkMB').disabled = on; $('shrinkOther').disabled = on;
    $('shrinkPreset').querySelectorAll('button').forEach(b => b.disabled = on);
    if (!on) $('shrinkFill').style.width = '0';
  }

  async function go() {
    if (!file || running) return;
    $('shrinkResult').hidden = true;
    setRunning(true); $('shrinkProgText').textContent = 'Starting…';
    const r = await App.api.shrink_start(file.path, limitMB());
    if (r && r.error) { setRunning(false); toast(r.error); return; }
    jobId = r;
  }

  App.on('job', j => {
    if (j.kind !== 'shrink' || !running) return;
    if (j.status === 'running') {
      if (j.progress != null) $('shrinkFill').style.width = Math.round(j.progress * 100) + '%';
      if (j.text) $('shrinkProgText').textContent = j.text;
    } else {
      setRunning(false);
      if (j.status === 'error') toast(j.text || 'Shrinking failed.');
      else if (j.status === 'cancelled') toast('Cancelled');
    }
  });
  App.on('shrink-done', r => {
    result = r;
    $('shrinkResult').hidden = false;
    $('shrinkResTitle').textContent = r.copied ? 'Copied: press Ctrl+V to paste' : 'Ready';
    $('shrinkResText').textContent = r.bytes === r.orig_bytes
      ? `${mbText(r.bytes)}, already under the ${mbText(r.target_bytes)} limit, so nothing was changed.`
      : `${mbText(r.bytes)}, under the ${mbText(r.target_bytes)} limit (it was ${mbText(r.orig_bytes)}).`;
  });

  App.pages.shrink = {
    async init() {
      const pick = async () => { const f = await App.api.pick_files(); if (f[0]) App.pages.shrink.drop(f); };
      $('shrinkDrop').onclick = pick; $('shrinkOther').onclick = pick;
      const p = await App.api.shrink_presets();
      presets = p.presets || [];
      $('shrinkMB').min = p.min_mb; $('shrinkMB').max = p.max_mb;
      const seg = $('shrinkPreset');
      presets.forEach((x, i) => { const b = el('button', i === 0 ? 'on' : '', `${x.name} (${x.mb} MB)`); b.dataset.v = x.mb; seg.append(b); });
      const custom = el('button', '', 'Custom'); custom.dataset.v = 'custom'; seg.append(custom);
      if (presets[0]) $('shrinkMB').value = presets[0].mb;
      bindSeg('shrinkPreset', v => { if (v !== 'custom') $('shrinkMB').value = v; else $('shrinkMB').focus(); refresh(); });
      $('shrinkMB').oninput = () => {
        const match = presets.find(x => x.mb === limitMB());
        seg.querySelectorAll('button').forEach(b => b.classList.toggle('on', match ? +b.dataset.v === match.mb : b.dataset.v === 'custom'));
        refresh();
      };
      $('shrinkGo').onclick = go; $('shrinkAnyway').onclick = go;
      $('shrinkCancel').onclick = () => { if (jobId != null) App.api.cancel_job(jobId); };
      $('shrinkTrim').onclick = () => { showPage('trimmer'); App.pages.trimmer.drop([{ path: file.path, kind: 'video' }]); };
      $('shrinkCopy').onclick = async () => { const r = await App.api.shrink_copy(result.path); toast(r.error || 'Copied: press Ctrl+V to paste'); };
      $('shrinkSaveAs').onclick = async () => { const r = await App.api.shrink_save_as(result.path, file ? file.path : ''); if (r && r.error) toast(r.error); else if (r) toast('Saved'); };
      $('shrinkShow').onclick = () => App.api.reveal_path(result.path);
    },
    drop(files) {
      const f = files.find(x => x.kind === 'image' || x.kind === 'video');
      if (!f) { toast('Shrink to size works on videos and pictures.'); return; }
      if (running) { toast('Wait for the current file, or cancel it first.'); return; }
      open(f.path);
    },
  };
})();
