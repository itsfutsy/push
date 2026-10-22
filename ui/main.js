// Main window: drop zone, recent uploads and the settings view. All work happens in Rust;
// this renders state from get_info and the uploads / upload-progress / settings / navigate events.
import {
  $, h, invoke, listen, onDragDrop, setPlatform, isMac, isWindows, cmdKey, isActive, icon, kindIcon, formatSize, formatProgress, percent,
  relativeTime, shortUrl, hostOf, formatShortcut, formatModifiers, BOTH_COMMANDS, announce, setRing, fadeIn, motion, reducedMotion, fitMiddle,
} from './lib.js';

const RETRYABLE = new Set(['auth', 'network', 'server']);

const state = {
  info: null,        // AppInfo
  uploads: [],
  view: 'main',
  dragging: false,
  dragCount: 0,
  success: null,     // { url, until } after an upload finishes
  errorId: null,     // the failure the drop zone reports
  confirmId: null,   // row asking "Delete …?"
};
const lastStatus = new Map(); // id -> status, to spot transitions
const thumbs = new Map();     // id -> local data URL, kept so a finished row doesn't flash while the remote image loads

const configured = () => state.info.hasToken && !!state.info.settings.serverUrl;

// ---------------------------------------------------------------- uploads

function applyUploads(list, initial = false) {
  // A new attempt clears the old error, then a failure among the new items reports itself.
  if (!initial && list.some((u) => !lastStatus.has(u.id))) state.errorId = null;
  for (const u of list) {
    const before = lastStatus.get(u.id);
    if (!initial) {
      if (u.status === 'done' && before && before !== 'done') succeed(u);
      if (u.status === 'failed' && before !== 'failed') state.errorId = u.id;
    }
    if (u.thumb) thumbs.set(u.id, u.thumb);
  }
  lastStatus.clear();
  for (const u of list) lastStatus.set(u.id, u.status);
  for (const id of thumbs.keys()) if (!lastStatus.has(id)) thumbs.delete(id);
  if (initial) state.errorId = list.find((u) => u.status === 'failed')?.id ?? null;
  state.uploads = list;
  renderZone();
  renderList(!initial);
}

function succeed(u) {
  state.success = { url: u.url, until: Date.now() + 1500 };
  announce('Link copied');
  setTimeout(renderZone, 1550);
}

function onProgress({ id, sent, total }) {
  const u = state.uploads.find((x) => x.id === id);
  if (!u) return;
  u.sent = sent;
  if (total) u.size = total;
  if (u.status === 'queued') u.status = 'uploading';
  const r = rows.get(id);
  if (r) updateRowText(r);
  renderZone();
}

// ---------------------------------------------------------------- drop zone

let zone = { state: null };

const openSettings = { action: 'Open Settings', run: () => showView('settings') };

function zoneView() {
  if (state.dragging) {
    return { state: 'over', title: 'Release to upload',
      sub: state.dragCount > 1 ? 'The links are copied when they’re done' : 'The link is copied when it’s done' };
  }
  const active = state.uploads.filter(isActive);
  const oldest = active.at(-1);
  const current = active.find((u) => u.status === 'uploading') ?? oldest;
  if (current) {
    return { state: 'uploading', title: active.length > 1 ? `Uploading ${active.length} files…` : 'Uploading…',
      current, action: 'Cancel', actionLabel: `Cancel uploading ${current.fileName}`, run: () => invoke('cancel_upload', { id: current.id }) };
  }
  if (state.success && Date.now() < state.success.until) {
    return { state: 'success', title: 'Link copied', sub: shortUrl(state.success.url) };
  }
  if (!configured()) {
    if (!state.info.hasToken && !state.info.settings.serverUrl) {
      return { state: 'setup', title: 'Connect your server', sub: 'Add its address and your upload token.', ...openSettings };
    }
    return state.info.hasToken
      ? { state: 'setup', title: 'Add your server', sub: 'Push needs its address before it can upload.', ...openSettings }
      : { state: 'setup', title: 'Add your upload token', sub: 'Push needs it before it can upload.', ...openSettings };
  }
  const failed = state.uploads.find((u) => u.id === state.errorId && u.status === 'failed');
  if (failed) return errorView(failed);
  return { state: 'idle', title: 'Drop a file to upload', sub: 'or click to choose one', hint: true };
}

