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

const ADMIN_NAV = [
  ['/admin', 'Requests'],
  ['/admin/check', 'Quick check'],
  ['/admin/schedule', 'Schedule'],
  ['/admin/stock', 'Stock'],
  ['/admin/equipment', 'Equipment'],
  ['/admin/rooms', 'Rooms'],
  ['/admin/groups', 'Groups'],
  ['/admin/events', 'Church events'],
  ['/admin/settings', 'Settings'],
];

function showNav(path) {
  const nav = document.getElementById('nav');
  const isAdmin = path.startsWith('/admin') && path !== '/admin/login';
  nav.hidden = !isAdmin;
  if (!isAdmin) return;
  nav.replaceChildren(...ADMIN_NAV.map(([href, label]) => {
    const active = href === '/admin' ? path === '/admin' || path.startsWith('/admin/loans') : path.startsWith(href);
    return h('a', { href, 'data-link': true, class: active ? 'active' : '' }, label);
  }));
}

async function route() {
  const path = location.pathname.replace(/\/+$/, '') || '/';
  showNav(path);
  const m = (re) => path.match(re);
  let r;
  try {
    if (path === '/') return await pageHome();
    if (path === '/request') return await pageRequestForm();
    if ((r = m(/^\/status\/([A-Za-z0-9]+)$/))) return await pageStatus(r[1]);
    if (path === '/admin/login') return pageLogin();
    if (path === '/admin') return await pageLoans();
    if ((r = m(/^\/admin\/loans\/(\d+)$/))) return await pageLoan(+r[1]);
    if (path === '/admin/check') return await pageCheck();
    if (path === '/admin/schedule') return await pageSchedule();
    if (path === '/admin/stock') return await pageStock();
    if (path === '/admin/equipment') return await pageEquipment();
    if (path === '/admin/rooms') return await pageRooms();
    if (path === '/admin/groups') return await pageGroups();
    if (path === '/admin/events') return await pageEvents();
    if (path === '/admin/settings') return await pageSettings();
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
    document.getElementById('brand-name').textContent = `${churchName} · AV`;
  } catch (_) { /* offline */ }
}

const MY_REQUESTS_KEY = 'roomlog.myRequests';
// Status links organisers saved before the rename from Quippy.
const LEGACY_MY_REQUESTS_KEY = 'quippy.myRequests';

function myRequests() {
  const saved = localStorage.getItem(MY_REQUESTS_KEY) ?? localStorage.getItem(LEGACY_MY_REQUESTS_KEY);
  return JSON.parse(saved || '[]');
}

async function pageHome() {
  let admin = false;
  try { await fetch('/api/me', { credentials: 'same-origin' }).then((r) => { admin = r.ok; }); } catch (_) {}
  if (admin) return navigate('/admin', true);
  const mine = myRequests();
  render(
    h('div', { class: 'hero' },
      h('h1', {}, `${churchName} AV equipment`),
      h('p', { class: 'muted' }, 'Borrow sound and video equipment for your off-site event.'),
      h('a', { class: 'btn primary', href: '/request', 'data-link': true }, 'Request equipment'),
      h('a', { class: 'btn', href: '/admin/login', 'data-link': true }, 'AV team login')),
    mine.length ? h('div', { class: 'card' },
      h('h2', {}, 'Your requests on this device'),
      h('ul', {}, mine.map((r) => h('li', {}, h('a', { href: `/status/${r.token}`, 'data-link': true }, r.label))))) : null,
  );
}

