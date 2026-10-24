// Settings page: appearance, features on/off, phone transfer, defaults, general.
(() => {
  const FEATURES = [
    ['transfer', 'transfer', 'Phone transfer', 'Send and receive files with your phone over your network'],
    ['share', 'link', 'Share links', 'Send files to anyone with a link, through Tailscale'],
    ['contacts', 'people', 'Send to contacts', 'Straight to another Swiftlane, no link needed'],
    ['worldclock', 'globe', 'World clock', 'Live times on a map, plan across time zones'],
    ['remote', 'remote', 'PC remote', 'Control volume, music, slides and more from your paired phone'],
    ['pdf', 'pdf', 'PDF tools', 'Merge, split, reorder, rotate, sign, make smaller'],
    ['trimmer', 'scissors', 'Video trimmer', 'Cut clips, shrink them to send, keep the sound'],
    ['vault', 'lock', 'Vault', 'Files only your password opens'],
    ['diskmap', 'disk', 'Disk space', 'See what fills up your drives'],
    ['netscan', 'wifi', 'Network devices', 'Who is on your home network'],
    ['exposure', 'radar', 'Is my PC exposed?', 'Firewall, open ports, remote access, router'],
    ['converter', 'convert', 'File converter', 'Images, video, audio and PDF'],
    ['images', 'image', 'Image editor', 'Crop, resize, straighten, adjust colours, compress'],
    ['downloader', 'download', 'Video downloader', 'YouTube and other sites'],
    ['currency', 'currency', 'Currency converter', 'Live exchange rates'],
    ['calculator', 'calc', 'Calculator', 'Scientific, ClassWiz style'],
    ['speedtest', 'gauge', 'Speed test', 'Download, upload and ping'],
    ['adblock', 'shield', 'Ad blocker test', 'Are ads and trackers blocked?'],
  ];
  const ACCENT_NAMES = { sunset: 'Sunset', aurora: 'Aurora', ocean: 'Ocean', forest: 'Forest', rose: 'Rose', ruby: 'Ruby', mono: 'Mono' };

  function renderFeatures() {
    const list = $('featureList'); list.replaceChildren();
    for (const [key, ic, name, desc] of FEATURES) {
      const row = el('div', 'setting'), fi = el('div', 'feature-icon'), text = el('div');
      fi.append(icon(ic));
      text.append(el('b', null, name), el('small', null, desc));
      const sw = el('label', 'switch'), input = el('input'); input.type = 'checkbox'; input.checked = App.state.features[key];
      sw.append(input, el('span'));
      input.onchange = async () => {
        if (key === 'share' && !input.checked && App.shareActive && !confirm(t('Switching sharing off ends every active link. Continue?'))) { input.checked = true; return; }
        App.state = await App.api.set_feature(key, input.checked);
        applyFeatures(); renderTransferSettings();
        if (key === 'transfer') toast(input.checked ? (App.state.server_error || 'Phone transfer on') : 'Phone transfer off · not listening on the network');
        if (key === 'share' && !input.checked) toast('Sharing off · every link was ended');
      };
      row.append(fi, text, sw); list.append(row);
    }
  }

  function renderTransferSettings() {
    $('transferSettings').hidden = !App.state.features.transfer;
    $('remotePowerRow').hidden = !App.state.features.remote;
    $('remotePower').checked = !!App.state.remote_power;
    $('saveDir').textContent = App.state.save_dir;
    $('notify').checked = App.state.notify;
    $('sortFiles').checked = App.state.sort_files;
    $('dlDir').textContent = App.state.download_dir || 'Your Swiftlane folder (Videos or Audio)';
    $('dlDirReset').hidden = !App.state.download_dir;
  }

  // ---- appearance
  function renderSwatches() {
    const theme = App.state.theme, box = $('swatches'); box.replaceChildren();
    for (const [key, [a, b]] of Object.entries(ACCENTS)) {
      const s = el('button', 'swatch' + (theme.accent === key ? ' on' : ''));
      s.style.background = a; s.title = ACCENT_NAMES[key];
      s.onclick = () => setTheme({ accent: key });
      box.append(s);
    }
    const c = el('label', 'swatch custom' + (theme.accent === 'custom' ? ' on' : ''));
    c.style.background = theme.accent === 'custom' ? theme.custom : 'conic-gradient(#f44, #fd0, #4f4, #0df, #44f, #f4f, #f44)';
    c.title = 'Your own colour';
    const picker = el('input'); picker.type = 'color'; picker.value = theme.custom;
    picker.oninput = () => { App.state.theme.custom = picker.value; App.state.theme.accent = 'custom'; applyTheme(App.state.theme); c.style.background = picker.value; };
    picker.onchange = () => setTheme({ accent: 'custom', custom: picker.value });
    c.append(picker, el('span', null, '+'));
    box.append(c);
  }
  async function setTheme(changes) {
    App.state.theme = await App.api.set_theme(changes);
    applyTheme(App.state.theme);
    renderSwatches();
  }
  function renderBackground() {
    const has = !!App.hasBg, th = App.state.theme;
    $('bgInfo').textContent = !has ? 'Swiftlane wallpaper' : App.bgKind === 'video' ? 'Your video' : App.bgKind === 'anim' ? 'Your animation' : 'Your picture';
    $('bgClear').hidden = !has; $('bgControls').hidden = !has;
    $('bgBlurOn').checked = th.blur_on !== false; $('bgBlurRow').hidden = th.blur_on === false;
    $('bgBlur').value = th.blur; $('bgBlurVal').textContent = th.blur;
    $('modeSeg').querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === (th.mode || 'dark')));
    $('bgDim').value = th.dim; $('bgDimVal').textContent = th.dim + '%';
  }

  App.pages.settings = {
    init() {
      $('settingsSub').textContent = 'Swiftlane ' + App.state.version;
      $('configDir').textContent = App.state.config_dir;
      $('autostart').checked = App.state.autostart;
      $('ctxMenu').checked = !!App.state.context_menu;
      $('ctxMenu').onchange = async e => {
        const r = await App.api.set_context_menu(e.target.checked);
        if (r.error) { toast(r.error); e.target.checked = !e.target.checked; return; }
        toast(r.enabled ? 'Added to the right-click menu' : 'Removed from the right-click menu');
      };
      $('language').value = App.state.language;
      $('chooseFolder').onclick = async () => { const dir = await App.api.choose_folder(); if (dir) { App.state.save_dir = dir; renderTransferSettings(); toast('Save folder changed'); } };
      $('remotePower').onchange = e => {
        if (e.target.checked && !confirm(t('Let your paired phone put this PC to sleep or shut it down? Shutting down always gives a 60-second warning you can cancel.'))) { e.target.checked = false; return; }
        App.api.set_option('remote_power', e.target.checked); App.state.remote_power = e.target.checked;
      };
      $('notify').onchange = e => { App.api.set_notify(e.target.checked); App.state.notify = e.target.checked; };
      $('sortFiles').onchange = e => { App.api.set_option('sort_files', e.target.checked); App.state.sort_files = e.target.checked; };
      $('autostart').onchange = async e => { if (!await App.api.set_autostart(e.target.checked)) { e.target.checked = !e.target.checked; toast('Couldn’t change the startup setting'); } };
      $('newCode').onclick = async () => {
        if (!confirm(t('Make a new pairing code? Your phone and the Send to PC Shortcut will need the new code.'))) return;
        App.state = await App.api.new_code(); App.renderConnect(); toast('New pairing code created · see Connect phone');
      };
      $('dlDirChoose').onclick = async () => { const d = await App.api.choose_download_dir(); if (d) { App.state.download_dir = d; renderTransferSettings(); } };
      $('dlDirReset').onclick = async () => { await App.api.reset_download_dir(); App.state.download_dir = ''; renderTransferSettings(); };
      bindSeg('calcAngleDefault', v => { App.api.calc('set_mode', { angle: v }); App.state.calc_angle = v; });
      $('calcAngleDefault').querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === App.state.calc_angle));
      $('language').onchange = async e => { await App.api.set_option('language', e.target.value); App.state.language = e.target.value; setLanguage(e.target.value); };

      $('bgChoose').onclick = async () => {
        let r;
        try { r = await App.api.choose_background(); } catch (e) { toast('Couldn’t open the file picker'); return; }
        if (!r) return;
        if (r.error) { toast(r.error); return; }
        App.state.theme.background = true; applyTheme(App.state.theme, r); renderBackground();
      };
      $('bgClear').onclick = async () => { await App.api.clear_background(); App.state.theme.background = false; applyTheme(App.state.theme, null); renderBackground(); };
      bindSeg('modeSeg', v => { App.state.theme.mode = v; App.api.set_theme({ mode: v }); applyTheme(App.state.theme); });
      $('bgBlurOn').onchange = e => { App.state.theme.blur_on = e.target.checked; App.api.set_theme({ blur_on: e.target.checked }); applyTheme(App.state.theme); renderBackground(); };
      for (const [id, key, unit] of [['bgBlur', 'blur', ''], ['bgDim', 'dim', '%']]) {
        $(id).oninput = e => { App.state.theme[key] = +e.target.value; $(id + 'Val').textContent = e.target.value + unit; applyTheme(App.state.theme); };
        $(id).onchange = e => App.api.set_theme({ [key]: +e.target.value });
      }
    },
    show() { renderFeatures(); renderTransferSettings(); renderSwatches(); renderBackground(); },
  };
})();