function errorView(u) {
  const host = hostOf(state.info.settings.serverUrl) || 'the server';
  const retry = { action: 'Try Again', run: () => invoke('retry_upload', { id: u.id }).catch((e) => { $('zone-sub').textContent = String(e); }) };
  const dismiss = { action: 'OK', run: () => invoke('cancel_upload', { id: u.id }) };
  switch (u.error?.kind) {
    case 'auth':
      return { state: 'error', title: 'Upload token not accepted', sub: 'Check the token in Settings.', ...openSettings };
    case 'network':
      return { state: 'error', title: `Can’t reach ${host}`, sub: 'Check your connection and try again.', ...retry };
    case 'unsupported':
      return { state: 'error', title: 'Can’t upload this item', sub: u.error.message, ...dismiss };
    case 'missing':
      return { state: 'error', title: 'File not found', sub: u.error.message, ...dismiss };
    default:
      return { state: 'error', title: 'Upload failed', sub: u.error?.message || 'Something went wrong.', ...retry };
  }
}

function renderZone() {
  const v = zoneView();
  const changed = v.state !== zone.state || v.title !== zone.title || v.action !== zone.action;
  zone = v;
  const el = $('zone');
  el.dataset.state = v.state;
  $('zone-title').textContent = v.title;

  const sub = $('zone-sub');
  if (v.current) {
    const pct = v.current.status === 'uploading' ? percent(v.current.sent, v.current.size) : 0;
    const tail = v.current.status === 'uploading' ? `\u00a0· ${pct}%` : '\u00a0· Waiting…';
    if (sub.dataset.id !== v.current.id) {
      sub.dataset.id = v.current.id;
      sub.replaceChildren(h('span', { class: 'ell' }), h('span', { class: 'num' }));
    }
    sub.lastChild.textContent = tail;
    fitMiddle(sub.firstChild, v.current.fileName);
    setRing($('zone-ring'), pct);
  } else {
    delete sub.dataset.id;
    sub.textContent = v.sub;
    if (v.state !== 'success') setRing($('zone-ring'), 0);
  }

  const hint = $('zone-hint');
  hint.hidden = !v.hint;
  if (v.hint) renderHint(hint);

  const action = $('zone-action');
  action.hidden = !v.action;
  action.textContent = v.action ?? '';
  if (v.actionLabel) action.setAttribute('aria-label', v.actionLabel);
  else action.removeAttribute('aria-label');

  $('zone-hit').setAttribute('aria-label', v.state === 'setup' ? 'Open Settings' : 'Choose Files…');
  if (changed) fadeIn($('zone-text'));
}

function renderHint(el) {
  const { shortcut } = state.info.settings;
  if (state.info.shortcutSupported && shortcut) {
    el.replaceChildren('Press ', h('kbd', {}, formatShortcut(shortcut)), ' to capture an area');
  } else {
    el.textContent = 'Set a capture shortcut in Settings';
  }
}

function onDrag(p) {
  if (p.type === 'enter') {
    state.dragCount = p.paths?.length ?? 1;
    if (state.view === 'settings') showView('main');
  }
  const dragging = p.type === 'enter' || p.type === 'over';
  if (dragging !== state.dragging) {
    state.dragging = dragging;
    renderZone();
  }
}

// ---------------------------------------------------------------- recent list

const rows = new Map(); // id -> { id, u, el, mode, thumb, ... }

function renderList(animate) {
  const list = $('list');
  const ids = new Set(state.uploads.map((u) => u.id));
  for (const [id, r] of rows) {
    if (!ids.has(id)) { rows.delete(id); leave(r.el); }
  }
  let prev = null;
  for (const u of state.uploads) {
    let r = rows.get(u.id);
    const isNew = !r;
    if (isNew) {
      r = { id: u.id, el: h('li', { class: 'row' }) };
      rows.set(u.id, r);
    }
    r.u = u;
    let next = prev ? prev.nextElementSibling : list.firstElementChild;
    while (next?.classList.contains('leaving')) next = next.nextElementSibling;
    if (next !== r.el) list.insertBefore(r.el, next);
    renderRow(r);
    if (isNew && animate) enter(r.el);
    prev = r.el;
  }
  $('empty').hidden = state.uploads.length > 0;
  $('recent-head').hidden = !state.uploads.length;
  $('show-all').hidden = !state.uploads.some((u) => u.status === 'done');
  updateFades();
}