function itemsEditor(catalog, initial = []) {
  const wrap = h('div');
  const rows = h('div');
  const byCat = groupBy(catalog, (c) => c.category || 'Other');
  const makeSelect = (value) => {
    const s = h('select', { 'aria-label': 'Equipment' }, h('option', { value: '' }, 'Choose equipment…'));
    for (const [cat, items] of byCat) {
      s.append(h('optgroup', { label: cat }, items.map((i) => h('option', { value: i.id }, i.name))));
    }
    if (value) s.value = value;
    return s;
  };
  const addRow = (type_id, quantity = 1) => {
    const sel = makeSelect(type_id);
    const qty = h('input', { type: 'number', min: 1, value: quantity, 'aria-label': 'Quantity' });
    const row = h('div', { class: 'item-row' }, sel, qty,
      h('button', { type: 'button', class: 'small', title: 'Remove', onclick: () => row.remove() }, '✕'));
    rows.append(row);
  };
  initial.forEach((i) => addRow(i.type_id, i.quantity));
  if (!initial.length) addRow();
  wrap.append(rows, h('button', { type: 'button', class: 'small', onclick: () => addRow() }, '+ Add item'));
  return {
    el: wrap,
    get() {
      return [...rows.children]
        .map((r) => ({ type_id: +r.querySelector('select').value, quantity: +r.querySelector('input').value }))
        .filter((i) => i.type_id && i.quantity > 0);
    },
  };
}

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
    catalog.length ? items.el : h('p', { class: 'muted' }, 'No equipment is currently listed for loan.'),
    h('div', { class: 'field', style: 'margin-top:1rem' }, h('label', {}, 'Notes'), f.notes),
    result,
    h('div', { class: 'row' }, checkBtn, submitBtn));
  form.addEventListener('submit', busy(submitBtn, async () => {
    const p = payload();
    if (!p.event_name.trim()) throw new Error('Event name is required');
    const res = await POST('/api/public/requests', p);
    const mine = myRequests();
    mine.unshift({ token: res.token, label: `${p.event_name} (${fmtDT(p.pickup_at)})` });
    localStorage.setItem(MY_REQUESTS_KEY, JSON.stringify(mine.slice(0, 20)));
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
      h('ul', {}, s.items.map((i) => h('li', {}, `${i.quantity} × ${i.name}`)))),
    s.collection.length ? h('div', { class: 'card' }, h('h2', {}, 'Where to collect'),
      s.collection.map((c) => [h('h3', {}, c.room), h('ul', {}, c.items.map((i) => h('li', {}, `${i.quantity} × ${i.name}`)))])) : null,
    cancelBtn,
  );
}

// ------------------------------------------------------------------- admin

function pageLogin() {
  const pw = h('input', { type: 'password', autocomplete: 'current-password' });
  const btn = h('button', { class: 'primary', type: 'submit' }, 'Log in');
  const form = h('form', { class: 'card', style: 'max-width:380px;margin:2rem auto' },
    h('h1', {}, 'AV team login'), field('Password', pw), btn);
  form.addEventListener('submit', busy(btn, async () => {
    await POST('/api/login', { password: pw.value });
    navigate('/admin', true);
  }));
  render(form);
  pw.focus();
}

const LOAN_FILTERS = [['open', 'Open'], ['pending', 'Pending'], ['approved', 'Approved'], ['collected', 'Out now'], ['returned', 'Returned'], ['declined', 'Declined'], ['cancelled', 'Cancelled'], ['all', 'All']];

async function pageLoans() {
  const params = new URLSearchParams(location.search);
  const status = params.get('status') || 'open';
  const loans = await GET(`/api/loans?status=${status}`);
  const now = new Date();
  render(
    h('div', { class: 'row' }, h('h1', {}, 'Loan requests'), h('span', { class: 'spacer' }),
      h('a', { class: 'btn', href: '/request', 'data-link': true }, '+ New request')),
    h('div', { class: 'row', style: 'margin-bottom:0.8rem' }, LOAN_FILTERS.map(([k, label]) =>
      h('a', { class: 'btn small' + (k === status ? ' primary' : ''), href: `/admin?status=${k}`, 'data-link': true }, label))),
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

const typeName = (refs, id) => (refs.types[id] ? refs.types[id].name : `#${id}`);
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
  const lines = asks.map((a) => `• ${fmtDT(a.start)} ${refs.events[a.event_id] || ''} in ${roomName(refs, a.room_id)}: ${a.reduce_to} × ${typeName(refs, a.type_id)} instead of the usual ${a.normally}`);
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
        h('td', {}, typeName(refs, i.type_id)),
        h('td', { class: 'num' }, i.requested),
        h('td', { class: 'num' }, i.shortfall ? h('strong', { style: 'color:var(--bad)' }, i.available) : i.available),
        h('td', {}, i.allocation.length ? i.allocation.map((a) => h('div', {}, `${a.qty} × ${roomName(refs, a.room_id)}`)) : h('span', { class: 'muted' }, 'none')),
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
      const agreeAll = ctx.loan ? h('button', { class: 'small primary' }, 'They agreed to all') : null;
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
          const btn = ctx.loan ? h('button', { class: 'small' }, 'Agreed') : null;
          if (btn) btn.addEventListener('click', busy(btn, async () => {
            await agree(a, ctx);
            toast('Recorded');
            ctx.reload && ctx.reload();
          }));
          return h('div', { class: 'ask row' },
            h('div', {},
              h('div', {}, h('strong', {}, refs.events[a.event_id] || 'Event'), ` · ${roomName(refs, a.room_id)} · ${fmtDT(a.start)}–${fmtTime(a.end)}`),
              h('div', {}, `${typeName(refs, a.type_id)}: `, h('span', { class: 'strike' }, a.normally), ` → ${a.reduce_to}`)),
            h('span', { class: 'spacer' }), btn);
        })));
    }
  }

  const unres = ev.items.filter((i) => i.unresolvable > 0);
  if (unres.length) {
    out.push(h('div', { class: 'banner bad' }, 'Cannot be found even if every church group agreed:',
      h('ul', {}, unres.map((i) => h('li', {},
        `${i.unresolvable} × ${typeName(refs, i.type_id)}`,
        i.blocking_loans.length ? h('span', {}, ' (some are out on loan ',
          i.blocking_loans.map((id, k) => [k ? ', ' : '', h('a', { href: `/admin/loans/${id}`, 'data-link': true }, `#${id}`)]), ')') : ' (not enough owned)')))));
  }
  return out;
}

