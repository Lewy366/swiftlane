// Updates: shows what a new version adds, and updates only when the user clicks.
(() => {
  // Release notes are plain text: lines starting with "- " become a list.
  function renderNotes(box, text) {
    box.replaceChildren();
    let list = null;
    for (const raw of String(text || '').split(/\r?\n/)) {
      const line = raw.trim();
      if (!line) { list = null; continue; }
      if (/^[-*•]\s+/.test(line)) {
        if (!list) { list = el('ul'); box.append(list); }
        list.append(el('li', null, line.replace(/^[-*•]\s+/, '')));
      } else { list = null; box.append(el('p', null, line)); }
    }
    if (!box.children.length) box.append(el('p', 'muted', 'No details were given for this update.'));
  }

  function render() {
    const u = App.state.update;
    $('updVersion').textContent = 'Swiftlane ' + App.state.version;
    $('updBox').hidden = !u;
    $('updatePill').hidden = !u;
    if (u) {
      $('updatePillText').textContent = 'Update to ' + u.version;
      $('updStatus').textContent = 'A new version is ready';
      $('updTitle').textContent = 'Swiftlane ' + u.version;
      $('updMeta').textContent = fmtSize(u.size) + (u.published ? ' · ' + new Date(u.published).toLocaleDateString() : '');
      renderNotes($('updNotes'), u.notes);
    }
  }

  async function check(manual) {
    $('updCheck').disabled = true; $('updStatus').textContent = 'Checking…';
    const r = await App.api.check_update();
    $('updCheck').disabled = false;
    if (r.error) { $('updStatus').textContent = r.error; return; }
    App.state.update = r.update;
    render();
    if (!r.update) $('updStatus').textContent = 'You have the latest version';
    if (manual && !r.update) toast('Swiftlane is up to date');
  }

  async function install() {
    const go = $('updGo');
    go.disabled = true; go.textContent = 'Downloading…'; $('updBar').hidden = false;
    let r = await App.api.install_update();
    if (r && r.confirm) {
      if (!confirm(t(r.confirm) + '\n\n' + t('Update now?'))) { reset(); return; }
      r = await App.api.install_update();
    }
    if (!r || r.error) { toast(r ? r.error : 'Update failed'); reset(); return; }
    go.textContent = 'Starting the update…';
    toast('Swiftlane closes now and reopens updated');
  }
  function reset() { const go = $('updGo'); go.disabled = false; go.textContent = 'Update now'; $('updBar').hidden = true; }

  App.on('update-available', u => { App.state.update = u; render(); });
  App.on('update-progress', d => { $('updFill').style.width = Math.round(d.fraction * 100) + '%'; });

  App.pages.updates = {
    init() {
      $('updAuto').checked = App.state.update_check !== false;
      $('updAuto').onchange = e => App.api.set_option('update_check', e.target.checked);
      $('updCheck').onclick = () => check(true);
      $('updGo').onclick = install;
      $('updatePill').onclick = () => { showPage('settings'); $('updatesCard').scrollIntoView({ behavior: 'smooth', block: 'center' }); };
      render();
      const n = App.state.updated;  // just updated: say what's new, once
      if (n) {
        $('wnTitle').textContent = 'Updated to Swiftlane ' + n.version;
        renderNotes($('wnNotes'), n.notes);
        $('whatsNew').hidden = false;
        $('wnOk').onclick = () => $('whatsNew').hidden = true;
      }
    },
  };
})();
