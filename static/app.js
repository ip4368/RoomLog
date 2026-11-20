'use strict';
/* RoomLog — church rooms, AV equipment inventory & loans. Vanilla JS, no build step. */

// ------------------------------------------------------------------ helpers

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'class') el.className = v;
    else if (k === 'value') el.value = v;
    else if (k === 'checked') el.checked = !!v;
    else if (k === 'html') throw new Error('no innerHTML');
    else el.setAttribute(k, v === true ? '' : v);
  }
  const add = (c) => {
    if (c === null || c === undefined || c === false) return;
    if (Array.isArray(c)) c.forEach(add);
    else el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  };
  children.forEach(add);
  return el;
}

const $app = () => document.getElementById('app');

function render(...nodes) {
  const app = $app();
  app.replaceChildren(...nodes.flat().filter(Boolean));
  window.scrollTo(0, 0);
}

let toastTimer;
function toast(msg, isError = false) {
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.className = 'toast show' + (isError ? ' error' : '');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.className = 'toast'), isError ? 5000 : 2500);
}

class ApiError extends Error {
  constructor(status, body) {
    super((body && body.error) || `Request failed (${status})`);
    this.status = status;
    this.body = body;
  }
}

async function api(method, path, body) {
  let res;
  try {
    res = await fetch(path, {
      method,
      headers: body !== undefined ? { 'Content-Type': 'application/json' } : {},
      body: body !== undefined ? JSON.stringify(body) : undefined,
      credentials: 'same-origin',
    });
  } catch (e) {
    throw new ApiError(0, { error: navigator.onLine ? 'Could not reach the server' : 'You are offline' });
  }
  let data = null;
  try { data = await res.json(); } catch (_) { /* empty body */ }
  if (res.status === 401 && path.startsWith('/api/') && !path.startsWith('/api/public') && path !== '/api/login') {
    me = null;
    navigate('/admin/login');
    throw new ApiError(401, data);
  }
  if (!res.ok) throw new ApiError(res.status, data);
  return data;
}

const GET = (p) => api('GET', p);
const POST = (p, b = {}) => api('POST', p, b);
const PUT = (p, b) => api('PUT', p, b);
const PATCH = (p, b) => api('PATCH', p, b);
const DEL = (p) => api('DELETE', p);

function busy(btn, fn) {
  return async (ev) => {
    if (ev) ev.preventDefault();
    const b = btn || (ev && ev.currentTarget);
    if (b) b.disabled = true;
    try { await fn(ev); } catch (e) { toast(e.message, true); } finally { if (b) b.disabled = false; }
  };
}

