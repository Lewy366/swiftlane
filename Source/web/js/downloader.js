// Video downloader page.
(() => {
  let info = null, choice = 'best', url = '';

  function fmtDuration(s) {
    if (!s) return '';
    s = Math.round(s);
    const h = Math.floor(s / 3600), m = Math.floor(s % 3600 / 60), sec = String(s % 60).padStart(2, '0');
    return h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
  }

  async function lookup() {
    url = $('dlUrl').value.trim();
    if (!url) return;
    const btn = $('dlFetch');
    btn.disabled = true; btn.textContent = 'Looking up…';
    $('dlError').hidden = true; $('dlPreview').hidden = true;
    try {
      info = await App.api.video_info(url);
    } finally { btn.disabled = false; btn.textContent = 'Look up'; }
    if (info.error) { $('dlError').textContent = info.error; $('dlError').hidden = false; return; }
    $('dlThumb').src = info.thumbnail || '';
    $('dlThumb').hidden = !info.thumbnail;
    $('dlTitle').textContent = info.title;
    $('dlMeta').textContent = [info.uploader, info.playlist ? info.count + ' videos' : fmtDuration(info.duration), info.site].filter(Boolean).join(' · ');
    $('dlPlaylistWrap').hidden = !info.playlist;
    $('dlPlaylist').checked = !!info.playlist;
    $('dlPreview').hidden = false;
  }

  App.pages.downloader = {
    init() {
      const chips = $('dlChoices');
      for (const [k, label] of Object.entries(App.state.download_choices)) {
        const c = el('button', 'chip' + (k === choice ? ' on' : ''), label);
        c.onclick = () => { choice = k; chips.querySelectorAll('.chip').forEach(x => x.classList.toggle('on', x === c)); };
        chips.append(c);
      }
      $('dlNode').hidden = App.state.node;
      $('dlFetch').onclick = lookup;
      $('dlUrl').onkeydown = e => { if (e.key === 'Enter') lookup(); };
      $('dlPaste').onclick = async () => {
        const text = (await App.api.paste_text() || '').trim();
        if (!text) { toast('Clipboard is empty'); return; }
        $('dlUrl').value = text; lookup();
      };
      $('dlGo').onclick = async () => {
        if (!info) return;
        // Swiftlane asks where to save (named after the video; a folder for playlists).
        const job = await App.api.download(url, choice, $('dlPlaylist').checked, info.title);
        if (job) toast('Download started');
      };
      document.querySelectorAll('[data-open-sub]').forEach(b => b.onclick = () => App.api.open_path(App.state.save_dir + '\\' + b.dataset.openSub));
    },
  };
})();
