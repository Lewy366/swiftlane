// Instant file search ("Find files"): fast no-admin folder indexing + optional elevated NTFS MFT scan.
(() => {
  const FILTERS = [
    { id: 'all', label: 'All', token: '' },
    { id: 'files', label: 'Files', token: 'file:' },
    { id: 'folders', label: 'Folders', token: 'folder:' },
    { id: 'images', label: 'Images', token: 'ext:jpg;jpeg;png;gif;webp;bmp;heic;heif;svg;ico;tif;tiff;avif' },
    { id: 'video', label: 'Video', token: 'ext:mp4;mkv;mov;avi;webm;m4v;wmv;flv;mpg;mpeg;ts' },
    { id: 'docs', label: 'Docs', token: 'ext:pdf;doc;docx;xls;xlsx;ppt;pptx;txt;md;rtf;csv;odt' },
  ];

  let state = { indexed_count: 0, mode: '', roots: [], elapsed_ms: 0, drives: [] };
  let activeFilter = 'all';
  let scanning = null;
  let debounceTimer = null;
  let querySeq = 0;

  function modeLabel(mode) {
    if (mode === 'mft') return 'NTFS MFT';
    if (mode === 'folder') return 'Folder scan';
    return '';
  }

  function setBusy(jobId) {
    scanning = jobId || null;
    const busy = !!scanning;
    const status = $('findStatus');
    if (status) status.hidden = !busy;
    for (const id of ['findIndexHome', 'findPick', 'findMftBtn']) {
      const btn = $(id);
      if (btn) btn.disabled = busy || (id === 'findMftBtn' && !$('findMftDrive').options.length);
    }
    if (busy) $('findStatusText').textContent = t('Starting…');
  }

  function populateDrives(drives) {
    const sel = $('findMftDrive');
    if (!sel) return;
    const prev = sel.value;
    const list = Array.isArray(drives) ? drives : [];
    const fixed = list.filter(d => !d.removable);
    const choices = fixed.length ? fixed : list;
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
    const mftBtn = $('findMftBtn');
    if (mftBtn && !scanning) mftBtn.disabled = !sel.options.length;
  }

  function updateMeta(queryResult) {
    const meta = $('findMeta');
    if (!meta) return;
    const count = queryResult ? queryResult.indexed_count : state.indexed_count;
    const mode = queryResult ? queryResult.mode : state.mode;
    if (!count) {
      meta.textContent = t('Not indexed yet — scan your folders or run an NTFS MFT scan.');
      return;
    }
    const parts = [`${count.toLocaleString()} indexed${modeLabel(mode) ? ` (${modeLabel(mode)})` : ''}`];
    if (queryResult) {
      parts.push(`${queryResult.total_matches.toLocaleString()} ${queryResult.total_matches === 1 ? 'match' : 'matches'}`);
      parts.push(`${queryResult.elapsed_ms} ms`);
    } else if (state.elapsed_ms != null) {
      parts.push(`${state.elapsed_ms} ms`);
    }
    meta.textContent = parts.join(' · ');
  }

  function effectiveQuery() {
    const raw = ($('findQuery') ? $('findQuery').value : '').trim();
    const f = FILTERS.find(x => x.id === activeFilter);
    const tok = f ? f.token : '';
    if (!tok) return raw;
    return raw ? `${tok} ${raw}` : tok;
  }

  function renderEmpty(message) {
    const list = $('findList');
    if (!list) return;
    const box = el('div', 'empty', message);
    list.replaceChildren(box);
  }

  function resultRow(item) {
    const { row, sub } = fileRow(item.name || item.path);
    if (item.is_dir) row.querySelector('.kind').replaceChildren(icon('folder'));

    const dirEl = el('span', null, item.dir || '');
    dirEl.setAttribute('translate', 'no');
    sub.append(dirEl);
    if (!item.is_dir && item.size != null) {
      sub.append(document.createTextNode(' · ' + fmtSize(item.size)));
    }
    sub.title = item.path;

    const acts = el('div', 'acts');
    acts.append(
      miniBtn('open', 'Open', () => App.api.find_open(item.path)),
      miniBtn('folder', 'Show in Explorer', () => App.api.find_reveal(item.path)),
      miniBtn('copy', 'Copy path', async () => {
        await App.api.copy_text(item.path);
        toast('Path copied');
      }),
    );
    row.append(acts);
    row.ondblclick = () => App.api.find_open(item.path);
    return row;
  }

  async function runQuery() {
    const seq = ++querySeq;
    if (!state.indexed_count) {
      updateMeta(null);
      renderEmpty('Scan your user folders (no admin needed) or run an NTFS MFT scan to search instantly.');
      return;
    }
    const res = await App.api.find_query(effectiveQuery(), 150);
    if (seq !== querySeq || !res || res.error) {
      if (res && res.error) toast(res.error);
      return;
    }
    state.indexed_count = res.indexed_count;
    state.mode = res.mode;
    updateMeta(res);
    const list = $('findList');
    if (!list) return;
    if (!res.items || !res.items.length) {
      renderEmpty('No matching files or folders.');
      return;
    }
    list.replaceChildren(...res.items.map(resultRow));
  }

  function scheduleQuery() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(runQuery, 35);
  }

  function setFilter(id) {
    activeFilter = FILTERS.some(f => f.id === id) ? id : 'all';
    const box = $('findFilter');
    if (box) {
      box.querySelectorAll('button').forEach(b => {
        const key = (b.dataset.v || b.dataset.filter || b.textContent || '').trim().toLowerCase();
        b.classList.toggle('on', key === activeFilter);
      });
    }
    runQuery();
  }

  function initFilters() {
    const box = $('findFilter');
    if (!box) return;
    let buttons = [...box.querySelectorAll('button')];
    if (!buttons.length) {
      buttons = FILTERS.map(f => {
        const b = el('button', 'chip' + (f.id === activeFilter ? ' on' : ''), f.label);
        b.type = 'button';
        b.dataset.v = f.id;
        return b;
      });
      box.replaceChildren(...buttons);
    }
    buttons.forEach(b => {
      const key = (b.dataset.v || b.dataset.filter || b.textContent || '').trim().toLowerCase();
      b.dataset.v = key;
      b.classList.toggle('on', key === activeFilter);
      b.onclick = () => setFilter(key);
    });
  }

  async function startFolderScan(path) {
    if (scanning) return;
    const r = await App.api.find_index(path || '');
    if (r && r.error) { toast(r.error); return; }
    if (r) setBusy(r);
  }

  async function startMftScan() {
    if (scanning) return;
    const sel = $('findMftDrive');
    const drive = sel ? sel.value : '';
    if (!drive) { toast('No NTFS drive selected'); return; }
    const r = await App.api.find_index_mft(drive);
    if (r && r.error) { toast(r.error); return; }
    if (r) setBusy(r);
  }

  function applyState(st) {
    if (!st || st.error) return;
    state = {
      indexed_count: st.indexed_count || 0,
      mode: st.mode || '',
      roots: st.roots || [],
      elapsed_ms: st.elapsed_ms || 0,
      drives: st.drives || state.drives || [],
    };
    populateDrives(state.drives);
    runQuery();
  }

  async function refreshState() {
    const st = await App.api.find_state();
    applyState(st);
  }

  App.on('findfiles', applyState);
  App.on('job', job => {
    if (job.kind !== 'findfiles' || job.id !== scanning) return;
    if (job.status === 'running') {
      $('findStatusText').textContent = job.text || '';
    } else {
      setBusy(null);
      if (job.status === 'error') toast(job.text || 'Indexing failed');
      else if (job.status === 'cancelled') toast('Scan cancelled');
    }
  });

  App.pages.findfiles = {
    init() {
      initFilters();
      const q = $('findQuery');
      if (q) {
        q.addEventListener('input', scheduleQuery);
        q.addEventListener('keydown', e => {
          if (e.key === 'Escape' && q.value) {
            q.value = '';
            runQuery();
          }
        });
      }
      const homeBtn = $('findIndexHome');
      if (homeBtn) homeBtn.onclick = () => startFolderScan('');
      const pickBtn = $('findPick');
      if (pickBtn) pickBtn.onclick = async () => {
        const folder = await App.api.find_pick();
        if (folder) startFolderScan(folder);
      };
      const mftBtn = $('findMftBtn');
      if (mftBtn) {
        if (!mftBtn.title) {
          mftBtn.title = 'Reads the NTFS Master File Table directly for near-instant whole-drive indexing. Windows will show a UAC administrator prompt.';
        }
        mftBtn.onclick = startMftScan;
      }
      const cancelBtn = $('findCancel');
      if (cancelBtn) cancelBtn.onclick = () => scanning && App.api.cancel_job(scanning);
    },
    show() {
      refreshState();
      const q = $('findQuery');
      if (q) q.focus();
    },
    dropAll(data) {
      if (data && data.folders && data.folders.length) startFolderScan(data.folders[0]);
    },
  };
})();