async function agree(a, ctx) {
  await POST('/api/overrides', {
    event_id: a.event_id,
    occurrence_start: a.start,
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

  if (loan.status === 'pending') {
    const ev = d.evaluation;
    if (ev.fully_available) {
      act('Approve & reserve', 'primary', async () => {
        await POST(`/api/loans/${id}/approve`, { message: message.value });
        toast('Approved');
      });
    } else {
      const canLend = ev.items.some((i) => i.available > 0);
      if (canLend) act('Approve partial loan', 'primary', async () => {
        if (!confirm('Not everything is available. Approve and reserve only what is available now?')) return false;
        await POST(`/api/loans/${id}/approve`, { force: true, message: message.value });
        toast('Approved (partial)');
      });
    }
    act('Decline', 'danger', setStatus('declined', 'Decline this request? Put a reason in the message to the organiser first.'));
  }
  if (loan.status === 'approved') {
    act('Mark collected', 'primary', setStatus('collected'));
    act('Back to pending', '', setStatus('pending', 'Release the reserved equipment and return to pending?'));
    act('Cancel loan', 'danger', setStatus('cancelled', 'Cancel this loan? Any agreements with groups for it are released.'));
  }
  if (loan.status === 'collected') act('Mark returned', 'primary', setStatus('returned'));
  if (['declined', 'cancelled'].includes(loan.status)) {
    act('Reopen', '', setStatus('pending'));
    act('Delete', 'danger', async () => {
      if (!confirm('Delete this request permanently?')) return false;
      await DEL(`/api/loans/${id}`);
      navigate('/admin');
      return false;
    });
  }

  const overdue = loan.status === 'collected' && toDate(loan.return_at) < new Date();
  render(
    h('div', { class: 'row' }, h('a', { href: '/admin', 'data-link': true, class: 'no-print' }, '← Requests')),
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
        h('dt', {}, 'Requested'), h('dd', {}, loan.items.map((i) => h('span', { class: 'pill' }, `${i.quantity} × ${typeName(refs, i.type_id)}`))),
        loan.notes ? [h('dt', {}, 'Notes'), h('dd', {}, loan.notes)] : null),
      h('div', { class: 'row no-print', style: 'margin-top:0.8rem' },
        h('span', { class: 'muted small' }, 'Organiser status link:'),
        h('a', { href: `/status/${loan.token}`, target: '_blank', class: 'small' }, 'open'),
        h('button', { class: 'small', onclick: () => copyText(statusLink) }, 'Copy'),
        loan.status === 'pending' ? h('button', { class: 'small', onclick: () => editLoanDialog(loan, refs, reload) }, 'Edit dates / items') : null)),

    d.competing_pending.length ? h('div', { class: 'banner info' }, 'Other pending requests want the same equipment around these dates: ',
      d.competing_pending.map((c, k) => [k ? ', ' : '', h('a', { href: `/admin/loans/${c.id}`, 'data-link': true }, `#${c.id} ${c.event_name || c.organiser_name}`)]),
      h('p', {}, 'Approving this one first reserves the equipment for it.')) : null,

    d.allocation_problems.length ? h('div', { class: 'banner bad' }, 'Schedule changed since approval: some rooms are now short.',
      h('ul', {}, d.allocation_problems.map((p) => h('li', {}, `${roomName(refs, p.room_id)}: over by ${p.quantity} × ${typeName(refs, p.type_id)}`)))) : null,

    d.evaluation ? renderEvaluation(d.evaluation, refs, ctx) : null,
    loan.allocations.length && loan.status !== 'pending' ? pickList(loan, refs) : null,

    d.agreed_overrides.length ? h('div', { class: 'card' }, h('h2', { style: 'margin-top:0' }, 'Agreements recorded for this loan'),
      d.agreed_overrides.map((o) => h('div', { class: 'row ask' },
        h('span', {}, `${refs.events[o.event_id] || 'Event'} · ${fmtDT(o.occurrence_start)} · ${o.type_id ? `${typeName(refs, o.type_id)} reduced to ${o.quantity}` : 'cancelled'}`),
        h('span', { class: 'spacer' }),
        h('button', { class: 'small', onclick: busy(null, async () => { await DEL(`/api/overrides/${o.id}`); reload(); }) }, 'Undo'))),
      h('p', { class: 'muted small' }, 'These are released automatically if the loan is declined, cancelled or returned.')) : null,

    h('div', { class: 'card no-print' },
      field('Message to organiser', message),
      field('Private notes', notes),
      saveBtn),
    h('div', { class: 'row no-print' }, actions),
  );
}

