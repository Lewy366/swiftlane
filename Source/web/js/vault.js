// Encrypted vault: create with a password, unlock, add/open/take out files, lock (also by itself after a while).
(() => {
  let state = null, filter = '';
  const selected = new Set();

  function strength(pw) {
    let s = 0;
    if (pw.length >= 8) s++; if (pw.length >= 12) s++; if (pw.length >= 16) s++;
    if (/[a-z]/.test(pw) && /[A-Z]/.test(pw)) s++; if (/\d/.test(pw)) s++; if (/[^A-Za-z0-9]/.test(pw)) s++;
    if (new Set(pw).size < 5) s = Math.min(s, 1);
    return Math.min(4, Math.floor(s * 4 / 6));
  }
  function meter(input, bar, label) {
    const s = strength($(input).value), names = ['Too weak', 'Weak', 'Okay', 'Strong', 'Very strong'];
    $(bar).style.setProperty('--s', s); $(bar).dataset.s = s;
    $(label).textContent = $(input).value ? t(names[s]) : '';
  }

  function render(s) {
    if (s) state = s;
    if (!state) return;
    $('vaultFolder').textContent = state.folder;
    $('vaultSetup').hidden = state.exists; $('vaultLocked').hidden = !state.exists || state.unlocked; $('vaultOpen').hidden = !state.unlocked;
    $('vaultLockBtn').hidden = !state.unlocked;
    if (!state.unlocked) { selected.clear(); $('vaultList').replaceChildren(); return; }
    const items = state.items.filter(i => !filter || i.name.toLowerCase().includes(filter));
    for (const id of [...selected]) if (!state.items.some(i => i.id === id)) selected.delete(id);
    $('vaultList').replaceChildren(...items.map(item => {
      const { row, sub } = fileRow(item.name);
      row.classList.toggle('on', selected.has(item.id));
      sub.textContent = `${fmtSize(item.size)} · ${t('added')} ${ago(item.added)}`;
      const acts = el('div', 'acts');
      acts.append(miniBtn('open', 'Open', () => call(App.api.vault_open(item.id))),
                  miniBtn('download', 'Take out a copy…', () => call(App.api.vault_export([item.id]))),
                  miniBtn('swap', 'Rename', () => rename(item)),
                  miniBtn('trash', 'Delete from the vault', () => remove([item.id])));
      row.append(acts);
      row.onclick = () => { selected.has(item.id) ? selected.delete(item.id) : selected.add(item.id); render(); };
      row.ondblclick = () => call(App.api.vault_open(item.id));
      return row;
    }));
    $('vaultEmpty').hidden = state.items.length > 0;
    $('vaultCount').textContent = `${state.items.length} ${t(state.items.length === 1 ? 'file' : 'files')} · ${fmtSize(state.items.reduce((a, i) => a + i.size, 0))}`;
    $('vaultSelInfo').textContent = selected.size ? selected.size + ' ' + t('selected') : '';
    $('vaultExportSel').disabled = $('vaultDelSel').disabled = !selected.size;
    $('vaultLockMin').querySelectorAll('button').forEach(b => b.classList.toggle('on', +b.dataset.v === state.lock_minutes));
  }

  async function call(promise) {
    const r = await promise;
    if (r && r.error) toast(r.error);
    else if (r && r.folder) render(r);
    return r;
  }
  async function refresh() { render(await App.api.vault_state()); }
  async function rename(item) {
    const name = prompt(t('New name'), item.name);
    if (name && name !== item.name) call(App.api.vault_rename(item.id, name));
  }
  async function remove(ids) {
    const msg = ids.length === 1 ? t('Delete this file from the vault? It can\'t be brought back.') : t('Delete the selected files from the vault? They can\'t be brought back.');
    if (confirm(msg)) { await call(App.api.vault_remove(ids)); selected.clear(); render(); }
  }
  async function add(paths) {
    const shred = $('vaultShred').checked;
    if (shred && !confirm(t('The originals will be overwritten and deleted after they are locked in. Continue?'))) return;
    call(App.api.vault_add(paths || null, shred));
  }

  App.on('vault', render);
  App.on('job', job => { if (job.kind === 'vault' && job.status !== 'running') refresh(); });

  App.pages.vault = {
    init() {
      $('vaultPw1').oninput = () => meter('vaultPw1', 'vaultMeter', 'vaultMeterLabel');
      $('vaultCreate').onclick = async () => {
        if (!$('vaultUnderstand').checked) { toast(t('Tick the box first: without the password nobody can open the vault, not even Swiftlane.')); return; }
        const r = await call(App.api.vault_create($('vaultPw1').value, $('vaultPw2').value));
        if (r && !r.error) { $('vaultPw1').value = $('vaultPw2').value = ''; toast(t('Vault created and unlocked')); }
      };
      const unlock = async () => {
        $('vaultUnlock').disabled = true;
        const r = await call(App.api.vault_unlock($('vaultPw').value));
        $('vaultUnlock').disabled = false;
        if (r && !r.error) $('vaultPw').value = ''; else { $('vaultPw').select(); }
      };
      $('vaultUnlock').onclick = unlock;
      $('vaultPw').onkeydown = e => { if (e.key === 'Enter') unlock(); };
      $('vaultOther').onclick = $('vaultOther2').onclick = () => call(App.api.vault_choose_folder());
      $('vaultLockBtn').onclick = () => call(App.api.vault_lock());
      $('vaultAdd').onclick = () => add();
      $('vaultExportSel').onclick = () => call(App.api.vault_export([...selected]));
      $('vaultDelSel').onclick = () => remove([...selected]);
      $('vaultFilter').oninput = () => { filter = $('vaultFilter').value.trim().toLowerCase(); render(); };
      bindSeg('vaultLockMin', v => App.api.set_option('vault_lock_minutes', +v).then(refresh));
      $('vaultChangePw').onclick = async () => {
        const r = await call(App.api.vault_password($('vaultOld').value, $('vaultNew1').value, $('vaultNew2').value));
        if (r === true) { ['vaultOld', 'vaultNew1', 'vaultNew2'].forEach(id => $(id).value = ''); toast(t('Password changed')); }
      };
    },
    show: refresh,
    drop(files) {
      if (!state || !state.unlocked) { toast(t('Unlock the vault first')); return; }
      add(files.map(f => f.path));
    },
  };
})();
