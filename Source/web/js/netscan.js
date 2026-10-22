// Network scanner: devices on the home network, which ones are new, names you give them.
(() => {
  let state = null, scanning = null;

  function deviceRow(d) {
    const title = d.label || d.name || t('Unknown device');
    const { row, sub } = fileRow(title);
    const k = row.querySelector('.kind'); k.className = 'kind net-kind';
    k.replaceChildren(icon(d.router ? 'wifi' : d.self ? 'pc' : d.randomized ? 'phone' : 'device'));
    const bits = [d.ip];
    if (d.mac) bits.push(d.mac.toUpperCase());
    if (d.label && d.name) bits.push(d.name);
    sub.textContent = bits.join(' · ');
    const tags = el('div', 'net-tags');
    if (d.new) tags.append(el('span', 'tag new', t('New')));
    if (d.self) tags.append(el('span', 'tag', t('This PC')));
    if (d.router) tags.append(el('span', 'tag', t('Router')));
    if (d.randomized && !d.router) tags.append(Object.assign(el('span', 'tag faint', t('Private address')), { title: t('Uses a made-up hardware address, like phones and tablets do') }));
    if (!d.pings && !d.self) tags.append(Object.assign(el('span', 'tag faint', t('Ignores ping')), { title: t('Found through the network table, not by ping') }));
    row.querySelector('.meta').append(tags);
    if (d.new) row.classList.add('net-new');
    const acts = el('div', 'acts');
    if (!d.self) acts.append(miniBtn('swap', 'Give it a name', () => label(d)));
    row.append(acts);
    return row;
  }

  function render(s) {
    if (s) state = s;
    if (!state) return;
    $('netWatch').checked = state.watch;
    const r = state.result;
    $('netResult').hidden = !r;
    if (!r) return;
    $('netInfo').textContent = `${r.network} · ${t('router')} ${r.gateway || '?'} · ${t('scanned')} ${ago(r.time)}`;
    const devs = r.devices, fresh = devs.filter(d => d.new);
    $('netCount').textContent = `${devs.length} ${t(devs.length === 1 ? 'device' : 'devices')}` + (fresh.length ? ` · ${fresh.length} ${t('new')}` : '');
    $('netAck').hidden = !fresh.length;
    $('netList').replaceChildren(...[...fresh, ...devs.filter(d => !d.new)].map(deviceRow));
    const missing = r.missing || [];
    $('netMissingCard').hidden = !missing.length;
    $('netMissing').replaceChildren(...missing.map(m => {
      const { row, sub } = fileRow(m.label || m.name || t('Unknown device'));
      row.querySelector('.kind').className = 'kind net-kind'; row.querySelector('.kind').replaceChildren(icon('device'));
      sub.textContent = `${t('last seen')} ${ago(m.last)} · ${m.ip}`;
      const acts = el('div', 'acts'); acts.append(miniBtn('trash', 'Forget this device', async () => render(await App.api.net_forget(m.key)))); row.append(acts);
      return row;
    }));
  }

  async function label(d) {
    const name = prompt(t('Name for this device (only stored on this PC)'), d.label || d.name || '');
    if (name != null) render(await App.api.net_label(d.key, name.trim()));
  }

  App.on('net', render);
  App.on('job', job => {
    if (job.kind !== 'net' || job.id !== scanning) return;
    if (job.status === 'running') $('netScan').textContent = t('Scanning…') + (job.text ? ' ' + job.text : '');
    else { scanning = null; $('netScan').disabled = false; $('netScan').textContent = t('Scan now'); if (job.status === 'error') toast(job.text); }
  });

  App.pages.netscan = {
    init() {
      $('netScan').onclick = async () => { if (scanning) return; $('netScan').disabled = true; scanning = await App.api.net_scan(); };
      $('netAck').onclick = async () => render(await App.api.net_ack());
      $('netWatch').onchange = () => App.api.set_option('net_watch', $('netWatch').checked);
    },
    async show() { render(await App.api.net_state()); },
  };
})();
