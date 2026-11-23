// Startup & background check: inspect and reversibly toggle everything that runs at Windows startup.
(() => {
  let items = null;
  let filter = 'all';
  let loading = false;

  const SOURCE_META = {
    user_run:       { label: 'User Run',       icon: 'bolt' },
    machine_run:    { label: 'System Run',     icon: 'pc' },
    machine_run32:  { label: 'System Run',     icon: 'pc' },
    user_folder:    { label: 'Startup Folder', icon: 'folder' },
    machine_folder: { label: 'Startup Folder', icon: 'folder' },
    task:           { label: 'Scheduled Task', icon: 'gear' },
  };

  function sourceMeta(source) {
    return SOURCE_META[source] || { label: 'Startup', icon: 'bolt' };
  }

  function setRefreshing(on) {
    loading = on;
    const btn = $('stRefresh');
    if (!btn) return;
    btn.disabled = on;
    btn.replaceChildren(icon('refresh'), ' ' + t(on ? 'Scanning…' : 'Refresh'));
  }

  function updateStats(pool) {
    const total = pool.length;
    let enabled = 0;
    for (const item of pool) {
      if (item.enabled) enabled++;
    }
    $('stTotal').textContent = String(total);
    $('stEnabled').textContent = String(enabled);
    $('stDisabled').textContent = String(total - enabled);
  }

  async function revealItem(item) {
    const r = await App.api.startup_reveal(item.id);
    if (r && r.error) toast(r.error);
  }

  function itemRow(item) {
    const src = sourceMeta(item.source);
    const row = el('div', 'row st-row' + (item.enabled ? '' : ' st-off'));

    const kind = el('div', 'kind net-kind st-kind');
    kind.title = t(src.label);
    kind.append(icon(src.icon));

    const meta = el('div', 'meta');

    const nameEl = el('div', 'name', item.name || item.id);
    nameEl.setAttribute('translate', 'no');
    if (item.description && item.description !== item.name) {
      nameEl.title = item.description;
    }

    const tags = el('div', 'net-tags st-tags');
    tags.append(el('span', 'tag', t(src.label)));

    if (item.publisher) {
      const pubTag = el('span', 'tag', item.publisher);
      pubTag.setAttribute('translate', 'no');
      tags.append(pubTag);
    } else {
      tags.append(el('span', 'tag faint', t('Unsigned / unknown')));
    }

    if (item.scope === 'system') {
      const adminTag = el('span', 'tag', t('Admin'));
      adminTag.title = t('System-wide entry; changing it may require administrator permission');
      tags.append(adminTag);
    }

    if (item.trigger) {
      tags.append(el('span', 'tag faint', item.trigger));
    }

    if (item.missing) {
      const missTag = el('span', 'tag new', t('Missing file'));
      missTag.title = t('The target executable could not be found on disk');
      tags.append(missTag);
    }

    const cmdText = item.command || item.target_path || '';
    const sub = el('div', 'sub', cmdText);
    sub.setAttribute('translate', 'no');
    if (cmdText) sub.title = cmdText;

    meta.append(nameEl, tags, sub);

    const acts = el('div', 'acts st-acts');
    acts.style.marginLeft = 'auto';
    acts.style.opacity = '1';
    acts.style.alignItems = 'center';
    acts.style.gap = '8px';

    if (item.can_reveal) {
      acts.append(miniBtn('folder', t('Show in Explorer'), () => revealItem(item)));
    }

    const sw = el('label', 'switch');
    const cb = el('input');
    cb.type = 'checkbox';
    cb.checked = !!item.enabled;
    cb.title = t(item.enabled ? 'Disable at startup' : 'Enable at startup');
    sw.append(cb, el('span'));

    cb.onchange = async () => {
      const want = cb.checked;
      cb.disabled = true;
      let r;
      try {
        r = await App.api.startup_toggle(item.id, want);
      } catch (err) {
        r = { error: String(err) };
      }
      cb.disabled = false;

      if (!r || r.error) {
        cb.checked = !want;
        toast((r && r.error) || t('Could not change startup entry'));
        return;
      }

      item.enabled = r.item ? !!r.item.enabled : want;
      cb.checked = item.enabled;
      cb.title = t(item.enabled ? 'Disable at startup' : 'Enable at startup');
      row.classList.toggle('st-off', !item.enabled);

      const showWin = $('stShowWin').checked;
      updateStats((items || []).filter(x => showWin || !x.builtin));
      toast(item.name + ' · ' + t(item.enabled ? 'Enabled' : 'Disabled'));

      if (filter === 'enabled' || filter === 'disabled') {
        render();
      }
    };

    acts.append(sw);
    row.append(kind, meta, acts);
    return row;
  }

  function render() {
    const list = $('stList');
    const empty = $('stEmpty');
    if (items === null) {
      list.replaceChildren();
      empty.textContent = t(loading ? 'Scanning startup entries…' : 'No matching startup entries.');
      empty.hidden = false;
      updateStats([]);
      return;
    }

    const showWin = $('stShowWin').checked;
    const pool = items.filter(item => showWin || !item.builtin);
    updateStats(pool);

    const q = ($('stSearch').value || '').trim().toLowerCase();
    const visible = pool.filter(item => {
      if (filter === 'enabled' && !item.enabled) return false;
      if (filter === 'disabled' && item.enabled) return false;
      if (filter === 'user' && item.scope !== 'user') return false;
      if (filter === 'system' && item.scope !== 'system') return false;
      if (q) {
        const srcLabel = sourceMeta(item.source).label;
        const hay = [
          item.name,
          item.publisher,
          item.description,
          item.command,
          item.target_path,
          item.trigger,
          srcLabel,
        ]
          .filter(Boolean)
          .join(' ')
          .toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });

    list.replaceChildren(...visible.map(itemRow));
    empty.textContent = t('No matching startup entries.');
    empty.hidden = visible.length > 0;
  }

  async function scan() {
    if (loading) return;
    setRefreshing(true);
    if (items === null) render();
    try {
      const r = await App.api.startup_scan();
      if (r && r.error) {
        toast(r.error);
        items = items || [];
      } else {
        items = (r && Array.isArray(r.items)) ? r.items : [];
      }
    } catch (err) {
      toast(String(err));
      items = items || [];
    } finally {
      setRefreshing(false);
      render();
    }
  }

  App.pages.startup = {
    init() {
      $('stRefresh').onclick = scan;
      $('stSearch').oninput = render;
      $('stShowWin').onchange = render;
      bindSeg('stFilter', v => {
        filter = v || 'all';
        render();
      });
    },
    async show() {
      if (items === null) {
        await scan();
      } else {
        render();
      }
    },
  };
})();