const pad = (n) => String(n).padStart(2, '0');
const toDate = (s) => (s instanceof Date ? s : new Date(s));
function fmtDT(s) {
  const d = toDate(s);
  return d.toLocaleString(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
}
function fmtDate(s) {
  return toDate(s).toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' });
}
function fmtTime(s) {
  const d = toDate(s);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
function localInput(d) {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
function isoDate(d) {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
const statusBadge = (s) => h('span', { class: `badge ${s}` }, s);
const sameDay = (a, b) => isoDate(toDate(a)) === isoDate(toDate(b));
function fmtRange(start, end) {
  const day = (d) => toDate(d).toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' });
  return sameDay(start, end)
    ? `${day(start)}, ${fmtTime(start)}–${fmtTime(end)}`
    : `${day(start)} ${fmtTime(start)} → ${day(end)} ${fmtTime(end)}`;
}
const ORDINAL = { 1: '1st', 2: '2nd', 3: '3rd', 4: '4th', 5: '5th', '-1': 'last' };

function field(labelText, input, { required = false, hint } = {}) {
  const id = input.id || 'f' + Math.random().toString(36).slice(2);
  input.id = id;
  return h('div', { class: 'field' },
    h('label', { for: id }, labelText, required ? h('span', { class: 'req' }, ' *') : null),
    input,
    hint ? h('div', { class: 'muted small' }, hint) : null);
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch (_) {
    const ta = h('textarea', { style: 'position:fixed;opacity:0' });
    ta.value = text;
    document.body.append(ta);
    ta.select();
    document.execCommand('copy');
    ta.remove();
  }
  toast('Copied');
}

function groupBy(arr, keyFn) {
  const m = new Map();
  for (const x of arr) {
    const k = keyFn(x);
    if (!m.has(k)) m.set(k, []);
    m.get(k).push(x);
  }
  return m;
}

// ------------------------------------------------------------------ router

function navigate(path, replace = false) {
  if (replace) history.replaceState(null, '', path);
  else history.pushState(null, '', path);
  route();
}

document.addEventListener('click', (e) => {
  const a = e.target.closest('a[data-link]');
  if (!a || e.metaKey || e.ctrlKey || e.shiftKey || a.target === '_blank') return;
  e.preventDefault();
  navigate(a.getAttribute('href'));
});
window.addEventListener('popstate', route);

// ------------------------------------------------------------- staff login

/** The logged-in staff member (from /api/me), or null. */
let me = null;

async function loadMe() {
  me = await GET('/api/me');
  return me;
}

/** Roles: admin (everything), bookings (room bookings), av (equipment & loans). */
const hasRole = (role) => !!me && (me.roles.includes('admin') || me.roles.includes(role));

const ROLE_LABELS = { admin: 'Admin', bookings: 'Room bookings', av: 'AV & loans' };

/** Read-only note shown instead of action buttons when a role is missing. */
const needsRoleNote = (role) => h('p', { class: 'muted small' },
  `Only staff with the "${ROLE_LABELS[role]}" role can change this.`);

// [href, label, role needed to see it (null = all staff), pending-count key]
const ADMIN_NAV = [
  ['/admin/bookings', 'Bookings', null, 'pending_bookings'],
  ['/admin/loans', 'Loans', null, 'pending_loans'],
  ['/admin/check', 'Quick check', null],
  ['/admin/schedule', 'Schedule', null],
  ['/admin/stock', 'Stock', null],
  ['/admin/equipment', 'Equipment', null],
  ['/admin/rooms', 'Rooms', null],
  ['/admin/groups', 'Groups', null],
  ['/admin/events', 'Church events', null],
  ['/admin/users', 'Users', 'admin'],
  ['/admin/settings', 'Settings', null],
];

function showNav(path) {
  const nav = document.getElementById('nav');
  const isAdmin = path.startsWith('/admin') && path !== '/admin/login' && !!me;
  nav.hidden = !isAdmin;
  if (!isAdmin) return;
  nav.replaceChildren(...ADMIN_NAV.filter(([, , role]) => !role || hasRole(role)).map(([href, label, , countKey]) => {
    const count = countKey ? me[countKey] : 0;
    return h('a', { href, 'data-link': true, class: path.startsWith(href) ? 'active' : '' },
      label, count ? h('span', { class: 'count' }, count) : null);
  }));
}

/** Where staff land after logging in. */
const adminHome = () => (hasRole('av') && !hasRole('bookings') ? '/admin/loans' : '/admin/bookings');

async function route() {
  const path = location.pathname.replace(/\/+$/, '') || '/';
  const m = (re) => path.match(re);
  let r;
  try {
    // Refresh who is logged in (and pending counts) on every console page.
    if (path.startsWith('/admin') && path !== '/admin/login') await loadMe();
    showNav(path);
    if (path === '/') return await pageHome();
    if (path === '/request') return await pageRequestForm();
    if (path === '/rooms') return await pageAvailability();
    if (path === '/book') return await pageBookRoom();
    if ((r = m(/^\/booking\/([A-Za-z0-9]+)$/))) return await pageBookingStatus(r[1]);
    if ((r = m(/^\/status\/([A-Za-z0-9]+)$/))) return await pageStatus(r[1]);
    if (path === '/admin/login') return pageLogin();
    if (path === '/admin') return navigate(adminHome(), true);
    if (path === '/admin/loans') return await pageLoans();
    if (path === '/admin/bookings') return await pageBookings();
    if (path === '/admin/bookings/new') return await pageBookingEdit(null);
    if ((r = m(/^\/admin\/bookings\/(\d+)\/edit$/))) return await pageBookingEdit(+r[1]);
    if ((r = m(/^\/admin\/bookings\/(\d+)$/))) return await pageBooking(+r[1]);
    if ((r = m(/^\/admin\/loans\/(\d+)$/))) return await pageLoan(+r[1]);
    if (path === '/admin/check') return await pageCheck();
    if (path === '/admin/schedule') return await pageSchedule();
    if (path === '/admin/stock') return await pageStock();
    if (path === '/admin/equipment') return await pageEquipment();
    if (path === '/admin/rooms') return await pageRooms();
    if (path === '/admin/groups') return await pageGroups();
    if (path === '/admin/events') return await pageEvents();
    if (path === '/admin/settings') return await pageSettings();
    if (path === '/admin/users') return await pageUsers();
    render(h('h1', {}, 'Not found'), h('p', {}, h('a', { href: '/', 'data-link': true }, 'Go home')));
  } catch (e) {
    if (e.status === 401) return;
    render(h('div', { class: 'banner bad' }, e.message));
  }
}

// ------------------------------------------------------------------ public

let churchName = 'RoomLog';
async function loadInfo() {
  try {
    const info = await GET('/api/public/info');
    churchName = info.church_name;
    document.getElementById('brand-name').textContent = churchName;
  } catch (_) { /* offline */ }
}

const MY_REQUESTS_KEY = 'roomlog.myRequests';
// Status links organisers saved before the rename from Quippy.
const LEGACY_MY_REQUESTS_KEY = 'quippy.myRequests';

function myRequests() {
  const saved = localStorage.getItem(MY_REQUESTS_KEY) ?? localStorage.getItem(LEGACY_MY_REQUESTS_KEY);
  return JSON.parse(saved || '[]');
}

/** Remember a request's private status link on this device. kind: 'loan' | 'booking'. */
function rememberRequest(kind, token, label) {
  const mine = myRequests();
  mine.unshift({ kind, token, label });
  localStorage.setItem(MY_REQUESTS_KEY, JSON.stringify(mine.slice(0, 20)));
}

const requestLink = (r) => (r.kind === 'booking' ? `/booking/${r.token}` : `/status/${r.token}`);

async function pageHome() {
  let staff = false;
  try { await fetch('/api/me', { credentials: 'same-origin' }).then((r) => { staff = r.ok; }); } catch (_) {}
  if (staff) return navigate('/admin', true);
  const mine = myRequests();
  render(
    h('div', { class: 'hero' },
      h('h1', {}, churchName),
      h('p', { class: 'muted' }, 'Book a room at church, or borrow sound and video equipment for an event elsewhere.'),
      h('a', { class: 'btn primary', href: '/book', 'data-link': true }, 'Book a room'),
      h('a', { class: 'btn', href: '/rooms', 'data-link': true }, 'See room availability'),
      h('a', { class: 'btn', href: '/request', 'data-link': true }, 'Borrow equipment'),
      h('div', { style: 'margin-top:1rem' }, h('a', { class: 'small', href: '/admin/login', 'data-link': true }, 'Staff login'))),
    mine.length ? h('div', { class: 'card' },
      h('h2', {}, 'Your requests on this device'),
      h('ul', {}, mine.map((r) => h('li', {}, h('a', { href: requestLink(r), 'data-link': true }, r.label))))) : null,
  );
}

// ---------------------------------------------------------- equipment tree
// Equipment is a tree of kinds ("Microphones" > "Vocal mic" > "Wired vocal mic")
// with models (what's stocked, e.g. "Shure SM58") as leaves. A request line is
// {kind_id, type_id}: "any model of this kind", or "exactly this model".

const wantKey = (w) => (w.kind_id != null ? `k${w.kind_id}` : `m${w.type_id}`);
const keyWant = (k) => (k[0] === 'k' ? { kind_id: +k.slice(1), type_id: null } : { kind_id: null, type_id: +k.slice(1) });

/**
 * Kinds and models in tree order, for pickers and tables: [{ key, depth, kind | model }].
 * At each level sub-kinds come first, then models. Models whose kind isn't in
 * `kinds` are listed at the top level.
 */
function treeRows(kinds, models) {
  const known = new Set(kinds.map((k) => k.id));
  const kidsOf = groupBy(kinds, (k) => (known.has(k.parent_id) ? k.parent_id : null));
  const modelsOf = groupBy(models, (m) => (known.has(m.kind_id) ? m.kind_id : null));
  const out = [];
  const seen = new Set();
  const walk = (parent, depth) => {
    const ks = [...(kidsOf.get(parent) || [])].sort((a, b) => (a.sort_order || 0) - (b.sort_order || 0) || a.name.localeCompare(b.name));
    for (const k of ks) {
      if (seen.has(k.id)) continue;
      seen.add(k.id);
      out.push({ key: `k${k.id}`, depth, kind: k });
      walk(k.id, depth + 1);
    }
    for (const m of [...(modelsOf.get(parent) || [])].sort((a, b) => a.name.localeCompare(b.name))) {
      out.push({ key: `m${m.id}`, depth, model: m });
    }
  };
  walk(null, 0);
  return out;
}

/** Kinds and models from a `refs` object, as arrays. */
const refsCatalog = (refs) => ({
  kinds: Object.entries(refs.kinds || {}).map(([id, k]) => ({ id: +id, ...k })),
  models: Object.entries(refs.types).map(([id, t]) => ({ id: +id, ...t })),
});

const indent = (depth) => '\u00a0\u00a0\u00a0'.repeat(depth);

/**
 * Indent tree options only while the list is open: a closed <select> shows the
 * chosen option's text, which shouldn't start with the indentation.
 */
function indentWhenOpen(s) {
  const label = (o, open) => {
    if (o.dataset.label != null) o.textContent = (open || !o.selected ? indent(+o.dataset.depth) : '') + o.dataset.label;
  };
  const show = (open) => { for (const o of s.options) label(o, open); };
  s.addEventListener('mousedown', () => show(true));
  s.addEventListener('change', () => show(false));
  s.addEventListener('blur', () => show(false));
  show(false);
  return s;
}

const treeOption = (attrs, depth, text) => h('option', { ...attrs, 'data-depth': depth, 'data-label': text }, indent(depth) + text);

/** A <select> of the tree. Requestable kinds read "Name (any)"; other kinds are headings. */
function wantSelect(catalog, value) {
  const s = h('select', { 'aria-label': 'Equipment' }, h('option', { value: '' }, 'Choose equipment…'));
  for (const r of treeRows(catalog.kinds, catalog.models)) {
    if (r.kind) {
      s.append(treeOption({ value: r.key, disabled: !r.kind.requestable, class: 'kind-option' },
        r.depth, r.kind.name + (r.kind.requestable ? ' (any)' : '')));
    } else {
      s.append(treeOption({ value: r.key }, r.depth, r.model.name));
    }
  }
  if (value) {
    if (![...s.options].some((o) => o.value === value && !o.disabled)) s.append(h('option', { value }, 'Other (no longer listed)'));
    s.value = value;
  }
  return indentWhenOpen(s);
}

/** Rows of "what × how many". catalog: { kinds, models }. */
function itemsEditor(catalog, initial = []) {
  const wrap = h('div');
  const rows = h('div');
  const addRow = (key, quantity = 1) => {
    const sel = wantSelect(catalog, key);
    const qty = h('input', { type: 'number', min: 1, value: quantity, 'aria-label': 'Quantity' });
    const row = h('div', { class: 'item-row' }, sel, qty,
      h('button', { type: 'button', class: 'small', title: 'Remove', onclick: () => row.remove() }, '✕'));
    rows.append(row);
  };
  initial.forEach((i) => addRow(wantKey(i), i.quantity));
  if (!initial.length) addRow();
  wrap.append(rows, h('button', { type: 'button', class: 'small', onclick: () => addRow() }, '+ Add item'));
  return {
    el: wrap,
    get() {
      return [...rows.children]
        .map((r) => ({ key: r.querySelector('select').value, quantity: +r.querySelector('input').value }))
        .filter((i) => i.key && i.quantity > 0)
        .map((i) => ({ ...keyWant(i.key), quantity: i.quantity }));
    },
  };
}

const ANY_HINT = 'Choose a kind marked "(any)" if you don\'t mind which model; the AV team will pick one.';

function publicCheckView(check) {
  if (check.fully_available) {
    return h('div', { class: 'banner ok' }, 'Good news: everything you asked for is currently available.',
      h('p', {}, 'Your request still needs to be confirmed by the AV team.'));
  }
  return h('div', { class: 'banner warn' }, 'Some items are limited for those dates.',
    h('ul', {}, check.items.map((i) => h('li', {},
      i.available >= i.requested ? `${i.name}: ${i.requested} available ✓`
        : `${i.name}: ${i.available} of ${i.requested} currently available`))),
    h('p', {}, 'You can still submit. The AV team will check whether more can be freed up and get back to you.'));
}

async function pageRequestForm() {
  const catalog = await GET('/api/public/catalog');
  const f = {
    organiser_name: h('input', { autocomplete: 'name' }),
    organisation: h('input', { autocomplete: 'organization' }),
    email: h('input', { type: 'email', autocomplete: 'email' }),
    phone: h('input', { type: 'tel', autocomplete: 'tel' }),
    event_name: h('input'),
    event_location: h('input'),
    pickup_at: h('input', { type: 'datetime-local' }),
    return_at: h('input', { type: 'datetime-local' }),
    notes: h('textarea', { placeholder: 'Anything we should know? e.g. setup help, specific models' }),
  };
  const items = itemsEditor(catalog);
  const result = h('div');
  const payload = () => ({
    ...Object.fromEntries(Object.entries(f).map(([k, el]) => [k, el.value])),
    items: items.get(),
  });
  const checkBtn = h('button', { type: 'button' }, 'Check availability');
  const submitBtn = h('button', { type: 'submit', class: 'primary' }, 'Submit request');
  checkBtn.addEventListener('click', busy(checkBtn, async () => {
    const p = payload();
    const check = await POST('/api/public/check', { pickup_at: p.pickup_at, return_at: p.return_at, items: p.items });
    result.replaceChildren(publicCheckView(check));
  }));
  const form = h('form', { class: 'card' },
    h('h2', {}, 'About you'),
    h('div', { class: 'grid' },
      field('Your name', f.organiser_name, { required: true }),
      field('Organisation / group', f.organisation),
      field('Email', f.email, { hint: 'Email or phone required' }),
      field('Phone', f.phone)),
    h('h2', {}, 'Your event'),
    h('div', { class: 'grid' },
      field('Event name', f.event_name, { required: true }),
      field('Where will the equipment be used?', f.event_location)),
    h('div', { class: 'grid' },
      field('Pick up from church', f.pickup_at, { required: true }),
      field('Return to church', f.return_at, { required: true })),
    h('h2', {}, 'Equipment'),
    catalog.models.length ? [h('p', { class: 'muted small' }, ANY_HINT), items.el] : h('p', { class: 'muted' }, 'No equipment is currently listed for loan.'),
    h('div', { class: 'field', style: 'margin-top:1rem' }, h('label', {}, 'Notes'), f.notes),
    result,
    h('div', { class: 'row' }, checkBtn, submitBtn));
  form.addEventListener('submit', busy(submitBtn, async () => {
    const p = payload();
    if (!p.event_name.trim()) throw new Error('Event name is required');
    const res = await POST('/api/public/requests', p);
    rememberRequest('loan', res.token, `Equipment: ${p.event_name} (${fmtDT(p.pickup_at)})`);
    const link = `${location.origin}/status/${res.token}`;
    render(
      h('h1', {}, 'Request sent'),
      publicCheckView(res.check),
      h('div', { class: 'card' },
        h('p', {}, 'Keep this link to check the status of your request and see where to collect the equipment once it is approved:'),
        h('p', {}, h('a', { href: `/status/${res.token}`, 'data-link': true }, link)),
        h('button', { onclick: () => copyText(link) }, 'Copy link')),
    );
  }));
  render(
    h('h1', {}, 'Borrow AV equipment'),
    h('p', { class: 'muted' }, `Fill this in to ask ${churchName} to lend equipment for an off-site event. Check availability before submitting if you like.`),
    form,
  );
}

const STATUS_TEXT = {
  pending: 'Waiting for the AV team to review your request.',
  approved: 'Approved! See below for where to collect each item.',
  collected: 'Equipment collected. Please return it by the return time.',
  returned: 'Returned. Thank you!',
  declined: 'Sorry, this request could not be fulfilled.',
  cancelled: 'This request was cancelled.',
};

async function pageStatus(token) {
  const s = await GET(`/api/public/requests/${token}`);
  const cancelBtn = ['pending', 'approved'].includes(s.status)
    ? h('button', { class: 'danger' }, 'Cancel this request') : null;
  if (cancelBtn) {
    cancelBtn.addEventListener('click', busy(cancelBtn, async () => {
      if (!confirm('Cancel this request?')) return;
      await POST(`/api/public/requests/${token}/cancel`);
      toast('Request cancelled');
      route();
    }));
  }
  render(
    h('h1', {}, s.event_name || 'Your request'),
    h('div', { class: 'card' },
      h('div', { class: 'row' }, statusBadge(s.status), h('span', {}, STATUS_TEXT[s.status] || '')),
      s.message ? h('div', { class: 'banner info', style: 'margin-top:1rem' }, `Message from the AV team: `, h('p', {}, s.message)) : null,
      h('dl', { class: 'kv' },
        h('dt', {}, 'Pick up'), h('dd', {}, fmtDT(s.pickup_at)),
        h('dt', {}, 'Return'), h('dd', {}, fmtDT(s.return_at)))),
    h('div', { class: 'card' }, h('h2', {}, 'Requested'),
      h('ul', {}, s.items.map((i) => h('li', {}, `${i.quantity} × ${i.name}`,
        i.given && i.given.length ? h('span', { class: 'muted' }, ` → ${i.given.map((g) => `${g.quantity} × ${g.name}`).join(', ')}`) : null)))),
    s.collection.length ? h('div', { class: 'card' }, h('h2', {}, 'Where to collect'),
      s.collection.map((c) => [h('h3', {}, c.room), h('ul', {}, c.items.map((i) => h('li', {}, `${i.quantity} × ${i.name}`)))])) : null,
    cancelBtn,
  );
}


// ------------------------------------------------------------ room booking

/** Split busy blocks into per-day pieces (a booking can run past midnight). */
function busyOnDay(busy, day) {
  const dayStart = new Date(`${day}T00:00`);
  const dayEnd = new Date(dayStart.getTime() + 86400000);
  return busy
    .map((b) => ({ ...b, s: toDate(b.start), e: toDate(b.end) }))
    .filter((b) => b.s < dayEnd && b.e > dayStart)
    .map((b) => ({ ...b, s: b.s < dayStart ? dayStart : b.s, e: b.e > dayEnd ? dayEnd : b.e, dayStart }))
    .sort((a, b) => a.s - b.s);
}

function timeline(pieces) {
  const pct = (d, start) => ((d - start) / 86400000) * 100;
  return h('div', { class: 'timeline', 'aria-hidden': 'true' },
    pieces.map((p) => h('span', { style: `left:${pct(p.s, p.dayStart)}%;width:${Math.max(0.6, pct(p.e, p.dayStart) - pct(p.s, p.dayStart))}%`, title: p.title })));
}

async function pageAvailability() {
  const params = new URLSearchParams(location.search);
  const from = params.get('from') || isoDate(new Date());
  const roomFilter = params.get('room') || '';
  const [rooms, avail] = await Promise.all([GET('/api/public/rooms'), GET(`/api/public/availability?from=${from}&days=7`)]);
  const shift = (days) => isoDate(new Date(new Date(`${from}T00:00`).getTime() + days * 86400000));
  const go = (f, r) => navigate(`/rooms?from=${f}${r ? `&room=${r}` : ''}`);
  const fromIn = h('input', { type: 'date', value: from, onchange: () => go(fromIn.value, roomSel.value) });
  const roomSel = h('select', { onchange: () => go(from, roomSel.value) }, h('option', { value: '' }, 'All rooms'),
    rooms.map((r) => h('option', { value: r.id }, r.name)));
  roomSel.value = roomFilter;
  const shown = rooms.filter((r) => !roomFilter || String(r.id) === roomFilter);
  const days = [...Array(7)].map((_, i) => shift(i));
  render(
    h('div', { class: 'row' }, h('h1', {}, 'Room availability'), h('span', { class: 'spacer' }),
      h('a', { class: 'btn primary', href: '/book', 'data-link': true }, 'Book a room')),
    h('p', { class: 'muted' }, 'Times shown are already taken. Requests waiting for a decision are not shown, so a free time could still be requested by someone else.'),
    h('div', { class: 'card row' },
      h('button', { class: 'small', onclick: () => go(shift(-7), roomFilter) }, '← Previous week'),
      fromIn,
      h('button', { class: 'small', onclick: () => go(shift(7), roomFilter) }, 'Next week →'),
      h('span', { class: 'spacer' }), roomSel),
    rooms.length === 0 ? h('p', { class: 'muted' }, 'No rooms are available to book online.') :
      h('div', { class: 'card' }, days.map((day) => [
        h('div', { class: 'day' }, fmtDate(`${day}T00:00`)),
        shown.map((room) => {
          const pieces = busyOnDay(avail.busy.filter((b) => b.room_id === room.id), day);
          return h('div', { class: 'avail-row' },
            h('div', { class: 'avail-room' }, room.name),
            h('div', { class: 'avail-busy' },
              timeline(pieces),
              pieces.length ? pieces.map((p) => h('span', { class: 'pill busy' }, `${fmtTime(p.s)}–${p.e.getTime() === p.dayStart.getTime() + 86400000 ? '24:00' : fmtTime(p.e)} ${p.title}`))
                : h('span', { class: 'muted small' }, 'Free all day')),
            h('a', { class: 'btn small', href: `/book?room=${room.id}&date=${day}`, 'data-link': true }, 'Book'));
        })])),
  );
}

/** Options for the "Repeats" select, which depend on the first date. */
function repeatOptions(dateStr) {
  const opts = [['none', 'Just once'], ['weekly:1', 'Every week'], ['weekly:2', 'Every 2 weeks'], ['weekly:3', 'Every 3 weeks'], ['weekly:4', 'Every 4 weeks']];
  if (dateStr) {
    const d = new Date(`${dateStr}T00:00`);
    const wd = d.toLocaleDateString(undefined, { weekday: 'long' });
    const nth = Math.ceil(d.getDate() / 7);
    const daysInMonth = new Date(d.getFullYear(), d.getMonth() + 1, 0).getDate();
    opts.push([`monthly:${nth}`, `Monthly on the ${ORDINAL[nth]} ${wd}`]);
    if (d.getDate() + 7 > daysInMonth) opts.push(['monthly:-1', `Monthly on the last ${wd}`]);
  }
  return opts;
}

/**
 * The booking form, shared by the public page and staff.
 * rooms: from /api/public/rooms (with the equipment in each room).
 */
function bookingForm(rooms, initial = {}, { staff = false } = {}) {
  const f = {
    room: h('select', {}, rooms.map((r) => h('option', { value: r.id }, r.name + (r.capacity ? ` (up to ${r.capacity} people)` : '')))),
    title: h('input', { value: initial.title || '' }),
    start: h('input', { type: 'datetime-local', value: initial.start_at ? initial.start_at.slice(0, 16) : '' }),
    end: h('input', { type: 'datetime-local', value: initial.end_at ? initial.end_at.slice(0, 16) : '' }),
    repeat: h('select'),
    until: h('input', { type: 'date', value: initial.until || '' }),
    attendees: h('input', { type: 'number', min: 1, value: initial.attendees ?? '' }),
    av_support: h('input', { type: 'checkbox', checked: initial.av_support }),
    public_title: h('input', { type: 'checkbox', checked: initial.public_title }),
    notes: h('textarea', { placeholder: 'Anything else? e.g. tables and chairs, kitchen use, access before the start time' }),
    organiser_name: h('input', { autocomplete: 'name', value: initial.organiser_name || '' }),
    organisation: h('input', { autocomplete: 'organization', value: initial.organisation || '' }),
    email: h('input', { type: 'email', autocomplete: 'email', value: initial.email || '' }),
    phone: h('input', { type: 'tel', autocomplete: 'tel', value: initial.phone || '' }),
  };
  f.notes.value = initial.notes || '';
  if (initial.room_id) {
    if (!rooms.some((r) => r.id === initial.room_id)) f.room.append(h('option', { value: initial.room_id }, `Room #${initial.room_id}`));
    f.room.value = initial.room_id;
  }
  const initialRepeat = initial.recurrence === 'weekly' ? `weekly:${initial.interval_weeks}`
    : initial.recurrence === 'monthly' ? `monthly:${initial.nth}` : 'none';
  const roomInfo = h('div', { class: 'muted small' });
  const equipment = h('div');
  const needQty = new Map((initial.needs || []).map((n) => [wantKey(n), n.quantity]));
  const repeatFields = h('div');

  const refreshRoom = () => {
    const room = rooms.find((r) => String(r.id) === f.room.value);
    roomInfo.textContent = room ? room.public_notes : '';
    if (!room || !room.equipment.length) {
      equipment.replaceChildren(h('p', { class: 'muted small' }, 'No equipment is listed for this room.'));
      return;
    }
    // The room's own tree: ask for "any" of a kind, or a specific model.
    const models = room.equipment.map((e) => ({ id: e.type_id, name: e.name, kind_id: e.kind_id, quantity: e.quantity }));
    equipment.replaceChildren(h('p', { class: 'muted small' }, ANY_HINT), h('div', {}, treeRows(room.kinds || [], models).map((r) => {
      const item = r.kind || r.model;
      const style = `padding-left:${r.depth * 1.2}rem`;
      if (r.kind && !r.kind.requestable) return h('div', { class: 'tree-heading', style }, item.name);
      const label = r.kind ? `${item.name} (any)` : item.name;
      const qty = h('input', { type: 'number', min: 0, max: item.quantity, value: needQty.get(r.key) || 0, 'aria-label': label, 'data-want': r.key });
      qty.addEventListener('change', () => needQty.set(r.key, +qty.value || 0));
      return h('div', { class: 'item-row' + (r.kind ? ' kind' : ''), style }, qty,
        h('span', {}, label, h('span', { class: 'muted small' }, ` (${item.quantity} in room)`)));
    })));
  };
  const refreshRepeat = (keep) => {
    const current = keep ?? f.repeat.value;
    f.repeat.replaceChildren(...repeatOptions(f.start.value.slice(0, 10)).map(([v, l]) => h('option', { value: v }, l)));
    f.repeat.value = [...f.repeat.options].some((o) => o.value === current) ? current : 'none';
    repeatFields.replaceChildren(...(f.repeat.value === 'none' ? []
      : [field('Last date', f.until, { required: true, hint: 'Repeating bookings can run for up to a year.' })]));
  };
  f.room.addEventListener('change', refreshRoom);
  f.repeat.addEventListener('change', () => refreshRepeat());
  f.start.addEventListener('change', () => {
    // Suggest a two-hour slot when the end is missing or before the start.
    if (f.start.value && (!f.end.value || f.end.value <= f.start.value)) {
      f.end.value = localInput(new Date(new Date(f.start.value).getTime() + 2 * 3600000));
    }
    refreshRepeat();
  });
  refreshRoom();
  refreshRepeat(initialRepeat);

  const payload = () => {
    const [kind, n] = f.repeat.value.split(':');
    return {
      room_id: +f.room.value, title: f.title.value, start_at: f.start.value, end_at: f.end.value,
      recurrence: kind, interval_weeks: kind === 'weekly' ? +n : 1, nth: kind === 'monthly' ? +n : 1,
      until: kind === 'none' ? null : f.until.value || null,
      attendees: f.attendees.value ? +f.attendees.value : null,
      needs: [...equipment.querySelectorAll('input[data-want]')].map((i) => ({ ...keyWant(i.dataset.want), quantity: +i.value || 0 })).filter((n) => n.quantity > 0),
      av_support: f.av_support.checked, public_title: f.public_title.checked, notes: f.notes.value,
      organiser_name: f.organiser_name.value, organisation: f.organisation.value, email: f.email.value, phone: f.phone.value,
    };
  };

  const result = h('div');
  const checkBtn = h('button', { type: 'button' }, 'Check availability');
  checkBtn.addEventListener('click', busy(checkBtn, async () => {
    const check = await POST('/api/public/bookings/check', payload());
    result.replaceChildren(bookingCheckView(check));
  }));

  const el = h('div', {},
    h('div', { class: 'card' },
      h('h2', {}, 'When and where'),
      h('div', { class: 'grid' }, field('Room', f.room, { required: true }), field('Event name', f.title, { required: true })),
      roomInfo,
      h('div', { class: 'grid', style: 'margin-top:0.7rem' },
        field('From', f.start, { required: true, hint: 'Include time to set up' }),
        field('Until', f.end, { required: true, hint: 'Include time to pack down' }),
        field('Repeats', f.repeat)),
      repeatFields,
      h('div', { class: 'grid' }, field('Expected number of people', f.attendees)),
      result,
      checkBtn),
    h('div', { class: 'card' },
      h('h2', {}, 'Equipment in the room'),
      h('p', { class: 'muted small' }, 'Say what you would like to use, so the AV team keeps it in the room for you.'),
      equipment,
      h('label', { class: 'check', style: 'margin-top:0.6rem' }, f.av_support, 'We would like someone from the AV team to help'),
      field('Notes', f.notes)),
    h('div', { class: 'card' },
      h('h2', {}, staff ? 'Organiser' : 'About you'),
      h('div', { class: 'grid' },
        field(staff ? 'Organiser name' : 'Your name', f.organiser_name, { required: true }),
        field('Organisation / group', f.organisation),
        field('Email', f.email, { hint: staff ? null : 'Email or phone required' }),
        field('Phone', f.phone)),
      h('label', { class: 'check' }, f.public_title, 'Show the event name on the public calendar (otherwise it shows as "Booked")')));
  return { el, payload, result };
}

function bookingCheckView(check) {
  if (!check.clashes.length) {
    return h('div', { class: 'banner ok' },
      check.dates > 1 ? `The room is free on all ${check.dates} dates.` : 'The room is free at that time.',
      h('p', {}, 'Your booking still needs to be approved by the church office.'));
  }
  return h('div', { class: 'banner warn' },
    check.dates > 1 ? `${check.clashes.length} of ${check.dates} dates clash with something already booked:` : 'The room is already booked at that time:',
    h('ul', {}, check.clashes.slice(0, 12).map((c) => h('li', {}, fmtRange(c.start, c.end), ': ',
      c.with.map((w) => `${w.title} (${fmtTime(w.start)}–${fmtTime(w.end)})`).join(', ')))),
    check.clashes.length > 12 ? h('p', {}, `…and ${check.clashes.length - 12} more.`) : null,
    h('p', {}, 'Try a different time or room. For repeating bookings you can still send the request, and the office can approve just the free dates.'));
}

async function pageBookRoom() {
  const rooms = await GET('/api/public/rooms');
  if (!rooms.length) return render(h('h1', {}, 'Book a room'), h('p', { class: 'muted' }, 'No rooms are available to book online at the moment.'));
  const params = new URLSearchParams(location.search);
  const initial = {};
  if (params.get('room')) initial.room_id = +params.get('room');
  if (params.get('date')) {
    initial.start_at = `${params.get('date')}T10:00`;
    initial.end_at = `${params.get('date')}T12:00`;
  }
  const form = bookingForm(rooms, initial);
  const submit = h('button', { type: 'submit', class: 'primary' }, 'Send booking request');
  const wrap = h('form', {}, form.el, h('div', { class: 'row' }, submit));
  wrap.addEventListener('submit', busy(submit, async () => {
    const p = form.payload();
    const res = await POST('/api/public/bookings', p);
    rememberRequest('booking', res.token, `Room: ${p.title} (${fmtDT(p.start_at)})`);
    const link = `${location.origin}/booking/${res.token}`;
    render(
      h('h1', {}, 'Booking request sent'),
      bookingCheckView(res.check),
      h('div', { class: 'card' },
        h('p', {}, 'The church office will review your request. Keep this link to check whether it has been approved:'),
        h('p', {}, h('a', { href: `/booking/${res.token}`, 'data-link': true }, link)),
        h('button', { onclick: () => copyText(link) }, 'Copy link')),
    );
  }));
  render(
    h('h1', {}, 'Book a room'),
    h('p', { class: 'muted' }, 'Ask to use a room at ', churchName, '. Every request is reviewed by the church office. ',
      h('a', { href: '/rooms', 'data-link': true }, 'See what is already booked.')),
    wrap,
  );
}

const BOOKING_STATUS_TEXT = {
  pending: 'Waiting for the church office to review your request.',
  approved: 'Approved. The room is booked for you.',
  declined: 'Sorry, this booking could not be approved.',
  cancelled: 'This booking was cancelled.',
};

async function pageBookingStatus(token) {
  const s = await GET(`/api/public/bookings/${token}`);
  const cancelBtn = ['pending', 'approved'].includes(s.status) ? h('button', { class: 'danger' }, 'Cancel this booking') : null;
  if (cancelBtn) {
    cancelBtn.addEventListener('click', busy(cancelBtn, async () => {
      if (!confirm(s.repeats ? 'Cancel this booking, including all its dates?' : 'Cancel this booking?')) return;
      await POST(`/api/public/bookings/${token}/cancel`);
      toast('Booking cancelled');
      route();
    }));
  }
  render(
    h('h1', {}, s.title),
    h('div', { class: 'card' },
      h('div', { class: 'row' }, statusBadge(s.status), h('span', {}, BOOKING_STATUS_TEXT[s.status] || '')),
      s.message ? h('div', { class: 'banner info', style: 'margin-top:1rem' }, 'Message from the church office: ', h('p', {}, s.message)) : null,
      h('dl', { class: 'kv' },
        h('dt', {}, 'Room'), h('dd', {}, s.room),
        h('dt', {}, 'When'), h('dd', {}, s.repeats ? s.description : fmtRange(s.start_at, s.end_at)),
        s.cancelled_dates.length ? [h('dt', {}, 'Except'), h('dd', {}, s.cancelled_dates.map((d) => h('span', { class: 'pill' }, fmtDT(d))))] : null,
        s.needs.length ? [h('dt', {}, 'Equipment'), h('dd', {}, s.needs.map((n) => h('span', { class: 'pill' }, `${n.quantity} × ${n.name}`)))] : null,
        s.av_support ? [h('dt', {}, 'AV help'), h('dd', {}, 'Requested')] : null)),
    cancelBtn,
  );
}

// ------------------------------------------------------------------- admin

function pageLogin() {
  showNav('/admin/login');
  const user = h('input', { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false' });
  const pw = h('input', { type: 'password', autocomplete: 'current-password' });
  const btn = h('button', { class: 'primary', type: 'submit' }, 'Log in');
  const form = h('form', { class: 'card', style: 'max-width:380px;margin:2rem auto' },
    h('h1', {}, 'Staff login'), field('Username', user), field('Password', pw), btn,
    h('p', { class: 'muted small' }, 'Forgotten your password? Ask an admin to reset it.'));
  form.addEventListener('submit', busy(btn, async () => {
    await POST('/api/login', { username: user.value, password: pw.value });
    await loadMe();
    navigate(adminHome(), true);
  }));
  render(form);
  user.focus();
}

const LOAN_FILTERS = [['open', 'Open'], ['pending', 'Pending'], ['approved', 'Approved'], ['collected', 'Out now'], ['returned', 'Returned'], ['declined', 'Declined'], ['cancelled', 'Cancelled'], ['all', 'All']];

async function pageLoans() {
  const params = new URLSearchParams(location.search);
  const status = params.get('status') || 'open';
  const loans = await GET(`/api/loans?status=${status}`);
  const now = new Date();
  render(
    h('div', { class: 'row' }, h('h1', {}, 'Loan requests'), h('span', { class: 'spacer' }),
      hasRole('av') ? h('a', { class: 'btn', href: '/request', 'data-link': true }, '+ New request') : null),
    h('div', { class: 'row', style: 'margin-bottom:0.8rem' }, LOAN_FILTERS.map(([k, label]) =>
      h('a', { class: 'btn small' + (k === status ? ' primary' : ''), href: `/admin/loans?status=${k}`, 'data-link': true }, label))),
    loans.length === 0 ? h('p', { class: 'muted' }, 'Nothing here.') :
      h('div', { class: 'card table-wrap' }, h('table', {},
        h('thead', {}, h('tr', {}, ['Status', 'Event', 'Organiser', 'Pick up', 'Return', 'Items'].map((t) => h('th', {}, t)))),
        h('tbody', {}, loans.map((l) => {
          const overdue = l.status === 'collected' && toDate(l.return_at) < now;
          return h('tr', { class: 'clickable', onclick: () => navigate(`/admin/loans/${l.id}`) },
            h('td', {}, statusBadge(l.status), overdue ? h('div', { class: 'badge declined' }, 'overdue') : null),
            h('td', {}, l.event_name || '—'),
            h('td', {}, l.organiser_name, l.organisation ? h('div', { class: 'muted small' }, l.organisation) : null),
            h('td', {}, fmtDT(l.pickup_at)),
            h('td', {}, fmtDT(l.return_at)),
            h('td', { class: 'num' }, l.item_count));
        })))),
  );
}

// ---------------------------------------------------------- admin: bookings

const BOOKING_FILTERS = [['open', 'Open'], ['pending', 'Pending'], ['approved', 'Approved'], ['declined', 'Declined'], ['cancelled', 'Cancelled'], ['all', 'All']];

async function pageBookings() {
  const status = new URLSearchParams(location.search).get('status') || 'open';
  const [bookings, rooms] = await Promise.all([GET(`/api/bookings?status=${status}`), GET('/api/rooms')]);
  const rName = Object.fromEntries(rooms.map((r) => [r.id, r.name]));
  render(
    h('div', { class: 'row' }, h('h1', {}, 'Room bookings'), h('span', { class: 'spacer' }),
      h('a', { class: 'btn', href: '/rooms', 'data-link': true }, 'Availability'),
      hasRole('bookings') ? h('a', { class: 'btn primary', href: '/admin/bookings/new', 'data-link': true }, '+ New booking') : null),
    h('div', { class: 'row', style: 'margin-bottom:0.8rem' }, BOOKING_FILTERS.map(([k, label]) =>
      h('a', { class: 'btn small' + (k === status ? ' primary' : ''), href: `/admin/bookings?status=${k}`, 'data-link': true }, label))),
    bookings.length === 0 ? h('p', { class: 'muted' }, 'Nothing here.') :
      h('div', { class: 'card table-wrap' }, h('table', {},
        h('thead', {}, h('tr', {}, ['Status', 'Event', 'Room', 'When', 'Organiser'].map((t) => h('th', {}, t)))),
        h('tbody', {}, bookings.map((b) => h('tr', { class: 'clickable', onclick: () => navigate(`/admin/bookings/${b.id}`) },
          h('td', {}, statusBadge(b.status)),
          h('td', {}, b.title,
            b.av_support || b.needs.length ? h('div', {}, h('span', { class: 'pill' }, b.av_support ? 'AV help wanted' : 'Uses equipment')) : null),
          h('td', {}, rName[b.room_id] || `#${b.room_id}`),
          h('td', {}, b.recurrence === 'none' ? fmtRange(b.start_at, b.end_at) : b.description),
          h('td', {}, b.organiser_name, b.organisation ? h('div', { class: 'muted small' }, b.organisation) : null)))))),
  );
}

async function pageBookingEdit(id) {
  if (!hasRole('bookings')) return render(h('h1', {}, 'Room booking'), needsRoleNote('bookings'));
  const [rooms, existing] = await Promise.all([GET('/api/public/rooms'), id ? GET(`/api/bookings/${id}`) : null]);
  const initial = existing ? existing.booking : {};
  const form = bookingForm(rooms, initial, { staff: true });
  const save = h('button', { type: 'submit', class: id ? 'primary' : '' }, id ? 'Save changes' : 'Save as pending');
  const saveApprove = id ? null : h('button', { type: 'button', class: 'primary' }, 'Save & approve');
  const wrap = h('form', {}, form.el, h('div', { class: 'row' }, saveApprove, save,
    h('a', { class: 'btn', href: id ? `/admin/bookings/${id}` : '/admin/bookings', 'data-link': true }, 'Cancel')));
  wrap.addEventListener('submit', busy(save, async () => {
    if (id) {
      await PUT(`/api/bookings/${id}`, form.payload());
      toast('Saved');
      return navigate(`/admin/bookings/${id}`);
    }
    const res = await POST('/api/bookings', form.payload());
    toast('Booking saved as pending');
    navigate(`/admin/bookings/${res.id}`);
  }));
  if (saveApprove) {
    saveApprove.addEventListener('click', busy(saveApprove, async () => {
      const res = await POST('/api/bookings', form.payload());
      try {
        await POST(`/api/bookings/${res.id}/approve`, {});
        toast('Booking approved');
      } catch (e) {
        // Usually a clash: the detail page shows what and lets staff decide.
        toast(`Saved as pending: ${e.message}`, true);
      }
      navigate(`/admin/bookings/${res.id}`);
    }));
  }
  render(
    h('div', { class: 'row' }, h('a', { href: id ? `/admin/bookings/${id}` : '/admin/bookings', 'data-link': true }, '← Back')),
    h('h1', {}, id ? `Edit booking: ${initial.title}` : 'New room booking'),
    id ? null : h('p', { class: 'muted' }, 'For bookings that came in by phone or email, or from the old spreadsheet.'),
    wrap,
  );
}

async function pageBooking(id) {
  const d = await GET(`/api/bookings/${id}`);
  const { booking: b, refs } = d;
  const reload = () => pageBooking(id);
  const canEdit = hasRole('bookings');
  const groups = Object.values(refs.groups);

  const message = h('textarea', { placeholder: 'Shown to the organiser on their status page' });
  message.value = b.message_to_organiser;
  const notes = h('textarea', { placeholder: 'Private notes' });
  notes.value = b.admin_notes;
  const saveNotes = h('button', { class: 'small' }, 'Save notes');
  saveNotes.addEventListener('click', busy(saveNotes, async () => {
    await PATCH(`/api/bookings/${id}`, { admin_notes: notes.value, message_to_organiser: message.value });
    toast('Saved');
  }));

  const actions = [];
  const act = (label, cls, fn) => {
    const btn = h('button', { class: cls }, label);
    btn.addEventListener('click', busy(btn, async () => { if (await fn() !== false) reload(); }));
    actions.push(btn);
  };
  const setStatus = (status, confirmText) => async () => {
    if (confirmText && !confirm(confirmText)) return false;
    await POST(`/api/bookings/${id}/status`, { status, message: message.value });
    toast(`Booking ${status}`);
  };

  // Which regular group the booking's event belongs to ("who to ask" for equipment).
  const groupSel = h('select', {},
    h('option', { value: 'new' }, `New group from organiser: ${b.organisation || b.organiser_name}`),
    h('option', { value: '' }, 'No regular group (one-off)'),
    groups.map((g) => h('option', { value: g.id }, g.name)));
  const existing = groups.find((g) => g.name === (b.organisation || b.organiser_name));
  groupSel.value = existing ? existing.id : b.recurrence === 'none' ? '' : 'new';
  const approveBody = (extra) => ({
    ...extra, message: message.value,
    create_group: groupSel.value === 'new', group_id: groupSel.value && groupSel.value !== 'new' ? +groupSel.value : null,
  });

  const clashCount = d.clashes.length;
  const total = d.dates.length;
  if (canEdit && b.status === 'pending') {
    if (!clashCount) {
      act('Approve', 'primary', async () => { await POST(`/api/bookings/${id}/approve`, approveBody({})); toast('Approved'); });
    } else {
      if (clashCount < total) {
        act(`Approve, skipping ${clashCount} clashing date${clashCount > 1 ? 's' : ''}`, 'primary', async () => {
          await POST(`/api/bookings/${id}/approve`, approveBody({ skip_clashing: true }));
          toast('Approved');
        });
      }
      act('Approve anyway (double-book)', '', async () => {
        if (!confirm('The room will be booked twice at the clashing times. Only do this if everyone involved has agreed. Continue?')) return false;
        await POST(`/api/bookings/${id}/approve`, approveBody({ force: true }));
        toast('Approved');
      });
    }
    act('Decline', 'danger', setStatus('declined', 'Decline this booking? Put a reason in the message to the organiser first.'));
  }
  if (canEdit && b.status === 'approved') {
    act('Back to pending', '', setStatus('pending', 'Take this booking off the calendar and return it to pending? Any changes made to its church event are lost.'));
    act('Cancel booking', 'danger', setStatus('cancelled', 'Cancel this booking and free the room?'));
  }
  if (canEdit && ['declined', 'cancelled'].includes(b.status)) {
    act('Reopen', '', setStatus('pending'));
    act('Delete', 'danger', async () => {
      if (!confirm('Delete this booking request permanently?')) return false;
      await DEL(`/api/bookings/${id}`);
      navigate('/admin/bookings');
      return false;
    });
  }

  const statusLink = `${location.origin}/booking/${b.token}`;
  const withLink = (w) => {
    const other = d.booked_events[w.event_id];
    const label = `${w.title} (${fmtTime(w.start)}–${fmtTime(w.end)})`;
    return other ? h('a', { href: `/admin/bookings/${other}`, 'data-link': true }, label) : label;
  };
  const clashList = h('ul', {}, d.clashes.slice(0, 30).map((c) => h('li', {}, fmtRange(c.start, c.end), ': ',
    c.with.map((w, k) => [k ? ', ' : '', withLink(w)]))),
  d.clashes.length > 30 ? h('li', {}, `…and ${d.clashes.length - 30} more`) : null);

  render(
    h('div', { class: 'row' }, h('a', { href: '/admin/bookings', 'data-link': true }, '← Room bookings')),
    h('div', { class: 'row' }, h('h1', {}, b.title), statusBadge(b.status)),
    h('div', { class: 'card' },
      h('dl', { class: 'kv' },
        h('dt', {}, 'Room'), h('dd', {}, roomName(refs, b.room_id)),
        h('dt', {}, 'When'), h('dd', {}, b.recurrence === 'none' ? fmtRange(b.start_at, b.end_at) : `${b.description} (${total} dates)`),
        d.cancelled_dates.length ? [h('dt', {}, 'Not on'), h('dd', {}, d.cancelled_dates.map((x) => h('span', { class: 'pill' }, fmtDT(x))))] : null,
        h('dt', {}, 'Organiser'), h('dd', {}, b.organiser_name, b.organisation ? ` (${b.organisation})` : ''),
        h('dt', {}, 'Contact'), h('dd', {},
          b.email ? h('a', { href: `mailto:${b.email}` }, b.email) : null, b.email && b.phone ? ' · ' : '',
          b.phone ? h('a', { href: `tel:${b.phone}` }, b.phone) : null, !b.email && !b.phone ? '—' : null),
        b.attendees ? [h('dt', {}, 'People'), h('dd', {}, b.attendees)] : null,
        h('dt', {}, 'Equipment'), h('dd', {}, b.needs.length ? b.needs.map((n) => h('span', { class: 'pill' }, `${n.quantity} × ${wantName(refs, n)}`)) : 'None',
          b.av_support ? h('span', { class: 'pill changed' }, 'AV help wanted') : null),
        h('dt', {}, 'Public calendar'), h('dd', {}, b.public_title ? `Shows "${b.title}"` : 'Shows "Booked"'),
        b.notes ? [h('dt', {}, 'Notes'), h('dd', {}, b.notes)] : null,
        b.decided_by ? [h('dt', {}, 'Decided by'), h('dd', {}, b.decided_by, b.decided_at ? h('span', { class: 'muted small' }, ` · ${fmtDT(b.decided_at)}`) : null)] : null),
      h('div', { class: 'row', style: 'margin-top:0.8rem' },
        h('span', { class: 'muted small' }, 'Organiser status link:'),
        h('a', { href: `/booking/${b.token}`, target: '_blank', class: 'small' }, 'open'),
        h('button', { class: 'small', onclick: () => copyText(statusLink) }, 'Copy'),
        canEdit && b.status === 'pending' ? h('a', { class: 'btn small', href: `/admin/bookings/${id}/edit`, 'data-link': true }, 'Edit request') : null,
        b.event_id ? h('a', { class: 'btn small', href: `/admin/schedule?from=${isoDate(toDate(b.start_at))}`, 'data-link': true }, 'View on schedule') : null)),

    clashCount ? h('div', { class: b.status === 'pending' ? 'banner bad' : 'banner warn' },
      b.status === 'pending'
        ? (total > 1 ? `${clashCount} of ${total} dates clash with something already in the room:` : 'This clashes with something already in the room:')
        : 'Overlaps with other things in the room:',
      clashList) : b.status === 'pending'
      ? h('div', { class: 'banner ok' }, total > 1 ? `The room is free on all ${total} dates.` : 'The room is free at that time.') : null,

    d.competing_pending.length ? h('div', { class: 'banner info' }, 'Other pending requests want this room at overlapping times: ',
      d.competing_pending.map((c, k) => [k ? ', ' : '', h('a', { href: `/admin/bookings/${c.id}`, 'data-link': true }, `${c.title} (${c.organiser_name})`)]),
      h('p', {}, 'Whichever is approved first gets the room.')) : null,

    d.equipment_impact.length ? h('div', { class: 'banner warn' }, 'The equipment this booking needs would leave approved loans short:',
      h('ul', {}, d.equipment_impact.map((x) => h('li', {},
        h('a', { href: `/admin/loans/${x.loan_id}`, 'data-link': true }, x.loan_name || `Loan #${x.loan_id}`),
        `: short by ${x.short_by} × ${nodeName(refs, x)} in ${roomName(refs, x.room_id)}`))),
      h('p', {}, 'Talk to the AV team before approving, or approve with less equipment.')) : null,

    canEdit ? h('div', { class: 'card' },
      b.status === 'pending' ? field('Regular group for this booking', groupSel, { hint: 'Used to contact them if the AV team needs to borrow equipment they use. Repeat hirers usually get their own group.' }) : null,
      field('Message to organiser', message),
      field('Private notes', notes),
      saveNotes) : needsRoleNote('bookings'),
    h('div', { class: 'row' }, actions),
  );
}

const typeName = (refs, id) => (refs.types[id] ? refs.types[id].name : `#${id}`);
const kindName = (refs, id) => (refs.kinds && refs.kinds[id] ? refs.kinds[id].name : `#${id}`);
/** A request line: "Shure SM58", or "Wired vocal mic (any)". */
const wantName = (refs, w) => (w.kind_id != null ? `${kindName(refs, w.kind_id)} (any)` : typeName(refs, w.type_id));
/** A tree node, e.g. the kind or model a problem is about. */
const nodeName = (refs, w) => (w.kind_id != null ? kindName(refs, w.kind_id) : typeName(refs, w.type_id));
const roomName = (refs, id) => refs.rooms[id] || `#${id}`;

/** Collect all "please manage with less" asks from an evaluation, grouped by group. */
function collectAsks(ev) {
  const asks = [];
  for (const item of ev.items) {
    for (const res of item.resolutions) for (const a of res.asks) asks.push(a);
  }
  return groupBy(asks, (a) => (a.group_id === null ? 'none' : a.group_id));
}

function contactMessage(group, asks, refs, ctx) {
  const hi = group && group.contact_name ? `Hi ${group.contact_name.split(' ')[0]},` : 'Hi,';
  const who = ctx.loan ? `${ctx.loan.organisation || ctx.loan.organiser_name}${ctx.loan.event_name ? ` (${ctx.loan.event_name})` : ''}` : 'An external event';
  const lines = asks.map((a) => `• ${fmtDT(a.start)} ${refs.events[a.event_id] || ''} in ${roomName(refs, a.room_id)}: ${a.reduce_to} × ${nodeName(refs, a)} instead of the usual ${a.normally}`);
  const period = ctx.loan ? ` from ${fmtDT(ctx.loan.pickup_at)} to ${fmtDT(ctx.loan.return_at)}` : '';
  return `${hi}\n\n${who} has asked to borrow some of the church's AV equipment${period}. To make it work, could ${group ? group.name : 'your group'} manage with a bit less on:\n\n${lines.join('\n')}\n\nIf that's not possible, no problem, just let me know. Thanks!\n\n${churchName} AV team`;
}

function renderEvaluation(ev, refs, ctx = {}) {
  const out = [];
  const short = ev.items.filter((i) => i.shortfall > 0);
  out.push(ev.fully_available
    ? h('div', { class: 'banner ok' }, 'Everything can be lent without affecting regular church users.')
    : h('div', { class: 'banner warn' }, `Short on ${short.length} item type${short.length > 1 ? 's' : ''}.`,
      h('p', {}, 'See below for who to ask, or approve a partial loan.')));

  out.push(h('div', { class: 'card table-wrap' },
    h('h2', { style: 'margin-top:0' }, 'Availability & where to collect'),
    h('table', {},
      h('thead', {}, h('tr', {}, h('th', {}, 'Item'), h('th', { class: 'num' }, 'Requested'), h('th', { class: 'num' }, 'Can lend'), h('th', {}, 'Collect from'), h('th', {}, 'Room detail'))),
      h('tbody', {}, ev.items.map((i) => h('tr', {},
        h('td', {}, wantName(refs, i)),
        h('td', { class: 'num' }, i.requested),
        h('td', { class: 'num' }, i.shortfall ? h('strong', { style: 'color:var(--bad)' }, i.available) : i.available),
        h('td', {}, i.allocation.length ? i.allocation.map((a) => h('div', {},
          i.kind_id != null ? `${a.qty} × ${typeName(refs, a.type_id)} (${roomName(refs, a.room_id)})` : `${a.qty} × ${roomName(refs, a.room_id)}`))
          : h('span', { class: 'muted' }, 'none')),
        h('td', { class: 'small muted' }, i.rooms.map((r) => h('div', {},
          `${roomName(refs, r.room_id)}: owns ${r.stock}, church needs ${r.peak_event_need}`,
          r.peak_on_loan ? `, ${r.peak_on_loan} on other loans` : '',
          ` → spare ${r.spare}`,
          r.overcommitted ? h('strong', { style: 'color:var(--bad)' }, ' (already overbooked!)') : null))),
      ))))));

  const byGroup = collectAsks(ev);
  if (byGroup.size) {
    out.push(h('h2', {}, 'Who to ask'));
    out.push(h('p', { class: 'muted small' }, 'If these regular users can manage with less during the loan, the full request can be met. When someone agrees, mark it here and the plan updates.'));
    for (const [gid, asks] of byGroup) {
      const g = gid === 'none' ? null : refs.groups[gid];
      const msg = contactMessage(g, asks, refs, ctx);
      const agreeAll = ctx.loan && hasRole('av') ? h('button', { class: 'small primary' }, 'They agreed to all') : null;
      if (agreeAll) agreeAll.addEventListener('click', busy(agreeAll, async () => {
        for (const a of asks) await agree(a, ctx);
        toast('Recorded');
        ctx.reload && ctx.reload();
      }));
      out.push(h('div', { class: 'card contact' },
        h('div', { class: 'row' },
          h('div', {},
            h('div', { class: 'who' }, g ? g.name : 'No regular group (one-off event)'),
            g ? h('div', { class: 'small' },
              g.contact_name || 'No contact name', ' · ',
              g.contact_phone ? h('a', { href: `tel:${g.contact_phone}` }, g.contact_phone) : 'no phone', ' · ',
              g.contact_email ? h('a', { href: `mailto:${g.contact_email}?subject=${encodeURIComponent('AV equipment request')}&body=${encodeURIComponent(msg)}` }, g.contact_email) : 'no email') : null),
          h('span', { class: 'spacer' }),
          h('button', { class: 'small', onclick: () => copyText(msg) }, 'Copy message'),
          g && g.contact_phone ? h('a', { class: 'btn small', href: `sms:${g.contact_phone.replace(/\s+/g, '')}?&body=${encodeURIComponent(msg)}` }, 'Text') : null,
          agreeAll),
        asks.map((a) => {
          const btn = ctx.loan && hasRole('av') ? h('button', { class: 'small' }, 'Agreed') : null;
          if (btn) btn.addEventListener('click', busy(btn, async () => {
            await agree(a, ctx);
            toast('Recorded');
            ctx.reload && ctx.reload();
          }));
          return h('div', { class: 'ask row' },
            h('div', {},
              h('div', {}, h('strong', {}, refs.events[a.event_id] || 'Event'), ` · ${roomName(refs, a.room_id)} · ${fmtDT(a.start)}–${fmtTime(a.end)}`),
              h('div', {}, `${wantName(refs, a)}: `, h('span', { class: 'strike' }, a.normally), ` → ${a.reduce_to}`)),
            h('span', { class: 'spacer' }), btn);
        })));
    }
  }

  const unres = ev.items.filter((i) => i.unresolvable > 0);
  if (unres.length) {
    out.push(h('div', { class: 'banner bad' }, 'Cannot be found even if every church group agreed:',
      h('ul', {}, unres.map((i) => h('li', {},
        `${i.unresolvable} × ${wantName(refs, i)}`,
        i.blocking_loans.length ? h('span', {}, ' (some are out on loan ',
          i.blocking_loans.map((id, k) => [k ? ', ' : '', h('a', { href: `/admin/loans/${id}`, 'data-link': true }, `#${id}`)]), ')') : ' (not enough owned)')))));
  }
  return out;
}

async function agree(a, ctx) {
  await POST('/api/overrides', {
    event_id: a.event_id,
    occurrence_start: a.start,
    kind_id: a.kind_id,
    type_id: a.type_id,
    quantity: a.reduce_to,
    note: `Agreed for loan #${ctx.loan.id}`,
    loan_id: ctx.loan.id,
  });
}

function pickList(loan, refs) {
  const byRoom = groupBy(loan.allocations, (a) => a.room_id);
  return h('div', { class: 'card' },
    h('div', { class: 'row' }, h('h2', { style: 'margin:0' }, loan.status === 'collected' ? 'Return to' : 'Collection list'),
      h('span', { class: 'spacer' }), h('button', { class: 'small', onclick: () => window.print() }, 'Print')),
    [...byRoom].map(([room, allocs]) => [
      h('h3', {}, roomName(refs, room)),
      h('ul', {}, allocs.map((a) => h('li', {}, `${a.quantity} × ${typeName(refs, a.type_id)}`)))]));
}

/** Is model `typeId` somewhere below kind `kindId`? */
function modelUnder(refs, typeId, kindId) {
  let k = refs.types[typeId] ? refs.types[typeId].kind_id : null;
  for (let depth = 0; k != null && depth < 32; depth++) {
    if (k === kindId) return true;
    k = refs.kinds[k] ? refs.kinds[k].parent_id : null;
  }
  return false;
}

/**
 * "Models to hand out" on a pending loan. Starts from the engine's suggestion;
 * the AV team can swap models, rooms and quantities. The server checks that the
 * choice serves the request and that the rooms can spare it.
 */
function allocEditor(loan, ev, refs, stock, onChange) {
  const suggested = new Map(ev.items.map((i) => [wantKey(i), i.allocation]));
  const owners = groupBy(stock, (s) => s.type_id);
  const byName = (a, b) => typeName(refs, a).localeCompare(typeName(refs, b));
  const lendable = (t) => refs.types[t] && refs.types[t].lendable;
  const lines = [];

  const lineEl = (item) => {
    const models = item.kind_id != null
      ? [...owners.keys()].filter((t) => modelUnder(refs, t, item.kind_id))
        .sort((a, b) => (lendable(b) - lendable(a)) || byName(a, b))
      : [item.type_id];
    const rows = h('div');
    const count = h('span', { class: 'muted small' });
    const update = () => {
      const chosen = line.chosen();
      count.textContent = `${chosen} of ${item.quantity} chosen`;
      count.style.color = chosen === item.quantity ? '' : 'var(--warn)';
      onChange();
    };
    const addRow = (pick = {}) => {
      const model = h('select', { 'aria-label': 'Model' }, models.map((t) => h('option', { value: t },
        typeName(refs, t) + (item.kind_id != null && !lendable(t) ? ' (not normally lent)' : ''))));
      const room = h('select', { 'aria-label': 'Room' });
      const qty = h('input', { type: 'number', min: 1, value: pick.qty || 1, 'aria-label': 'Quantity' });
      const fillRooms = (roomId) => {
        room.replaceChildren(...(owners.get(+model.value) || []).map((o) =>
          h('option', { value: o.room_id }, `${roomName(refs, o.room_id)} (owns ${o.quantity})`)));
        if (roomId != null) room.value = roomId;
      };
      if (pick.type_id != null) model.value = pick.type_id;
      fillRooms(pick.room_id);
      model.addEventListener('change', () => { fillRooms(); update(); });
      room.addEventListener('change', update);
      qty.addEventListener('input', update);
      const row = h('div', { class: 'item-row' }, model, room, qty,
        h('button', { type: 'button', class: 'small', title: 'Remove', onclick: () => { row.remove(); update(); } }, '✕'));
      row.pick = () => ({ room_id: +room.value, type_id: +model.value, quantity: +qty.value });
      rows.append(row);
    };
    const reset = () => {
      rows.replaceChildren();
      for (const p of suggested.get(wantKey(item)) || []) addRow(p);
    };
    const line = {
      picks: () => [...rows.children].map((r) => r.pick()).filter((p) => p.room_id && p.type_id && p.quantity > 0),
      chosen: () => line.picks().reduce((n, p) => n + p.quantity, 0),
      requested: item.quantity,
      reset: () => { reset(); update(); },
    };
    reset();
    lines.push(line);
    update();
    return h('div', { style: 'margin-bottom:0.8rem' },
      h('div', { class: 'row' }, h('strong', {}, `${item.quantity} × ${wantName(refs, item)}`), count),
      rows,
      models.length
        ? h('button', { type: 'button', class: 'small', onclick: () => { addRow(); update(); } }, '+ Add')
        : h('span', { class: 'muted small' }, 'No room owns anything that fits this line.'));
  };

  const normal = (picks) => JSON.stringify(picks.map((p) => [p.room_id, p.type_id, p.quantity ?? p.qty]).sort());
  const original = normal(loan.items.flatMap((i) => suggested.get(wantKey(i)) || []));
  const editor = {
    get: () => lines.flatMap((l) => l.picks()),
    total: () => lines.reduce((n, l) => n + l.chosen(), 0),
    short: () => lines.some((l) => l.chosen() < l.requested),
    dirty: () => normal(editor.get()) !== original,
  };
  const items = loan.items.map(lineEl);
  editor.el = h('div', { class: 'card no-print' },
    h('div', { class: 'row' }, h('h2', { style: 'margin:0' }, 'Models to hand out'), h('span', { class: 'spacer' }),
      h('button', { type: 'button', class: 'small', onclick: () => { lines.forEach((l) => l.reset()); } }, 'Reset to suggestion')),
    h('p', { class: 'muted small' }, 'Suggested from what each room can spare. Change the models or rooms if you prefer; you will be warned if a room can\'t spare them.'),
    items);
  return editor;
}

async function pageLoan(id) {
  const d = await GET(`/api/loans/${id}`);
  const { loan, refs } = d;
  const reload = () => pageLoan(id);
  const ctx = { loan, reload };

  const message = h('textarea', { placeholder: 'Shown to the organiser on their status page' });
  message.value = loan.message_to_organiser;
  const notes = h('textarea', { placeholder: 'Private notes' });
  notes.value = loan.admin_notes;
  const saveBtn = h('button', { class: 'small' }, 'Save notes');
  saveBtn.addEventListener('click', busy(saveBtn, async () => {
    await PATCH(`/api/loans/${id}`, { admin_notes: notes.value, message_to_organiser: message.value });
    toast('Saved');
  }));

  const statusLink = `${location.origin}/status/${loan.token}`;
  const actions = [];
  const act = (label, cls, fn) => {
    const b = h('button', { class: cls }, label);
    b.addEventListener('click', busy(b, async () => { if (await fn() !== false) reload(); }));
    actions.push(b);
  };
  const setStatus = (status, confirmText) => async () => {
    if (confirmText && !confirm(confirmText)) return false;
    await POST(`/api/loans/${id}/status`, { status, message: message.value });
    toast(`Marked ${status}`);
  };

  let editor = null;
  if (loan.status === 'pending' && hasRole('av')) {
    const approveBtn = h('button', { class: 'primary' });
    const updateApprove = () => {
      approveBtn.textContent = editor.short() ? 'Approve partial loan' : 'Approve & reserve';
      approveBtn.disabled = editor.total() === 0;
    };
    editor = allocEditor(loan, d.evaluation, refs, d.stock, () => editor && updateApprove());
    approveBtn.addEventListener('click', busy(approveBtn, async () => {
      const partial = editor.short();
      if (partial && !confirm('Not everything requested is chosen. Approve and reserve only the models chosen?')) return;
      const body = { message: message.value, partial };
      if (editor.dirty()) body.allocations = editor.get();
      try {
        await POST(`/api/loans/${id}/approve`, body);
      } catch (e) {
        // A room can't spare what was chosen: overbooking needs an explicit OK.
        if (e.status !== 409 || !e.body || !Array.isArray(e.body.detail)) throw e;
        const over = e.body.detail.map((p) => `• ${roomName(refs, p.room_id)}: over by ${p.over} × ${nodeName(refs, p)}`);
        if (!confirm(`${e.message}:\n${over.join('\n')}\n\nReserve anyway? Church events in those rooms will be short.`)) return;
        await POST(`/api/loans/${id}/approve`, { ...body, force: true });
      }
      toast(partial ? 'Approved (partial)' : 'Approved');
      reload();
    }));
    actions.push(approveBtn);
    updateApprove();
    act('Decline', 'danger', setStatus('declined', 'Decline this request? Put a reason in the message to the organiser first.'));
  }
  if (!hasRole('av')) { /* read-only view */ } else if (loan.status === 'approved') {
    act('Mark collected', 'primary', setStatus('collected'));
    act('Back to pending', '', setStatus('pending', 'Release the reserved equipment and return to pending?'));
    act('Cancel loan', 'danger', setStatus('cancelled', 'Cancel this loan? Any agreements with groups for it are released.'));
  }
  if (loan.status === 'collected' && hasRole('av')) act('Mark returned', 'primary', setStatus('returned'));
  if (['declined', 'cancelled'].includes(loan.status) && hasRole('av')) {
    act('Reopen', '', setStatus('pending'));
    act('Delete', 'danger', async () => {
      if (!confirm('Delete this request permanently?')) return false;
      await DEL(`/api/loans/${id}`);
      navigate('/admin/loans');
      return false;
    });
  }

  const overdue = loan.status === 'collected' && toDate(loan.return_at) < new Date();
  render(
    h('div', { class: 'row' }, h('a', { href: '/admin/loans', 'data-link': true, class: 'no-print' }, '← Loan requests')),
    h('div', { class: 'row' }, h('h1', {}, loan.event_name || `Request #${loan.id}`), statusBadge(loan.status),
      overdue ? h('span', { class: 'badge declined' }, 'overdue') : null),
    h('div', { class: 'card' },
      h('dl', { class: 'kv' },
        h('dt', {}, 'Organiser'), h('dd', {}, loan.organiser_name, loan.organisation ? ` (${loan.organisation})` : ''),
        h('dt', {}, 'Contact'), h('dd', {},
          loan.email ? h('a', { href: `mailto:${loan.email}` }, loan.email) : null, loan.email && loan.phone ? ' · ' : '',
          loan.phone ? h('a', { href: `tel:${loan.phone}` }, loan.phone) : null),
        h('dt', {}, 'Used at'), h('dd', {}, loan.event_location || '—'),
        h('dt', {}, 'Pick up'), h('dd', {}, fmtDT(loan.pickup_at)),
        h('dt', {}, 'Return'), h('dd', {}, fmtDT(loan.return_at), h('span', { class: 'muted small' }, ` (+${d.buffer_hours}h turnaround)`)),
        h('dt', {}, 'Requested'), h('dd', {}, loan.items.map((i, k) => {
          // For "any of a kind", show which models were given.
          const given = i.kind_id != null && d.given[k] && d.given[k].length
            ? ` → ${d.given[k].map((g) => `${g.qty} × ${typeName(refs, g.type_id)}`).join(', ')}` : '';
          return h('span', { class: 'pill' }, `${i.quantity} × ${wantName(refs, i)}`, given ? h('span', { class: 'muted' }, given) : null);
        })),
        loan.notes ? [h('dt', {}, 'Notes'), h('dd', {}, loan.notes)] : null,
        loan.decided_by ? [h('dt', {}, 'Decided by'), h('dd', {}, loan.decided_by)] : null),
      h('div', { class: 'row no-print', style: 'margin-top:0.8rem' },
        h('span', { class: 'muted small' }, 'Organiser status link:'),
        h('a', { href: `/status/${loan.token}`, target: '_blank', class: 'small' }, 'open'),
        h('button', { class: 'small', onclick: () => copyText(statusLink) }, 'Copy'),
        loan.status === 'pending' && hasRole('av') ? h('button', { class: 'small', onclick: () => editLoanDialog(loan, refs, reload) }, 'Edit dates / items') : null)),

    d.competing_pending.length ? h('div', { class: 'banner info' }, 'Other pending requests want the same equipment around these dates: ',
      d.competing_pending.map((c, k) => [k ? ', ' : '', h('a', { href: `/admin/loans/${c.id}`, 'data-link': true }, `#${c.id} ${c.event_name || c.organiser_name}`)]),
      h('p', {}, 'Approving this one first reserves the equipment for it.')) : null,

    d.allocation_problems.length ? h('div', { class: 'banner bad' }, 'Schedule changed since approval: some rooms are now short.',
      h('ul', {}, d.allocation_problems.map((p) => h('li', {}, `${roomName(refs, p.room_id)}: over by ${p.over} × ${nodeName(refs, p)}`)))) : null,

    d.evaluation ? renderEvaluation(d.evaluation, refs, ctx) : null,
    editor ? editor.el : null,
    loan.allocations.length && loan.status !== 'pending' ? pickList(loan, refs) : null,

    d.agreed_overrides.length ? h('div', { class: 'card' }, h('h2', { style: 'margin-top:0' }, 'Agreements recorded for this loan'),
      d.agreed_overrides.map((o) => h('div', { class: 'row ask' },
        h('span', {}, `${refs.events[o.event_id] || 'Event'} · ${fmtDT(o.occurrence_start)} · ${o.type_id != null || o.kind_id != null ? `${wantName(refs, o)} reduced to ${o.quantity}` : 'cancelled'}`),
        h('span', { class: 'spacer' }),
        h('button', { class: 'small', onclick: busy(null, async () => { await DEL(`/api/overrides/${o.id}`); reload(); }) }, 'Undo'))),
      h('p', { class: 'muted small' }, 'These are released automatically if the loan is declined, cancelled or returned.')) : null,

    hasRole('av') ? h('div', { class: 'card no-print' },
      field('Message to organiser', message),
      field('Private notes', notes),
      saveBtn) : needsRoleNote('av'),
    h('div', { class: 'row no-print' }, actions),
  );
}

function editLoanDialog(loan, refs, reload) {
  const catalog = refsCatalog(refs);
  const pickup = h('input', { type: 'datetime-local', value: loan.pickup_at.slice(0, 16) });
  const ret = h('input', { type: 'datetime-local', value: loan.return_at.slice(0, 16) });
  const items = itemsEditor(catalog, loan.items);
  const save = h('button', { class: 'primary' }, 'Save');
  const dlg = h('dialog', {},
    h('h2', { style: 'margin-top:0' }, 'Edit request'),
    h('div', { class: 'grid' }, field('Pick up', pickup), field('Return', ret)),
    items.el,
    h('div', { class: 'row', style: 'margin-top:1rem' }, save, h('button', { onclick: () => dlg.close() }, 'Cancel')));
  save.addEventListener('click', busy(save, async () => {
    await PATCH(`/api/loans/${loan.id}`, { pickup_at: pickup.value, return_at: ret.value, items: items.get() });
    dlg.close();
    reload();
  }));
  dlg.addEventListener('close', () => dlg.remove());
  document.body.append(dlg);
  dlg.showModal();
}

async function pageCheck() {
  const [kinds, equipment] = await Promise.all([GET('/api/kinds'), GET('/api/equipment')]);
  const catalog = { kinds, models: equipment.filter((e) => e.total > 0) };
  const now = new Date();
  now.setMinutes(0, 0, 0);
  const pickup = h('input', { type: 'datetime-local', value: localInput(new Date(now.getTime() + 86400000)) });
  const ret = h('input', { type: 'datetime-local', value: localInput(new Date(now.getTime() + 2 * 86400000)) });
  const items = itemsEditor(catalog);
  const out = h('div');
  const btn = h('button', { class: 'primary' }, 'Check');
  btn.addEventListener('click', busy(btn, async () => {
    const r = await POST('/api/check', { pickup_at: pickup.value, return_at: ret.value, items: items.get() });
    out.replaceChildren(...renderEvaluation(r.evaluation, r.refs, {}).flat().filter(Boolean));
  }));
  render(
    h('h1', {}, 'Quick availability check'),
    h('p', { class: 'muted' }, 'Answer "can we lend X on these dates?" without creating a request.'),
    h('div', { class: 'card' }, h('div', { class: 'grid' }, field('Pick up', pickup), field('Return', ret)), items.el,
      h('div', { style: 'margin-top:1rem' }, btn)),
    out,
  );
}

async function pageSchedule() {
  const params = new URLSearchParams(location.search);
  const from = params.get('from') || isoDate(new Date());
  const days = params.get('days') || '14';
  const d = await GET(`/api/schedule?from=${from}&days=${days}`);
  const { refs } = d;
  const fromIn = h('input', { type: 'date', value: from });
  const daysIn = h('select', {}, [7, 14, 28, 60].map((n) => h('option', { value: n }, `${n} days`)));
  daysIn.value = days;
  const go = () => navigate(`/admin/schedule?from=${fromIn.value}&days=${daysIn.value}`);
  fromIn.addEventListener('change', go);
  daysIn.addEventListener('change', go);

  const byDay = groupBy(d.occurrences, (o) => o.start.slice(0, 10));
  const reload = () => pageSchedule();
  render(
    h('h1', {}, 'Church schedule'),
    h('p', { class: 'muted' }, 'Church events and approved room bookings.'),
    h('div', { class: 'row card' }, field('From', fromIn), field('Show', daysIn)),
    d.loans.length ? h('div', { class: 'card' }, h('h2', { style: 'margin-top:0' }, 'Loans in this period'),
      d.loans.map((l) => h('div', { class: 'row' }, statusBadge(l.status),
        h('a', { href: `/admin/loans/${l.id}`, 'data-link': true }, l.event_name || l.organiser_name),
        h('span', { class: 'muted small' }, `${fmtDT(l.pickup_at)} → ${fmtDT(l.return_at)}`)))) : null,
    d.occurrences.length === 0 ? h('p', { class: 'muted' }, 'No church events in this period.') :
      h('div', { class: 'card' }, [...byDay].map(([day, occs]) => [
        h('div', { class: 'day' }, fmtDate(day + 'T00:00:00')),
        occs.map((o) => {
          const group = o.group_id ? refs.groups[o.group_id] : null;
          const cancelOv = o.overrides.find((x) => x.type_id === null && x.kind_id === null);
          const toggle = h('button', { class: 'small' }, o.cancelled ? 'Restore' : 'Cancel this date');
          toggle.addEventListener('click', busy(toggle, async () => {
            if (o.cancelled) await DEL(`/api/overrides/${cancelOv.id}`);
            else {
              const note = prompt('Reason (optional), e.g. "School holidays"');
              if (note === null) return;
              await POST('/api/overrides', { event_id: o.event_id, occurrence_start: o.start, type_id: null, note });
            }
            reload();
          }));
          return h('div', { class: 'occ' + (o.cancelled ? ' cancelled' : '') },
            h('div', { class: 'time' }, `${fmtTime(o.start)}–${fmtTime(o.end)}`),
            h('div', { style: 'flex:1;min-width:220px' },
              h('div', {}, h('strong', {}, o.title), ` · ${roomName(refs, o.room_id)}`, group ? h('span', { class: 'muted' }, ` · ${group.name}`) : null,
                d.booked_events[o.event_id] ? ' ' : null,
                d.booked_events[o.event_id] ? h('a', { class: 'pill', href: `/admin/bookings/${d.booked_events[o.event_id]}`, 'data-link': true }, 'booking') : null,
                o.cancelled ? h('span', { class: 'badge cancelled', style: 'margin-left:0.4rem' }, 'cancelled') : null),
              h('div', {}, o.needs.map((n) => {
                const u = o.usual_needs.find((x) => wantKey(x) === wantKey(n));
                const usual = u ? u.quantity : undefined;
                const changed = usual !== n.quantity;
                return h('span', { class: 'pill' + (changed ? ' changed' : '') }, `${n.quantity} × ${wantName(refs, n)}`, changed ? ` (usually ${usual ?? 0})` : '');
              })),
              o.overrides.filter((x) => x.type_id !== null || x.kind_id !== null).map((x) => h('div', { class: 'small muted row' },
                `${wantName(refs, x)} → ${x.quantity}${x.note ? ` · ${x.note}` : ''}`,
                hasRole('av') ? h('button', { class: 'small', onclick: busy(null, async () => { await DEL(`/api/overrides/${x.id}`); reload(); }) }, 'Undo') : null))),
            toggle);
        })])),
  );
}

async function pageStock() {
  const [rooms, kinds, equipment, stock] = await Promise.all([GET('/api/rooms'), GET('/api/kinds'), GET('/api/equipment'), GET('/api/stock')]);
  if (!rooms.length || !equipment.length) {
    return render(h('h1', {}, 'Stock'), h('div', { class: 'banner info' }, 'Add some ',
      h('a', { href: '/admin/rooms', 'data-link': true }, 'rooms'), ' and ',
      h('a', { href: '/admin/equipment', 'data-link': true }, 'equipment'), ' first.'));
  }
  const q = new Map(stock.map((s) => [`${s.room_id}:${s.type_id}`, s.quantity]));
  // Kinds above each model, for the per-kind subtotal rows.
  const parent = new Map(kinds.map((k) => [k.id, k.parent_id]));
  const ancestors = (kindId) => {
    const out = [];
    for (let k = kindId; k != null && parent.has(k) && !out.includes(k); k = parent.get(k)) out.push(k);
    return out;
  };
  const above = new Map(equipment.map((t) => [t.id, ancestors(t.kind_id)]));
  const cells = []; // { room (null = total), kind | type, td }
  const refresh = () => {
    for (const c of cells) {
      const models = c.type != null ? [c.type] : equipment.filter((t) => above.get(t.id).includes(c.kind)).map((t) => t.id);
      const rs = c.room != null ? [c.room] : rooms.map((r) => r.id);
      const sum = models.reduce((s, t) => s + rs.reduce((a, r) => a + (q.get(`${r}:${t}`) || 0), 0), 0);
      c.td.textContent = sum;
    }
  };
  const body = treeRows(kinds, equipment).map((r) => {
    const indented = `padding-left:${0.6 + r.depth * 1.2}rem`;
    if (r.kind) {
      const k = r.kind;
      const tds = [...rooms.map((room) => ({ room: room.id })), { room: null }].map((c) => {
        const td = h('td', { class: 'num' });
        cells.push({ ...c, kind: k.id, td });
        return td;
      });
      return h('tr', { class: 'cat' }, h('td', { style: indented }, k.name, k.requestable ? h('span', { class: 'muted small' }, ' (any)') : null), tds);
    }
    const t = r.model;
    const totalCell = h('td', { class: 'num' });
    cells.push({ room: null, type: t.id, td: totalCell });
    return h('tr', {},
      h('th', { style: indented }, t.name, t.lendable ? null : h('div', { class: 'muted small' }, 'not lendable')),
      rooms.map((room) => {
        const input = h('input', { type: 'number', min: 0, value: q.get(`${room.id}:${t.id}`) || 0, 'aria-label': `${t.name} in ${room.name}`, disabled: !hasRole('av') });
        input.addEventListener('change', async () => {
          try {
            const quantity = Math.max(0, parseInt(input.value || '0', 10));
            await PUT('/api/stock', { room_id: room.id, type_id: t.id, quantity });
            q.set(`${room.id}:${t.id}`, quantity);
            refresh();
            toast('Saved');
          } catch (e) { toast(e.message, true); }
        });
        return h('td', { class: 'num' }, input);
      }),
      totalCell);
  });
  refresh();
  render(
    h('h1', {}, 'Stock by room'),
    h('p', { class: 'muted' }, 'Where each model normally lives. Kind rows show subtotals. Changes save automatically.'),
    hasRole('av') ? null : needsRoleNote('av'),
    h('div', { class: 'card table-wrap' }, h('table', { class: 'stock-matrix' },
      h('thead', {}, h('tr', {}, h('th', {}, 'Equipment'), rooms.map((r) => h('th', { class: 'num' }, r.name)), h('th', { class: 'num' }, 'Total'))),
      h('tbody', {}, body))),
  );
}

/**
 * An inline add/edit form for one entity, shown in `editor`. Fields:
 * { key, label, type: text|number|textarea|checkbox|select, options(item) → [[value, label, disabled?]] }.
 * Select values are ids: '' becomes null, anything else a number.
 */
function crudEditor(editor, { fields, endpoint, rowLabel }) {
  return (item) => {
    const isNew = !item.id;
    const inputs = {};
    const els = fields.map((f) => {
      let input;
      if (f.type === 'checkbox') {
        input = h('input', { type: 'checkbox', checked: item[f.key] });
        inputs[f.key] = input;
        return h('div', { class: 'field' }, h('label', { class: 'check' }, input, f.label));
      }
      if (f.type === 'select') {
        input = h('select', {}, f.options(item).map(([v, l, disabled]) => h('option', { value: v, disabled }, l)));
        input.value = item[f.key] ?? '';
      } else {
        input = f.type === 'textarea' ? h('textarea') : h('input', { type: f.type || 'text', list: f.list || null });
        input.value = item[f.key] ?? '';
      }
      inputs[f.key] = input;
      return field(f.label, input, { required: f.required, hint: f.hint });
    });
    const save = h('button', { class: 'primary', type: 'submit' }, isNew ? 'Add' : 'Save');
    const form = h('form', { class: 'card' }, h('h2', { style: 'margin-top:0' }, isNew ? `Add ${rowLabel}` : `Edit ${rowLabel}`),
      h('div', { class: 'grid' }, els),
      h('div', { class: 'row' }, save, h('button', { type: 'button', onclick: () => editor.replaceChildren() }, 'Cancel'),
        isNew ? null : h('span', { class: 'spacer' }),
        isNew ? null : h('button', { type: 'button', class: 'danger', onclick: busy(null, async () => {
          if (!confirm(`Delete this ${rowLabel}?`)) return;
          await DEL(`${endpoint}/${item.id}`);
          toast('Deleted');
          route();
        }) }, 'Delete')));
    form.addEventListener('submit', busy(save, async () => {
      const body = { ...item };
      for (const f of fields) {
        const v = f.type === 'checkbox' ? inputs[f.key].checked : inputs[f.key].value;
        body[f.key] = f.type === 'number' || f.type === 'select' ? (v === '' ? null : Number(v)) : v;
      }
      if (isNew) await POST(endpoint, body); else await PUT(`${endpoint}/${item.id}`, body);
      toast('Saved');
      route();
    }));
    editor.replaceChildren(form);
    form.scrollIntoView({ behavior: 'smooth' });
    form.querySelector('input,textarea').focus();
  };
}

/** Generic list + inline editor page for simple entities. */
function simpleCrud({ title, intro, items, columns, fields, endpoint, blank, rowLabel, canEdit = true, editRole }) {
  const editor = h('div');
  const openEditor = crudEditor(editor, { fields, endpoint, rowLabel });
  render(
    h('div', { class: 'row' }, h('h1', {}, title), h('span', { class: 'spacer' }),
      canEdit ? h('button', { class: 'primary', onclick: () => openEditor({ ...blank }) }, `+ Add ${rowLabel}`) : null),
    intro ? h('p', { class: 'muted' }, intro) : null,
    canEdit ? null : needsRoleNote(editRole),
    editor,
    items.length === 0 ? h('p', { class: 'muted' }, 'None yet.') :
      h('div', { class: 'card table-wrap' }, h('table', {},
        h('thead', {}, h('tr', {}, columns.map((c) => h('th', { class: c.num ? 'num' : '' }, c.label)))),
        h('tbody', {}, items.map((it) => h('tr', { class: canEdit ? 'clickable' : '', onclick: canEdit ? () => openEditor(it) : null },
          columns.map((c) => h('td', { class: c.num ? 'num' : '' }, c.render ? c.render(it) : it[c.key]))))))),
  );
}

async function pageRooms() {
  simpleCrud({
    title: 'Rooms', rowLabel: 'room', endpoint: '/api/rooms', items: await GET('/api/rooms'),
    intro: 'Rooms people can book, and storage areas where equipment lives.',
    blank: { name: '', notes: '', bookable: true, capacity: null, public_notes: '' },
    columns: [
      { label: 'Name', key: 'name' },
      { label: 'Bookable', render: (r) => (r.bookable ? 'Yes' : 'No') },
      { label: 'Capacity', key: 'capacity', num: true },
      { label: 'Staff notes', key: 'notes' }],
    fields: [
      { key: 'name', label: 'Name', required: true },
      { key: 'capacity', label: 'Capacity (people)', type: 'number' },
      { key: 'public_notes', label: 'Description for people booking', type: 'textarea', hint: 'e.g. "Kitchen next door, piano, seats 80"' },
      { key: 'notes', label: 'Staff notes', type: 'textarea' },
      { key: 'bookable', label: 'Can be booked online (untick for store rooms and offices)', type: 'checkbox' }],
  });
}

async function pageEquipment() {
  const [kinds, models] = await Promise.all([GET('/api/kinds'), GET('/api/equipment')]);
  const canEdit = hasRole('av');
  const editor = h('div');
  const rows = treeRows(kinds, models);
  const kindById = new Map(kinds.map((k) => [k.id, k]));
  // A kind can't move under itself or its own sub-kinds.
  const isUnder = (kindId, ancestorId) => {
    for (let k = kindId, n = 0; k != null && n < 50; k = kindById.get(k)?.parent_id, n++) if (k === ancestorId) return true;
    return false;
  };
  const kindOptions = (none, exclude) => [['', none],
    ...rows.filter((r) => r.kind).map((r) => [r.kind.id, indent(r.depth) + r.kind.name, exclude != null && isUnder(r.kind.id, exclude)])];
  const editKind = crudEditor(editor, {
    endpoint: '/api/kinds', rowLabel: 'kind',
    fields: [
      { key: 'name', label: 'Name', required: true, hint: 'e.g. "Wireless vocal mic". Names are unique across the whole tree.' },
      { key: 'parent_id', label: 'Inside', type: 'select', options: (k) => kindOptions('— top level —', k.id || null) },
      { key: 'notes', label: 'Notes for people asking', type: 'textarea', hint: 'e.g. "Dynamic, with clip"' },
      { key: 'requestable', label: 'People can ask for "any" of these (untick for headings like Cables, where models aren\'t interchangeable)', type: 'checkbox' }],
  });
  const editModel = crudEditor(editor, {
    endpoint: '/api/equipment', rowLabel: 'model',
    fields: [
      { key: 'name', label: 'Name', required: true, hint: 'e.g. "Shure SM58 vocal mic"' },
      { key: 'kind_id', label: 'Kind', type: 'select', options: () => kindOptions('— none —') },
      { key: 'notes', label: 'Notes', type: 'textarea' },
      { key: 'lendable', label: 'Can be lent out (untick for installed equipment)', type: 'checkbox' }],
  });
  const newKind = (parent_id = null) => editKind({ name: '', parent_id, requestable: true, sort_order: 0, notes: '' });
  const newModel = (kind_id = null) => editModel({ name: '', kind_id, notes: '', lendable: true });

  // Units owned below each kind.
  const totals = new Map();
  for (const m of models) {
    for (let k = m.kind_id, n = 0; k != null && kindById.has(k) && n < 50; k = kindById.get(k).parent_id, n++) {
      totals.set(k, (totals.get(k) || 0) + m.total);
    }
  }
  const stop = (fn) => (e) => { e.stopPropagation(); fn(); };
  render(
    h('div', { class: 'row' }, h('h1', {}, 'Equipment'), h('span', { class: 'spacer' }),
      canEdit ? h('button', { onclick: () => newKind() }, '+ Add kind') : null,
      canEdit ? h('button', { class: 'primary', onclick: () => newModel() }, '+ Add model') : null),
    h('p', { class: 'muted' }, 'Models are what you own (set how many are in each room on the Stock page). ',
      'Kinds group interchangeable models, so people can ask for "any wireless vocal mic" and the AV team picks which. ',
      'Kinds can sit inside other kinds, e.g. Microphones › Vocal mic › Wireless vocal mic.'),
    canEdit ? null : needsRoleNote('av'),
    editor,
    rows.length === 0 ? h('p', { class: 'muted' }, 'None yet.') :
      h('div', { class: 'card table-wrap' }, h('table', { class: 'tree-table' },
        h('thead', {}, h('tr', {}, h('th', {}, 'Name'), h('th', {}, ''), h('th', { class: 'num' }, 'Total owned'), canEdit ? h('th', {}, '') : null)),
        h('tbody', {}, rows.map((r) => {
          const indented = `padding-left:${0.6 + r.depth * 1.4}rem`;
          if (r.kind) {
            const k = r.kind;
            return h('tr', { class: 'kind' + (canEdit ? ' clickable' : ''), onclick: canEdit ? () => editKind(k) : null },
              h('td', { style: indented }, h('strong', {}, k.name), k.notes ? h('div', { class: 'muted small' }, k.notes) : null),
              h('td', { class: 'small' }, k.requestable ? 'Kind · can ask for any' : h('span', { class: 'muted' }, 'Heading')),
              h('td', { class: 'num' }, totals.get(k.id) || 0),
              canEdit ? h('td', { class: 'nowrap' },
                h('button', { class: 'small', onclick: stop(() => newKind(k.id)) }, '+ Kind'), ' ',
                h('button', { class: 'small', onclick: stop(() => newModel(k.id)) }, '+ Model')) : null);
          }
          const m = r.model;
          return h('tr', { class: canEdit ? 'clickable' : '', onclick: canEdit ? () => editModel(m) : null },
            h('td', { style: indented }, m.name, m.notes ? h('div', { class: 'muted small' }, m.notes) : null),
            h('td', { class: 'small muted' }, m.lendable ? 'Model' : 'Model · not lendable'),
            h('td', { class: 'num' }, m.total),
            canEdit ? h('td', {}) : null);
        })))),
  );
}

async function pageGroups() {
  simpleCrud({
    title: 'Regular groups', rowLabel: 'group', endpoint: '/api/groups', items: await GET('/api/groups'),
    intro: 'Groups that use the church regularly, and who to contact if they need to make do with less.',
    blank: { name: '', contact_name: '', contact_phone: '', contact_email: '', notes: '' },
    columns: [
      { label: 'Group', key: 'name' }, { label: 'Point of contact', key: 'contact_name' },
      { label: 'Phone', key: 'contact_phone' }, { label: 'Email', key: 'contact_email' }],
    fields: [
      { key: 'name', label: 'Group name', required: true },
      { key: 'contact_name', label: 'Point of contact' },
      { key: 'contact_phone', label: 'Phone', type: 'tel' },
      { key: 'contact_email', label: 'Email', type: 'email' },
      { key: 'notes', label: 'Notes', type: 'textarea' }],
  });
}

async function pageEvents() {
  const [events, rooms, groups, kinds, equipment] = await Promise.all(
    [GET('/api/events'), GET('/api/rooms'), GET('/api/groups'), GET('/api/kinds'), GET('/api/equipment')]);
  const rName = Object.fromEntries(rooms.map((r) => [r.id, r.name]));
  const gName = Object.fromEntries(groups.map((g) => [g.id, g.name]));
  const names = { types: Object.fromEntries(equipment.map((t) => [t.id, t])), kinds: Object.fromEntries(kinds.map((k) => [k.id, k])) };
  const editor = h('div');

  const openEditor = (ev) => {
    const isNew = !ev.id;
    const title = h('input', { value: ev.title });
    const room = h('select', {}, rooms.map((r) => h('option', { value: r.id }, r.name)));
    if (ev.room_id) room.value = ev.room_id;
    const group = h('select', {}, h('option', { value: '' }, '— none (one-off / no regular group) —'), groups.map((g) => h('option', { value: g.id }, g.name)));
    group.value = ev.group_id ?? '';
    const date = h('input', { type: 'date', value: ev.start_date });
    const time = h('input', { type: 'time', value: ev.start_time });
    const endTime = h('input', { type: 'time' });
    const durDays = h('input', { type: 'number', min: 0, max: 6, value: 0, style: 'max-width:5rem' });
    // Convert duration to an end time (+ days) for easier editing.
    const setEnd = () => {
      const [hh, mm] = (time.value || '00:00').split(':').map(Number);
      const endMin = hh * 60 + mm + (ev.duration_minutes || 120);
      durDays.value = Math.floor(endMin / 1440);
      endTime.value = `${pad(Math.floor((endMin % 1440) / 60))}:${pad(endMin % 60)}`;
    };
    setEnd();
    const rec = h('select', {}, h('option', { value: 'none' }, 'Once'), h('option', { value: 'weekly' }, 'Weekly'), h('option', { value: 'monthly' }, 'Monthly (nth weekday)'));
    rec.value = ev.recurrence;
    const interval = h('select', {}, [1, 2, 3, 4].map((n) => h('option', { value: n }, n === 1 ? 'every week' : `every ${n} weeks`)));
    interval.value = ev.interval_weeks;
    const nth = h('select', {}, [[1, '1st'], [2, '2nd'], [3, '3rd'], [4, '4th'], [5, '5th'], [-1, 'last']].map(([v, l]) => h('option', { value: v }, `${l} of the month`)));
    nth.value = ev.nth;
    const until = h('input', { type: 'date', value: ev.until || '' });
    const notes = h('textarea');
    notes.value = ev.notes || '';
    const needs = itemsEditor({ kinds, models: equipment }, ev.needs.length ? ev.needs : []);
    const recFields = h('div', { class: 'grid' });
    const updateRec = () => {
      recFields.replaceChildren(...[
        rec.value === 'weekly' ? field('Repeat', interval) : null,
        rec.value === 'monthly' ? field('Which', nth, { hint: 'Weekday is taken from the first date' }) : null,
        rec.value !== 'none' ? field('Until (optional)', until) : null].filter(Boolean));
    };
    rec.addEventListener('change', updateRec);
    updateRec();
    const save = h('button', { class: 'primary', type: 'submit' }, isNew ? 'Add event' : 'Save');
    const form = h('form', { class: 'card' },
      h('h2', { style: 'margin-top:0' }, isNew ? 'Add church event' : 'Edit church event'),
      ev.booking_id ? h('div', { class: 'banner info' }, 'This event comes from ',
        h('a', { href: `/admin/bookings/${ev.booking_id}`, 'data-link': true }, `room booking #${ev.booking_id}`),
        h('p', {}, 'Changes here update the live booking. To remove it, cancel the booking.')) : null,
      h('div', { class: 'grid' },
        field('Title', title, { required: true }),
        field('Room', room, { required: true }),
        field('Regular group', group)),
      h('div', { class: 'grid' },
        field(rec.value === 'none' ? 'Date' : 'First date', date, { required: true }),
        field('Start', time, { required: true }),
        field('End', endTime, { required: true }),
        field('Ends days later', durDays, { hint: 'For overnight events' })),
      h('div', { class: 'grid' }, field('Repeats', rec)),
      recFields,
      h('h3', {}, 'Minimum equipment that must stay in the room'),
      h('p', { class: 'muted small' }, 'Only equipment from this room counts for this event. Choose a kind marked "(any)" when any model will do, so the AV team can lend whichever is spare.'),
      needs.el,
      h('div', { class: 'field', style: 'margin-top:1rem' }, h('label', {}, 'Notes'), notes),
      h('div', { class: 'row' }, save, h('button', { type: 'button', onclick: () => editor.replaceChildren() }, 'Cancel'),
        isNew || ev.booking_id ? null : h('span', { class: 'spacer' }),
        isNew || ev.booking_id ? null : h('button', { type: 'button', class: 'danger', onclick: busy(null, async () => {
          if (!confirm('Delete this event and all its occurrences?')) return;
          await DEL(`/api/events/${ev.id}`);
          route();
        }) }, 'Delete')));
    form.addEventListener('submit', busy(save, async () => {
      if (!time.value || !endTime.value) throw new Error('Start and end times are required');
      const [sh, sm] = time.value.split(':').map(Number);
      const [eh, em] = endTime.value.split(':').map(Number);
      const duration = (+durDays.value || 0) * 1440 + (eh * 60 + em) - (sh * 60 + sm);
      if (!(duration > 0)) throw new Error('End must be after start');
      const body = {
        title: title.value, room_id: +room.value, group_id: group.value ? +group.value : null,
        start_date: date.value, start_time: time.value, duration_minutes: duration,
        recurrence: rec.value, interval_weeks: +interval.value, nth: +nth.value,
        until: rec.value === 'none' ? null : until.value || null, notes: notes.value, needs: needs.get(),
      };
      const res = isNew ? await POST('/api/events', body) : await PUT(`/api/events/${ev.id}`, body);
      toast('Saved');
      if (!res.clashes.length) return route();
      // Saved, but the room is double-booked on some dates: make sure staff see it.
      editor.replaceChildren(h('div', { class: 'banner warn' },
        `Saved, but "${body.title}" overlaps other things in ${rName[body.room_id]} on ${res.clashes.length} date${res.clashes.length > 1 ? 's' : ''}:`,
        h('ul', {}, res.clashes.slice(0, 10).map((c) => h('li', {}, fmtRange(c.start, c.end), ': ', c.with.map((w) => w.title).join(', ')))),
        res.clashes.length > 10 ? h('p', {}, `…and ${res.clashes.length - 10} more.`) : null,
        h('p', {}, 'If that is not intended, change the time or room, or cancel the clashing dates on the Schedule page.'),
        h('button', { onclick: () => route() }, 'OK')));
      editor.scrollIntoView({ behavior: 'smooth' });
    }));
    editor.replaceChildren(form);
    form.scrollIntoView({ behavior: 'smooth' });
  };

  if (!rooms.length) {
    return render(h('h1', {}, 'Church events'), h('div', { class: 'banner info' }, 'Add ', h('a', { href: '/admin/rooms', 'data-link': true }, 'rooms'), ' first.'));
  }
  const blank = { title: '', room_id: rooms[0].id, group_id: null, start_date: isoDate(new Date()), start_time: '10:00', duration_minutes: 120, recurrence: 'weekly', interval_weeks: 1, nth: 1, until: null, notes: '', needs: [] };
  const byRoom = groupBy(events, (e) => e.room_id);
  render(
    h('div', { class: 'row' }, h('h1', {}, 'Church events'), h('span', { class: 'spacer' }),
      h('button', { class: 'primary', onclick: () => openEditor({ ...blank }) }, '+ Add event')),
    h('p', { class: 'muted' }, 'Regular and one-off events at church, and the minimum equipment each needs to stay in its room.'),
    editor,
    events.length === 0 ? h('p', { class: 'muted' }, 'No events yet.') :
      [...byRoom].map(([roomId, evs]) => h('div', { class: 'card table-wrap' },
        h('h2', { style: 'margin-top:0' }, rName[roomId]),
        h('table', {}, h('tbody', {}, evs.map((e) => h('tr', { class: 'clickable', onclick: () => openEditor(e) },
          h('td', {}, h('strong', {}, e.title), e.booking_id ? [' ', h('span', { class: 'pill' }, 'booking')] : null,
            h('div', { class: 'muted small' }, e.group_id ? gName[e.group_id] : 'No regular group')),
          h('td', {}, e.description),
          h('td', {}, e.needs.map((n) => h('span', { class: 'pill' }, `${n.quantity} × ${wantName(names, n)}`))))))))),
  );
}

async function pageSettings() {
  const s = await GET('/api/settings');
  const isAdmin = hasRole('admin');
  const name = h('input', { value: s.church_name, disabled: !isAdmin });
  const buffer = h('input', { type: 'number', min: 0, max: 336, value: s.buffer_hours, disabled: !isAdmin });
  const save = h('button', { class: 'primary', type: 'submit' }, 'Save');
  const form = h('form', { class: 'card' },
    h('h2', {}, 'Church'),
    field('Church name', name),
    field('Equipment turnaround buffer (hours)', buffer, { hint: 'How long after a loan is due back before the equipment counts as available again (checking, setting back up).' }),
    isAdmin ? save : needsRoleNote('admin'));
  form.addEventListener('submit', busy(save, async () => {
    await PUT('/api/settings', { church_name: name.value, buffer_hours: +buffer.value });
    await loadInfo();
    toast('Saved');
  }));

  const links = [['Book a room', '/book'], ['Room availability', '/rooms'], ['Borrow equipment', '/request']];
  render(
    h('h1', {}, 'Settings'),
    accountCard(),
    form,
    h('div', { class: 'card' }, h('h2', {}, 'Links for organisers'),
      links.map(([label, path]) => {
        const link = `${location.origin}${path}`;
        return h('div', { class: 'row', style: 'margin-bottom:0.4rem' }, h('strong', {}, label), h('a', { href: link, target: '_blank' }, link),
          h('button', { class: 'small', onclick: () => copyText(link) }, 'Copy'));
      })),
  );
}

function accountCard() {
  const current = h('input', { type: 'password', autocomplete: 'current-password' });
  const next = h('input', { type: 'password', autocomplete: 'new-password', minlength: 8 });
  const change = h('button', { type: 'submit' }, 'Change password');
  const form = h('form', {}, h('div', { class: 'grid' }, field('Current password', current), field('New password', next, { hint: 'At least 8 characters' })), change);
  form.addEventListener('submit', busy(change, async () => {
    await POST('/api/me/password', { current_password: current.value, new_password: next.value });
    current.value = next.value = '';
    toast('Password changed. Other devices have been logged out.');
  }));
  const logout = h('button', {}, 'Log out');
  logout.addEventListener('click', busy(logout, async () => { await POST('/api/logout'); me = null; navigate('/', true); }));
  return h('div', { class: 'card' },
    h('div', { class: 'row' }, h('h2', { style: 'margin:0' }, 'Your account'), h('span', { class: 'spacer' }), logout),
    h('p', {}, `${me.display_name} (${me.username}) · `, me.roles.map((r) => h('span', { class: 'pill' }, ROLE_LABELS[r] || r))),
    form);
}

async function pageUsers() {
  if (!hasRole('admin')) return render(h('h1', {}, 'Users'), needsRoleNote('admin'));
  const users = await GET('/api/users');
  const editor = h('div');
  const openEditor = (u) => {
    const isNew = !u.id;
    const username = h('input', { value: u.username, autocapitalize: 'none', spellcheck: 'false' });
    const display = h('input', { value: u.display_name });
    const email = h('input', { type: 'email', value: u.email });
    const password = h('input', { type: 'password', autocomplete: 'new-password' });
    const active = h('input', { type: 'checkbox', checked: u.active });
    const roleBoxes = Object.keys(ROLE_LABELS).map((r) => [r, h('input', { type: 'checkbox', checked: u.roles.includes(r) })]);
    const save = h('button', { class: 'primary', type: 'submit' }, isNew ? 'Add user' : 'Save');
    const form = h('form', { class: 'card' },
      h('h2', {}, isNew ? 'Add staff member' : `Edit ${u.display_name}`),
      h('div', { class: 'grid' },
        field('Name', display, { required: true }),
        field('Username', username, { required: true, hint: 'Used to log in' }),
        field('Email', email),
        field(isNew ? 'Password' : 'New password', password, { required: isNew, hint: isNew ? 'At least 8 characters. They can change it under Settings.' : 'Leave blank to keep their current password' })),
      h('div', { class: 'field' }, h('label', {}, 'Roles'),
        roleBoxes.map(([r, box]) => h('label', { class: 'check' }, box, ROLE_LABELS[r],
          h('span', { class: 'muted small' }, {
            admin: ' (users, settings and everything else)',
            bookings: ' (approve and manage room bookings)',
            av: ' (equipment, stock and loans)',
          }[r]))),
        h('p', { class: 'muted small' }, 'Everyone can see the whole console and edit rooms, groups and church events.')),
      h('div', { class: 'field' }, h('label', { class: 'check' }, active, 'Account active (untick to block login)')),
      h('div', { class: 'row' }, save, h('button', { type: 'button', onclick: () => editor.replaceChildren() }, 'Cancel'),
        isNew || u.id === me.id ? null : h('span', { class: 'spacer' }),
        isNew || u.id === me.id ? null : h('button', { type: 'button', class: 'danger', onclick: busy(null, async () => {
          if (!confirm(`Delete ${u.display_name}'s account? (Untick "active" instead to keep their name on past decisions.)`)) return;
          await DEL(`/api/users/${u.id}`);
          toast('Deleted');
          route();
        }) }, 'Delete')));
    form.addEventListener('submit', busy(save, async () => {
      const body = {
        username: username.value, display_name: display.value, email: email.value, password: password.value,
        active: active.checked, roles: roleBoxes.filter(([, b]) => b.checked).map(([r]) => r),
      };
      if (isNew) await POST('/api/users', body); else await PUT(`/api/users/${u.id}`, body);
      toast('Saved');
      route();
    }));
    editor.replaceChildren(form);
    form.scrollIntoView({ behavior: 'smooth' });
    display.focus();
  };
  render(
    h('div', { class: 'row' }, h('h1', {}, 'Staff accounts'), h('span', { class: 'spacer' }),
      h('button', { class: 'primary', onclick: () => openEditor({ username: '', display_name: '', email: '', roles: [], active: true }) }, '+ Add user')),
    h('p', { class: 'muted' }, 'Each staff member logs in with their own account. Their name is recorded on the requests they approve or decline.'),
    editor,
    h('div', { class: 'card table-wrap' }, h('table', {},
      h('thead', {}, h('tr', {}, ['Name', 'Username', 'Roles', ''].map((t) => h('th', {}, t)))),
      h('tbody', {}, users.map((u) => h('tr', { class: 'clickable', onclick: () => openEditor(u) },
        h('td', {}, u.display_name, u.email ? h('div', { class: 'muted small' }, u.email) : null),
        h('td', {}, u.username),
        h('td', {}, u.roles.map((r) => h('span', { class: 'pill' }, ROLE_LABELS[r] || r))),
        h('td', {}, u.active ? '' : h('span', { class: 'badge cancelled' }, 'disabled'))))))),
  );
}

// -------------------------------------------------------------------- boot

function updateOnline() {
  document.getElementById('offline').hidden = navigator.onLine;
}
window.addEventListener('online', updateOnline);
window.addEventListener('offline', updateOnline);
updateOnline();

if ('serviceWorker' in navigator) {
  window.addEventListener('load', () => navigator.serviceWorker.register('/sw.js').catch(() => {}));
}

loadInfo().finally(route);
