// AirPlay to this PC: get the receiver once, switch it on, see the PIN and the phones it remembers.
(() => {
  let state = null, jobId = null;

  function render(s) {
    if (s) state = s;
    if (!state) return;
    const s2 = state;
    $('apGet').hidden = s2.installed;
    $('apMain').hidden = !s2.installed;
    $('apInstall').hidden = !s2.available || s2.installing;
    $('apNotYet').hidden = s2.available;
    $('apName').textContent = s2.name;
    $('apOn').checked = s2.on && s2.status !== 'error';
    $('apError').textContent = s2.error ? t(s2.error) : '';
    $('apError').hidden = !s2.error;
    const live = s2.status === 'ready' || s2.status === 'showing';
    $('apLive').hidden = !live;
    if (live) {
      $('apDot').className = 'ap-dot ' + s2.status;
      $('apStatus').textContent = s2.status === 'showing'
        ? `${t('Showing now')}: ${s2.client || t('a phone')}` : t('Ready: pick this PC on your iPhone');
      $('apPin').textContent = s2.pin || '';
    }
    const phones = s2.phones || [];
    $('apForget').hidden = !phones.length;
    $('apNoPhones').hidden = phones.length > 0;
    $('apPhones').replaceChildren(...phones.map(p => {
      const { row, sub } = fileRow(p.name);
      row.querySelector('.kind').replaceChildren(icon('phone'));
      sub.textContent = p.id;
      return row;
    }));
  }

  async function checkNetwork() {
    const priv = await App.api.airplay_network();
    $('apPublic').hidden = priv !== false;
  }

  App.on('airplay', render);
  App.on('job', job => {
    if (job.kind !== 'airplay' || job.id !== jobId) return;
    const busy = job.status === 'running';
    $('apBar').hidden = !busy; $('apStep').hidden = !busy;
    if (busy) {
      $('apFill').style.width = Math.round((job.progress || 0) * 100) + '%';
      $('apStep').textContent = t(job.text || 'Starting…');
    } else {
      jobId = null;
      if (job.status === 'error') toast(t(job.text));
      if (job.status === 'done') toast(t('AirPlay receiver installed'));
    }
  });

  App.pages.airplay = {
    init() {
      $('apInstall').onclick = async () => {
        if (jobId) return;
        $('apInstall').hidden = true;
        jobId = await App.api.airplay_install();
      };
      $('apOn').onchange = async () => {
        const on = $('apOn').checked;
        const r = await App.api.airplay_switch(on);
        if (r && r.error) { $('apOn').checked = false; toast(t(r.error)); return; }
        render(r);
        if (on) checkNetwork();
      };
      $('apForget').onclick = async () => {
        if (!confirm(t('Forget every phone? Each one has to type the PIN again next time.'))) return;
        render(await App.api.airplay_forget());
      };
      $('apSource').onclick = e => { e.preventDefault(); App.api.airplay_open_source(); };
    },
    async show() {
      render(await App.api.airplay_state());
      $('apSize').textContent = `${t('One-time download')} · ${Math.round(state.size / 1e6)} MB`;
      if (state.on) checkNetwork();
    },
  };
})();
