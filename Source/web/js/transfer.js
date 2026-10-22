// Phone transfer + Connect pages.
(() => {
  const incoming = new Map();
  let codeShown = false;

  function renderOutbox(items) {
    const list = $('outList'); list.replaceChildren();
    for (const item of items) {
      const ui = fileRow(item.name);
      ui.sub.textContent = fmtSize(item.size);
      if (item.downloads) ui.sub.append(el('span', 'ok', ' · saved on phone' + (item.downloads > 1 ? ' ×' + item.downloads : '')));
      const acts = el('div', 'acts');
      acts.append(miniBtn('x', 'Stop sharing', async () => renderOutbox(await App.api.remove_outbox(item.id))));
      ui.row.append(acts); list.append(ui.row);
    }
    $('outCount').textContent = items.length || '';
    $('clearOut').hidden = !items.length;
  }

  function receivedSub(s) { return s.dataset.folder + ' · ' + fmtSize(+s.dataset.size) + ' · ' + ago(+s.dataset.time); }
  function receivedRow(entry) {
    const ui = fileRow(entry.name);
    Object.assign(ui.sub.dataset, { time: entry.time, size: entry.size, folder: entry.folder });
    ui.sub.textContent = receivedSub(ui.sub);
    const acts = el('div', 'acts');
    acts.append(miniBtn('open', 'Open', () => App.api.open_received(entry.id)), miniBtn('folder', 'Show in folder', () => App.api.reveal_received(entry.id)));
    ui.row.append(acts);
    ui.row.ondblclick = () => App.api.open_received(entry.id);
    return ui.row;
  }
  function updateReceivedMeta() {
    const n = $('inList').querySelectorAll('.row:not(.incoming)').length;
    $('inCount').textContent = n || '';
    $('inEmpty').hidden = $('inList').children.length > 0;
  }
  setInterval(() => document.querySelectorAll('#inList .sub[data-time]').forEach(s => s.textContent = receivedSub(s)), 30000);

  function renderConnect() {
    const s = App.state;
    $('qr').innerHTML = s.qr || '<span style="color:#555;font-size:12px;text-align:center">QR needs the<br>qrcode package</span>';
    $('qr').classList.toggle('blur', !codeShown);
    $('qrNote').textContent = codeShown ? "Scan with your phone's camera to open Swiftlane." : "Click the code to reveal it, then scan it with your phone's camera.";
    $('code').textContent = codeShown ? s.code_pretty : '••••-••••-••••-••••';
    const addrs = $('addrs'); addrs.replaceChildren();
    for (const a of s.addresses) {
      const row = el('div', 'addr'), d = el('div');
      d.append(el('b', null, a.host), el('small', null, a.label)); d.title = a.shortcut;
      row.append(d, miniBtn('copy', 'Copy Shortcut URL', () => copy(a.shortcut, 'Shortcut URL copied')));
      addrs.append(row);
    }
  }
  function renderTls() {
    const t2 = App.state.tls || {};
    $('tlsCard').hidden = !t2.ready;
    if (!t2.ready) return;
    $('tlsCertQr').innerHTML = t2.cert_qr || '';
    $('tlsSecureQr').innerHTML = t2.secure_qr || '';
    $('tlsFp').textContent = t2.fingerprint;
    $('httpsOnly').checked = !!t2.https_only;
    $('tlsState').textContent = t2.https_only ? 'Encrypted only' : 'Encrypted and unencrypted allowed';
    $('tlsState').className = 'tls-state ' + (t2.https_only ? 'ok' : 'warn');
  }
  App.renderTls = renderTls;
  async function copy(text, msg) { await App.api.copy_text(text); toast(msg); }
  function toggleCode() { codeShown = !codeShown; renderConnect(); }

  function serverNotice() {
    const n = $('serverNotice');
    n.hidden = !App.state.server_error;
    n.querySelector('span').textContent = App.state.server_error || '';
  }

  App.pages.transfer = {
    init() {
      renderOutbox(App.state.outbox);
      $('inList').replaceChildren(...App.state.received.map(receivedRow));
      updateReceivedMeta();
      $('outDrop').onclick = async () => {
        const files = await App.api.pick_files();
        if (files.length) renderOutbox(await App.api.add_outbox(files.map(f => f.path)));
      };
      $('clearOut').onclick = async () => renderOutbox(await App.api.clear_outbox());
      $('openFolder').onclick = () => App.api.open_folder();
    },
    show() { serverNotice(); },
    async drop(files) { renderOutbox(await App.api.add_outbox(files.map(f => f.path))); toast(files.length + ' file(s) ready for your phone'); },
  };
  App.pages.connect = {
    init() {
      $('toggleCode').onclick = toggleCode;
      $('qrWrap').onclick = () => { if (!codeShown) toggleCode(); };
      $('copyCode').onclick = () => copy(App.state.code_pretty, 'Pairing code copied');
      $('tlsSecureQr').onclick = () => $('tlsSecureQr').classList.remove('blur');
      $('httpsOnly').onchange = async e => {
        if (e.target.checked && !confirm(t('Only allow encrypted connections? Phones that haven’t installed the certificate, and the old Send to PC Shortcut address, will stop working until they’re set up.'))) { e.target.checked = false; return; }
        await App.api.set_option('https_only', e.target.checked);
        App.state = await App.api.get_state(); renderConnect(); renderTls();
        toast(e.target.checked ? 'Only encrypted connections are allowed now' : 'Unencrypted connections are allowed again');
      };
    },
    show() { codeShown = false; renderConnect(); renderTls(); $('tlsSecureQr').classList.add('blur'); },
  };
  App.renderConnect = renderConnect;

  App.on('incoming', data => {
    let ui = incoming.get(data.transfer);
    if (!ui) {
      ui = fileRow(data.name); ui.row.classList.add('incoming');
      ui.bar = el('div', 'bar'); ui.fill = el('i'); ui.bar.append(ui.fill); ui.meta.append(ui.bar);
      incoming.set(data.transfer, ui); $('inList').prepend(ui.row); updateReceivedMeta();
    }
    ui.fill.style.width = (data.total ? 100 * data.done / data.total : 0) + '%';
    ui.sub.textContent = 'Receiving · ' + fmtSize(data.done) + ' of ' + fmtSize(data.total);
  });
  App.on('received', data => {
    const ui = incoming.get(data.transfer); incoming.delete(data.transfer);
    const row = receivedRow(data.entry);
    ui ? ui.row.replaceWith(row) : $('inList').prepend(row);
    updateReceivedMeta();
    if (App.page !== 'transfer') {
      const badge = $('navTransferBadge');
      if (badge) { badge.hidden = false; badge.textContent = (+badge.textContent || 0) + 1; }
    }
  });
  App.on('incoming-failed', data => {
    const ui = incoming.get(data.transfer); incoming.delete(data.transfer);
    if (ui) { ui.sub.textContent = 'Transfer interrupted'; ui.bar.remove(); setTimeout(() => { ui.row.remove(); updateReceivedMeta(); }, 4000); }
  });
  App.on('outbox', renderOutbox);
  App.on('sent', async data => { toast('“' + data.name + '” saved on your phone'); renderOutbox(await App.api.get_outbox()); });
  // The sidebar is rebuilt now and then (and may not show Phone transfer at all), so listen on the document.
  document.addEventListener('click', e => {
    const badge = $('navTransferBadge');
    if (badge && e.target.closest && e.target.closest('.nav[data-page="transfer"]')) { badge.hidden = true; badge.textContent = ''; }
  });
})();
