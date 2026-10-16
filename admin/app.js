// The admin panel: one page, hash routes (#/accounts, #/player/<id>/buddies, ...), data from /admin/api/*, changes
// posted to /admin/<action> (each answers {ok, message}). No build step; the server embeds this file.
'use strict';

const app = document.getElementById('app');
const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const num = (n) => Number(n || 0).toLocaleString('en-US');
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const pad = (n) => String(n).padStart(2, '0');
function when(t, withTime = true) {
  if (!t || t <= 0) return 'never';
  const d = new Date(t * 1000);
  const day = `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
  return withTime ? `${day}, ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}` : day;
}
function ago(t, now) {
  if (!t || t <= 0) return 'never';
  const s = Math.max(0, (now || Date.now() / 1000) - t);
  if (s < 90) return 'now';
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  if (s < 2 * 86400) return 'yesterday';
  return `${Math.round(s / 86400)} days ago`;
}
function length(s) {
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  return d ? `${d} d ${h} h` : h ? `${h} h ${m} min` : `${m} min`;
}
const isoDay = (t) => { const d = new Date(t * 1000); return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`; };

// ---- talking to the server ----

class SignIn extends Error {}
let signedOut = false;

async function api(path) {
  const r = await fetch('/admin/api/' + path, { credentials: 'same-origin' });
  if (r.status === 401) { signedOut = true; throw new SignIn(); }
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  return r.json();
}

async function act(path, data, quiet) {
  const r = await fetch(path, {
    method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams(data),
  });
  if (r.status === 401 && path !== '/admin/signin') { signedOut = true; render(); return { ok: false }; }
  let j;
  try { j = await r.json(); } catch { j = { ok: r.ok, message: r.ok ? 'Done.' : `Not done (${r.status}).` }; }
  if (!quiet) toast(j.message, j.ok);
  return j;
}

function toast(message, ok = true) {
  const t = document.createElement('div');
  t.className = 'toast' + (ok ? '' : ' bad');
  t.textContent = message || (ok ? 'Done.' : 'Not done.');
  document.getElementById('toasts').append(t);
  setTimeout(() => t.remove(), ok ? 5000 : 9000);
}

// ---- names, avatars, pickers ----

let names = { items: {}, buddies: [], avatars: {} };
const itemName = (id) => names.items[id] || `Item ${id}`;

async function loadNames() {
  try { names = await api('names'); } catch (e) { if (e instanceof SignIn) throw e; }
  const items = Object.entries(names.items).sort((a, b) => a[1].localeCompare(b[1]));
  const opt = (id, n) => `<option value="${esc(n)} #${id}"></option>`;
  let dl = document.getElementById('pickers');
  if (!dl) { dl = document.createElement('div'); dl.id = 'pickers'; dl.className = 'hidden'; document.body.append(dl); }
  dl.innerHTML = `<datalist id="dl-items">${items.map(([id, n]) => opt(id, n)).join('')}</datalist>`
    + `<datalist id="dl-buddies">${names.buddies.map((b) => opt(b.id, b.name)).join('')}</datalist>`;
}

