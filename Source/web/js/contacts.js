// Send to contacts: Swiftlane to Swiftlane, straight between PCs. Contact names come from other people:
// always shown as text (textContent), never as HTML.
(() => {
  let state = null, files = [];
  const picked = new Set();
  const STATUS = { connecting: 'Connecting…', waiting: 'Waiting for them to accept…', asking: 'Waiting for your answer', sending: 'Sending', receiving: 'Receiving', done: 'Done', declined: 'Declined', failed: 'Failed' };

  function renderFiles() {
    $('pcFiles').replaceChildren(...files.map((f, i) => {
      const { row, sub } = fileRow(f.name); sub.textContent = fmtSize(f.size);
      const acts = el('div', 'acts'); acts.append(miniBtn('x', 'Remove', () => { files.splice(i, 1); renderFiles(); })); row.append(acts);
      return row;
    }));
    $('pcFilesWrap').hidden = !files.length;
    $('pcFilesInfo').textContent = files.length ? `${files.length} ${t(files.length === 1 ? 'file' : 'files')} · ${fmtSize(files.reduce((a, f) => a + f.size, 0))}` : '';
    updateSend();
  }
  function updateSend() {
    $('pcSend').disabled = !files.length || !picked.size;
    $('pcSend').textContent = picked.size > 1 ? t('Send to') + ` ${picked.size} ` + t('people') : t('Send');
  }

  function render(s) {
    if (s) state = s;
    if (!state) return;
    $('pcCode').textContent = state.code || '—';
    $('pcSafety').textContent = state.safety || '';
    if (document.activeElement !== $('pcMyName')) $('pcMyName').value = state.name;
    const status = $('pcStatus');
    if (!state.on) { status.className = 'rm-status warn'; status.textContent = t('Send to contacts is switched off in Settings.'); }
    else if (state.error) { status.className = 'rm-status warn'; status.textContent = state.error; }
    else if (!state.contacts.length) { status.className = 'rm-status'; status.textContent = t('Add a contact to start. Until then, nobody can reach this PC this way.'); }
    else if (state.running) { status.className = 'rm-status ok'; status.textContent = t('Ready. Your contacts can send to you while Swiftlane is open.'); }
    else { status.className = 'rm-status'; status.textContent = t('Starting…'); }
    // who to send to
    for (const id of [...picked]) if (!state.contacts.some(c => c.id === id)) picked.delete(id);
    $('pcPick').replaceChildren(...state.contacts.map(c => {
      const b = el('button', 'chip' + (picked.has(c.id) ? ' on' : ''), c.name);
      b.setAttribute('translate', 'no');
      b.onclick = () => { picked.has(c.id) ? picked.delete(c.id) : picked.add(c.id); render(); };
      return b;
    }));
    $('pcPickEmpty').hidden = state.contacts.length > 0;
    updateSend();
    // contacts list
    $('pcList').replaceChildren(...state.contacts.map(c => {
      const row = el('div', 'row'), meta = el('div', 'meta'), k = el('div', 'kind pc-kind');
      k.append(icon('people'));
      const nm = el('div', 'name', c.name); nm.setAttribute('translate', 'no');
      const sub = el('div', 'sub', `${t('Check')}: ${c.safety}` + (c.last ? ` · ${t('last connected')} ${ago(c.last)}` : ` · ${t('not connected yet')}`));
      sub.setAttribute('translate', 'no');
      meta.append(nm, sub);
      const acts = el('div', 'acts');
      acts.append(miniBtn('swap', 'Rename', async () => { const n = prompt(t('Name for this contact'), c.name); if (n) render(await App.api.contact_rename(c.id, n)); }),
                  miniBtn('trash', 'Remove contact', async () => { if (confirm(t('Remove this contact? They can no longer send to you, and you can\'t send to them.'))) render(await App.api.contact_remove(c.id)); }));
      row.append(k, meta, acts); return row;
    }));
    $('pcListEmpty').hidden = state.contacts.length > 0;
    renderTransfers(state.transfers);
    for (const o of state.offers || []) showOffer(o);
  }

  function renderTransfers(list) {
    $('pcTransfers').replaceChildren(...(list || []).map(tr => {
      const row = el('div', 'row'), meta = el('div', 'meta'), k = el('div', 'kind pc-kind');
      k.append(icon(tr.dir === 'out' ? 'up' : 'download'));
      const n = tr.files.length, who = el('div', 'name', `${tr.dir === 'out' ? t('To') : t('From')} ${tr.contact}`);
      who.setAttribute('translate', 'no');
      const sub = el('div', 'sub', `${n} ${t(n === 1 ? 'file' : 'files')} · ${fmtSize(tr.total)} · ${t(STATUS[tr.status] || tr.status)}` + (tr.error ? ' · ' + tr.error : ''));
      meta.append(who, sub);
      if (['sending', 'receiving'].includes(tr.status) && tr.total) {
        const bar = el('div', 'bar'), fill = el('i'); fill.style.width = (100 * tr.done / tr.total).toFixed(1) + '%'; bar.append(fill); meta.append(bar);
      }
      row.classList.toggle('pc-bad', tr.status === 'failed');
      row.append(k, meta); return row;
    }));
    $('pcTransfersEmpty').hidden = !!(list && list.length);
  }

  // incoming: a window on top of whatever page is open
  const shown = new Set();
  function showOffer(o) {
    if (shown.has(o.id)) return;
    shown.add(o.id);
    const n = o.files.length;
    $('offerTitle').textContent = `${o.contact} ${t('wants to send you')} ${n} ${t(n === 1 ? 'file' : 'files')}`;
    $('offerSize').textContent = `${fmtSize(o.total)} · ${t('they\'re in your contacts')}`;
    $('offerFiles').replaceChildren(...o.files.slice(0, 50).map(f => { const { row, sub } = fileRow(f.name); sub.textContent = fmtSize(f.size); return row; }),
                                    ...(n > 50 ? [el('div', 'muted small', `+ ${n - 50} ${t('more')}`)] : []));
    $('offerModal').hidden = false;
    const answer = async ok => { $('offerModal').hidden = true; const r = await App.api.peer_answer(o.id, ok); if (r && r.error) toast(r.error); };
    $('offerAccept').onclick = () => answer(true);
    $('offerDecline').onclick = () => answer(false);
  }

  App.on('contacts', render);
  App.on('peer-offer', showOffer);
  App.on('peer-transfer', tr => {
    if (!state) return;
    const i = state.transfers.findIndex(x => x.id === tr.id);
    i >= 0 ? state.transfers.splice(i, 1, tr) : state.transfers.unshift(tr);
    if (tr.id && !['asking'].includes(tr.status) && shown.has(tr.id)) $('offerModal').hidden = true;  // ended before you answered
    renderTransfers(state.transfers);
    if (['done', 'failed', 'declined'].includes(tr.status)) App.api.contacts_state().then(render);  // 'last connected' etc.
  });

  App.pages.contacts = {
    async init() {
      $('pcDrop').onclick = async () => { files.push(...(await App.api.pick_files()).filter(f => !files.some(x => x.path === f.path))); renderFiles(); };
      $('pcClear').onclick = () => { files = []; renderFiles(); };
      $('pcSend').onclick = async () => {
        const r = await App.api.peer_send([...picked], files.map(f => f.path));
        if (r && r.error) { toast(r.error); return; }
        toast(t('Sent the request. They need to accept it.'));
        files = []; renderFiles();
      };
      $('pcLink').onclick = () => { if (!files.length) { toast(t('Add some files first')); return; } const f = files; files = []; renderFiles(); showPage('share'); App.pages.share.dropAll({ files: f, folders: [] }); };
      $('pcCopy').onclick = () => { if (state && state.code) { App.api.copy_text(state.code); toast(t('Contact code copied')); } };
      $('pcAdd').onclick = async () => {
        const r = await App.api.contact_add($('pcAddCode').value.trim(), $('pcAddName').value.trim());
        if (r && r.error) { toast(r.error); return; }
        $('pcAddCode').value = $('pcAddName').value = ''; render(r); toast(t('Contact added. Ask them to add your code too.'));
      };
      $('pcMyName').onchange = async () => render(await App.api.set_contact_name($('pcMyName').value));
      const s = await App.api.contacts_state();  // offers that arrived before the window was ready
      render(s);
    },
    show: async () => render(await App.api.contacts_state()),
    drop(list) { files.push(...list.filter(f => !files.some(x => x.path === f.path))); renderFiles(); },
  };
})();