function editLoanDialog(loan, refs, reload) {
  const catalog = Object.entries(refs.types).map(([id, t]) => ({ id: +id, name: t.name, category: t.category }));
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
  const equipment = await GET('/api/equipment');
  const catalog = equipment.filter((e) => e.total > 0);
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
          const cancelOv = o.overrides.find((x) => x.type_id === null);
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
                o.cancelled ? h('span', { class: 'badge cancelled', style: 'margin-left:0.4rem' }, 'cancelled') : null),
              h('div', {}, Object.entries(o.needs).map(([t, q]) => {
                const usual = o.usual_needs[t];
                const changed = usual !== q;
                return h('span', { class: 'pill' + (changed ? ' changed' : '') }, `${q} × ${typeName(refs, t)}`, changed ? ` (usually ${usual ?? 0})` : '');
              })),
              o.overrides.filter((x) => x.type_id !== null).map((x) => h('div', { class: 'small muted row' },
                `${typeName(refs, x.type_id)} → ${x.quantity}${x.note ? ` · ${x.note}` : ''}`,
                h('button', { class: 'small', onclick: busy(null, async () => { await DEL(`/api/overrides/${x.id}`); reload(); }) }, 'Undo')))),
            toggle);
        })])),
  );
}

async function pageStock() {
  const [rooms, equipment, stock] = await Promise.all([GET('/api/rooms'), GET('/api/equipment'), GET('/api/stock')]);
  if (!rooms.length || !equipment.length) {
    return render(h('h1', {}, 'Stock'), h('div', { class: 'banner info' }, 'Add some ',
      h('a', { href: '/admin/rooms', 'data-link': true }, 'rooms'), ' and ',
      h('a', { href: '/admin/equipment', 'data-link': true }, 'equipment types'), ' first.'));
  }
  const q = new Map(stock.map((s) => [`${s.room_id}:${s.type_id}`, s.quantity]));
  const totals = new Map();
  const byCat = groupBy(equipment, (e) => e.category || 'Other');
  const body = [];
  for (const [cat, types] of byCat) {
    body.push(h('tr', { class: 'cat' }, h('td', { colspan: rooms.length + 2 }, cat)));
    for (const t of types) {
      const totalCell = h('td', { class: 'num' }, t.total);
      totals.set(t.id, totalCell);
      body.push(h('tr', {},
        h('th', {}, t.name, t.lendable ? null : h('div', { class: 'muted small' }, 'not lendable')),
        rooms.map((r) => {
          const input = h('input', { type: 'number', min: 0, value: q.get(`${r.id}:${t.id}`) || 0, 'aria-label': `${t.name} in ${r.name}` });
          input.addEventListener('change', async () => {
            try {
              const quantity = Math.max(0, parseInt(input.value || '0', 10));
              await PUT('/api/stock', { room_id: r.id, type_id: t.id, quantity });
              q.set(`${r.id}:${t.id}`, quantity);
              totalCell.textContent = rooms.reduce((s, rr) => s + (q.get(`${rr.id}:${t.id}`) || 0), 0);
              toast('Saved');
            } catch (e) { toast(e.message, true); }
          });
          return h('td', { class: 'num' }, input);
        }),
        totalCell));
    }
  }
  render(
    h('h1', {}, 'Stock by room'),
    h('p', { class: 'muted' }, 'Where each item normally lives. Changes save automatically.'),
    h('div', { class: 'card table-wrap' }, h('table', { class: 'stock-matrix' },
      h('thead', {}, h('tr', {}, h('th', {}, 'Equipment'), rooms.map((r) => h('th', { class: 'num' }, r.name)), h('th', { class: 'num' }, 'Total'))),
      h('tbody', {}, body))),
  );
}