function enter(el) {
  const { easing, duration } = motion('smooth');
  el.animate(
    reducedMotion.matches ? [{ opacity: 0 }, { opacity: 1 }] : [{ height: '0px', opacity: 0 }, { height: '44px', opacity: 1 }],
    { duration, easing },
  );
}

function leave(el) {
  el.classList.add('leaving');
  el.inert = true;
  const { easing, duration } = motion('smooth');
  const anim = el.animate(
    reducedMotion.matches ? [{ opacity: 1 }, { opacity: 0 }] : [{ height: '44px', opacity: 1 }, { height: '0px', opacity: 0 }],
    { duration: Math.min(duration, 300), easing, fill: 'forwards' },
  );
  anim.finished.then(() => { el.remove(); updateFades(); });
}

function rowMode(r) {
  if (state.confirmId === r.id) return 'confirm';
  return isActive(r.u) ? 'active' : r.u.status;
}

function renderRow(r) {
  const mode = rowMode(r);
  if (r.mode !== mode) {
    r.mode = mode;
    buildRow(r);
  }
  updateThumb(r);
  updateRowText(r);
}

function buildRow(r) {
  const { u, el } = r;
  el.dataset.mode = r.mode;
  r.thumb ??= h('span', { class: 'thumb' });
  r.name = h('span', { class: 'name' });
  r.nameKey = null;
  r.meta = h('span', { class: 'meta num' });
  r.metaKey = null;
  const parts = [];

  if (r.mode === 'done') {
    r.hit = h('button', {
      class: 'row-hit', type: 'button', 'aria-label': `Copy link for ${u.fileName}`,
      onclick: () => copyLink(r), ondblclick: () => invoke('open_link', { id: r.id }),
      onpointerenter: () => { r.hit.title = r.name.textContent === r.u.fileName ? '' : r.u.fileName; },
    });
    parts.push(r.hit);
  }
  parts.push(r.thumb, h('span', { class: 'text' }, r.name, r.meta));

  let trail;
  if (r.mode === 'done') {
    trail = h('span', { class: 'trail' },
      iconButton('link', 'Copy Link', () => copyLink(r)),
      iconButton('trash-2', 'Delete', () => askDelete(r), 'del'));
  } else if (r.mode === 'active') {
    r.ring = $('row-ring').content.firstElementChild.cloneNode(true);
    trail = h('span', { class: 'trail' },
      h('button', { class: 'icon-btn stop', type: 'button', 'aria-label': `Cancel uploading ${u.fileName}`, title: 'Cancel Upload',
        onclick: () => invoke('cancel_upload', { id: r.id }) }, r.ring));
  } else if (r.mode === 'failed') {
    trail = h('span', { class: 'trail' },
      RETRYABLE.has(u.error?.kind) && iconButton('rotate-cw', 'Try Again', () => retry(r)),
      iconButton('x', 'Dismiss', () => invoke('cancel_upload', { id: r.id })));
  } else if (r.mode === 'confirm') {
    r.name.className = 'name confirm-q';
    trail = h('span', { class: 'trail gap' },
      h('button', { class: 'btn small cancel', type: 'button', onclick: closeConfirm }, 'Cancel'),
      h('button', { class: 'btn small danger', type: 'button', onclick: () => deleteUpload(r) }, 'Delete'));
  }
  parts.push(trail);
  el.replaceChildren(...parts);
}

function iconButton(name, label, onclick, cls = '') {
  return h('button', { class: `icon-btn ${cls}`.trim(), type: 'button', 'aria-label': label, title: label, onclick }, icon(name));
}

