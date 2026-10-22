// Scientific calculator page (Casio fx-87DE X ClassWiz style).
(() => {
  const MODES = [
    ['calc', 'Calculate'], ['cmplx', 'Complex'], ['basen', 'Base-N'], ['stat', 'Statistics'], ['dist', 'Distribution'],
    ['eqn', 'Equation'], ['matrix', 'Matrix'], ['vector', 'Vector'], ['table', 'Table'], ['ratio', 'Ratio'], ['const', 'Constants & units'],
  ];
  let mode = 'calc', mem = null, exactView = true, engView = false, dmsView = false, last = null;
  const history = { calc: [], cmplx: [] };
  const calc = (method, args) => App.api.calc(method, args || {});

  // ------------------------------------------------------------ helpers
  function field(label, id, value = '', type = 'text', attrs = {}) {
    const f = el('label', 'field'); f.append(el('span', null, label));
    const i = el('input', 'input'); i.id = id; i.type = type; i.value = value; i.spellcheck = false;
    Object.assign(i, attrs); f.append(i); return f;
  }
  function seg(id, options, current, onChange) {
    const s = el('div', 'seg'); s.id = id;
    for (const [v, label] of options) {
      const b = el('button', v === current ? 'on' : '', label); b.dataset.v = v;
      b.onclick = () => { s.querySelectorAll('button').forEach(x => x.classList.toggle('on', x === b)); onChange && onChange(v); };
      s.append(b);
    }
    return s;
  }
  function card(title, ...children) { const c = el('section', 'card'); if (title) c.append(el('h2', null, title)); c.append(...children); return c; }
  function answerBox() { const a = el('div', 'answer-box'); a.hidden = true; return a; }
  function showAnswer(box, r, pieces) {
    box.hidden = false; box.replaceChildren();
    if (!r || r.error) { box.append(el('span', 'err', r ? r.error : 'Math ERROR')); return; }
    pieces(box);
  }
  function valueNode(v) {  // {exact, decimal} -> node
    const span = el('span');
    if (v.exact && exactView) span.innerHTML = v.exact; else span.textContent = v.decimal;
    return span;
  }
  function results(pairs) {  // [[label, text or node], ...]
    const g = el('div', 'results');
    for (const [k, v] of pairs) { const d = el('div'); const b = el('b'); typeof v === 'string' ? b.textContent = v : b.append(v); d.append(el('small', null, k), b); g.append(d); }
    return g;
  }
  function vals(ids) { return ids.map(id => $(id).value.trim()); }

  // ------------------------------------------------------------ Calculate / Complex
  const K = (label, ins, cls = 'f', title = '') => ({ label, ins, cls, title });
  // Always visible, right under the display (the keys you use most).
  const QUICK = [K('x²', '²'), K('xʸ', '^'), K('√', '√('), K('x⁻¹', '⁻¹'), K('log', 'log('), K('ln', 'ln('),
    K('sin', 'sin('), K('cos', 'cos('), K('tan', 'tan('), K('π', 'π'), K('S⇔D', '@sd', 'f accent', 'Exact ⇄ decimal'), K('SOLVE', '@solve', 'f accent')];
  // Everything else, one tab at a time so the keypad stays compact.
  const TABS = [
    ['More', [K('x³', '³'), K('∛', '∛('), K('ⁿ√', 'root(', 'f', 'root(n, x)'), K('10ˣ', '10^'), K('eˣ', 'e^'), K('logₐb', 'log(', 'f', 'log(a, b) = log base a of b'),
      K('x!', '!'), K('nPr', 'nPr('), K('nCr', 'nCr('), K('Abs', 'abs('), K('%', '%'), K('e', 'e')]],
    ['Trig', [K('sin⁻¹', 'asin('), K('cos⁻¹', 'acos('), K('tan⁻¹', 'atan('), K('sinh', 'sinh('), K('cosh', 'cosh('), K('tanh', 'tanh('),
      K('sinh⁻¹', 'asinh('), K('cosh⁻¹', 'acosh('), K('tanh⁻¹', 'atanh('), K('°', '°'), K('′', '′'), K('″', '″')]],
    ['Calculus', [K('∫', '∫(', 'f', '∫(f(x), a, b)'), K('d/dx', 'd/dx(', 'f', 'd/dx(f(x), at x)'), K('Σ', 'Σ(', 'f', 'Σ(f(x), from, to)'),
      K('Π', 'Π(', 'f', 'Π(f(x), from, to)'), K('x', 'x'), K('=', '=', 'f', 'Equation sign for SOLVE')]],
    ['Numbers', [K('Pol', 'Pol('), K('Rec', 'Rec('), K('GCD', 'GCD('), K('LCM', 'LCM('), K('Int', 'Int('), K('Intg', 'Intg('),
      K('÷R', 'Rem(', 'f', 'Division with remainder: Rem(a, b)'), K('FACT', 'FACT(', 'f', 'Prime factors'), K('Ran#', 'Ran#'),
      K('RanInt', 'RanInt('), K('Rnd', 'Rnd('), K('ENG', '@eng')]],
    ['Memory', [K('A', ' A'), K('B', ' B'), K('C', ' C'), K('D', ' D'), K('E', ' E'), K('F', ' F'),
      K('M', ' M'), K('x', ' x'), K('y', ' y'), K('STO', '→', 'f', 'Store: 5→A'), K('M+', '@m+'), K('M−', '@m-'),
      K('PreAns', 'PreAns'), K('°′″', '@dms', 'f', 'Show result in degrees, minutes, seconds'), K('CLR', '@clr', 'f', 'Clear all variables')]],
  ];
  const CMPLX_TAB = ['Complex', [K('i', 'i'), K('∠', '∠', 'f', 'Polar form: r∠θ'), K('arg', 'arg('), K('Conjg', 'conjg('), K('Re', 'ReP('), K('Im', 'ImP(')]];
  const NKEYS = [
    K('(', '(', 'op'), K(')', ')', 'op'), K(',', ',', 'op'), K('Ans', 'Ans', 'op'), K('×10ˣ', 'ᴇ', 'op'),
    K('7', '7', 'num'), K('8', '8', 'num'), K('9', '9', 'num'), K('DEL', '@del', 'del'), K('AC', '@ac', 'del'),
    K('4', '4', 'num'), K('5', '5', 'num'), K('6', '6', 'num'), K('×', '×', 'op big'), K('÷', '÷', 'op big'),
    K('1', '1', 'num'), K('2', '2', 'num'), K('3', '3', 'num'), K('+', '+', 'op big'), K('−', '−', 'op big'),
    K('0', '0', 'num'), K('.', '.', 'num'), K('(−)', '-', 'num', 'Negative sign'), K('=', '@exe', 'exe wide')];
  let tab = 'More';

  // Display flags double as the setup buttons: click DEG to cycle Deg/Rad/Gra, NORM1 to cycle formats.
  const ANGLES = ['deg', 'rad', 'gra'], FORMATS = ['norm1', 'norm2', 'fix', 'sci'];
  let setupQueue = Promise.resolve();  // fast clicks run one after another, each seeing the latest setting
  function flags() {
    const f = $('cFlags'); if (!f) return;
    const flag = (text, title, onClick) => {
      const b = el('button', 'flag', text); b.title = title;
      if (onClick) b.onclick = () => { setupQueue = setupQueue.then(onClick).catch(() => {}); };
      else b.disabled = true;
      return b;
    };
    f.replaceChildren(
      flag(mem.angle.toUpperCase(), 'Angle unit · click to switch Deg / Rad / Gra', async () => {
        mem = await calc('set_mode', { angle: ANGLES[(ANGLES.indexOf(mem.angle) + 1) % 3] }); flags();
      }),
      flag({ norm1: 'NORM1', norm2: 'NORM2', fix: 'FIX ' + mem.digits, sci: 'SCI ' + mem.digits }[mem.fmt], 'Number format · click to switch Norm 1 / Norm 2 / Fix / Sci', async () => {
        const next = FORMATS[(FORMATS.indexOf(mem.fmt) + 1) % 4];
        let digits = null;
        if (next === 'fix' || next === 'sci') {
          digits = prompt(t(next === 'fix' ? 'Fix: decimal places (0–9)' : 'Sci: significant digits (1–10)'), next === 'fix' ? '2' : '4');
          if (digits === null) return;
        }
        mem = await calc('set_mode', { fmt: next, digits }); flags(); renderAnswer();
      }),
      flag(exactView ? 'EXACT' : 'DECIMAL', 'Click (or S⇔D) to switch exact ⇄ decimal', () => key({ ins: '@sd' })),
      ...(mode === 'cmplx' ? [flag('CMPLX', 'Complex mode')] : []),
      ...(engView ? [flag('ENG', 'Engineering notation')] : []));
  }

  function renderAnswer() {
    const box = $('cAns'), sub = $('cSub'); if (!box) return;
    box.replaceChildren(); sub.textContent = '';
    if (!last) return;
    if (last.error) { box.append(el('span', 'err', last.error)); return; }
    if (last.pair) {
      box.append(...last.pair.flatMap(([k, v], i) => [el('span', 'muted', (i ? '   ' : '') + k + ' = '), valueNode(v)]));
      if (last.note) sub.textContent = last.note;
      return;
    }
    if (last.x) { box.append(el('span', 'muted', 'x = '), valueNode(last.x)); sub.textContent = 'L − R = ' + last.lr; return; }
    if (engView && last.eng) box.textContent = last.eng;
    else if (dmsView && last.dms) box.textContent = last.dms;
    else box.append(valueNode(last));
    const extra = [];
    if (last.polar) extra.push('Polar: ' + last.polar);
    if (last.exact && exactView) extra.push('≈ ' + last.decimal);
    if (last.stored) extra.push('Stored in ' + last.stored);
    if (last.memory) extra.push('M = ' + last.memory);
    sub.textContent = extra.join('   ·   ');
  }

  function addHistory(expr, r) {
    const list = history[mode];
    list.unshift({ expr, r });
    if (list.length > 60) list.pop();
    renderHistory();
  }
  function renderHistory() {
    const h = $('cHist'); if (!h) return;
    h.replaceChildren();
    for (const { expr, r } of history[mode]) {
      const d = el('div', 'hist'), a = el('div', 'a');
      if (r.error) a.append(el('span', 'err', r.error));
      else if (r.pair) a.textContent = r.pair.map(([k, v]) => `${k}=${v.decimal}`).join('  ');
      else if (r.x) a.textContent = 'x = ' + r.x.decimal;
      else a.append(valueNode(r));
      d.append(el('div', 'q', expr), a);
      d.onclick = () => { const i = $('cExpr'); i.value = expr; i.focus(); };
      h.append(d);
    }
    if (!history[mode].length) h.append(el('div', 'empty', 'Your calculations show up here.'));
  }
  async function renderVars() {
    mem = await calc('memory');
    const v = $('cVars'); if (!v) return;
    v.replaceChildren(...Object.entries(mem.vars).map(([k, val]) => { const d = el('div'); d.append(el('b', null, k), document.createTextNode(val)); d.title = k + ' = ' + val; return d; }));
    flags();
  }

  function insert(text) {
    const i = $('cExpr');
    const s = i.selectionStart ?? i.value.length, e = i.selectionEnd ?? i.value.length;
    i.value = i.value.slice(0, s) + text + i.value.slice(e);
    i.selectionStart = i.selectionEnd = s + text.length;
    i.focus();
  }
  async function key(k) {
    const i = $('cExpr');
    switch (k.ins) {
      case '@exe': return execute();
      case '@del': {
        const s = i.selectionStart, e = i.selectionEnd;
        if (s !== e) i.value = i.value.slice(0, s) + i.value.slice(e);
        else if (s > 0) { i.value = i.value.slice(0, s - 1) + i.value.slice(s); i.selectionStart = i.selectionEnd = s - 1; }
        return i.focus();
      }
      case '@ac': i.value = ''; last = null; renderAnswer(); return i.focus();
      case '@sd': exactView = !exactView; engView = dmsView = false; renderAnswer(); renderHistory(); flags(); return;
      case '@eng': engView = !engView; dmsView = false; renderAnswer(); flags(); return;
      case '@dms': dmsView = !dmsView; engView = false; renderAnswer(); return;
      case '@clr': if (confirm(t('Clear all variables, Ans and memory?'))) { await calc('clear_memory'); renderVars(); toast('Memory cleared'); } return;
      case '@m+': case '@m-': {
        const expr = i.value.trim() || 'Ans';
        last = await calc('memory_add', { text: expr, sign: k.ins === '@m+' ? 1 : -1 });
        renderAnswer(); renderVars(); return;
      }
      case '@solve': return solvePrompt();
      default: insert(k.ins);
    }
  }
  async function execute() {
    const expr = $('cExpr').value.trim();
    if (!expr) return;
    if (/=/.test(expr.replace(/→.*/, ''))) return solvePrompt();
    last = await calc('evaluate', { text: expr, complex_mode: mode === 'cmplx' });
    engView = dmsView = false;
    renderAnswer(); addHistory(expr, last); renderVars();
    $('cExpr').select();
  }
  function solvePrompt() {
    const row = $('cSolve'); row.hidden = false; $('cGuess').focus(); $('cGuess').select();
  }
  async function solve() {
    const expr = $('cExpr').value.trim();
    last = await calc('solve', { equation: expr, guess: $('cGuess').value || '0' });
    $('cSolve').hidden = true;
    renderAnswer(); addHistory(expr, last); renderVars();
  }

  function buildCalculate() {
    const wrap = el('div', 'calc'), left = el('div'), right = el('div');
    const screen = el('div', 'screen');
    const fl = el('div', 'screen-flags'); fl.id = 'cFlags';
    const expr = el('input', 'expr'); expr.id = 'cExpr'; expr.placeholder = mode === 'cmplx' ? 'e.g. (1+2i)(3−i)' : 'e.g. sin(30)+√(2)÷2';
    expr.spellcheck = false; expr.autocomplete = 'off';
    expr.onkeydown = e => {
      if (e.key === 'Enter') { e.preventDefault(); execute(); }
      if (e.key === 'Escape') { expr.value = ''; }
    };
    const ans = el('div', 'answer'); ans.id = 'cAns';
    const sub = el('div', 'answer-sub'); sub.id = 'cSub';
    const solveRow = el('div'); solveRow.id = 'cSolve'; solveRow.hidden = true;
    solveRow.style.cssText = 'display:flex; gap:8px; align-items:center; margin-top:10px';
    const guess = el('input', 'input'); guess.id = 'cGuess'; guess.value = '0'; guess.style.width = '120px';
    guess.onkeydown = e => { if (e.key === 'Enter') solve(); };
    const sb = el('button', 'btn primary', 'Solve'); sb.onclick = solve;
    solveRow.append(el('span', 'muted small', 'SOLVE for x · start near'), guess, sb);
    screen.append(fl, expr, ans, sub, solveRow);

    const keyBtn = k => { const b = el('button', 'key ' + k.cls, k.label); if (k.title) b.title = k.title; b.onclick = () => key(k); return b; };
    const quick = el('div', 'fkeys'); QUICK.forEach(k => quick.append(keyBtn(k)));

    const tabs = mode === 'cmplx' ? [CMPLX_TAB, ...TABS] : TABS;
    if (!tabs.some(([t]) => t === tab)) tab = tabs[0][0];
    const tabKeys = el('div', 'fkeys');
    const drawTab = () => { tabKeys.replaceChildren(...tabs.find(([t]) => t === tab)[1].map(keyBtn)); };
    const tabBar = seg('cTabs', tabs.map(([t]) => [t, t]), tab, v => { tab = v; drawTab(); });
    tabBar.classList.add('key-tabs');
    drawTab();

    const nk = el('div', 'nkeys'); NKEYS.forEach(k => nk.append(keyBtn(k)));

    const device = el('div', 'device');
    device.append(screen, quick, tabBar, tabKeys, nk);
    const help = el('p', 'muted small');
    help.innerHTML = 'Keyboard works too: <code>Enter</code> = calculate, <code>Esc</code> = clear · Store: <code>5→A</code> · Integral: <code>∫(x², 0, 3)</code> · SOLVE: <code>x²=2</code>';
    left.append(device, help);

    const hist = card('History'); const hl = el('div', 'history'); hl.id = 'cHist'; hist.append(hl);
    const clear = el('button', 'btn ghost', 'Clear'); clear.style.marginLeft = 'auto';
    clear.onclick = () => { history[mode] = []; renderHistory(); };
    hist.querySelector('h2').append(clear);
    const vars = card('Variables'); const vl = el('div', 'vars'); vl.id = 'cVars'; vars.append(vl);
    right.append(hist, vars);
    wrap.append(left, right);
    setTimeout(() => { flags(); renderAnswer(); renderHistory(); renderVars(); expr.focus(); });
    return wrap;
  }

  // ------------------------------------------------------------ Base-N
  function buildBaseN() {
    let base = 10;
    const box = answerBox();
    const input = el('input', 'input expr'); input.id = 'bExpr'; input.placeholder = 'e.g. FF + 1   ·   1010 and 0110   ·   not 5';
    input.style.cssText = 'padding:12px 14px; font-size:19px';
    const run = async () => {
      if (!input.value.trim()) return;
      const r = await calc('base_n', { text: input.value, base });
      showAnswer(box, r, b => b.append(results([['DEC', r.dec], ['HEX', r.hex], ['BIN', r.bin], ['OCT', r.oct]])));
    };
    input.onkeydown = e => { if (e.key === 'Enter') run(); };
    const keys = el('div', 'fkeys'); keys.style.marginTop = '12px';
    for (const t of ['A', 'B', 'C', 'D', 'E', 'F', 'and', 'or', 'xor', 'xnor', 'not', 'neg', '(', ')', '+', '−', '×', '÷']) {
      const b = el('button', 'key f', t);
      b.onclick = () => { input.value += /^[a-z]/.test(t) ? ` ${t} ` : t; input.focus(); };
      keys.append(b);
    }
    const go = el('button', 'btn primary big', 'Calculate'); go.onclick = run; go.style.marginTop = '14px';
    const c = card('Base-N (32-bit)',
      seg('bBase', [['10', 'DEC'], ['16', 'HEX'], ['2', 'BIN'], ['8', 'OCT']], '10', v => { base = +v; run(); }),
      el('p', 'muted small', 'Numbers are read in the selected base. Prefix d, h, b or o to mix bases (e.g. hFF + d1). Division drops the remainder, like the ClassWiz.'),
      input, keys, go, box);
    return c;
  }

  // ------------------------------------------------------------ Statistics
  function buildStats() {
    let two = false, useFreq = false, reg = 'lin';
    const wrap = el('div', 'grid-side'), left = el('div'), right = el('div');
    const table = el('table', 'table'), tbody = el('tbody');
    const rows = [];
    function addRow(x = '', y = '', f = '1') {
      const tr = el('tr'), cells = [x, y, f].map(v => { const td = el('td'), i = el('input', 'input'); i.value = v; td.append(i); return td; });
      const del = el('td'); del.append(miniBtn('x', 'Remove', () => { tr.remove(); rows.splice(rows.indexOf(tr), 1); }));
      tr.append(...cells, del); tbody.append(tr); rows.push(tr);
      cells[0].querySelector('input').onkeydown = e => { if (e.key === 'Enter') { addRow(); rows[rows.length - 1].querySelector('input').focus(); } };
      sync();
    }
    function sync() {
      table.querySelectorAll('th')[1].hidden = !two; table.querySelectorAll('th')[2].hidden = !useFreq;
      for (const tr of rows) { tr.children[1].hidden = !two; tr.children[2].hidden = !useFreq; }
      regWrap.hidden = !two; estWrap.hidden = !two;
    }
    const head = el('thead'), hr = el('tr'); ['x', 'y', 'Freq', ''].forEach(t => hr.append(el('th', null, t))); head.append(hr); table.append(head, tbody);
    const regSel = el('select', 'input'); for (const [k, v] of Object.entries(App.state.regressions)) { const o = el('option', null, v); o.value = k; regSel.append(o); }
    regSel.onchange = () => reg = regSel.value;
    const regWrap = el('label', 'field'); regWrap.append(el('span', null, 'Regression'), regSel);
    const estWrap = el('div', 'row-fields'); estWrap.append(field('Estimate ŷ for x =', 'sEx'), field('Estimate x̂ for y =', 'sEy'));
    const freq = el('label', 'check'); const fc = el('input'); fc.type = 'checkbox'; fc.onchange = () => { useFreq = fc.checked; sync(); }; freq.append(fc, document.createTextNode(' Frequency column'));
    const add = el('button', 'btn', '+ Row'); add.onclick = () => addRow();
    const paste = el('button', 'btn ghost', 'Paste numbers'); paste.title = 'Paste a column (or two columns) of numbers from Excel or text';
    paste.onclick = async () => {
      const text = await App.api.paste_text();
      const lines = (text || '').split(/\r?\n/).map(l => l.trim()).filter(Boolean);
      if (!lines.length) { toast('Clipboard has no numbers'); return; }
      tbody.replaceChildren(); rows.length = 0;
      for (const l of lines) { const p = l.split(/[\t;, ]+/); addRow(p[0] || '', p[1] || '', p[2] || '1'); }
      if (lines.some(l => l.split(/[\t;, ]+/).length > 1) && !two) { modeSeg.querySelector('[data-v="2"]').click(); }
    };
    const box = answerBox();
    const go = el('button', 'btn primary big', 'Calculate');
    go.onclick = async () => {
      const data = rows.map(tr => [...tr.querySelectorAll('input')].map(i => i.value.trim())).filter(r => r[0] !== '');
      if (!data.length) { toast('Enter some data first'); return; }
      const nums = data.map(r => r.map(Number));
      if (nums.some(r => isNaN(r[0]) || (two && isNaN(r[1])) || (useFreq && isNaN(r[2])))) { toast('Only numbers in the table, please'); return; }
      const args = { xs: nums.map(r => r[0]) };
      if (two) { args.ys = nums.map(r => r[1]); args.regression = reg; args.estimate_x = $('sEx').value || null; args.estimate_y = $('sEy').value || null; }
      if (useFreq) args.freqs = nums.map(r => r[2]);
      const r = await calc('statistics', args);
      showAnswer(box, r, b => {
        if (r.formula) b.append(el('div', 'muted small', r.formula));
        b.append(results(r.results));
        const est = [];
        if (r.y_hat) est.push(['ŷ', r.y_hat]); if (r.x_hat) est.push(['x̂', r.x_hat]);
        if (est.length) { b.append(el('div', 'label', 'Estimates')); b.append(results(est)); }
      });
    };
    const modeSeg = seg('sMode', [['1', '1-Variable'], ['2', '2-Variable']], '1', v => { two = v === '2'; sync(); });
    const tools = el('div'); tools.style.cssText = 'display:flex; gap:8px; align-items:center; flex-wrap:wrap; margin:12px 0';
    tools.append(modeSeg, freq, el('span', 'spacer'), add, paste);
    left.append(card('Data', tools, table));
    right.append(card('Calculation', regWrap, estWrap, go, box));
    go.style.cssText = 'width:100%; margin-top:14px';
    wrap.append(left, right);
    for (let k = 0; k < 4; k++) addRow();
    return wrap;
  }

  // ------------------------------------------------------------ Distribution
  function buildDist() {
    const TYPES = {
      normal_pd: ['Normal PD', [['x', 'x'], ['mu', 'μ', '0'], ['sigma', 'σ', '1']]],
      normal_cd: ['Normal CD', [['lower', 'Lower'], ['upper', 'Upper'], ['mu', 'μ', '0'], ['sigma', 'σ', '1']]],
      inverse_normal: ['Inverse Normal', [['area', 'Area (left tail)'], ['mu', 'μ', '0'], ['sigma', 'σ', '1']]],
      binomial_pd: ['Binomial PD', [['x', 'x (empty = list all)'], ['n', 'N'], ['p', 'p']]],
      binomial_cd: ['Binomial CD', [['x', 'x (empty = list all)'], ['n', 'N'], ['p', 'p']]],
      poisson_pd: ['Poisson PD', [['x', 'x'], ['lambda', 'λ']]],
      poisson_cd: ['Poisson CD', [['x', 'x'], ['lambda', 'λ']]],
    };
    let kind = 'normal_cd';
    const fields = el('div', 'row-fields'), box = answerBox();
    function draw() {
      fields.replaceChildren(...TYPES[kind][1].map(([k, label, def]) => field(label, 'd_' + k, def || '', 'text')));
      box.hidden = true;
    }
    const sel = el('select', 'input'); sel.style.maxWidth = '260px';
    for (const [k, [label]] of Object.entries(TYPES)) { const o = el('option', null, label); o.value = k; sel.append(o); }
    sel.value = kind; sel.onchange = () => { kind = sel.value; draw(); };
    const go = el('button', 'btn primary big', 'Calculate'); go.style.marginTop = '14px';
    go.onclick = async () => {
      const params = {};
      for (const [k] of TYPES[kind][1]) { const v = $('d_' + k).value.trim(); if (v !== '') params[k] = v; }
      const r = await calc('distribution', { kind, params });
      showAnswer(box, r, b => {
        if (r.list) { const t = el('table', 'table'); t.append(...r.list.map(([x, p]) => { const tr = el('tr'); tr.append(el('td', null, x), el('td', null, p)); return tr; })); b.append(t); }
        else b.append(results([[r.x !== undefined ? 'x' : 'P', r.x !== undefined ? r.x : r.p]]));
      });
    };
    draw();
    return card('Distribution', sel, el('div', null), fields, go, box);
  }

  // ------------------------------------------------------------ Equation
  function buildEqn() {
    let kind = 'poly', n = 2, rel = '>';
    const body = el('div'), box = answerBox();
    const SUP = ['', '', '²', '³', '⁴'];
    function draw() {
      body.replaceChildren(); box.hidden = true;
      if (kind === 'sim') {
        const t = el('table', 'table'), vars = ['x', 'y', 'z', 't'].slice(0, n);
        const hr = el('tr'); [...vars.map(v => 'coef. ' + v), '= c'].forEach(h => hr.append(el('th', null, h))); t.append(hr);
        for (let r = 0; r < n; r++) {
          const tr = el('tr');
          for (let c = 0; c <= n; c++) { const td = el('td'), i = el('input', 'input'); i.id = `e_${r}_${c}`; i.value = '0'; td.append(i); tr.append(td); }
          t.append(tr);
        }
        body.append(t);
      } else {
        const row = el('div', 'row-fields');
        for (let k = 0; k <= n; k++) {
          const power = n - k;
          row.append(field(String.fromCharCode(97 + k) + (power ? ' (x' + SUP[power] + ')' : ' (constant)'), 'e_c' + k, k === 0 ? '1' : '0'));
        }
        body.append(row);
        const preview = el('div', 'muted small'); preview.style.marginTop = '8px';
        preview.textContent = (kind === 'ineq' ? 'a·x' + SUP[n] + ' + … ' + rel + ' 0' : 'a·x' + SUP[n] + ' + … = 0');
        if (kind === 'ineq') body.prepend(seg('eRel', [['>', '> 0'], ['>=', '≥ 0'], ['<', '< 0'], ['<=', '≤ 0']], rel, v => { rel = v; preview.textContent = 'a·x' + SUP[n] + ' + … ' + rel + ' 0'; }));
        body.append(preview);
      }
    }
    const sizeSeg = el('div');
    function drawSize() {
      sizeSeg.replaceChildren(kind === 'sim'
        ? seg('eN', [['2', '2 unknowns'], ['3', '3 unknowns'], ['4', '4 unknowns']], String(n), v => { n = +v; draw(); })
        : seg('eN', [['2', 'Degree 2'], ['3', 'Degree 3'], ['4', 'Degree 4']], String(n), v => { n = +v; draw(); }));
    }
    const go = el('button', 'btn primary big', 'Solve'); go.style.marginTop = '14px';
    go.onclick = async () => {
      let r;
      if (kind === 'sim') {
        const rows = []; for (let i = 0; i < n; i++) { const row = []; for (let c = 0; c <= n; c++) row.push($(`e_${i}_${c}`).value.trim() || '0'); rows.push(row); }
        r = await calc('simultaneous', { rows });
        return showAnswer(box, r, b => r.message ? b.append(el('span', 'warn', r.message)) : b.append(results(r.solution.map(([k, v]) => [k, valueNode(v)]))));
      }
      const coeffs = []; for (let k = 0; k <= n; k++) coeffs.push($('e_c' + k).value.trim() || '0');
      if (kind === 'ineq') { r = await calc('inequality', { coeffs, relation: rel }); return showAnswer(box, r, b => b.append(document.createTextNode(r.text))); }
      r = await calc('polynomial', { coeffs });
      showAnswer(box, r, b => {
        b.append(results(r.roots.map((v, i) => ['x' + (i + 1), valueNode(v)])));
        if (r.vertex) { b.append(el('div', 'label', 'Vertex (' + r.vertex.kind + ')')); b.append(results([['x', valueNode(r.vertex.x)], ['y', valueNode(r.vertex.y)]])); }
      });
    };
    const kindSeg = seg('eKind', [['sim', 'Simultaneous'], ['poly', 'Polynomial'], ['ineq', 'Inequality']], kind, v => { kind = v; n = 2; drawSize(); draw(); });
    const top = el('div'); top.style.cssText = 'display:flex; gap:10px; flex-wrap:wrap; margin-bottom:14px'; top.append(kindSeg, sizeSeg);
    drawSize(); draw();
    return card('Equation', top, body, go, box);
  }

  // ------------------------------------------------------------ Matrix / Vector
  function buildMatrix(vector) {
    const names = vector ? ['VctA', 'VctB', 'VctC', 'VctD'] : ['MatA', 'MatB', 'MatC', 'MatD'];
    const dims = {}; names.forEach(nm => dims[nm] = vector ? [3, 1] : [2, 2]);
    const values = {}; let current = names[0];
    const editor = el('div'), box = answerBox();
    function grid() {
      const [r, c] = dims[current];
      const g = el('div', 'mat-grid'); g.style.gridTemplateColumns = `repeat(${vector ? r : c}, auto)`;
      const cells = vector ? r : r * c;
      for (let k = 0; k < cells; k++) {
        const i = el('input', 'input'); i.value = (values[current] || [])[k] ?? '0';
        i.oninput = () => { values[current] = [...g.querySelectorAll('input')].map(x => x.value); };
        g.append(i);
      }
      return g;
    }
    function draw() {
      editor.replaceChildren();
      const sizes = el('div'); sizes.style.cssText = 'display:flex; gap:10px; align-items:center; margin:10px 0';
      if (vector) {
        sizes.append(seg('vDim', [['2', '2D'], ['3', '3D']], String(dims[current][0]), v => { dims[current] = [+v, 1]; values[current] = null; draw(); }));
      } else {
        const rs = seg('mRows', [['1', '1'], ['2', '2'], ['3', '3'], ['4', '4']], String(dims[current][0]), v => { dims[current][0] = +v; values[current] = null; draw(); });
        const cs = seg('mCols', [['1', '1'], ['2', '2'], ['3', '3'], ['4', '4']], String(dims[current][1]), v => { dims[current][1] = +v; values[current] = null; draw(); });
        sizes.append(el('span', 'muted small', 'Rows'), rs, el('span', 'muted small', 'Columns'), cs);
      }
      const store = el('button', 'btn', 'Save ' + current);
      store.onclick = async () => {
        const [r, c] = dims[current];
        const flat = values[current] || Array(vector ? r : r * c).fill('0');
        const rows = vector ? [flat] : Array.from({ length: r }, (_, i) => flat.slice(i * c, i * c + c));
        const res = await calc('set_matrix', { name: current, rows });
        res.error ? toast(res.error) : toast(current + ' saved');
      };
      sizes.append(el('span', 'spacer'), store);
      editor.append(sizes, grid());
    }
    const pick = seg('mPick', names.map(nm => [nm, nm]), current, v => { current = v; draw(); });
    const input = el('input', 'input expr'); input.placeholder = vector ? 'e.g. cross(VctA, VctB)   ·   angle(VctA, VctB)' : 'e.g. MatA×MatB   ·   det(MatA)   ·   MatA⁻¹';
    input.style.cssText = 'padding:12px 14px; font-size:18px; margin-top:12px';
    const run = async () => {
      const r = await calc('matrix_eval', { text: input.value, vector });
      showAnswer(box, r, b => { if (r.matrix) b.innerHTML = r.matrix; else b.append(valueNode(r)); });
    };
    input.onkeydown = e => { if (e.key === 'Enter') run(); };
    const keys = el('div', 'fkeys'); keys.style.marginTop = '10px';
    const KEYS = vector ? [...names, 'VctAns', 'dot(', 'cross(', 'angle(', 'UnitV(', 'abs(', '+', '−', '×', ',', '(', ')']
      : [...names, 'MatAns', 'det(', 'Trn(', 'Identity(', '⁻¹', '²', '³', '+', '−', '×', '(', ')'];
    for (const t of KEYS) { const b = el('button', 'key f', t); b.onclick = () => { input.value += t; input.focus(); }; keys.append(b); }
    const go = el('button', 'btn primary big', 'Calculate'); go.style.marginTop = '12px'; go.onclick = run;
    const wrap = el('div', 'grid2');
    wrap.append(card(vector ? 'Edit vectors' : 'Edit matrices', pick, editor),
      card('Calculate', el('p', 'muted small', 'Save each ' + (vector ? 'vector' : 'matrix') + ' first, then calculate with it.'), input, keys, go, box));
    draw();
    return wrap;
  }

  // ------------------------------------------------------------ Table
  function buildTable() {
    const box = el('div');
    const go = el('button', 'btn primary big', 'Make table'); go.style.marginTop = '14px';
    go.onclick = async () => {
      const [f, g, s, e, st] = vals(['tF', 'tG', 'tStart', 'tEnd', 'tStep']);
      const r = await calc('table', { fx: f, gx: g, start: s, end: e, step: st });
      box.replaceChildren();
      if (r.error) { box.append(el('div', 'answer-box')); box.firstChild.append(el('span', 'err', r.error)); return; }
      const t = el('table', 'table'), hr = el('tr');
      ['x', 'f(x)', g ? 'g(x)' : null].filter(Boolean).forEach(h => hr.append(el('th', null, h))); t.append(hr);
      r.rows.forEach(row => { const tr = el('tr'); row.forEach(v => tr.append(el('td', null, v))); t.append(tr); });
      box.append(t);
    };
    const fns = el('div', 'row-fields'); fns.append(field('f(x) =', 'tF', 'x²+1'), field('g(x) = (optional)', 'tG', ''));
    const range = el('div', 'row-fields'); range.style.marginTop = '12px';
    range.append(field('Start', 'tStart', '1'), field('End', 'tEnd', '5'), field('Step', 'tStep', '1'));
    return card('Table', fns, range, go, box);
  }

  // ------------------------------------------------------------ Ratio
  function buildRatio() {
    let which = 'x:d';
    const box = answerBox(), inputs = el('div', 'row-fields');
    function draw() {
      inputs.replaceChildren(field('A', 'rA', '3'), field('B', 'rB', '4'), field(which === 'x:d' ? 'D' : 'C', 'rK', '12'));
      box.hidden = true;
    }
    const go = el('button', 'btn primary big', 'Solve'); go.style.marginTop = '14px';
    go.onclick = async () => {
      const [a, b, k] = vals(['rA', 'rB', 'rK']);
      const r = await calc('ratio', { a, b, c_or_d: k, which });
      showAnswer(box, r, bx => bx.append(results([['X', valueNode(r)]])));
    };
    draw();
    return card('Ratio', seg('rWhich', [['x:d', 'A : B = X : D'], ['c:x', 'A : B = C : X']], which, v => { which = v; draw(); }), el('div'), inputs, go, box);
  }

  // ------------------------------------------------------------ Constants & conversions
  function buildConst() {
    const wrap = el('div', 'grid2');
    const list = el('div', 'list'); let group = '';
    for (const [g, sym, name, value, unit] of App.state.constants) {
      if (g !== group) { group = g; list.append(el('div', 'label', g)); }
      const row = el('div', 'row'), meta = el('div', 'meta');
      meta.append(el('div', 'name', `${sym} · ${name}`), el('div', 'sub', `${value} ${unit}`));
      const use = el('button', 'btn ghost', 'Use'); use.title = 'Insert into Calculate';
      use.onclick = () => { switchMode('calc'); setTimeout(() => insert('(' + String(value).replace('e', 'ᴇ') + ')')); };
      row.append(meta, use, miniBtn('copy', 'Copy value', () => { App.api.copy_text(String(value)); toast('Copied'); }));
      list.append(row);
    }
    const conv = el('select', 'input');
    for (const [id, a, b] of App.state.conversions) { const o = el('option', null, `${a} → ${b}`); o.value = id; conv.append(o); }
    let reverse = false;
    const dir = el('button', 'btn', '⇄ Reverse'); dir.onclick = () => { reverse = !reverse; label(); run(); };
    const val = el('input', 'input'); val.value = '1'; val.style.fontSize = '18px';
    const out = el('div', 'answer-box');
    const lab = el('div', 'muted small'); lab.style.margin = '6px 0';
    function label() { const opt = App.state.conversions.find(c => c[0] === conv.value); lab.textContent = reverse ? `${opt[2]} → ${opt[1]}` : `${opt[1]} → ${opt[2]}`; }
    async function run() {
      const r = await App.api.calc_convert(val.value, conv.value, reverse);
      out.replaceChildren(r.error ? el('span', 'err', r.error) : document.createTextNode(`${val.value} ${r.from} = ${r.value} ${r.to}`));
    }
    conv.onchange = () => { label(); run(); }; val.oninput = run;
    const row = el('div'); row.style.cssText = 'display:flex; gap:10px'; row.append(conv, dir);
    wrap.append(card('Scientific constants (CODATA)', list),
      card('Unit conversions', row, lab, val, out));
    label(); run();
    return wrap;
  }

  // ------------------------------------------------------------ mode switching
  function switchMode(m) {
    mode = m;
    $('calcModes').querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === m));
    const body = $('calcBody');
    const build = { calc: buildCalculate, cmplx: buildCalculate, basen: buildBaseN, stat: buildStats, dist: buildDist, eqn: buildEqn,
      matrix: () => buildMatrix(false), vector: () => buildMatrix(true), table: buildTable, ratio: buildRatio, const: buildConst }[m];
    last = null;
    body.replaceChildren(build());
  }

  App.pages.calculator = {
    init() {
      mem = App.state.calc;
      const modes = $('calcModes');
      for (const [v, label] of MODES) { const b = el('button', v === mode ? 'on' : '', label); b.dataset.v = v; b.onclick = () => switchMode(v); modes.append(b); }
      switchMode('calc');
    },
    show() { const i = $('cExpr'); if (i) setTimeout(() => i.focus()); },
  };
})();
