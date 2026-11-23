// Upload page of a Swiftlane request link: sends each file as-is, one after another.
const cfg = document.getElementById('cfg').dataset, list = document.getElementById('list');
let budget = +cfg.max, slots = +cfg.files;
function fmt(n) { const u = ['B', 'KB', 'MB', 'GB']; let i = 0; while (n >= 1024 && i < 3) { n /= 1024; i++; } return (i ? n.toFixed(1) : n) + ' ' + u[i]; }
function row(name) {
  const r = document.createElement('div'); r.className = 'row';
  const n = document.createElement('span'); n.className = 'name'; n.textContent = name;
  const s = document.createElement('span'); s.className = 'size';
  r.append(n, s); list.append(r); list.hidden = false; return s;
}
function send(file, status) {
  return new Promise(done => {
    const x = new XMLHttpRequest();
    x.open('POST', cfg.url + '&name=' + encodeURIComponent(file.name));
    x.upload.onprogress = e => { if (e.lengthComputable) status.textContent = Math.round(100 * e.loaded / e.total) + '%'; };
    x.onload = () => { let m = ''; try { m = JSON.parse(x.responseText).error || ''; } catch (e) {} status.textContent = x.status === 200 ? 'Sent ✓' : (m || 'Failed'); done(x.status === 200); };
    x.onerror = () => { status.textContent = 'Failed'; done(false); };
    x.send(file);
  });
}
async function take(files) {
  for (const f of files) {
    const status = row(f.name);
    if (slots <= 0) { status.textContent = 'Not sent: no more files allowed'; continue; }
    if (f.size > budget) { status.textContent = 'Not sent: too big (' + fmt(f.size) + ')'; continue; }
    if (!f.size) { status.textContent = 'Not sent: empty file'; continue; }
    if (await send(f, status)) { budget -= f.size; slots -= 1; }
  }
}
document.getElementById('pick').onchange = e => take([...e.target.files]);
const drop = document.getElementById('drop');
drop.ondragover = e => { e.preventDefault(); };
drop.ondrop = e => { e.preventDefault(); take([...e.dataTransfer.files]); };