function updateRowText(r, fade = false) {
  const { u } = r;
  if (r.mode === 'confirm') {
    const ell = h('span', { class: 'ell' });
    r.name.replaceChildren(h('span', {}, 'Delete “'), ell, h('span', {}, '”?'));
    fitMiddle(ell, u.fileName);
    r.meta.textContent = 'The link stops working.';
    return;
  }
  if (r.nameKey !== u.fileName) {
    fitMiddle(r.name, u.fileName);
    if (r.name.clientWidth) r.nameKey = u.fileName; // measured; otherwise refit when the list is visible
  }
  let meta;
  if (r.mode === 'done') {
    meta = r.flash ? [icon(r.flash.icon, r.flash.tone), h('span', {}, r.flash.text)]
      : [`${formatSize(u.size)} · ${relativeTime(u.createdAt)}`];
  } else if (r.mode === 'active') {
    const uploading = u.status === 'uploading';
    meta = [uploading ? formatProgress(u.sent, u.size) : 'Waiting…'];
    setRing(r.ring, uploading ? percent(u.sent, u.size) : 0);
  } else {
    meta = [icon('circle-alert', 'warn'), h('span', {}, u.error?.message || 'Upload failed')];
  }
  const key = meta.map((m) => (typeof m === 'string' ? m : m.textContent)).join('|');
  if (key === r.metaKey) return;
  r.metaKey = key;
  r.meta.replaceChildren(...meta);
  if (fade) fadeIn(r.meta);
}

function updateThumb(r) {
  const { u } = r;
  const local = thumbs.get(u.id);
  const remote = u.kind === 'image' && u.status === 'done' && u.url && r.failedUrl !== u.url ? u.url : null;
  const want = local || remote;
  if (want === r.src) {
    if (local && remote && !r.preloading) preloadRemote(r, remote);
    return;
  }
  r.src = want;
  if (!want) {
    r.thumb.className = 'thumb';
    r.thumb.replaceChildren(icon(kindIcon(u.kind)));
    return;
  }
  const img = h('img', { alt: '', decoding: 'async', loading: 'lazy', draggable: 'false' });
  img.addEventListener('error', () => {
    if (want === local) thumbs.delete(u.id);
    else r.failedUrl = want;
    r.src = undefined;
    updateThumb(r);
  });
  img.src = want;
  r.thumb.className = 'thumb has-img';
  r.thumb.replaceChildren(img);
  if (local && remote) preloadRemote(r, remote);
}

// Swap a finished row's local thumbnail for the remote image once it has loaded, then free the data URL.
function preloadRemote(r, url) {
  r.preloading = true;
  const img = new Image();
  img.onload = () => { thumbs.delete(r.id); r.preloading = false; if (rows.get(r.id) === r) updateThumb(r); };
  img.onerror = () => { r.failedUrl = url; };
  img.src = url;
}

async function copyLink(r) {
  try {
    await invoke('copy_link', { id: r.id });
  } catch (e) {
    return flashRowError(r, e);
  }
  flash(r, { icon: 'check', tone: 'ok', text: 'Link copied' }, 1500);
}

function flashRowError(r, e) {
  flash(r, { icon: 'circle-alert', tone: 'bad', text: String(e) }, 4000);
}

function flash(r, note, ms) {
  clearTimeout(r.timer);
  r.flash = note;
  updateRowText(r, true);
  announce(note.text);
  r.timer = setTimeout(() => { r.flash = null; updateRowText(r, true); }, ms);
}

async function retry(r) {
  try { await invoke('retry_upload', { id: r.id }); } catch (e) { r.meta.textContent = String(e); }
}

function askDelete(r) {
  closeConfirm();
  state.confirmId = r.id;
  renderRow(r);
  r.el.querySelector('.cancel').focus();
}

function closeConfirm() {
  const r = rows.get(state.confirmId);
  state.confirmId = null;
  if (r) renderRow(r);
}

async function deleteUpload(r) {
  for (const b of r.el.querySelectorAll('button')) b.disabled = true;
  try {
    await invoke('delete_upload', { id: r.id }); // the uploads event removes the row
  } catch (e) {
    if (state.confirmId === r.id) state.confirmId = null;
    renderRow(r);
    flashRowError(r, e);
  }
}

