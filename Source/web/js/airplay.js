// Screen mirroring page: AirPlay (iPhone, iPad, Mac), Miracast (Android, through Windows' own
// Wireless Display) and casting from a laptop's browser, plus the Allow/Deny prompt for casts.
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

  // ---- Android (Miracast through Windows' own Wireless Display)
  function renderMiracast(m) {
    $('mcNo').hidden = m.supported !== false;
    $('mcSetup').hidden = m.supported === false || m.installed;
    $('mcReady').hidden = !m.installed;
    $('mcInstall').hidden = !!m.installing;
    $('mcBar').hidden = !m.installing;
  }
  App.on('miracast', renderMiracast);

  // ---- Laptops (casting from a browser)
  function renderCast(c) {
    $('caOn').checked = c.on;
    $('caLink').hidden = !c.link;
    if (c.link) {
      $('caUrl').textContent = c.link;
      $('caQr').innerHTML = c.qr;   // SVG made by Swiftlane itself (core.qr_svg)
      $('caFp').textContent = c.fingerprint;
    }
  }
  const asked = new Set();
  App.on('cast-offer', o => {
    if (asked.has(o.id)) return;
    asked.add(o.id);
    $('castWho').textContent = `${o.name} · ${o.ip}`;
    $('castModal').hidden = false;
    const answer = async ok => {
      $('castModal').hidden = true;
      const r = await App.api.cast_decide(o.id, ok);
      if (r && r.error) toast(t(r.error));
    };
    $('castAllow').onclick = () => answer(true);
    $('castDeny').onclick = () => answer(false);
    $('castAllow').focus();
  });

  App.pages.airplay = {
    init() {
      $('mcInstall').onclick = async () => {
        renderMiracast({ supported: true, installed: false, installing: true });
        const r = await App.api.miracast_install();
        if (r && r.error) toast(t(r.error));
        renderMiracast(await App.api.miracast_state());
      };
      $('mcStart').onclick = async () => { const r = await App.api.miracast_start(); if (r && r.error) toast(t(r.error)); };
      $('mcSettings').onclick = () => App.api.miracast_settings();
      $('caOn').onchange = async () => {
        const r = await App.api.cast_switch($('caOn').checked);
        if (r && r.error) { $('caError').textContent = t(r.error); $('caError').hidden = false; $('caOn').checked = false; return; }
        $('caError').hidden = true;
        renderCast(r);
      };
      $('caCopy').onclick = async () => {
        try { await navigator.clipboard.writeText($('caUrl').textContent); toast(t('Link copied')); }
        catch (e) { toast(t('Couldn\'t copy. Select the link and press Ctrl+C.')); }
      };
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
      const [ap, ca, mc] = await Promise.all([
        App.api.airplay_state(),
        App.api.cast_state(),
        App.api.miracast_state(),
      ]);
      render(ap);
      $('apSize').textContent = `${t('One-time download')} · ${Math.round(state.size / 1e6)} MB`;
      if (state.on) checkNetwork();
      renderCast(ca);
      renderMiracast(mc);
    },
  };
})();