/** "Name #123", "123" or an exact name -> 123 */
function pickId(value, kind) {
  const v = String(value || '').trim();
  let m = v.match(/#(\d+)$/) || v.match(/^(\d+)$/);
  if (m) return m[1];
  const lower = v.toLowerCase();
  if (kind === 'buddy') { const b = names.buddies.find((x) => x.name.toLowerCase() === lower); return b ? String(b.id) : ''; }
  const hit = Object.entries(names.items).find(([, n]) => n.toLowerCase() === lower);
  return hit ? hit[0] : '';
}

function initials(name) {
  const w = String(name || '?').trim().split(/\s+/);
  return esc(((w[0] || '?')[0] + (w[1] ? w[1][0] : '')).toUpperCase());
}

/** An avatar item's icon (from the extractor's art), or the initials. */
function avatar(pic, name, cls = '') {
  const fallback = `<span class="avatar ${cls}" aria-hidden="true">${initials(name)}</span>`;
  if (!pic) return fallback;
  return `<img class="avatar ${cls}" src="/admin/art/avatar/${Number(pic)}.png" alt="" data-fallback="${esc(initials(name))}">`;
}
const buddyAvatar = (id, name, cls) => avatar(names.avatars[id] || 0, name, cls);

document.addEventListener('error', (e) => {
  const img = e.target;
  if (img.tagName === 'IMG' && img.dataset.fallback !== undefined) {
    const s = document.createElement('span');
    s.className = img.className;
    s.textContent = img.dataset.fallback;
    img.replaceWith(s);
  }
}, true);

const chip = (text, cls = '') => `<span class="chip ${esc(cls)}">${esc(text)}</span>`;
const kicker = (k, title, sub = '') => `<div class="t"><span class="kicker">${esc(k)}</span><h1>${esc(title)}</h1>${sub ? `<span class="muted">${sub}</span>` : ''}</div>`;

// an item row editor for mails and codes: picker + count, more rows on demand
function itemRows(list = [{}]) {
  const row = (it = {}) => `<div class="row irow"><label class="f" style="flex:1 1 220px">Item<input list="dl-items" data-pick="item" value="${it.id ? esc(`${itemName(it.id)} #${it.id}`) : ''}" placeholder="Start typing a name"></label>
    <label class="f" style="width:110px">Count<input data-count value="${esc(it.count || 1)}" inputmode="numeric"></label></div>`;
  return `<div data-items>${list.map(row).join('')}<button type="button" class="ghost small" data-add-row>Add an item</button><input type="hidden" name="items"></div>`;
}

// ---- the frame: side menu and routing ----

const NAV = [
  ['Overview'], ['dashboard', 'Dashboard', '#/'],
  ['Players'], ['accounts', 'Accounts', '#/accounts'], ['codes', 'Codes and mail', '#/codes'],
  ['Game'], ['events', 'Event calendar', '#/events'], ['guilds', 'Guilds', '#/guilds'],
  ['Server'], ['logs', 'Logs', '#/logs'], ['backups', 'Backups', '#/backups'], ['settings', 'Settings', '#/settings'], ['tools', 'Test tools', '#/debug'],
];
let badges = {};

function frame(active, body) {
  const nav = NAV.map(([id, label, href]) => {
    if (!label) return `<div class="nav-head">${esc(id)}</div>`;
    const b = badges[id] ? `<span class="badge ${badges[id].cls || ''}">${esc(badges[id].text)}</span>` : '';
    return `<a href="${href}" class="${id === active ? 'on' : ''}" ${id === active ? 'aria-current="page"' : ''}><span>${esc(label)}</span>${b}</a>`;
  }).join('');
  return `<div class="shell"><aside class="side">
    <div class="brand"><div class="diamond" aria-hidden="true"><span><b>XH</b></span></div><div><span class="kick">XXL Woofia server</span><span class="word">XXLHunks</span></div></div>
    <div class="strip" aria-hidden="true"></div>
    <nav class="nav" aria-label="Admin sections">${nav}</nav>
    <div class="side-foot"><div class="strip" aria-hidden="true" style="margin:0 0 8px"></div>
      <span><span class="dot" aria-hidden="true"></span>Server running</span>
      <button type="button" class="small" data-signout>Sign out</button></div>
  </aside><main class="main" id="main"><div class="wrap">${body}</div></main></div>`;
}

let timers = [];
const every = (ms, fn) => timers.push(setInterval(fn, ms));
function stopTimers() { timers.forEach(clearInterval); timers = []; }

const ROUTES = [
  [/^#?\/?$/, 'dashboard', dashboard],
  [/^#\/accounts$/, 'accounts', accountsPage],
  [/^#\/player\/(\d+)(?:\/(\w+))?$/, 'accounts', playerPage],
  [/^#\/events$/, 'events', eventsPage],
  [/^#\/guilds(?:\/(\d+))?$/, 'guilds', guildsPage],
  [/^#\/codes$/, 'codes', codesPage],
  [/^#\/logs(?:\/(\w+))?$/, 'logs', logsPage],
  [/^#\/backups$/, 'backups', backupsPage],
  [/^#\/settings$/, 'settings', settingsPage],
  [/^#\/debug(?:\/(\d+))?$/, 'tools', toolsPage],
];

let current = null;
async function render(keepScroll) {
  stopTimers();
  const y = keepScroll ? window.scrollY : 0;
  if (signedOut) { app.innerHTML = signInPage(); return; }
  const hash = location.hash || '#/';
  const route = ROUTES.find(([re]) => re.test(hash)) || ROUTES[0];
  const args = hash.match(route[0]).slice(1);
  current = route;
  try {
    if (!names.loaded) { await loadNames(); names.loaded = true; }
    const body = await route[2](...args);
    app.innerHTML = frame(route[1], body);
    if (route.after) route.after();
    applyFilters();
    const after = afterRender.splice(0);
    after.forEach((f) => f());
    window.scrollTo(0, y);
  } catch (e) {
    if (e instanceof SignIn) { app.innerHTML = signInPage(); return; }
    app.innerHTML = frame(route[1], `<div class="panel"><h2>Could not load this page</h2><p class="muted">${esc(e.message)}</p></div>`);
  }
}
const afterRender = [];
window.addEventListener('hashchange', () => render());

async function refreshBadges() {
  try {
    const o = await api('overview');
    badges = {};
    if (o.online.length) badges.accounts = { text: `${o.online.length} on` };
    if (o.running_events.length) badges.events = { text: `${o.running_events.length} live`, cls: 'purple' };
    if (o.unanswered) badges.logs = { text: `${o.unanswered}`, cls: 'red' };
    return o;
  } catch (e) { if (e instanceof SignIn) throw e; return null; }
}

// ---- forms: every <form data-act="/admin/..."> posts itself; buttons with data-act post their data-* fields ----

function formData(form) {
  form.querySelectorAll('[data-items]').forEach((box) => {
    const parts = [];
    box.querySelectorAll('.irow').forEach((r) => {
      const id = pickId(r.querySelector('[data-pick]').value, 'item');
      const n = r.querySelector('[data-count]').value.replace(/,/g, '').trim();
      if (id) parts.push(`${id}:${n || 1}`);
    });
    box.querySelector('input[name=items]').value = parts.join(',');
  });
  const data = new FormData(form);
  form.querySelectorAll('[data-pick][name]').forEach((el) => data.set(el.name, pickId(el.value, el.dataset.pick)));
  form.querySelectorAll('input[type=checkbox][name]').forEach((el) => { if (!el.checked) data.delete(el.name); });
  return data;
}

async function confirmBox(text) {
  const d = document.createElement('dialog');
  d.innerHTML = `<form method="dialog" style="display:flex;flex-direction:column;gap:18px"><p style="margin:0;line-height:1.5">${esc(text)}</p>
    <div class="row" style="justify-content:flex-end"><button value="no">Cancel</button><button value="yes" class="primary">Confirm</button></div></form>`;
  document.body.append(d);
  d.showModal();
  return new Promise((res) => d.addEventListener('close', () => { res(d.returnValue === 'yes'); d.remove(); }));
}

document.addEventListener('submit', async (e) => {
  const form = e.target;
  if (form.matches('[data-signin]')) { e.preventDefault(); return signIn(form); }
  if (!form.dataset.act) return;
  e.preventDefault();
  if (form.dataset.confirm && !(await confirmBox(form.dataset.confirm))) return;
  const btn = form.querySelector('button[type=submit], button:not([type])');
  if (btn) btn.disabled = true;
  const r = await act(form.dataset.act, formData(form));
  if (btn) btn.disabled = false;
  if (r.ok && form.dataset.go) location.hash = form.dataset.go;
  else if (r.ok || form.dataset.reload === 'always') render(true);
});

document.addEventListener('click', async (e) => {
  const el = e.target.closest('[data-act-btn], [data-go], [data-add-row], [data-signout], [data-pill]');
  if (!el) return;
  if (el.dataset.addRow !== undefined) {
    const box = el.closest('[data-items]');
    const first = box.querySelector('.irow');
    const copy = first.cloneNode(true);
    copy.querySelectorAll('input').forEach((i) => { i.value = i.dataset.count !== undefined ? '1' : ''; });
    el.before(copy);
    return;
  }
  if (el.dataset.signout !== undefined) {
    await act('/admin/signout', {}, true);
    signedOut = true;
    return render();
  }
  if (el.dataset.go) { location.hash = el.dataset.go; return; }
  if (el.dataset.pill) return pillClick(el);
  const { actBtn, confirm, ...rest } = el.dataset;
  if (confirm && !(await confirmBox(confirm))) return;
  el.disabled = true;
  const r = await act(actBtn, rest);
  el.disabled = false;
  if (r.ok) render(true);
});

// client-side filters: <div data-filter-scope> with pills [data-pill=key:value] and rows [data-tags]
const filters = {};
function pillClick(el) {
  const [key, value] = el.dataset.pill.split(':');
  filters[key] = value;
  el.parentElement.querySelectorAll('[data-pill]').forEach((b) => b.setAttribute('aria-pressed', b === el ? 'true' : 'false'));
  applyFilters();
}
function applyFilters() {
  const q = (document.querySelector('[data-search]')?.value || '').trim().toLowerCase();
  document.querySelectorAll('[data-tags]').forEach((row) => {
    const tags = row.dataset.tags.toLowerCase();
    let show = !q || tags.includes(q);
    for (const [k, v] of Object.entries(filters)) if (v && v !== 'all' && !tags.includes(`${k}:${v}`.toLowerCase())) show = false;
    row.classList.toggle('hidden', !show);
  });
  const shown = document.querySelectorAll('[data-tags]:not(.hidden)').length;
  const counter = document.querySelector('[data-shown]');
  if (counter) counter.textContent = `${shown} shown`;
}
document.addEventListener('input', (e) => { if (e.target.matches('[data-search]')) applyFilters(); });
const pills = (key, opts, on = 'all') => {
  filters[key] = filters[key] || on;
  return `<div class="pills" role="group">${opts.map(([v, label]) => `<button type="button" data-pill="${key}:${v}" aria-pressed="${filters[key] === v}">${esc(label)}</button>`).join('')}</div>`;
};

// ---- sign in ----

function signInPage() {
  return `<div class="signin"><div class="signin-in">
    <div class="art"><div class="art-frame"><img src="/admin/art/loading.jpg" alt="Ying Hugeeu and friends over the neon city, from the game's loading screen" onerror="this.outerHTML='<div class=noart>The extractor\\'s art step adds the game picture here.</div>'"></div>
      <div class="art-name"><span class="kicker">XXL Woofia server</span><span class="word">XXLHunks</span></div></div>
    <div class="dialog-wrap"><div class="dialog">
      <div class="ribbon"><span class="chk" aria-hidden="true"></span><span class="title">Admin Sign In</span><span class="chk" aria-hidden="true"></span></div>
      <form class="dialog-box" data-signin>
        <p style="margin:0;line-height:1.55;text-align:center;color:#E4DCEC">On the machine that runs the server this page opens without a key. From anywhere else, including a browser outside Docker, use the admin key from your settings (WOOFIA_ADMIN_KEY).</p>
        <label class="f"><span style="font-family:var(--cond);font-size:18px;font-weight:700;color:var(--mint-text)">Admin key</span>
          <input name="key" type="password" autocomplete="current-password" placeholder="16 characters or more" required style="background:#3A3442;border-color:#3A3442"></label>
        <label class="check"><input type="checkbox" name="keep" value="1"> Keep me signed in on this browser</label>
        <button class="primary" style="align-self:center;min-width:200px;font-size:20px">Confirm</button>
        <p class="small muted" style="margin:0;text-align:center">A wrong key is refused, and repeated tries are slowed down.</p>
      </form></div></div></div></div>`;
}

async function signIn(form) {
  const data = new FormData(form);
  if (!form.keep.checked) data.delete('keep');
  const r = await act('/admin/signin', data, true);
  if (r.ok) { signedOut = false; names.loaded = false; render(); } else toast(r.message, false);
}

// ---- dashboard ----

async function dashboard() {
  const o = await refreshBadges() || await api('overview');
  every(15000, () => { if (current && current[1] === 'dashboard') render(true); });
  const stats = [
    ['Players online', o.online.length, 'live game calls in the last 5 min'],
    ['Accounts', o.accounts, `${o.new_this_week} new this week`],
    ['Logged in today', o.logged_in_today, 'since the daily reset'],
    ['Unanswered calls', o.unanswered, 'since the last restart'],
    ['Guilds', o.guilds, `${o.escape_runs} Escape Room run(s) going`],
  ];
  const online = o.online.length ? `<div class="scroll"><table><thead><tr><th>Player</th><th>Lv</th><th>Where</th><th>Last call</th></tr></thead><tbody>
    ${o.online.map((p) => `<tr class="click" data-go="#/player/${p.id}"><td><span class="who">${avatar(p.pic, p.name, 'sm')}<a href="#/player/${p.id}">${esc(p.name || 'Unnamed')}</a><span class="mono dim small">${p.id}</span></span></td><td>${p.lv}</td><td>${esc(p.where)}</td><td class="muted">${ago(p.last_call, o.now)}</td></tr>`).join('')}
    </tbody></table></div>` : '<div class="empty">No one is playing right now.</div>';
  const events = o.running_events.length ? o.running_events.slice(0, 8).map((e) => {
    const always = !e.end || e.end - e.start > 300 * 86400;
    const pct = always ? 100 : Math.min(100, Math.max(0, ((o.now - e.start) / (e.end - e.start)) * 100));
    return `<div style="display:flex;flex-direction:column;gap:6px"><div class="kv"><span style="color:var(--text)">${esc(e.name)}</span><span>${always ? 'always on' : `${length(e.end - o.now)} left`}</span></div>
      <div class="bar-track"><div class="bar-fill" style="width:${pct}%"></div></div></div>`;
  }).join('') : '<div class="empty">No events running.</div>';
  const recent = o.recent_admin.length ? o.recent_admin.map((a) => `<div class="kv" style="justify-content:flex-start;gap:14px;padding:8px 0;border-bottom:1px solid var(--row)"><span class="mono small" style="flex:0 0 52px">${esc(when(a.time).split(', ')[1] || '')}</span><span style="color:var(--text)">${esc(a.text)}</span></div>`).join('') : '<div class="empty">No changes made from this panel yet.</div>';
  return `<div class="head">${kicker('Server', 'Dashboard', `Up ${length(o.now - o.up_since)} · game clock ${when(o.now)} UTC${o.clock_ahead ? ` (${length(o.clock_ahead)} ahead)` : ''}`)}
    <div class="row"><a class="btn" href="#/codes">Mail everyone</a><a class="btn" href="#/codes">New redeem code</a>
    <button class="primary" data-act-btn="/admin/backup" data-what="all">Snapshot now</button></div></div>
    <div class="stats">${stats.map(([l, v, n]) => `<div class="stat"><span class="label">${l}</span><span class="value">${num(v)}</span><span class="note">${n}</span></div>`).join('')}</div>
    <div class="grid" style="--min:440px">
      <section class="panel"><div class="row between"><h2>Online now</h2><a href="#/accounts" class="small">All accounts</a></div>${online}</section>
      <section class="panel"><div class="row between"><h2>Running events</h2><a href="#/events" class="small">Event calendar</a></div>${events}</section>
      <section class="panel"><div class="row between"><h2>Calls the server could not answer</h2><a href="#/logs/unanswered" class="small">Logs</a></div>
        ${o.unanswered ? `<p style="margin:0">${o.unanswered} call(s) since the last restart had no server logic.</p>` : '<div class="empty">None since the last restart.</div>'}</section>
      <section class="panel"><div class="row between"><h2>Recent admin actions</h2><a href="#/logs/admin" class="small">Full record</a></div><div>${recent}</div></section>
    </div>`;
}

// ---- accounts ----

async function accountsPage() {
  const [d] = await Promise.all([api('accounts'), refreshBadges()]);
  const rows = d.accounts.map((a) => {
    const tags = `${a.name} ${a.id} ${a.logins.join(' ')} status:${a.status} ${a.created > Date.now() / 1000 - 7 * 86400 ? 'status:new' : ''} ${a.status === 'banned' || a.status === 'suspended' ? 'status:blocked' : ''}`;
    return `<tr class="click" data-go="#/player/${a.id}" data-tags="${esc(tags)}">
      <td><span class="who">${avatar(a.pic, a.name)}<a href="#/player/${a.id}"><b>${esc(a.name || 'Unnamed')}</b></a></span></td>
      <td class="mono muted">${a.id}</td><td>${a.lv}</td><td class="muted">${esc(a.logins.join(', ') || 'device only')}</td>
      <td class="muted">${when(a.created, false)}</td><td class="muted">${a.status === 'online' ? esc(a.where || 'online') : ago(a.last_login)}</td>
      <td>${chip(a.status, a.status)}</td></tr>`;
  }).join('');
  return `<div class="head">${kicker('Players', 'Accounts', `${d.accounts.length} accounts, ${d.logins} logins. An account is made the first time a login reaches the game.`)}</div>
    <div class="row"><label class="f" style="flex:1 1 300px">Search by player name, login or account id<input type="search" data-search placeholder="e.g. Hunk or 100000002"></label>
      ${pills('status', [['all', 'All'], ['online', 'Online'], ['new', 'New this week'], ['blocked', 'Suspended or banned']])}</div>
    <section class="panel flush"><div class="bar"><span class="muted" data-shown>${d.accounts.length} shown</span></div>
      <div class="scroll"><table><thead><tr><th>Player</th><th>Account</th><th>Lv</th><th>Login</th><th>Made</th><th>Last seen</th><th>Status</th></tr></thead><tbody>${rows || ''}</tbody></table></div>
      ${d.accounts.length ? '' : '<div class="empty" style="margin:16px">No accounts yet.</div>'}</section>
    <div class="grid" style="--min:420px">
      <section class="panel"><h2>Add a login</h2><p class="small muted" style="margin:0">The login and password go on the game's login screen (the login looks like an email, e.g. name@woofia.test). Leave the account blank to start a new one.</p>
        <form data-act="/admin/login" class="fields"><label class="f">Login<input name="name" required></label><label class="f">Password<input name="password" type="password" autocomplete="new-password" required></label>
          <label class="f">Plays account (optional)<input name="account" placeholder="account id" inputmode="numeric"></label><button class="primary">Add login</button></form></section>
      <section class="panel"><h2>Move a device to another account</h2><p class="small muted" style="margin:0">Undo a link code, or point a client account (…@horny) at another game account by hand.</p>
        <form data-act="/admin/bind" class="fields"><label class="f">Client account<input name="name" placeholder="...@horny" required></label><label class="f">Plays account<input name="id" placeholder="account id" required></label><button>Bind</button></form></section>
    </div>`;
}

// ---- one player ----

const TABS = [['overview', 'Overview'], ['buddies', 'Buddies'], ['items', 'Items'], ['mailbox', 'Mailbox'], ['stages', 'Stages'], ['guild', 'Guild']];

function playerHeader(a, tab) {
  return `<a href="#/accounts" class="small" style="width:fit-content">Back to accounts</a>
    <div class="ph"><div class="row" style="gap:20px;align-items:center">
      <div class="frame">${avatar(a.pic, a.name)}<div class="lvd" aria-hidden="true"><span><b>${a.lv}</b></span></div></div>
      <div style="display:flex;flex-direction:column;gap:6px"><div class="row" style="align-items:center;gap:10px"><h1>${esc(a.name || 'Unnamed')}</h1>${chip(a.status, a.status)}</div>
        ${a.signature ? `<span class="sig">${esc(a.signature)}</span>` : ''}
        <span class="small muted">Account <span class="mono">${a.id}</span> · ${esc(a.logins.map((l) => l.login).join(', ') || 'device only')} · made ${when(a.created, false)} · last seen ${a.status === 'online' ? 'now' : ago(a.last_login)}</span></div></div>
      <div class="row"><a class="btn" href="#/player/${a.id}/mailbox">Send mail</a><button data-act-btn="/admin/backup" data-what="account" data-account="${a.id}">Snapshot</button></div></div>
    <nav class="tabs" aria-label="Player sections">${TABS.map(([id, l]) => `<a href="#/player/${a.id}/${id}" class="${id === tab ? 'on' : ''}">${l}</a>`).join('')}</nav>`;
}

async function playerPage(id, tab = 'overview') {
  const a = await api(`account/${id}`);
  const body = await ({ overview: pOverview, buddies: pBuddies, items: pItems, mailbox: pMail, stages: pStages, guild: pGuild }[tab] || pOverview)(a);
  return playerHeader(a, tab) + body;
}

function pOverview(a) {
  const wallet = [...a.wallet.map((w) => ({ ...w, form: '/admin/item', field: 'qty', extra: `<input type="hidden" name="id" value="${w.id}">` })),
    { name: 'H Coin', count: a.hcoin, form: '/admin/hcoin', field: 'qty', extra: '' }];
  const ban = a.ban ? `<div class="inset">${a.status === 'banned' ? 'Banned' : `Suspended until ${when(a.ban.until)} UTC`}${a.ban.reason ? ` · ${esc(a.ban.reason)}` : ''}
    <button class="small" style="margin-left:12px" data-act-btn="/admin/unban" data-account="${a.id}">Lift it</button></div>` : '';
  return `<div class="stats">${wallet.map((w) => `<form class="stat" data-act="${w.form}"><input type="hidden" name="account" value="${a.id}">${w.extra}
      <span class="label">${esc(w.name)}</span><span class="value">${num(w.count)}</span>
      <div class="row" style="gap:8px;flex-wrap:nowrap"><label class="sr" for="w-${esc(w.name)}">New ${esc(w.name)} count</label><input id="w-${esc(w.name)}" name="${w.field}" placeholder="set to" inputmode="numeric"><button>Set</button></div></form>`).join('')}</div>
    <div class="grid" style="--min:420px">
      <section class="panel"><h2>Profile</h2>
        <form data-act="/admin/rename" class="fields"><input type="hidden" name="account" value="${a.id}"><label class="f">Player name<input name="name" value="${esc(a.name)}"></label><button>Rename</button></form>
        <form data-act="/admin/level" class="fields"><input type="hidden" name="account" value="${a.id}"><label class="f">Account level<input name="lv" value="${a.lv}" inputmode="numeric"></label><button>Set level</button></form>
        <p class="small muted" style="margin:0">Towers open at levels 15, 25 and 30. The game shows changes after its next login.</p>
        <div style="display:flex;flex-direction:column;gap:8px;padding-top:10px;border-top:1px solid var(--line)">
          <div class="kv"><span>Hook-up entries left today</span><span>${a.hookups_left} (bought ${a.hookups_bought})</span></div>
          <div class="kv"><span>Main story</span><span>${esc(a.story || 'not started')}</span></div>
          <div class="kv"><span>Buddies owned</span><span>${a.buddies} (${a.buddies_total} can be summoned)</span></div>
          <div class="kv"><span>Guild</span><span>${a.guild ? `<a href="#/guilds/${a.guild.id}">${esc(a.guild.name)}</a> (${esc(a.guild.role)})` : 'none'}</span></div>
        </div></section>
      <section class="panel"><h2>Sign-in</h2>
        <div class="scroll"><table><thead><tr><th>Login or device</th><th>Kind</th><th></th></tr></thead><tbody>
          ${a.logins.map((l) => `<tr><td>${esc(l.login)}</td><td class="muted">Login and password</td><td><form data-act="/admin/password" class="row" style="gap:6px;flex-wrap:nowrap"><input type="hidden" name="login" value="${esc(l.login)}"><label class="sr" for="pw-${esc(l.login)}">New password</label><input id="pw-${esc(l.login)}" name="password" type="password" autocomplete="new-password" placeholder="new password" style="width:150px"><button class="small">Reset</button></form></td></tr>`).join('')}
          ${a.devices.map((d) => `<tr><td class="mono small">${esc(d)}</td><td class="muted">Device (link code or by hand)</td><td><button class="small" data-act-btn="/admin/unbind" data-name="${esc(d)}" data-confirm="This device stops playing the account; its next login starts a new account.">Unbind</button></td></tr>`).join('')}
          ${a.logins.length + a.devices.length ? '' : '<tr><td colspan="3" class="muted">Nothing plays this account.</td></tr>'}
        </tbody></table></div>
        ${a.link_codes.length ? `<div class="small muted">Link codes</div>${a.link_codes.map((c) => `<div class="inset kv"><span class="mono" style="color:var(--text)">${esc(c.code)}</span><span>${c.live ? `works until ${when(c.expires)}` : 'expired'}</span></div>`).join('')}` : ''}
        <form data-act="/admin/login" class="fields"><input type="hidden" name="account" value="${a.id}"><label class="f">Add a login for this account<input name="name" placeholder="name@woofia.test"></label><label class="f">Password<input name="password" type="password" autocomplete="new-password"></label><button>Add</button></form>
      </section></div>
    <section class="panel"><h2>Account status</h2>${ban}
      <div class="grid" style="--min:280px">
        <form class="box" data-act="/admin/ban"><input type="hidden" name="account" value="${a.id}"><b>Suspend</b><span class="small muted">Logins are refused until the date, and a game in progress is sent back to the login. Nothing is removed.</span>
          <label class="f">Until (UTC)<input type="date" name="until" required></label><label class="f">Reason (for the record)<input name="reason"></label><button class="warn" style="align-self:flex-start">Suspend</button></form>
        <form class="box" data-act="/admin/ban" data-confirm="Ban this account until you lift it?"><input type="hidden" name="account" value="${a.id}"><b>Ban</b><span class="small muted">Logins are refused until you lift it. The player sees "This account is banned." on the login screen.</span>
          <label class="f">Reason (for the record)<input name="reason"></label><button class="bad" style="align-self:flex-start">Ban</button></form>
        <form class="box" data-act="/admin/delete" data-go="#/accounts" style="background:#1F0F18;border-color:#6A2340"><input type="hidden" name="account" value="${a.id}"><b style="color:var(--red)">Delete account</b>
          <span class="small" style="color:var(--text2)">A snapshot is taken first, so Backups can bring it back. Its logins, devices, link codes and guild place go too.</span>
          <label class="f">Type the player name to confirm<input name="confirm" placeholder="${esc(a.name)}" required></label><button class="fill-bad" style="align-self:flex-start">Delete with backup</button></form>
      </div></section>`;
}

async function pBuddies(a) {
  const d = await api(`account/${a.id}/buddies`);
  const rows = d.buddies.map((b) => `<tr data-tags="${esc(`${b.name} ${b.id} rarity:${b.rarity}`)}">
    <td><span class="who">${buddyAvatar(b.id, b.name, 'sm')}<b>${esc(b.name)}</b><span class="mono dim small">${b.id}</span></span></td><td>${chip(b.rarity, b.rarity)}</td>
    <td>${b.lv}</td><td>${b.limit_break}</td><td>${b.rank}</td><td>${esc((b.skills || []).join(' / '))}</td><td>${b.bond} / ${b.bond_max}</td><td class="muted">${b.outfit ? `outfit ${b.outfit}` : 'default'}</td>
    <td><details><summary class="small" style="cursor:pointer;color:var(--link)">Edit</summary>
      <form data-act="/admin/char" class="fields" style="--fmin:90px;margin-top:8px"><input type="hidden" name="account" value="${a.id}"><input type="hidden" name="id" value="${b.id}">
        <label class="f">Level<input name="lv" value="${b.lv}"></label><label class="f">Rank<input name="rank" value="${b.rank}"></label><label class="f">Limit break<input name="break" value="${b.limit_break}"></label><button class="small">Save</button></form>
      <form data-act="/admin/bond" class="fields" style="--fmin:90px;margin-top:8px"><input type="hidden" name="account" value="${a.id}"><input type="hidden" name="char" value="${b.id}">
        <label class="f">Bond level<input name="lv" value="${b.bond}"></label><label class="f">EXP into it<input name="exp" value="${b.bond_exp}"></label><button class="small">Set bond</button></form>
      <button class="small" style="margin-top:8px" data-act-btn="/admin/maxout" data-account="${a.id}" data-char="${b.id}">Max out</button></details></td></tr>`).join('');
  return `<section class="panel"><div class="row">
      <form data-act="/admin/char" class="row" style="flex:1 1 420px"><input type="hidden" name="account" value="${a.id}">
        <label class="f" style="flex:1 1 260px">Give a buddy<input list="dl-buddies" data-pick="buddy" name="id" placeholder="Start typing a name, e.g. Kumoyama" required></label><button class="primary">Give</button></form>
      <button data-act-btn="/admin/all-summons" data-account="${a.id}" data-confirm="Give every buddy any banner can give, all maxed out?">Give every summonable buddy</button></div></section>
    <div class="row between">${pills('rarity', [['all', `All ${d.buddies.length}`], ['XXL', 'XXL'], ['XL', 'XL'], ['L', 'L'], ['M', 'M']])}
      <div class="row"><form data-act="/admin/maxout" class="row" style="gap:8px"><input type="hidden" name="account" value="${a.id}">
        <label class="f">Max out every owned buddy of<select name="rarity"><option value="4">XXL</option><option value="3">XL</option><option value="2">L</option><option value="1">M</option></select></label><button>Max out</button></form>
        <button data-act-btn="/admin/outfits-reset" data-account="${a.id}" data-confirm="Every buddy back to the default outfit?">Reset outfits</button></div></div>
    <section class="panel flush"><div class="bar"><span class="muted" data-shown>${d.buddies.length} shown</span><span class="small muted">${d.total} buddies can be summoned</span></div>
      <div class="scroll"><table><thead><tr><th>Buddy</th><th>Rarity</th><th>Level</th><th>Limit break</th><th>Rank</th><th>Skills</th><th>Bond</th><th>Outfit</th><th></th></tr></thead><tbody>${rows}</tbody></table></div></section>`;
}

async function pItems(a) {
  const d = await api(`account/${a.id}/items`);
  const kinds = [...new Set(d.items.map((i) => i.kind))].sort();
  const rows = d.items.map((i) => `<tr data-tags="${esc(`${i.name} ${i.id} kind:${i.kind}`)}"><td><b>${esc(i.name)}</b> <span class="mono dim small">${i.id}</span></td><td class="muted">${esc(i.kind)}</td><td class="n mono">${num(i.count)}</td>
    <td><form data-act="/admin/item" class="row" style="gap:6px;flex-wrap:nowrap"><input type="hidden" name="account" value="${a.id}"><input type="hidden" name="id" value="${i.id}"><label class="sr" for="it-${i.id}">New count for ${esc(i.name)}</label><input id="it-${i.id}" name="qty" style="width:120px" inputmode="numeric"><button class="small">Set</button></form></td></tr>`).join('');
  return `<div class="row"><label class="f" style="flex:1 1 300px">Find an item in the bag<input type="search" data-search placeholder="Name or id"></label>
      ${pills('kind', [['all', 'Everything'], ...kinds.map((k) => [k, k])])}</div>
    <div class="grid" style="--min:340px">
      <section class="panel flush span2"><div class="bar"><span class="muted" data-shown>${d.items.length} shown</span></div><div class="scroll"><table><thead><tr><th>Item</th><th>Kind</th><th class="n">Count</th><th>Set to</th></tr></thead><tbody>${rows}</tbody></table></div></section>
      <section class="panel"><h2>Set an item</h2>
        <form data-act="/admin/item" style="display:flex;flex-direction:column;gap:12px"><input type="hidden" name="account" value="${a.id}">
          <label class="f">Item<input list="dl-items" data-pick="item" name="id" placeholder="Start typing, e.g. Energy Drink" required></label><label class="f">Count<input name="qty" value="1" inputmode="numeric"></label>
          <button class="primary">Put in the bag</button></form>
        <p class="small muted" style="margin:0">This sets the count. To give on top of what they have, mail it from the Mailbox tab.</p></section></div>`;
}

async function pMail(a) {
  const d = await api(`account/${a.id}/mail`);
  const waiting = d.mail.filter((m) => m.state === 'unclaimed').length;
  const rows = d.mail.map((m) => `<tr><td><b>${esc(m.subject)}</b><div class="small muted">${esc(m.content).slice(0, 120)}</div></td>
    <td class="muted">${m.items.length ? esc(m.items.map((i) => `${i.name} ×${num(i.count)}`).join(', ')) : 'none'}</td><td class="muted">${when(m.sent)}</td>
    <td>${chip(m.state, m.state)}</td><td>${m.state === 'unclaimed' || m.state === 'unread' ? `<button class="small" data-act-btn="/admin/mail-take" data-account="${a.id}" data-sn="${esc(m.sn)}" data-confirm="Take this mail back out of the mailbox?">Take back</button>` : ''}</td></tr>`).join('');
  return `<div class="grid" style="--min:380px">
    <section class="panel flush span2"><div class="bar"><h2>In the mailbox</h2><span class="small muted">${waiting} unclaimed · mail over the bag's limit stays unclaimed</span></div>
      <div class="scroll"><table><thead><tr><th>Mail</th><th>Items</th><th>Sent</th><th>State</th><th></th></tr></thead><tbody>${rows}</tbody></table></div>${d.mail.length ? '' : '<div class="empty" style="margin:16px">The mailbox is empty.</div>'}</section>
    <section class="panel"><h2>Send mail to ${esc(a.name || 'this player')}</h2>
      <form data-act="/admin/mail-one" style="display:flex;flex-direction:column;gap:12px"><input type="hidden" name="account" value="${a.id}">
        <label class="f">Title<input name="subject" placeholder="A gift from the server" required></label><label class="f">Text<textarea name="content"></textarea></label>
        <span class="small muted">Items (none: a notice)</span>${itemRows([{}])}
        <label class="f">Expires after<select name="days"><option value="30">30 days</option><option value="7">7 days</option><option value="365">a year</option><option value="0">never</option></select></label>
        <button class="primary">Send</button></form></section></div>`;
}

async function pStages(a) {
  const d = await api(`account/${a.id}/stages`);
  const story = d.chapters.filter((c) => c.story);
  const other = d.chapters.filter((c) => !c.story && c.cleared > 0);
  const done = story.reduce((s, c) => s + c.cleared, 0), all = story.reduce((s, c) => s + c.stages, 0), stars = story.reduce((s, c) => s + c.stars, 0);
  return `<div class="grid" style="--min:380px">
    <section class="panel span2"><div class="row between"><h2>Main story</h2><span class="small muted">${done} of ${all} stages cleared · ${stars} stars</span></div>
      ${story.map((c) => `<div class="row" style="flex-wrap:nowrap;align-items:center;gap:14px"><span style="flex:0 0 96px">Chapter ${c.chapter}</span><div class="bar-track" style="flex:1 1 auto"><div class="bar-fill" style="width:${c.stages ? (c.cleared / c.stages) * 100 : 0}%"></div></div><span class="mono small muted" style="flex:0 0 120px;text-align:right">${c.cleared} / ${c.stages} · ${c.stars}★</span></div>`).join('')}</section>
    <section class="panel"><h2>Clear stages</h2>
      <button data-act-btn="/admin/story" data-account="${a.id}" data-confirm="Mark every main story stage cleared with 3 stars (no rewards)?" style="align-self:flex-start">Clear the whole main story</button>
      <form data-act="/admin/story" class="fields"><input type="hidden" name="account" value="${a.id}"><label class="f">Clear one stage (stage id)<input name="stage" placeholder="e.g. 1003325" required></label><button>Clear</button></form>
      <span class="small muted">Some stages open things: 1003325 opens the Crocodile Diner vote.</span>
      <button data-act-btn="/admin/unfinished" data-account="${a.id}" style="align-self:flex-start">Clear unfinished stages</button>
      <span class="small muted">Drops stage records never cleared and the run in progress, for a player stuck in a stage.${d.pending ? ` In progress now: stage ${d.pending.stage}.` : ''}</span>
      <form data-act="/admin/chapter-reset" class="fields" data-confirm="Remove this chapter's progress?"><input type="hidden" name="account" value="${a.id}"><label class="f">Reset a chapter's progress<input name="chapter" placeholder="chapter" required></label><button class="warn">Reset</button></form></section>
    <section class="panel"><h2>Other stages played</h2>${other.length ? other.map((c) => `<div class="inset kv"><span>Chapter ${c.chapter}</span><span>${c.cleared} of ${c.stages} cleared</span></div>`).join('') : '<div class="empty">None yet.</div>'}</section></div>`;
}

async function pGuild(a) {
  const d = await api(`account/${a.id}/guild`);
  const rejoin = `<button data-act-btn="/admin/rejoin-now" data-account="${a.id}">Allow joining a guild now</button>`;
  const energy = `<form data-act="/admin/item" class="fields"><input type="hidden" name="account" value="${a.id}"><input type="hidden" name="id" value="29"><label class="f">Escape Energy (now ${d.energy})<input name="qty" value="${d.energy}"></label><button>Set</button></form>`;
  if (!d.guild) {
    return `<section class="panel"><h2>Not in a guild</h2><p class="muted" style="margin:0">${d.left_at ? `Left a guild ${ago(d.left_at)}; joining another waits 2 hours.` : 'Never joined one here.'}</p><div class="row">${rejoin}</div>${energy}</section>`;
  }
  const g = d.guild;
  const e = d.escape || {};
  return `<div class="grid" style="--min:380px">
    <section class="panel"><div class="row between"><h2>${esc(g.name)}</h2><a href="#/guilds/${g.id}" class="small">Open the guild</a></div>
      <div class="kv"><span>Role</span><span>${esc(d.role)}</span></div><div class="kv"><span>Members</span><span>${g.members}</span></div>
      <div class="kv"><span>Leader</span><span>${esc(g.leader_name)}</span></div><div class="kv"><span>Activity this week</span><span>${num(d.activity)}</span></div>
      <form data-act="/admin/guild-role" class="fields" style="padding-top:12px;border-top:1px solid var(--line)"><input type="hidden" name="guild" value="${g.id}"><input type="hidden" name="member" value="${a.id}">
        <label class="f">Role<select name="role">${[['member', 'Member'], ['vice', 'Vice leader'], ['leader', 'Leader']].map(([v, l]) => `<option value="${v}" ${l === d.role ? 'selected' : ''}>${l}</option>`).join('')}</select></label><button>Set role</button></form>
      <button class="bad" style="align-self:flex-start" data-act-btn="/admin/guild-kick" data-guild="${g.id}" data-member="${a.id}" data-confirm="Take this player out of the guild? Escape Energy goes to 0 and joining another waits 2 hours.">Remove from the guild</button></section>
    <section class="panel"><h2>This player in the guild</h2>
      <div class="inset kv"><span>Escape Energy</span><span>${d.energy}</span></div>
      <div class="inset kv"><span>Stars donated today</span><span>${d.stars_today}</span></div>
      <div class="inset kv"><span>Wish</span><span>${d.wish ? `${esc(d.wish.name)} (${d.wish.stars} star(s))` : 'none'}</span></div>
      <div class="inset kv"><span>Escape Room</span><span>${esc(e.state || 'none')}${e.ends ? ` until ${when(e.ends)}` : ''}</span></div>
      <div class="row">${rejoin}</div>${energy}</section></div>`;
}

// ---- event calendar ----

let calPick = null;
async function eventsPage() {
  const d = await api('events');
  const now = d.now;
  const day0 = Math.floor(now / 86400) * 86400 - 3 * 86400;
  const span = 28 * 86400;
  const days = Array.from({ length: 28 }, (_, i) => {
    const t = day0 + i * 86400, dt = new Date(t * 1000);
    return `<span class="cal-day ${i === 3 ? 'today' : ''}">${dt.getUTCDate() === 1 || i === 0 ? `${MONTHS[dt.getUTCMonth()]} ` : ''}${dt.getUTCDate()}</span>`;
  }).join('');
  const kindClass = (e) => e.kinds.includes('summon banner') ? 'k-summon' : e.kinds.includes('sign-in') ? 'k-signin' : e.kinds.length ? 'k-event' : 'k-other';
  const shown = d.window.filter((e) => !e.part && !e.newcomer && (e.end <= 0 || e.end > day0) && e.start < day0 + span);
  if (!calPick || !shown.concat(d.past).find((e) => e.id === calPick)) calPick = shown[0]?.id;
  const rows = shown.map((e) => {
    const s = Math.max(0, Math.floor((e.start - day0) / 86400));
    const end = e.end <= 0 ? 28 : Math.min(28, Math.ceil((e.end - day0) / 86400));
    const always = e.end <= 0 || e.end - e.start > 300 * 86400;
    return `<div class="cal-row"><button type="button" class="cal-name" data-cal="${e.id}">${esc(e.name)}</button>
      <button type="button" class="cal-bar ${kindClass(e)} ${e.id === calPick ? 'sel' : ''}" data-cal="${e.id}" style="grid-column:${s + 2} / ${Math.max(s + 3, end + 2)}" aria-label="${esc(e.name)}">${always ? 'always on' : `${Math.round((e.end - e.start) / 86400)} days${e.scheduled ? ' · rerun' : ''}`}</button></div>`;
  }).join('');
  const sel = shown.concat(d.past).find((e) => e.id === calPick);
  const detail = sel ? `<section class="panel"><div class="row between"><h2>${esc(sel.name)}</h2>${chip(sel.state, sel.state)}</div>
      <span class="small muted">${esc(sel.kinds.join(', ') || 'event')} · ${when(sel.start)} to ${sel.end > 0 ? when(sel.end) : 'no end'} UTC${sel.scheduled ? ' · rerun on this server' : ' · the game\'s own dates'}</span>
      <form data-act="/admin/events" class="fields"><input type="hidden" name="id" value="${sel.id}">
        <label class="f">Starts (UTC; blank: now)<input type="date" name="date" value="${sel.scheduled ? isoDay(sel.start) : ''}"></label>
        <label class="f">Days (blank: the original length)<input name="days" inputmode="numeric" value="${sel.scheduled ? Math.round((sel.end - sel.start) / 86400) : ''}"></label><button class="primary">${sel.scheduled ? 'Save dates' : 'Run it on these dates'}</button></form>
      ${sel.scheduled ? `<button style="align-self:flex-start" data-act-btn="/admin/events" data-remove="${sel.id}" data-confirm="Take this event off the schedule? It goes back to the game's own dates.">Remove from the schedule</button>` : ''}
      <span class="small muted">Players see new dates after their next login. The server writes them into the game's tables.${sel.art !== 'ok' && sel.art !== 'none' ? ` Art: ${esc(sel.art)}${sel.art_notes ? ` (${esc(sel.art_notes)})` : ''}.` : ''}</span></section>` : '<section class="panel"><div class="empty">Pick an event.</div></section>';
  afterRender.push(() => document.querySelectorAll('[data-cal]').forEach((b) => b.addEventListener('click', () => { calPick = Number(b.dataset.cal); render(true); })));
  const past = d.past.map((e) => `<div class="row between" data-tags="${esc(`${e.name} ${e.id} ${e.kinds.join(' ')}`)}" style="padding:8px 0;border-bottom:1px solid var(--row);align-items:center">
      <div style="display:flex;flex-direction:column;gap:2px"><span>${esc(e.name)}</span><span class="small muted">${esc(e.kinds.join(', ') || 'event')} · ran ${when(e.start, false)}${e.art !== 'ok' && e.art !== 'none' ? ` · art ${esc(e.art)}` : ''}</span></div>
      <div class="row" style="gap:6px"><button class="small" data-cal="${e.id}">Dates</button><button class="small" data-act-btn="/admin/events" data-id="${e.id}">Rerun now</button></div></div>`).join('');
  return `<div class="head">${kicker('Calendar', 'Event calendar', 'Three days back to four weeks ahead. Pick an event to change its dates; times are UTC.')}
      <button data-act-btn="/admin/events" data-clear="1" data-confirm="Clear every rerun? Events go back to the game's own dates.">Clear the schedule</button></div>
    <section class="panel" style="overflow-x:auto"><div class="cal"><div class="cal-row" style="min-height:24px"><span></span>${days}</div>${rows || '<div class="empty">Nothing on the calendar.</div>'}</div></section>
    <div class="grid" style="--min:400px">${detail}
      <section class="panel"><h2>Run a past event again</h2><label class="f">Find<input type="search" data-search placeholder="Event name"></label><div style="max-height:520px;overflow:auto">${past}</div></section></div>`;
}

// ---- guilds ----

async function guildsPage(gid) {
  const d = await api('guilds');
  const pick = gid ? Number(gid) : d.guilds[0]?.id;
  const list = d.guilds.map((g) => `<a href="#/guilds/${g.id}" class="btn" style="justify-content:flex-start;flex-direction:column;align-items:flex-start;padding:10px 12px;height:auto;${g.id === pick ? 'background:var(--chip);border-color:var(--magenta)' : 'background:transparent;border-color:transparent'}">
      <b>${esc(g.name)}</b><span class="small muted">${g.members} member(s) · Lv ${g.lv} · Escape Room ${esc(g.escape.state)}</span></a>`).join('');
  if (!pick) return `<div class="head">${kicker('Scene', 'Guilds')}</div><div class="empty">No guilds yet.</div>`;
  const g = await api(`guild/${pick}`);
  const e = g.escape;
  const leader = g.leader;
  const rows = g.members.map((m) => `<tr><td><span class="who">${avatar(m.pic, m.name, 'sm')}<a href="#/player/${m.id}">${esc(m.name || m.id)}</a>${m.online ? chip('online', 'online') : ''}</span></td><td>${m.lv}</td>
    <td><form data-act="/admin/guild-role" class="row" style="gap:6px;flex-wrap:nowrap"><input type="hidden" name="guild" value="${g.id}"><input type="hidden" name="member" value="${m.id}">
      <label class="sr" for="r-${m.id}">Role</label><select id="r-${m.id}" name="role">${[['leader', 'Leader'], ['vice', 'Vice leader'], ['member', 'Member']].map(([v, l]) => `<option value="${v}" ${l === m.role ? 'selected' : ''}>${l}</option>`).join('')}</select><button class="small">Set</button></form></td>
    <td class="muted">${m.online ? 'now' : ago(m.last_login)}</td>
    <td><button class="small bad" data-act-btn="/admin/guild-kick" data-guild="${g.id}" data-member="${m.id}" data-confirm="Take ${esc(m.name)} out of the guild?">Remove</button></td></tr>`).join('');
  return `<div class="head">${kicker('Scene', 'Guilds', 'Members, roles and the Escape Room for every guild on the server.')}</div>
    <div class="row" style="align-items:flex-start;gap:16px">
      <nav class="panel" aria-label="Guild list" style="flex:1 1 260px;padding:8px;gap:4px">${list}</nav>
      <div style="flex:999 1 560px;min-width:0;display:flex;flex-direction:column;gap:16px">
        <section class="panel flush"><div class="bar"><h2>${esc(g.name)}</h2><div class="row">
            <form data-act="/admin/guild-rename" class="row" style="gap:6px"><input type="hidden" name="guild" value="${g.id}"><label class="sr" for="gname">New name</label><input id="gname" name="name" placeholder="new name" style="width:180px"><button>Rename</button></form>
            <button class="bad" data-act-btn="/admin/guild-disband" data-guild="${g.id}" data-confirm="Close ${esc(g.name)}? Every member leaves.">Disband</button></div></div>
          <div class="scroll"><table><thead><tr><th>Member</th><th>Lv</th><th>Role</th><th>Last seen</th><th></th></tr></thead><tbody>${rows}</tbody></table></div></section>
        <section class="panel"><div class="row between"><h2>Escape Room</h2>${chip(`${e.state}${e.held ? ' (held)' : ''}`, e.state === 'Active' ? 'online' : '')}</div>
          <span class="small muted">${e.ends ? `Until ${when(e.ends)} UTC · ` : ''}level ${e.level || 1} · ${e.stars || 0} stars · ${e.keys || 0} keys in the bag · ${e.players || 0} player(s) in this run</span>
          <div class="grid" style="--min:240px">
            <form data-act="/admin/escape-end" class="fields"><input type="hidden" name="account" value="${leader}"><label class="f">Run<select name="end"><option value="1">End now (tally, mail rewards)</option><option value="0">Open again</option><option value="prepare">Hold in Preparing (star donations)</option><option value="progress">Hold in Active</option></select></label><button>Set</button></form>
            <form data-act="/admin/escape-keys" class="fields"><input type="hidden" name="account" value="${leader}"><label class="f">Keys into the shared bag (of each)<input name="n" value="3"></label><button>Give</button></form></div>
          <div class="row" style="padding-top:12px;border-top:1px solid var(--line);align-items:center"><button data-act-btn="/admin/leader-away" data-account="${g.members.find((m) => m.id !== leader)?.id || leader}">Make the leader absent</button>
            <span class="small muted">Leader last seen 8 days ago, so another member's next guild call takes over.</span></div></section>
      </div></div>`;
}

// ---- codes and mail ----

async function codesPage() {
  const [c, m, gl] = await Promise.all([api('codes'), api('groupmail'), api('guilds')]);
  const codes = c.codes.map((x) => `<tr><td class="mono"><b>${esc(x.code)}</b></td><td class="muted">${esc(x.rewards.map((r) => `${r.name} ×${num(r.count)}`).join(', '))}</td>
    <td class="mono">${x.used}${x.limit ? ` of ${x.limit}` : ''}</td><td class="muted">${x.expires ? when(x.expires, false) : 'never'}</td><td>${chip(x.state, x.state)}</td>
    <td><div class="row" style="gap:6px;flex-wrap:nowrap"><button class="small" data-act-btn="/admin/code-change" data-code="${esc(x.code)}" data-do="${x.state === 'off' ? 'on' : 'off'}">Turn ${x.state === 'off' ? 'on' : 'off'}</button>
      <button class="small bad" data-act-btn="/admin/code-change" data-code="${esc(x.code)}" data-do="remove" data-confirm="Remove the code ${esc(x.code)}?">Remove</button></div></td></tr>`).join('');
  const sent = m.mails.map((x) => {
    const items = x.items.length ? x.items.map((i) => `${i.name} ×${num(i.count)}`).join(', ') : 'notice';
    const state = x.cancelled ? 'cancelled' : x.sent ? `${when(x.sent)} · ${x.taken} of ${x.copies} taken from the mailbox` : `waiting for ${when(x.at)} UTC`;
    const btn = x.cancelled ? '' : x.sent ? `<button class="small" data-act-btn="/admin/mail-group-change" data-id="${x.id}" data-do="takeback" data-confirm="Take back every copy nobody claimed?">Take back unclaimed</button>`
      : `<button class="small" data-act-btn="/admin/mail-group-change" data-id="${x.id}" data-do="cancel">Cancel</button>`;
    return `<div class="row between" style="padding:10px 0;border-bottom:1px solid var(--row);align-items:center"><div style="display:flex;flex-direction:column;gap:2px"><span>${esc(x.subject)}</span>
      <span class="small muted">${esc(toText(x.to))} · ${esc(items)} · ${esc(state)}</span></div>${btn}</div>`;
  }).join('');
  afterRender.push(() => {
    const sel = document.getElementById('to-kind'), arg = document.getElementById('to-arg'), hid = document.getElementById('to');
    const sync = () => { arg.classList.toggle('hidden', sel.value === 'all'); arg.querySelector('input,select')?.setAttribute('name', ''); hid.value = sel.value === 'all' ? 'all' : `${sel.value}:${arg.querySelector(sel.value === 'guild' ? 'select' : 'input').value}`; };
    sel.addEventListener('change', () => { arg.innerHTML = sel.value === 'guild' ? `<label class="f">Guild<select>${gl.guilds.map((g) => `<option value="${g.id}">${esc(g.name)}</option>`).join('')}</select></label>` : `<label class="f">${sel.value === 'level' ? 'Level at least' : 'Days'}<input value="${sel.value === 'level' ? 20 : 7}" inputmode="numeric"></label>`; sync(); });
    arg.addEventListener('input', sync); arg.addEventListener('change', sync); sync();
    const at = document.getElementById('at-when'), atInput = document.getElementById('at');
    document.querySelectorAll('input[name=when]').forEach((r) => r.addEventListener('change', () => { at.classList.toggle('hidden', r.value === 'now' && r.checked); if (r.value === 'now' && r.checked) atInput.value = ''; }));
  });
  return `<div class="head">${kicker('Rewards', 'Codes and mail', 'Redeem codes players type in the game\'s settings, and mail to many players at once.')}</div>
    <section class="panel flush"><div class="bar"><h2>Redeem codes</h2><span class="small muted">Each player can use a code once.</span></div>
      <div class="scroll"><table><thead><tr><th>Code</th><th>Rewards</th><th>Used</th><th>Expires</th><th>State</th><th></th></tr></thead><tbody>${codes}</tbody></table></div>${c.codes.length ? '' : '<div class="empty" style="margin:16px">No codes yet.</div>'}
      <form data-act="/admin/code-save" style="padding:16px;border-top:1px solid var(--line);display:flex;flex-direction:column;gap:12px"><b>New code (or change one with the same code)</b>
        <div class="fields"><label class="f">Code<input name="code" placeholder="SUMMER10" required style="text-transform:uppercase"></label><label class="f">Use limit, all players<input name="limit" placeholder="none" inputmode="numeric"></label><label class="f">Expires (UTC)<input type="date" name="expires"></label></div>
        ${itemRows([{}])}<button class="primary" style="align-self:flex-start">Save code</button></form></section>
    <div class="grid" style="--min:420px">
      <section class="panel"><h2>Mail to many players</h2>
        <form data-act="/admin/mail-group" style="display:flex;flex-direction:column;gap:12px">
          <div class="fields"><label class="f">Who gets it<select id="to-kind"><option value="all">Every account</option><option value="level">Accounts at a level or higher</option><option value="seen">Accounts seen in the last days</option><option value="guild">Members of one guild</option></select></label><div id="to-arg"></div></div>
          <input type="hidden" name="to" id="to" value="all">
          <label class="f">Title<input name="subject" required></label><label class="f">Text<textarea name="content"></textarea></label>
          <span class="small muted">Items (none: a notice)</span>${itemRows([{}])}
          <label class="f">Expires after<select name="days"><option value="30">30 days</option><option value="7">7 days</option><option value="365">a year</option><option value="0">never</option></select></label>
          <div class="row" role="radiogroup" aria-label="When"><label class="check"><input type="radio" name="when" value="now" checked> Now</label><label class="check"><input type="radio" name="when" value="at"> At a time</label>
            <label class="f hidden" id="at-when">At (UTC)<input type="datetime-local" name="at" id="at"></label></div>
          <button class="primary" style="align-self:flex-start">Send</button></form></section>
      <section class="panel"><h2>Sent and waiting</h2>${sent || '<div class="empty">Nothing sent to many players yet.</div>'}</section></div>`;
}
const toText = (to) => to === 'all' ? 'every account' : to.startsWith('level:') ? `level ${to.slice(6)}+` : to.startsWith('seen:') ? `seen in ${to.slice(5)} days` : to.startsWith('guild:') ? `guild ${to.slice(6)}` : to;

// ---- logs ----

let liveLines = [], liveLast = 0;
async function logsPage(kind = 'live') {
  const tabs = [['live', 'Live'], ['unanswered', 'Unanswered calls'], ['admin', 'Admin record']];
  const nav = `<nav class="tabs" aria-label="Log">${tabs.map(([k, l]) => `<a href="#/logs/${k}" class="${k === kind ? 'on' : ''}">${l}</a>`).join('')}</nav>`;
  const cls = (t) => /^admin|admin:/.test(t) ? 'admin' : /UNHANDLED|NOT |refused|REFUSED|wrong/i.test(t) ? 'warn' : 'x';
  let body;
  if (kind === 'live') {
    const d = await api(`logs?kind=live&after=${liveLast}`);
    liveLines = liveLines.concat(d.lines).slice(-1500);
    liveLast = d.last;
    every(2000, async () => {
      if (!current || current[1] !== 'logs') return;
      try {
        const n = await api(`logs?kind=live&after=${liveLast}`);
        if (!n.lines.length) return;
        liveLines = liveLines.concat(n.lines).slice(-1500);
        liveLast = n.last;
        const box = document.getElementById('log');
        if (!box) return;
        const follow = document.getElementById('follow')?.checked;
        box.insertAdjacentHTML('beforeend', n.lines.map(line).join(''));
        applyFilters();
        if (follow) box.scrollTop = box.scrollHeight;
      } catch { /* the next tick tries again */ }
    });
    afterRender.push(() => { const b = document.getElementById('log'); if (b) b.scrollTop = b.scrollHeight; });
    body = liveLines.map(line).join('');
  } else {
    const d = await api(`logs?kind=${kind}`);
    body = d.lines.map((l) => line({ time: l.time, text: kind === 'admin' ? `${l.from}  ${l.text}` : l.text })).join('');
    if (!d.lines.length) body = `<div style="padding:30px;text-align:center;font-family:var(--sans);color:var(--muted)">${kind === 'admin' ? 'No changes made from this panel yet.' : 'Every game call has had an answer.'}</div>`;
  }
  function line(l) { const t = when(l.time).split(', ')[1] || ''; return `<div data-tags="${esc(l.text)}"><span class="t">${esc(t)}</span><span class="${cls(l.text)}">${esc(l.text)}</span></div>`; }
  return `<div class="head">${kicker('Record', 'Logs', 'What the server is doing, what it could not answer, and every change made from this panel.')}</div>${nav}
    <div class="row"><label class="f" style="flex:1 1 300px">Filter<input type="search" data-search placeholder="Player, account id or text"></label>
      ${kind === 'live' ? '<label class="check"><input type="checkbox" id="follow" checked> Follow new lines</label>' : ''}
      <button type="button" onclick="(function(){const t=[...document.querySelectorAll('#log > div:not(.hidden)')].map(d=>d.textContent).join('\\n');const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([t],{type:'text/plain'}));a.download='log.txt';a.click();})()">Download</button></div>
    <div class="log" id="log">${body}</div>`;
}

// ---- backups ----

async function backupsPage() {
  const [d, acc] = await Promise.all([api('backups'), api('accounts')]);
  const s = d.auto;
  const size = (b) => b > 1e6 ? `${(b / 1e6).toFixed(1)} MB` : `${Math.max(1, Math.round(b / 1e3))} KB`;
  const rows = d.snapshots.map((x) => `<tr><td>${when(x.at)}</td><td>${x.kind === 'all' ? 'Everything' : `${esc(x.player || 'Account')} (${x.account})`}</td><td class="muted">${esc(x.why)}${x.note ? ` · ${esc(x.note)}` : ''}</td>
    <td class="n mono muted">${size(x.bytes)}</td><td><div class="row" style="gap:6px;flex-wrap:nowrap;justify-content:flex-end">
      <button class="small" data-act-btn="/admin/backup-restore" data-name="${esc(x.name)}" data-confirm="Put this snapshot back over what is there now? A snapshot of now is taken first.">Restore</button>
      <a class="btn small" style="min-height:34px" href="/admin/backup/${encodeURIComponent(x.name)}.tar" download>Download</a>
      <button class="small bad" data-act-btn="/admin/backup-remove" data-name="${esc(x.name)}" data-confirm="Remove this snapshot for good?">Remove</button></div></td></tr>`).join('');
  return `<div class="head">${kicker('Safety', 'Backups', 'Snapshots of saved accounts, guilds and settings, kept in the data folder (data/backups).')}</div>
    <div class="grid" style="--min:340px">
      <section class="panel"><h2>Take a snapshot</h2>
        <form data-act="/admin/backup" style="display:flex;flex-direction:column;gap:12px">
          <label class="f">What<select name="what" onchange="document.getElementById('bk-acc').classList.toggle('hidden', this.value!=='account')"><option value="all">Everything (accounts, logins, guilds, events, codes)</option><option value="account">One account</option></select></label>
          <label class="f hidden" id="bk-acc">Account<select name="account">${acc.accounts.map((a) => `<option value="${a.id}">${esc(a.name || 'Unnamed')} (${a.id})</option>`).join('')}</select></label>
          <label class="f">Note (optional)<input name="note" placeholder="before the anniversary rerun"></label><button class="primary" style="align-self:flex-start">Snapshot now</button></form></section>
      <section class="panel"><h2>Automatic snapshots</h2>
        <form data-act="/admin/backup-auto" class="fields">
          <label class="f">Every<select name="every">${[[24, 'day'], [6, '6 hours'], [12, '12 hours'], [0, 'off']].map(([v, l]) => `<option value="${v}" ${s.every_hours === v ? 'selected' : ''}>${l}</option>`).join('')}</select></label>
          <label class="f">At (UTC hour, daily)<input name="hour" value="${s.at_hour}" inputmode="numeric"></label><label class="f">Keep the newest<input name="keep" value="${s.keep}" inputmode="numeric"></label><button>Save</button></form>
        <span class="small muted">${s.last ? `Last automatic snapshot ${when(s.last)} UTC. ` : ''}A snapshot is also taken before an account is deleted and before any restore.</span></section></div>
    <section class="panel flush"><div class="bar"><h2>Snapshots</h2></div><div class="scroll"><table><thead><tr><th>Taken (UTC)</th><th>What</th><th>Why</th><th class="n">Size</th><th></th></tr></thead><tbody>${rows}</tbody></table></div>
      ${d.snapshots.length ? '' : '<div class="empty" style="margin:16px">No snapshots yet.</div>'}</section>
    <section class="panel danger"><h2 style="color:var(--red)">Restoring</h2><p style="margin:0;line-height:1.55;color:var(--text2)">A restore puts the snapshot back over what is there now. A snapshot of now is taken first, so a restore can be undone. Restoring one account leaves every other account as it is. Players in the game see the change after their next login.</p></section>`;
}

// ---- settings ----

async function settingsPage() {
  const d = await api('settings');
  const c = d.clock, f = d.files;
  const rows = d.settings.map((s) => `<tr><td class="mono small">${esc(s.name)}</td><td class="mono small" style="color:${s.value.startsWith('set') ? 'var(--mint-text)' : s.set ? 'var(--text)' : 'var(--dim)'}">${esc(s.value)}</td><td class="muted">${esc(s.from)}</td><td class="muted">${esc(s.what)}</td></tr>`).join('');
  afterRender.push(() => {
    const form = document.getElementById('clock-to');
    if (!form) return;
    form.addEventListener('submit', (e) => {
      const t = Date.parse(form.querySelector('input').value + ':00Z') / 1000;
      const real = c.now - c.ahead;
      form.querySelector('[name=secs]').value = String(Math.round(t - real - c.ahead));
    }, true);
  });
  const clock = c.test ? `<section class="panel"><div class="row between"><h2>Game clock</h2>${chip('test clock on', 'suspended')}</div>
      <span class="value mono" style="font-size:24px">${when(c.now)} UTC</span><span class="small muted">${c.ahead ? `${length(c.ahead)} ahead of real time.` : 'Real time.'} Daily resets, event dates and timers all follow it.</span>
      <div class="row">${[[3600, '+1 hour'], [86400, '+1 day'], [7 * 86400, '+7 days']].map(([s, l]) => `<button data-act-btn="/admin/clock" data-secs="${s}">${l}</button>`).join('')}</div>
      <form data-act="/admin/clock" class="fields" id="clock-to"><label class="f">Set to (UTC, later than now)<input type="datetime-local" required></label><input type="hidden" name="secs"><button>Set</button></form>
      <button class="primary" style="align-self:flex-start" data-act-btn="/admin/clock" data-reset="1">Back to real time</button></section>`
    : `<section class="panel"><h2>Game clock</h2><span class="value mono" style="font-size:24px">${when(c.now)} UTC</span><span class="small muted">Real time. A test server (WOOFIA_TEST_CLOCK) can move it from here.</span></section>`;
  return `<div class="head">${kicker('Config', 'Settings', 'Read only. Change them in .env or the environment, then restart the server.')}</div>
    <section class="panel flush"><div class="scroll"><table><thead><tr><th>Setting</th><th>Value</th><th>From</th><th>What it does</th></tr></thead><tbody>${rows}</tbody></table></div></section>
    <div class="grid" style="--min:380px">${clock}
      <section class="panel"><h2>Game files</h2>
        <div class="kv"><span>Catalog</span><span class="mono small">${esc(f.catalog)}</span></div><div class="kv"><span>Items</span><span>${num(f.items)}</span></div>
        <div class="kv"><span>Buddies</span><span>${num(f.buddies)}</span></div><div class="kv"><span>Stages</span><span>${num(f.stages)}</span></div>
        <div class="kv"><span>Events</span><span>${num(f.events)}</span></div><div class="kv"><span>Names</span><span>${num(f.names)}</span></div>
        <div class="kv"><span>Art (avatars, sign-in picture)</span><span>${f.art ? 'yes' : 'no: run the extractor with --art'}</span></div></section></div>`;
}

// ---- test tools ----

async function toolsPage(id) {
  const acc = await api('accounts');
  const pick = id ? Number(id) : acc.accounts[0]?.id;
  if (!pick) return `<div class="head">${kicker('Debug', 'Test tools')}</div><div class="empty">No accounts yet.</div>`;
  const T = (path, label, note, input) => `<form data-act="${path}" style="display:flex;flex-direction:column;gap:6px;padding:10px 0;border-top:1px solid var(--row)"><input type="hidden" name="account" value="${pick}">
    <div class="row" style="gap:8px;align-items:center">${input ? `<label class="sr" for="${esc(input[0])}-${path}">${esc(input[1])}</label><input id="${esc(input[0])}-${path}" name="${esc(input[0])}" value="${esc(input[2] ?? '')}" placeholder="${esc(input[1])}" style="width:150px">` : ''}<button>${esc(label)}</button></div>
    <span class="small muted">${esc(note)}</span></form>`;
  const groups = [
    ['Timers', [
      T('/admin/bbq-finish', 'Finish the BBQ now', 'The dish on the grill is done at once.'),
      T('/admin/dispatch-finish', 'Finish part-time jobs now', 'Jobs under way can be claimed at once.'),
      T('/admin/rename-reset', 'Allow a name change now', 'Clears the 7-day rename wait.'),
      T('/admin/packs-period-end', "End a mall pack's buy period", "The buy limit starts over, as at the period's end. Blank for every pack; 1101 is the Free Golden Bell.", ['merch', 'Merch id', '1101'])]],
    ['Newcomer events', [
      T('/admin/newcomer', 'Start newcomer events from today', 'First Encounter, Welcome Party and the newbie missions.'),
      T('/admin/newcomer-earlier', 'Move the start earlier', 'Welcome Party (1001) started this many days earlier; claimed days are kept, so the days between are missed (for testing make-ups).', ['days', 'Days earlier', '2'])]],
    ['Stages', [
      T('/admin/story', 'Clear the whole main story', 'Every story stage with 3 stars, no rewards.'),
      T('/admin/unfinished', 'Clear unfinished stages', 'Stage records never cleared and the run in progress.')]],
    ['Buddies', [
      T('/admin/all-summons', 'Give every summonable buddy, maxed out', 'Every buddy any banner can give.'),
      T('/admin/outfits-reset', 'Reset outfits', 'Every buddy back to the default outfit.')]],
    ['Guild and Escape Room', [
      T('/admin/rejoin-now', 'Allow joining a guild now', 'Clears the 2-hour wait after leaving a guild.'),
      T('/admin/leader-away', 'Make the guild leader absent', "Leader last seen 8 days ago: another member's next guild call takes over."),
      T('/admin/escape-keys', 'Give Escape Room keys', "Into the guild's shared bag, this many of each.", ['n', 'Keys of each', '3'])]],
    ['Whole account', [
      T('/admin/fresh', 'Make fresh', 'Drops sign-ins, missions and mall records, as a brand new account would have them.')]],
  ];
  afterRender.push(() => {
    const sel = document.getElementById('tool-acc');
    sel?.addEventListener('change', () => { location.hash = `#/debug/${sel.value}`; });
    // the newcomer move names its event
    document.querySelectorAll('form[data-act="/admin/newcomer-earlier"]').forEach((f) => f.insertAdjacentHTML('afterbegin', '<input type="hidden" name="id" value="1001">'));
  });
  return `<div class="head">${kicker('Debug', 'Test tools', 'Shortcuts for checking features in the game. Each one acts on the account picked here.')}
      <label class="f" style="min-width:260px">Account<select id="tool-acc">${acc.accounts.map((a) => `<option value="${a.id}" ${a.id === pick ? 'selected' : ''}>${esc(a.name || 'Unnamed')} (${a.id})</option>`).join('')}</select></label></div>
    <div class="grid" style="--min:340px">${groups.map(([t, tools]) => `<section class="panel" style="gap:4px"><h2 style="margin-bottom:8px">${esc(t)}</h2>${tools.join('')}</section>`).join('')}</div>`;
}

refreshBadges().catch(() => {}).finally(() => render());
setInterval(() => { if (!signedOut && current && current[1] !== 'dashboard') refreshBadges().catch(() => {}); }, 30000);