/** Generic list + inline editor page for simple entities. */
function simpleCrud({ title, intro, items, columns, fields, endpoint, blank, rowLabel }) {
  const editor = h('div');
  const openEditor = (item) => {
    const isNew = !item.id;
    const inputs = {};
    const els = fields.map((f) => {
      let input;
      if (f.type === 'checkbox') {
        input = h('input', { type: 'checkbox', checked: item[f.key] });
        inputs[f.key] = input;
        return h('div', { class: 'field' }, h('label', { class: 'check' }, input, f.label));
      }
      input = f.type === 'textarea' ? h('textarea') : h('input', { type: f.type || 'text', list: f.list || null });
      input.value = item[f.key] ?? '';
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
      for (const f of fields) body[f.key] = f.type === 'checkbox' ? inputs[f.key].checked : inputs[f.key].value;
      if (isNew) await POST(endpoint, body); else await PUT(`${endpoint}/${item.id}`, body);
      toast('Saved');
      route();
    }));
    editor.replaceChildren(form);
    form.scrollIntoView({ behavior: 'smooth' });
    form.querySelector('input,textarea').focus();
  };
  render(
    h('div', { class: 'row' }, h('h1', {}, title), h('span', { class: 'spacer' }),
      h('button', { class: 'primary', onclick: () => openEditor({ ...blank }) }, `+ Add ${rowLabel}`)),
    intro ? h('p', { class: 'muted' }, intro) : null,
    editor,
    items.length === 0 ? h('p', { class: 'muted' }, 'None yet.') :
      h('div', { class: 'card table-wrap' }, h('table', {},
        h('thead', {}, h('tr', {}, columns.map((c) => h('th', { class: c.num ? 'num' : '' }, c.label)))),
        h('tbody', {}, items.map((it) => h('tr', { class: 'clickable', onclick: () => openEditor(it) },
          columns.map((c) => h('td', { class: c.num ? 'num' : '' }, c.render ? c.render(it) : it[c.key]))))))),
  );
}

async function pageRooms() {
  simpleCrud({
    title: 'Rooms', rowLabel: 'room', endpoint: '/api/rooms', items: await GET('/api/rooms'),
    intro: 'Rooms (or storage areas) where equipment lives.',
    blank: { name: '', notes: '' },
    columns: [{ label: 'Name', key: 'name' }, { label: 'Notes', key: 'notes' }],
    fields: [{ key: 'name', label: 'Name', required: true }, { key: 'notes', label: 'Notes', type: 'textarea' }],
  });
}

