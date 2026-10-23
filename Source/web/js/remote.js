// PC remote page: switch it on, see how to use it on the phone, try the keys here, and watch presses arrive.
(() => {
  const LABELS = {
    volume_up: 'Louder', volume_down: 'Quieter', mute: 'Mute', play_pause: 'Play / pause', next: 'Next', previous: 'Previous',
    slide_next: 'Next slide', slide_previous: 'Previous slide', lock: 'Lock', sleep: 'Sleep', shutdown: 'Shut down', shutdown_cancel: 'Cancel shut down',
  };
  let state = null;

  function render(s) {
    if (s) state = s;
    if (!state) return;
    $('rmOn').checked = state.on;
    $('rmPower').checked = state.power;
    $('rmPowerRow').hidden = !state.on;
    const status = $('rmStatus');
    if (!state.transfer) { status.className = 'rm-status warn'; status.textContent = t('Phone transfer is off, so phones can\'t connect to this PC at all. Switch it on in Settings first.'); }
    else if (state.on) { status.className = 'rm-status ok'; status.textContent = t('On. Paired phones show a Remote tab within a few seconds.'); }
    else { status.className = 'rm-status'; status.textContent = t('Off. Phones don\'t get the Remote tab, and presses are refused.'); }
    $('rmLog').replaceChildren(...state.log.map(e => {
      const row = el('div', 'row' + (e.error ? ' rm-err' : '')), meta = el('div', 'meta');
      const k = el('div', 'kind rm-kind'); k.append(icon(e.error ? 'x' : 'remote'));
      meta.append(el('div', 'name', t(LABELS[e.action] || e.action)));
      const sub = el('div', 'sub', `${t('from')} ${e.name || t('your phone')} · ${e.ip} · ${ago(e.time)}` + (e.error ? ' · ' + e.error : ''));
      sub.setAttribute('translate', 'no'); meta.append(sub);
      row.append(k, meta); return row;
    }));
    $('rmEmpty').hidden = state.log.length > 0;
  }
  async function refresh() { render(await App.api.remote_state()); }

  App.on('remote-press', render);

  App.pages.remote = {
    init() {
      $('rmOn').onchange = async e => {
        App.state = await App.api.set_feature('remote', e.target.checked);
        applyFeatures(); refresh();
        toast(e.target.checked ? t('PC remote on') : t('PC remote off'));
      };
      $('rmPower').onchange = async e => {
        await App.api.set_option('remote_power', e.target.checked); App.state.remote_power = e.target.checked; refresh();
      };
      $('rmConnect').onclick = () => showPage('connect');
      document.querySelectorAll('#rmTry [data-act]').forEach(b => b.onclick = async () => {
        const r = await App.api.remote_try(b.dataset.act);
        if (r && r.error) toast(r.error); else toast(t(LABELS[b.dataset.act]) + ' ✓');
      });
      setInterval(() => { if (App.page === 'remote' && state && state.log.length) render(); }, 15000);  // keep "x min ago" fresh
    },
    show: refresh,
  };
})();
