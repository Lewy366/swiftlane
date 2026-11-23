// QR Studio: offline Wi-Fi, link, contact, email and text QR generator + offline image/screenshot decoder.
(() => {
  const KIND_LABELS = {
    wifi: 'Wi-Fi',
    url: 'Link',
    vcard: 'Contact',
    email: 'Email',
    text: 'Text',
  };

  let tab = 'make';
  let kind = 'wifi';
  let currentPayload = '';
  let genTimer = null;
  let genSeq = 0;

  const draft = {
    wifi: { ssid: '', password: '', security: 'WPA', hidden: false },
    url: { url: '' },
    text: { text: '' },
    vcard: { name: '', org: '', phone: '', email: '', url: '' },
    email: { to: '', subject: '', body: '' },
  };

  function setTab(next) {
    tab = next === 'scan' ? 'scan' : 'make';
    const seg = $('qrTab');
    if (seg) {
      seg.querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === tab));
    }
    const makePanel = $('qrMakePanel');
    const scanPanel = $('qrScanPanel');
    if (makePanel) makePanel.hidden = tab !== 'make';
    if (scanPanel) scanPanel.hidden = tab !== 'scan';
  }

  function setActionButtonsEnabled(enabled) {
    for (const id of ['qrSavePng', 'qrCopyImg', 'qrSaveSvg', 'qrCopyText']) {
      const btn = $(id);
      if (btn) btn.disabled = !enabled;
    }
  }

  function clearPreview(message, isErr) {
    const preview = $('qrPreview');
    if (preview) {
      preview.style.background = '';
      preview.replaceChildren(el('span', isErr ? 'err small' : 'muted', message));
    }
    const raw = $('qrRaw');
    if (raw) raw.textContent = '';
    currentPayload = '';
    setActionButtonsEnabled(false);
  }

  function renderSafeSvg(svgText, bgHex) {
    const parser = new DOMParser();
    const doc = parser.parseFromString(svgText, 'image/svg+xml');
    const root = doc.documentElement;
    if (!root || root.tagName.toLowerCase() !== 'svg' || doc.querySelector('parsererror')) {
      return false;
    }
    const svgNode = document.importNode(root, true);
    const preview = $('qrPreview');
    if (!preview) return false;
    preview.style.background = bgHex || '#ffffff';
    preview.replaceChildren(svgNode);
    return true;
  }

  function cleanVcardLine(s) {
    return (s || '').replace(/[\r\n]+/g, ' ').trim();
  }

  function buildVcardPayload(v) {
    const name = cleanVcardLine(v.name);
    const org = cleanVcardLine(v.org);
    const phone = cleanVcardLine(v.phone);
    const email = cleanVcardLine(v.email);
    const url = cleanVcardLine(v.url);
    if (!name && !org && !phone && !email && !url) return '';
    const lines = ['BEGIN:VCARD', 'VERSION:3.0'];
    if (name) {
      lines.push('FN:' + name);
      lines.push('N:;' + name + ';;;');
    }
    if (org) lines.push('ORG:' + org);
    if (phone) lines.push('TEL:' + phone);
    if (email) lines.push('EMAIL:' + email);
    if (url) lines.push('URL:' + url);
    lines.push('END:VCARD');
    return lines.join('\n');
  }

  function buildEmailPayload(e) {
    const to = (e.to || '').trim();
    if (!to) return '';
    const params = [];
    if (e.subject && e.subject.trim()) params.push('subject=' + encodeURIComponent(e.subject.trim()));
    if (e.body && e.body.trim()) params.push('body=' + encodeURIComponent(e.body.trim()));
    return 'mailto:' + to + (params.length ? '?' + params.join('&') : '');
  }

  async function computePayload() {
    if (kind === 'wifi') {
      const w = draft.wifi;
      if (!w.ssid) return '';
      const res = await App.api.qr_wifi_payload(w.ssid, w.password, w.security, w.hidden);
      return (res && res.payload) || '';
    }
    if (kind === 'url') {
      const raw = draft.url.url.trim();
      if (!raw) return '';
      if (!raw.includes('://') && /^[^\s/]+\.[^\s/]+/.test(raw)) return 'https://' + raw;
      return raw;
    }
    if (kind === 'text') {
      return draft.text.text;
    }
    if (kind === 'vcard') {
      return buildVcardPayload(draft.vcard);
    }
    if (kind === 'email') {
      return buildEmailPayload(draft.email);
    }
    return '';
  }

  async function generateNow() {
    const seq = ++genSeq;
    const payload = await computePayload();
    if (seq !== genSeq) return;
    if (!payload) {
      clearPreview(t('Fill in the fields to generate a QR code'), false);
      return;
    }
    const fg = $('qrFg') ? $('qrFg').value : '#0e121c';
    const bg = $('qrBg') ? $('qrBg').value : '#ffffff';
    const ec = $('qrEc') ? $('qrEc').value : 'M';
    const res = await App.api.qr_generate(payload, fg, bg, ec);
    if (seq !== genSeq) return;
    if (!res || res.error) {
      clearPreview((res && res.error) || t('Could not generate QR code'), true);
      return;
    }
    if (!renderSafeSvg(res.svg, bg)) {
      clearPreview(t('Could not display QR preview'), true);
      return;
    }
    currentPayload = res.payload || payload;
    const raw = $('qrRaw');
    if (raw) raw.textContent = currentPayload;
    setActionButtonsEnabled(true);
  }

  function scheduleGenerate() {
    clearTimeout(genTimer);
    genTimer = setTimeout(generateNow, 60);
  }

  // ---------- dynamic fields per QR kind

  function makeField(labelText, control) {
    const f = el('div', 'field');
    f.append(el('span', null, labelText), control);
    return f;
  }

  function textInput(value, placeholder, onInput, type) {
    const inp = el('input', 'input');
    inp.type = type || 'text';
    inp.value = value || '';
    inp.placeholder = placeholder || '';
    inp.autocomplete = 'off';
    inp.spellcheck = false;
    inp.oninput = () => {
      onInput(inp.value);
      scheduleGenerate();
    };
    return inp;
  }

  function makePasswordControl(value, placeholder, onInput) {
    const wrap = el('span', 'pw-wrap');
    const inp = textInput(value, placeholder, onInput, 'password');
    const eye = miniBtn('eye', t('Show password'), () => {
      const show = inp.type === 'password';
      inp.type = show ? 'text' : 'password';
      eye.classList.toggle('on', show);
      eye.title = t(show ? 'Hide password' : 'Show password');
      inp.focus();
    });
    eye.type = 'button';
    eye.classList.add('pw-eye');
    inp.addEventListener('input', () => {
      if (!inp.value && inp.type !== 'password') {
        inp.type = 'password';
        eye.classList.remove('on');
        eye.title = t('Show password');
      }
    });
    wrap.append(inp, eye);
    return { wrap, inp };
  }

  function renderFields() {
    const box = $('qrFields');
    if (!box) return;
    box.replaceChildren();

    if (kind === 'wifi') {
      const w = draft.wifi;
      const ssidInp = textInput(w.ssid, 'Home Wi-Fi', v => { w.ssid = v; });

      const secSel = el('select', 'input');
      for (const [val, lbl] of [['WPA', 'WPA / WPA2 / WPA3'], ['WEP', 'WEP'], ['nopass', 'None (open network)']]) {
        const opt = el('option', null, lbl);
        opt.value = val;
        if (val === w.security) opt.selected = true;
        secSel.append(opt);
      }

      const pw = makePasswordControl(w.password, 'Wi-Fi password', v => { w.password = v; });
      const pwField = makeField('Password', pw.wrap);
      const syncPwVisibility = () => {
        const openNet = w.security === 'nopass';
        pwField.hidden = openNet;
        pw.inp.disabled = openNet;
      };
      secSel.onchange = () => {
        w.security = secSel.value;
        syncPwVisibility();
        scheduleGenerate();
      };
      syncPwVisibility();

      const hiddenLbl = el('label', 'check');
      const hiddenChk = el('input');
      hiddenChk.type = 'checkbox';
      hiddenChk.checked = !!w.hidden;
      hiddenChk.onchange = () => {
        w.hidden = hiddenChk.checked;
        scheduleGenerate();
      };
      hiddenLbl.append(hiddenChk, el('span', null, 'Hidden network'));

      const row = el('div', 'row-fields');
      row.append(makeField('Network name (SSID)', ssidInp), makeField('Security', secSel));
      box.append(row, pwField, hiddenLbl);
    } else if (kind === 'url') {
      const u = draft.url;
      box.append(makeField('Website link', textInput(u.url, 'https://example.com', v => { u.url = v; }, 'url')));
    } else if (kind === 'text') {
      const tx = draft.text;
      const ta = el('textarea', 'input');
      ta.rows = 4;
      ta.value = tx.text;
      ta.placeholder = 'Type or paste any text, note, or code…';
      ta.oninput = () => {
        tx.text = ta.value;
        scheduleGenerate();
      };
      box.append(makeField('Text', ta));
    } else if (kind === 'vcard') {
      const v = draft.vcard;
      const row1 = el('div', 'row-fields');
      row1.append(
        makeField('Full name', textInput(v.name, 'Ada Lovelace', val => { v.name = val; })),
        makeField('Organization', textInput(v.org, 'Optional', val => { v.org = val; })),
      );
      const row2 = el('div', 'row-fields');
      row2.append(
        makeField('Phone', textInput(v.phone, '+1 555 0100', val => { v.phone = val; }, 'tel')),
        makeField('Email', textInput(v.email, 'ada@example.com', val => { v.email = val; }, 'email')),
      );
      box.append(
        row1,
        row2,
        makeField('Website', textInput(v.url, 'https://example.com', val => { v.url = val; }, 'url')),
      );
    } else if (kind === 'email') {
      const em = draft.email;
      const bodyTa = el('textarea', 'input');
      bodyTa.rows = 3;
      bodyTa.value = em.body;
      bodyTa.placeholder = 'Optional message';
      bodyTa.oninput = () => {
        em.body = bodyTa.value;
        scheduleGenerate();
      };
      box.append(
        makeField('Recipient email', textInput(em.to, 'name@example.com', v => { em.to = v; }, 'email')),
        makeField('Subject', textInput(em.subject, 'Optional subject', v => { em.subject = v; })),
        makeField('Message', bodyTa),
      );
    }

    scheduleGenerate();
  }

  // ---------- offline scan / decode results

  async function copyWithToast(text, msg) {
    await App.api.copy_text(text);
    toast(t(msg || 'Copied'));
  }

  function buildResultCard(item, source) {
    const card = el('div', 'qr-res-card');
    const head = el('div', 'qr-res-head');

    const badge = el('span', 'chip on', KIND_LABELS[item.kind] || 'Text');
    head.append(badge);
    if (source) {
      const src = el('span', 'muted small', source);
      src.setAttribute('translate', 'no');
      head.append(src);
    }
    head.append(el('span', 'spacer'));

    if (item.kind === 'wifi' && item.wifi && item.wifi.password) {
      const pwBtn = el('button', 'btn small', 'Copy password');
      pwBtn.onclick = () => copyWithToast(item.wifi.password, 'Wi-Fi password copied');
      head.append(pwBtn);
    }

    const copyBtn = el('button', 'btn small', 'Copy text');
    copyBtn.onclick = () => copyWithToast(item.text || '', 'Copied');
    head.append(copyBtn);

    if (item.kind === 'url' && item.text) {
      const openBtn = el('button', 'btn small primary', 'Open link');
      openBtn.onclick = async () => {
        const res = await App.api.qr_open_url(item.text);
        if (res && res.error) toast(res.error);
      };
      head.append(openBtn);
    }

    card.append(head);

    if (item.kind === 'wifi' && item.wifi) {
      const grid = el('div', 'qr-wifi-grid');
      const ssidCode = el('code', null, item.wifi.ssid || '(empty)');
      ssidCode.setAttribute('translate', 'no');
      grid.append(
        el('small', null, 'Network'),
        ssidCode,
        miniBtn('copy', t('Copy'), () => copyWithToast(item.wifi.ssid || '', 'Network name copied')),
      );

      if (item.wifi.password) {
        const pwCode = el('code', null, item.wifi.password);
        pwCode.setAttribute('translate', 'no');
        grid.append(
          el('small', null, 'Password'),
          pwCode,
          miniBtn('copy', t('Copy password'), () => copyWithToast(item.wifi.password, 'Wi-Fi password copied')),
        );
      }

      const secText = (item.wifi.security || 'WPA') + (item.wifi.hidden ? ' · Hidden' : '');
      const secCode = el('code', null, secText);
      secCode.setAttribute('translate', 'no');
      grid.append(el('small', null, 'Security'), secCode, el('span'));

      card.append(grid);
    }

    const body = el('div', 'qr-res-text', item.text || '');
    body.setAttribute('translate', 'no');
    card.append(body);
    return card;
  }

  function renderScanResults(res) {
    if (!res) return;
    if (res.error) {
      toast(res.error);
      return;
    }
    const list = $('qrScanResults');
    const empty = $('qrScanEmpty');
    if (!list || !empty) return;

    const items = Array.isArray(res.items) ? res.items : [];
    list.replaceChildren();
    if (!items.length) {
      empty.hidden = false;
      empty.textContent = 'No QR code found in ' + (res.source || 'that image') + '. Try cropping closer or using a sharper screenshot.';
      toast(t('No QR code found'));
      return;
    }

    empty.hidden = true;
    for (const item of items) {
      list.append(buildResultCard(item, res.source));
    }
    toast(items.length === 1 ? t('Decoded 1 QR code') : 'Decoded ' + items.length + ' QR codes');
  }

  function handlePaste(e) {
    if (App.page !== 'qrstudio') return;
    const clip = e.clipboardData;
    const items = clip && clip.items ? [...clip.items] : [];
    const imgItem = items.find(it => it.type && it.type.startsWith('image/'));
    if (imgItem) {
      const file = imgItem.getAsFile();
      if (!file) return;
      e.preventDefault();
      setTab('scan');
      const reader = new FileReader();
      reader.onload = async () => {
        if (typeof reader.result === 'string') {
          renderScanResults(await App.api.qr_decode_data(reader.result));
        }
      };
      reader.readAsDataURL(file);
      return;
    }
    // Fallback on Scan tab when focused outside text inputs (handles Windows CF_DIB-only clipboard images)
    const active = document.activeElement;
    const inEditable = active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.isContentEditable);
    if (tab === 'scan' && !inEditable) {
      e.preventDefault();
      App.api.qr_decode_clipboard().then(renderScanResults);
    }
  }

  App.pages.qrstudio = {
    init() {
      bindSeg('qrTab', v => setTab(v));
      bindSeg('qrKind', v => {
        kind = v || 'wifi';
        renderFields();
      });

      for (const id of ['qrEc', 'qrFg', 'qrBg']) {
        const ctrl = $(id);
        if (ctrl) ctrl.addEventListener('input', scheduleGenerate);
        if (ctrl) ctrl.addEventListener('change', scheduleGenerate);
      }

      const savePng = $('qrSavePng');
      if (savePng) savePng.onclick = async () => {
        if (!currentPayload) return;
        const res = await App.api.qr_save_png(currentPayload, $('qrFg').value, $('qrBg').value, $('qrEc').value, 1024);
        if (res && res.saved) toast('Saved ' + baseName(res.saved));
        else if (res && res.error) toast(res.error);
      };

      const saveSvg = $('qrSaveSvg');
      if (saveSvg) saveSvg.onclick = async () => {
        if (!currentPayload) return;
        const res = await App.api.qr_save_svg(currentPayload, $('qrFg').value, $('qrBg').value, $('qrEc').value);
        if (res && res.saved) toast('Saved ' + baseName(res.saved));
        else if (res && res.error) toast(res.error);
      };

      const copyImg = $('qrCopyImg');
      if (copyImg) copyImg.onclick = async () => {
        if (!currentPayload) return;
        const res = await App.api.qr_copy_png(currentPayload, $('qrFg').value, $('qrBg').value, $('qrEc').value);
        if (res && res.ok) toast(t('QR image copied to clipboard'));
        else if (res && res.error) toast(res.error);
      };

      const copyTxt = $('qrCopyText');
      if (copyTxt) copyTxt.onclick = () => {
        if (!currentPayload) return;
        copyWithToast(currentPayload, 'Payload copied');
      };

      const drop = $('qrDrop');
      if (drop) drop.onclick = async () => {
        renderScanResults(await App.api.qr_decode_pick());
      };

      const pasteBtn = $('qrPasteClip');
      if (pasteBtn) pasteBtn.onclick = async () => {
        renderScanResults(await App.api.qr_decode_clipboard());
      };

      document.addEventListener('paste', handlePaste);
      renderFields();
    },

    show() {
      if (!$('qrFields').children.length) renderFields();
    },

    async drop(files) {
      if (!files || !files.length || !files[0].path) return;
      setTab('scan');
      renderScanResults(await App.api.qr_decode_file(files[0].path));
    },
  };
})();