function updateFades() {
  const list = $('list');
  list.classList.toggle('fade-top', list.scrollTop > 0);
  list.classList.toggle('fade-bottom', list.scrollTop + list.clientHeight < list.scrollHeight - 1);
}

function refreshTimes() {
  for (const r of rows.values()) if (r.mode === 'done') updateRowText(r);
}

// ---------------------------------------------------------------- views

async function showView(next) {
  if (next === state.view) return;
  if (state.view === 'settings') {
    if (recording) await stopRecording();
    const saved = await Promise.all([commitUrl(), commitToken()]);
    if (!saved.every(Boolean)) return; // keep Settings open to show what's wrong
  }
  closeConfirm();
  const from = $(state.view), to = $(next);
  state.view = next;
  const dir = next === 'settings' ? 1 : -1;
  const dx = reducedMotion.matches ? 0 : 12;
  const { easing, duration } = motion('smooth');
  from.inert = true;
  to.inert = false;
  to.hidden = false;
  // The old view clears out before the new one settles in, so the two never muddle together.
  from.animate([{ opacity: 1 }, { opacity: 0, transform: `translateX(${-dx * dir}px)` }], { duration: 110, easing: 'ease-in', fill: 'forwards' })
    .finished.then((a) => { if (state.view !== from.id) from.hidden = true; a.cancel(); });
  to.animate([{ opacity: 0, transform: `translateX(${dx * dir}px)` }, { opacity: 1, transform: 'none' }],
    { duration, easing, delay: 70, fill: 'backwards' });
  to.focus({ preventScroll: true });
  if (next === 'settings') {
    renderSettings();
    checkConnection();
  } else {
    renderZone();
  }
}

// ---------------------------------------------------------------- settings

function renderSettings() {
  const { settings, hasToken, shortcutSupported, version } = state.info;
  const url = $('server-url');
  if (document.activeElement !== url) url.value = settings.serverUrl;
  const vault = isMac() ? 'Keychain' : isWindows() ? 'Credential Manager' : '';
  $('token').placeholder = hasToken ? (vault ? `Saved in ${vault}` : 'Saved') : 'Paste token';
  $('token-note').textContent = isMac() ? 'Your token is stored in your Keychain.'
    : isWindows() ? 'Your token is stored in Windows Credential Manager.' : 'Your token is stored only on this computer.';
  $('shortcut-row').hidden = !shortcutSupported;
  $('command-row').hidden = shortcutSupported;
  $('command-note').hidden = shortcutSupported;
  $('login').setAttribute('aria-checked', String(settings.launchAtLogin));
  $('version').textContent = `Push ${version}`;
  renderRecorder();
}

function setFieldError(id, message) {
  const el = $(`${id}-err`);
  el.textContent = message ?? '';
  el.hidden = !message;
}

async function commitUrl() {
  const el = $('server-url');
  const url = el.value.trim();
  if (url === state.info.settings.serverUrl) { setFieldError('server-url'); return true; }
  try {
    state.info.settings = await invoke('set_server_url', { url });
    el.value = state.info.settings.serverUrl;
    setFieldError('server-url');
    renderZone();
    checkConnection();
    return true;
  } catch (e) {
    setFieldError('server-url', String(e));
    return false;
  }
}

async function commitToken() {
  const el = $('token');
  const token = el.value.trim();
  if (!token) { setFieldError('token'); return true; }
  try {
    await invoke('set_token', { token });
    el.value = '';
    state.info.hasToken = true;
    setFieldError('token');
    renderSettings();
    renderZone();
    checkConnection();
    // Uploads that failed on the old token can go again.
    for (const u of state.uploads) {
      if (u.status === 'failed' && u.error?.kind === 'auth') invoke('retry_upload', { id: u.id }).catch(() => {});
    }
    return true;
  } catch (e) {
    setFieldError('token', String(e));
    return false;
  }
}