async function pageEquipment() {
  const items = await GET('/api/equipment');
  const cats = [...new Set(items.map((i) => i.category).filter(Boolean))];
  simpleCrud({
    title: 'Equipment types', rowLabel: 'equipment type', endpoint: '/api/equipment', items,
    intro: 'Kinds of equipment. Set how many are in each room on the Stock page.',
    blank: { name: '', category: '', notes: '', lendable: true },
    columns: [
      { label: 'Name', key: 'name' }, { label: 'Category', key: 'category' },
      { label: 'Lendable', render: (i) => (i.lendable ? 'Yes' : 'No') },
      { label: 'Total owned', key: 'total', num: true }],
    fields: [
      { key: 'name', label: 'Name', required: true, hint: 'e.g. "Shure SM58 vocal mic"' },
      { key: 'category', label: 'Category', list: 'cats', hint: 'e.g. Microphones, Cables, Speakers' },
      { key: 'notes', label: 'Notes', type: 'textarea' },
      { key: 'lendable', label: 'Can be lent out (untick for installed equipment)', type: 'checkbox' }],
  });
  $app().append(h('datalist', { id: 'cats' }, cats.map((c) => h('option', { value: c }))));
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
  const [events, rooms, groups, equipment] = await Promise.all([GET('/api/events'), GET('/api/rooms'), GET('/api/groups'), GET('/api/equipment')]);
  const rName = Object.fromEntries(rooms.map((r) => [r.id, r.name]));
  const gName = Object.fromEntries(groups.map((g) => [g.id, g.name]));
  const tName = Object.fromEntries(equipment.map((t) => [t.id, t.name]));
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
    const needs = itemsEditor(equipment, ev.needs.length ? ev.needs : []);
    const recFields = h('div', { class: 'grid' });
    const updateRec = () => {
      recFields.replaceChildren(
        rec.value === 'weekly' ? field('Repeat', interval) : null,
        rec.value === 'monthly' ? field('Which', nth, { hint: 'Weekday is taken from the first date' }) : null,
        rec.value !== 'none' ? field('Until (optional)', until) : null);
    };
    rec.addEventListener('change', updateRec);
    updateRec();
    const save = h('button', { class: 'primary', type: 'submit' }, isNew ? 'Add event' : 'Save');
    const form = h('form', { class: 'card' },
      h('h2', { style: 'margin-top:0' }, isNew ? 'Add church event' : 'Edit church event'),
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
      h('p', { class: 'muted small' }, 'Only equipment from this room counts for this event.'),
      needs.el,
      h('div', { class: 'field', style: 'margin-top:1rem' }, h('label', {}, 'Notes'), notes),
      h('div', { class: 'row' }, save, h('button', { type: 'button', onclick: () => editor.replaceChildren() }, 'Cancel'),
        isNew ? null : h('span', { class: 'spacer' }),
        isNew ? null : h('button', { type: 'button', class: 'danger', onclick: busy(null, async () => {
          if (!confirm('Delete this event and all its occurrences?')) return;
          await DEL(`/api/events/${ev.id}`);
          route();
        }) }, 'Delete')));
    form.addEventListener('submit', busy(save, async () => {
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
      if (isNew) await POST('/api/events', body); else await PUT(`/api/events/${ev.id}`, body);
      toast('Saved');
      route();
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
          h('td', {}, h('strong', {}, e.title), h('div', { class: 'muted small' }, e.group_id ? gName[e.group_id] : 'No regular group')),
          h('td', {}, e.description),
          h('td', {}, e.needs.map((n) => h('span', { class: 'pill' }, `${n.quantity} × ${tName[n.type_id]}`))))))))),
  );
}

async function pageSettings() {
  const s = await GET('/api/settings');
  const name = h('input', { value: s.church_name });
  const buffer = h('input', { type: 'number', min: 0, max: 336, value: s.buffer_hours });
  const save = h('button', { class: 'primary', type: 'submit' }, 'Save');
  const form = h('form', { class: 'card' },
    field('Church name', name),
    field('Turnaround buffer (hours)', buffer, { hint: 'How long after a loan is due back before the equipment counts as available again (checking, setting back up).' }),
    save);
  form.addEventListener('submit', busy(save, async () => {
    await PUT('/api/settings', { church_name: name.value, buffer_hours: +buffer.value });
    await loadInfo();
    toast('Saved');
  }));
  const link = `${location.origin}/request`;
  const logout = h('button', {}, 'Log out');
  logout.addEventListener('click', busy(logout, async () => { await POST('/api/logout'); navigate('/', true); }));
  render(
    h('h1', {}, 'Settings'),
    form,
    h('div', { class: 'card' }, h('h2', { style: 'margin-top:0' }, 'Link for event organisers'),
      h('p', {}, h('a', { href: link, target: '_blank' }, link)),
      h('button', { onclick: () => copyText(link) }, 'Copy link')),
    logout,
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
