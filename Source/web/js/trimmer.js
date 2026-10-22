// Video trimmer: pick start and end on a filmstrip, then cut fast, exactly, to a size, or keep the sound.
(() => {
  let video = null, frameTimer = null;
  const fmtT = s => { s = Math.max(0, s); const m = Math.floor(s / 60); return m + ':' + (s % 60).toFixed(1).padStart(4, '0'); };
  const parseT = v => { const m = String(v).trim().match(/^(?:(\d+):)?(\d+(?:\.\d+)?)$/); return m ? (+(m[1] || 0)) * 60 + +m[2] : null; };

  async function open(path) {
    toast('Opening…');
    const r = await App.api.trim_open(path);
    if (r.error) { toast(r.error); return; }
    video = r;
    $('trimEmpty').hidden = true; $('trimWork').hidden = false;
    $('trimName').textContent = r.name; $('trimMeta').textContent = `${fmtT(r.duration)} · ${r.width}×${r.height} · ${fmtSize(r.bytes)}`;
    $('trimPreview').src = r.preview || '';
    const strip = $('trimStrip'); strip.replaceChildren(...r.strip.map(src => { const i = el('img'); i.src = src || ''; i.alt = ''; return i; }));
    for (const id of ['trimStart', 'trimEnd']) { $(id).max = r.duration; $(id).step = Math.max(0.05, r.duration / 2000); }
    $('trimStart').value = 0; $('trimEnd').value = r.duration; sync('trimStart');
  }

  function sync(changed) {
    let a = +$('trimStart').value, b = +$('trimEnd').value;
    if (a > b - 0.1) { if (changed === 'trimStart') a = Math.max(0, b - 0.1); else b = Math.min(video.duration, a + 0.1); }
    $('trimStart').value = a; $('trimEnd').value = b;
    $('trimStartT').value = fmtT(a); $('trimEndT').value = fmtT(b);
    $('trimSel').style.left = (a / video.duration * 100) + '%'; $('trimSel').style.width = ((b - a) / video.duration * 100) + '%';
    $('trimLen').textContent = 'Clip: ' + fmtT(b - a);
    clearTimeout(frameTimer);
    frameTimer = setTimeout(async () => { const f = await App.api.trim_frame(video.path, changed === 'trimEnd' ? b : a); if (f) $('trimPreview').src = f; }, 180);
    estimate();
  }
  function estimate() {
    const mode = segValue('trimMode'), len = +$('trimEnd').value - +$('trimStart').value;
    $('trimTarget').hidden = mode !== 'size';
    const hint = { fast: 'Instant and no quality loss. The clip may start up to a few seconds earlier (at the nearest keyframe).',
                   exact: 'Cuts exactly where you chose. Takes a moment; quality stays high.',
                   size: 'Squeezes the clip to fit the size, lowering quality only as much as needed.',
                   audio: 'Keeps only the sound, as an MP3.' }[mode];
    $('trimHint').textContent = hint;
    if (mode === 'fast' && video) $('trimEst').textContent = '≈ ' + fmtSize(video.bytes * len / video.duration);
    else $('trimEst').textContent = '';
  }

  App.pages.trimmer = {
    init() {
      $('trimDrop').onclick = async () => { const f = await App.api.pick_files(); if (f[0]) open(f[0].path); };
      $('trimOther').onclick = async () => { const f = await App.api.pick_files(); if (f[0]) open(f[0].path); };
      for (const id of ['trimStart', 'trimEnd']) $(id).oninput = () => sync(id);
      for (const [id, range] of [['trimStartT', 'trimStart'], ['trimEndT', 'trimEnd']])
        $(id).onchange = () => { const v = parseT($(id).value); if (v != null) { $(range).value = Math.min(video.duration, v); sync(range); } else sync(range); };
      bindSeg('trimMode', estimate);
      bindSeg('trimPreset', v => { if (v !== 'custom') $('trimMB').value = v; });
      $('trimSave').onclick = async () => {
        const mode = segValue('trimMode');
        await App.api.trim_save(video.path, +$('trimStart').value, +$('trimEnd').value, mode, +$('trimMB').value || 10, video.height);
      };
    },
    drop(files) { const v = files.find(f => f.kind === 'video') || files[0]; if (v) open(v.path); },
  };
})();