let checkSeq = 0;
async function checkConnection() {
  const seq = ++checkSeq;
  // Only show "Checking…" if the answer takes a moment, so a quick check doesn't flicker.
  const slow = setTimeout(() => setStatus('checking', 'Checking…'), 200);
  let c;
  try { c = await invoke('test_connection'); } catch (e) { c = { status: 'unreachable', message: String(e) }; }
  clearTimeout(slow);
  if (seq !== checkSeq) return;
  const text = { connected: 'Connected', unauthorized: 'Can’t connect', unreachable: 'Can’t connect', unconfigured: 'Not set up' }[c.status];
  setStatus(c.status, text, c.status === 'connected' ? '' : c.message);
}

function setStatus(status, text, reason) {
  $('status').dataset.status = status;
  $('status-text').textContent = text;
  if (reason === undefined) return;
  const el = $('status-reason');
  el.textContent = reason;
  el.hidden = !reason;
}

// Shortcut recorder. While recording the current shortcut is paused so pressing it doesn't capture.
let recording = false;
let saving = false;
let held = '';
const commandKeys = new Set(); // MetaLeft and MetaRight, while recording

function renderRecorder() {
  const sc = state.info.settings.shortcut;
  const rec = $('recorder');
  rec.classList.toggle('recording', recording);
  rec.classList.toggle('held', recording && !!held);
  rec.classList.toggle('unset', !recording && !sc);
  $('rec-key').textContent = recording ? held || 'Type shortcut' : sc ? formatShortcut(sc) : 'Record Shortcut';
  $('rec-clear').hidden = recording || !sc;
}

function startRecording() {
  if (recording) return;
  recording = true;
  held = '';
  commandKeys.clear();
  setFieldError('shortcut');
  renderRecorder();
  $('rec-key').focus(); // WebKit doesn't focus buttons on click, and the keys must come here
  invoke('pause_shortcut', { paused: true }).catch(() => {});
}

async function stopRecording() {
  if (!recording) return;
  recording = false;
  held = '';
  renderRecorder();
  await invoke('pause_shortcut', { paused: false }).catch(() => {});
}

function modifierSet(e) {
  const s = new Set();
  if (e.ctrlKey) s.add('ctrl');
  if (e.altKey) s.add('alt');
  if (e.shiftKey) s.add('shift');
  if (e.metaKey) s.add('cmd');
  return s;
}

function onRecorderKey(e) {
  if (!recording) return;
  e.preventDefault();
  e.stopPropagation();
  if (e.type === 'keyup' || ['Meta', 'Control', 'Alt', 'Shift', 'CapsLock', 'Fn'].includes(e.key)) {
    // Both Command keys on their own are a shortcut too. WebKit reports letting go of one while the other
    // is held as another keydown, and a keyup only once both are up, so each keydown toggles its key.
    if (!e.metaKey) commandKeys.clear();
    else if (isMac() && e.type === 'keydown' && (e.code === 'MetaLeft' || e.code === 'MetaRight') && !commandKeys.delete(e.code)) {
      commandKeys.add(e.code);
      if (commandKeys.size === 2 && !e.ctrlKey && !e.altKey && !e.shiftKey && !saving) { saveShortcut(BOTH_COMMANDS); return; }
    }
    held = formatModifiers(modifierSet(e));
    renderRecorder();
    return;
  }
  if (saving || e.repeat) return;
  const plain = !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey;
  if (e.key === 'Escape') { stopRecording(); return; }
  if (plain && (e.key === 'Backspace' || e.key === 'Delete')) { saveShortcut(null); return; }
  if (!e.metaKey && !e.ctrlKey) {
    setFieldError('shortcut', isMac() ? 'Include ⌘ or ⌃ in the shortcut.' : `Include Ctrl or ${isWindows() ? 'Win' : 'Super'} in the shortcut.`);
    return;
  }
  const accel = [e.metaKey && (isMac() ? 'Command' : 'Super'), e.ctrlKey && 'Control', e.altKey && 'Alt', e.shiftKey && 'Shift', e.code];
  saveShortcut(accel.filter(Boolean).join('+'));
}

async function saveShortcut(accelerator) {
  saving = true;
  if (recording) await invoke('pause_shortcut', { paused: false }).catch(() => {});
  try {
    state.info.settings = await invoke('set_shortcut', { accelerator });
    recording = false;
    setFieldError('shortcut');
    renderZone();
  } catch (e) {
    if (recording) await invoke('pause_shortcut', { paused: true }).catch(() => {});
    setFieldError('shortcut', String(e)); // keep recording so another combo can be tried
  } finally {
    saving = false;
    held = '';
    renderRecorder();
  }
}

