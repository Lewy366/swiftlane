// Share links page: drop files or folders, get a link at once, manage time, people and passwords.
(() => {
  let info = null;            // {shares, funnel, note, defaults, busy}
  const open = new Set();     // shares whose "What's included" is expanded

  const HOURS = [[1, '1 hour'], [6, '6 hours'], [24, '24 hours'], [72, '3 days'], [168, '7 days'], [720, '30 days']];

  function left(seconds) {
    if (seconds < 3600) return Math.max(1, Math.round(seconds / 60)) + ' min';
    if (seconds < 86400 * 2) return Math.round(seconds / 3600) + ' h';
    return Math.round(seconds / 86400) + ' days';
  }

  // ---------- Tailscale status
  function renderNotice() {
    const f = info.funnel || {}, note = info.note, box = $('shNotice'), acts = $('shNoticeActs');
    const anyActive = info.shares.some(s => s.active);
    acts.replaceChildren();
    let text = '', cls = '';
    const btn = (label, fn, primary) => { const b = el('button', 'btn small' + (primary ? ' primary' : ''), label); b.onclick = fn; acts.append(b); };
    const check = () => btn('Check again', async () => { $('shNoticeText').textContent = t('Checking…'); setInfo(await App.api.share_retry()); });
    if (note && note.approve) {
      text = 'Tailscale needs your OK once before links can be reached from the internet. Open the page, allow it, then check again.';
      btn('Open Tailscale', () => App.api.share_open_page(note.approve), true); check(); cls = 'warn';
    } else if (note && note.error) {
      text = note.error; check(); cls = 'err';
    } else if (f.state === 'no-tailscale') {
      text = "Tailscale isn't installed on this PC. Links need it to be reachable from outside your home."; cls = 'warn';
    } else if (f.state === 'not-running') {
      text = "Tailscale isn't running or you're signed out. Start Tailscale, then check again."; check(); cls = 'warn';
    } else if (f.state === 'needs-https' || f.state === 'needs-funnel') {
      text = 'One-time setup in your Tailscale account: turn on HTTPS certificates and allow Funnel for this PC. Swiftlane asks Tailscale for this when you make your first link.';
      btn('Open Tailscale admin', () => App.api.share_open_page('https://login.tailscale.com/admin/dns')); check(); cls = 'warn';
    } else if (f.state === 'in-use') {
      text = 'Tailscale Funnel is already used by something else on this PC, so Swiftlane leaves it alone and links can’t be reached.'; check(); cls = 'err';
    } else if (f.state === 'on' && anyActive) {
      text = 'Links are reachable from the internet while Swiftlane is open. Ending every link closes access again.'; cls = 'ok';
    } else if (f.state === 'error') {
      text = 'Couldn’t ask Tailscale: ' + (f.error || ''); check(); cls = 'err';
    }
    box.hidden = !text;
    box.className = 'notice' + (cls ? ' notice-' + cls : '');
    $('shNoticeText').textContent = text;
  }

  // ---------- the list
  function linkRow(s, l) {
    const row = el('div', 'sh-link'), meta = el('div', 'meta'), sub = el('div', 'sub');
    const name = el('div', 'name', l.label); if (!/^(Person \d+|Anyone with the link)$/.test(l.label)) name.setAttribute('translate', 'no');
    // "files" links count every download; personal links count whether that person has downloaded
    const uses = l.counts !== 'files' ? (l.uses ? 'Has downloaded' : 'Not downloaded yet')
      : l.max == null ? (l.uses === 1 ? '1 download' : l.uses + ' downloads') : `${l.uses} of ${l.max} downloads`;
    let state = l.blocked ? el('span', 'err', 'Blocked') : (!s.active ? null : (l.open ? null : el('span', 'warn', l.counts === 'files' ? 'Limit reached' : 'Used up')));
    if (l.counts !== 'files' && l.uses && !l.blocked) state = null;  // "Has downloaded" already says it
    sub.append(el('span', null, uses));
    if (state) sub.append(document.createTextNode(' · '), state);
    if (l.last) sub.append(document.createTextNode(' · '), el('span', null, 'downloaded ' + ago(l.last)));
    meta.append(name, sub);
    const acts = el('div', 'acts');
    if (s.active && l.url) acts.append(miniBtn('copy', 'Copy link', async () => { await App.api.share_copy(s.id, l.id); toast('Link copied'); }));
    if (s.active) acts.append(miniBtn('block', l.blocked ? 'Unblock this link' : 'Block this link', async () => setInfo(await App.api.share_link(s.id, l.id, { blocked: !l.blocked }))));
    row.append(meta, acts);
    return row;
  }

  // A request link: people send files to you
  function requestCard(s) {
    const card = el('div', 'sh-share' + (s.active ? '' : ' ended'));
    const head = el('div', 'sh-head'), title = el('div', 'name'), sub = el('div', 'sub');
    const tn = el('span', null, s.title); tn.setAttribute('translate', 'no'); title.append(el('span', 'sh-kind', 'Request'), tn);
    sub.append(el('span', null, s.files === 1 ? '1 file received' : s.files + ' files received'), document.createTextNode(' · ' + fmtSize(s.total) + ' of ' + fmtSize(s.max_bytes) + ' · '));
    sub.append(s.ended ? el('span', 'warn', 'Ended') : !s.active ? el('span', 'warn', 'Expired') : el('span', null, 'ends in ' + left(s.expires - Date.now() / 1000)));
    head.append(title, sub);
    const acts = el('div', 'sh-acts');
    const btn = (label, fn, cls) => { const b = el('button', 'btn small ' + (cls || 'ghost'), label); b.onclick = fn; acts.append(b); };
    if (s.active && s.links[0] && s.links[0].url) btn('Copy link', async () => { await App.api.share_copy(s.id, s.links[0].id); toast('Link copied'); }, '');
    btn('Open folder', () => App.api.share_open_folder(s.id));
    if (s.active) btn('End', async () => { setInfo(await App.api.share_end(s.id)); toast('Link ended'); }, 'danger');
    else btn('Remove', async () => setInfo(await App.api.share_remove(s.id)), 'danger');
    card.append(head, acts);
    if (s.preview.length) {
      const list = el('div', 'sh-files');
      for (const f of s.preview) { const r = el('div', 'sh-file'), n = el('span', 'name', f.name); n.setAttribute('translate', 'no'); r.append(n, el('span', 'muted', fmtSize(f.size))); list.append(r); }
      card.append(list);
    }
    return card;
  }

  function shareCard(s) {
    if (s.kind === 'request') return requestCard(s);
    const card = el('div', 'sh-share' + (s.active ? '' : ' ended'));
    const head = el('div', 'sh-head'), meta = el('div', 'meta'), sub = el('div', 'sub');
    const title = el('div', 'name'), more = s.title.match(/^(.*) (\+ \d+ more)$/);  // names stay as they are, "+ 1 more" is translated
    const titleName = el('span', null, more ? more[1] : s.title); titleName.setAttribute('translate', 'no');
    title.append(titleName); if (more) title.append(el('span', null, ' ' + more[2]));
    const now = Date.now() / 1000;
    sub.append(el('span', null, s.files === 1 ? '1 file' : s.files + ' files'), document.createTextNode(' · ' + fmtSize(s.total) + ' · '));
    if (s.ended) sub.append(el('span', 'warn', 'Ended'));
    else if (!s.active) sub.append(el('span', 'warn', 'Expired'));
    else sub.append(el('span', null, 'ends in ' + left(s.expires - now)));
    if (s.password) { const lock = el('span', 'sh-pw'); lock.append(icon('lock')); lock.title = 'Password protected'; sub.append(document.createTextNode(' '), lock); }
    meta.append(title, sub); head.append(meta);
    const acts = el('div', 'sh-acts');
    const btn = (label, fn, cls) => { const b = el('button', 'btn small ' + (cls || 'ghost'), label); b.onclick = fn; acts.append(b); return b; };
    if (s.active) {
      if (s.links.length === 1 && s.links[0].url) btn('Copy link', async () => { await App.api.share_copy(s.id, s.links[0].id); toast('Link copied'); }, '');
      const counted = s.links.find(l => l.counts === 'files');
      if (counted) btn('Download limit…', async () => {
        const v = prompt(t('How many times may the files be downloaded in total? Each file counts once, “Download all” counts once. Leave empty for no limit.'),
                         counted.max == null ? '' : String(counted.max));
        if (v === null) return;
        const n = v.trim() === '' ? null : parseInt(v, 10);
        if (n !== null && !(n >= 1)) { toast('Enter a number of 1 or more, or leave it empty'); return; }
        const r = await App.api.share_link(s.id, counted.id, { limit: n });
        if (r.error) toast(r.error); else { setInfo(r); toast(n == null ? 'No download limit' : 'Download limit changed'); }
      });
      btn('+ Person', async () => {
        const name = prompt(t('Name of the person (optional):'), '');
        if (name === null) return;
        const r = await App.api.share_add_links(s.id, 1, name.trim() ? [name.trim()] : null);
        if (r.error) toast(r.error); else { setInfo(r); toast('Personal link added'); }
      });
      const ext = el('select', 'input sh-extend'); ext.title = 'Make it available for longer';
      ext.append(el('option', null, t('Extend…')));  // options aren't translated automatically
      for (const [h, label] of HOURS) { const o = el('option', null, t(label)); o.value = h; ext.append(o); }
      ext.onchange = async () => { const r = await App.api.share_extend(s.id, +ext.value); if (r.error) toast(r.error); else { setInfo(r); toast('Extended'); } };
      acts.append(ext);
      btn(s.password ? 'Change password' : 'Add password', async () => {
        const pw = prompt(t(s.password ? 'New password (leave empty to remove it):' : 'Password for this share:'), '');
        if (pw === null) return;
        setInfo(await App.api.share_password(s.id, pw)); toast(pw ? 'Password set · people must enter it again' : 'Password removed');
      });
      btn('End', async () => { setInfo(await App.api.share_end(s.id)); toast('Link ended'); }, 'danger');
    } else {
      btn('Make available again', async () => { const r = await App.api.share_extend(s.id, info.defaults.hours || 24); if (r.error) toast(r.error); else setInfo(r); });
      btn('Remove', async () => setInfo(await App.api.share_remove(s.id)), 'danger');
    }
    const links = el('div', 'sh-links');
    for (const l of s.links) links.append(linkRow(s, l));
    const incl = el('button', 'btn ghost small sh-incl', open.has(s.id) ? 'Hide files' : "What's included");
    incl.onclick = () => { open.has(s.id) ? open.delete(s.id) : open.add(s.id); render(); };
    card.append(head, acts, links, incl);
    if (open.has(s.id)) {
      const list = el('div', 'sh-files');
      for (const f of s.preview) {
        const r = el('div', 'sh-file'), n = el('span', 'name', f.name); n.setAttribute('translate', 'no');
        const times = el('span', 'sh-times' + (f.downloads ? '' : ' muted'), f.downloads === 1 ? 'downloaded once' : f.downloads ? `downloaded ${f.downloads} times` : 'not downloaded');
        r.append(n, el('span', 'muted', fmtSize(f.size)), times); list.append(r);
      }
      if (s.files > s.preview.length) list.append(el('div', 'muted small', `and ${s.files - s.preview.length} more`));
      const sk = s.skipped || {}, out = [];
      if (sk.hidden) out.push(`${sk.hidden} hidden`); if (sk.links) out.push(`${sk.links} shortcuts or links`); if (sk.unreadable) out.push(`${sk.unreadable} unreadable`);
      if (out.length) list.append(el('div', 'muted small', 'Left out: ' + out.join(', ')));
      card.append(list);
    }
    return card;
  }

  function render() {
    if (!info) return;
    const list = $('shList'); list.replaceChildren(...info.shares.map(shareCard));
    const active = info.shares.filter(s => s.active).length;
    App.shareActive = active > 0;
    $('shCount').textContent = active || '';
    $('shEmpty').hidden = info.shares.length > 0;
    $('shEndAll').hidden = active < 2;
    const badge = $('navShareBadge');  // not there while Share links is switched off
    if (badge) { badge.hidden = !active; badge.textContent = active || ''; }
    renderNotice();
  }

  function setInfo(r) {
    if (!r) return;
    if (r.error) { toast(r.error); return; }
    info = r; render();
  }

  // ---------- making a share
  function options() {
    const mode = segValue('shMode');
    return {
      hours: +$('shHours').value, mode,
      count: mode === 'open' ? ($('shLimit').value ? +$('shLimit').value : null) : +$('shPeople').value || 1,
      password: $('shPassword').value,
    };
  }

  async function create(paths) {
    if (!paths.length) return;
    toast('Preparing…');
    let r = await App.api.share_create(paths, options(), false);
    if (r && r.confirm) {
      const msg = r.confirm.map(w => t(w)).join('\n\n') + '\n\n' + t('Share anyway?');
      if (!confirm(msg)) return;
      r = await App.api.share_create(paths, options(), true);
    }
    if (!r) return;
    if (r.error) { toast(r.error); return; }
    $('shPassword').value = '';
    setInfo(r);
    toast(r.copied ? 'Link copied · paste it to whoever should get the files' : 'Links made · copy each person’s link from the list');
  }

  function saveDefaults() {
    const o = options();
    App.api.set_share_defaults({ hours: o.hours, mode: o.mode, count: o.mode === 'open' ? o.count : undefined,
      people: o.mode === 'personal' ? o.count : undefined, keep_awake: $('shAwake').checked });
  }
  function showMode() {
    const personal = segValue('shMode') === 'personal';
    $('shCountWrap').hidden = personal; $('shPeopleWrap').hidden = !personal;
  }

  App.pages.share = {
    async init() {
      bindSeg('shMode', () => { showMode(); saveDefaults(); });
      $('shDrop').onclick = async () => create(await App.api.share_pick(false));
      $('shPickFolder').onclick = async () => create(await App.api.share_pick(true));
      $('shRequest').onclick = async () => {
        const title = prompt(t('What should people send you? (shown on their page, e.g. “Holiday photos for Lewy”)'), '');
        if (title === null) return;
        const mb = prompt(t('Most they may send in total (MB):'), '2048');
        if (mb === null) return;
        const files = prompt(t('Most files they may send:'), '50');
        if (files === null) return;
        const o = options();
        const r = await App.api.share_request({ title, max_mb: +mb || 2048, max_files: +files || 50, hours: o.hours, password: o.password });
        if (r.error) { toast(r.error); return; }
        $('shPassword').value = ''; setInfo(r);
        toast('Request link copied · files people send land in “From links”');
      };
      $('shEndAll').onclick = async () => { if (confirm(t('End every link?'))) { setInfo(await App.api.share_end_all()); toast('All links ended'); } };
      for (const id of ['shHours', 'shLimit', 'shPeople', 'shAwake']) $(id).onchange = saveDefaults;
      info = await App.api.share_info(false);
      const d = info.defaults;
      if (![...$('shHours').options].some(o => +o.value === +d.hours)) { const o = el('option', null, d.hours + ' h'); o.value = d.hours; $('shHours').append(o); }
      $('shHours').value = String(+d.hours);
      $('shMode').querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === d.mode));
      $('shLimit').value = d.count || ''; $('shPeople').value = d.people || 1; $('shAwake').checked = d.keep_awake !== false;
      showMode(); render();
    },
    async show() { setInfo(await App.api.share_info(true)); },
    drop(files) { create(files.map(f => f.path)); },
    dropAll(data) { create([...data.files.map(f => f.path), ...data.folders]); },
    relang() {
      for (const o of $('shHours').options) { o.dataset.en = o.dataset.en || o.textContent; o.textContent = t(o.dataset.en); }
      render();
    },
  };
  App.on('shares', r => { info = r; render(); });
})();