async function toggleLogin() {
  const el = $('login');
  const enabled = el.getAttribute('aria-checked') !== 'true';
  el.setAttribute('aria-checked', String(enabled));
  try {
    state.info.settings = await invoke('set_launch_at_login', { enabled });
    setFieldError('login');
  } catch (e) {
    el.setAttribute('aria-checked', String(!enabled));
    setFieldError('login', String(e));
  }
}

// ---------------------------------------------------------------- input

function onKey(e) {
  const mod = cmdKey(e);
  if (mod && e.key === ',') { e.preventDefault(); showView('settings'); return; }
  if (state.view === 'main') {
    if (mod && e.key.toLowerCase() === 'o') { e.preventDefault(); invoke('pick_files'); }
    else if (e.key === 'Escape') {
      if (state.confirmId) closeConfirm();
      else if (zone.state === 'uploading') zone.run();
    }
    return;
  }
  if (recording || e.isComposing) return;
  if (e.key === 'Escape') { e.preventDefault(); showView('main'); }
  else if (e.key === 'Enter') {
    e.preventDefault();
    if (e.target.id === 'server-url') commitUrl();
    else if (e.target.id === 'token') commitToken();
    else showView('main'); // Return is the default button: Done
  }
}

function wire() {
  document.addEventListener('keydown', onKey);
  $('open-settings').addEventListener('click', () => showView('settings'));
  $('show-all').addEventListener('click', () => invoke('show_library'));
  $('done').addEventListener('click', () => showView('main'));
  $('zone-hit').addEventListener('click', () => (zone.state === 'setup' ? showView('settings') : invoke('pick_files')));
  $('zone-action').addEventListener('click', () => zone.run());
  $('list').addEventListener('scroll', updateFades, { passive: true });
  // Refit middle-truncated names when the list's width changes (shown again, scroll bar style).
  new ResizeObserver(() => {
    for (const r of rows.values()) { r.nameKey = null; updateRowText(r); }
  }).observe($('list'));
  document.addEventListener('pointerdown', (e) => {
    if (state.confirmId && !rows.get(state.confirmId)?.el.contains(e.target)) closeConfirm();
  });
  $('server-url').addEventListener('blur', commitUrl);
  $('token').addEventListener('blur', commitToken);
  // WebKit blurs a focused button on mousedown, which would stop recording before the click restarts it.
  $('rec-key').addEventListener('mousedown', (e) => { if (recording) e.preventDefault(); });
  $('rec-key').addEventListener('click', () => (recording ? stopRecording() : startRecording()));
  $('rec-key').addEventListener('keydown', onRecorderKey);
  $('rec-key').addEventListener('keyup', onRecorderKey);
  $('rec-key').addEventListener('blur', () => { if (!saving) stopRecording(); });
  addEventListener('blur', () => { if (!saving) stopRecording(); }); // never leave the shortcut paused
  $('rec-clear').addEventListener('click', () => saveShortcut(null));
  $('login').addEventListener('click', toggleLogin);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshTimes(); });
  setInterval(refreshTimes, 30_000);
}

async function init() {
  wire();
  // Subscribe before reading the first state so nothing that happens in between is lost.
  // Events that arrive before it are replayed on top of it, in order.
  let early = [];
  const on = (fn) => (payload) => (early ? early.push(() => fn(payload)) : fn(payload));
  await Promise.all([
    listen('uploads', on((list) => applyUploads(list))),
    listen('upload-progress', on(onProgress)),
    listen('settings', on((s) => { state.info.settings = s; renderSettings(); renderZone(); })),
    listen('navigate', on(({ view }) => showView(view))),
    onDragDrop(on(onDrag)),
  ]);
  state.info = await invoke('get_info');
  setPlatform(state.info.platform);
  applyUploads(state.info.uploads, true);
  renderSettings();
  for (const fn of early) fn();
  early = null;
}

init();
