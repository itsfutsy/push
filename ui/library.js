// Library window: every finished upload in a grid, newest first and grouped by day, with a Quick Look
// style preview. All work happens in Rust; this renders the uploads from get_info and the uploads event.
import {
  $, h, invoke, listen, setPlatform, cmdKey, icon, kindIcon, formatSize, shortUrl, announce, fadeIn, motion, reducedMotion,
  fitMiddle, previewUrl,
} from './lib.js';

// What the server shows in the browser; anything else downloads from its link.
const INLINE = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'bmp', 'mp4', 'm4v', 'mov', 'webm', 'mp3', 'm4a', 'wav', 'ogg', 'txt']);
const extOf = (name) => (/\.([a-z0-9]+)$/i.exec(name)?.[1] ?? '').toLowerCase();
const inline = (u) => INLINE.has(extOf(u.name));
const media = (u) => u.kind === 'video' || u.kind === 'audio';

const state = {
  uploads: [],     // finished uploads, newest first
  order: [],       // ids shown, in order
  filter: 'all',
  query: '',
  selection: new Set(), // ids picked, as in Finder: click, ⌘-click, Shift-click, ⌘A
  current: null,        // where the arrow keys move from, and what Space previews
  anchor: null,         // where a Shift range starts
};

const grid = $('grid');
const tiles = new Map();     // id -> { u, el, act, badge, meta }
const durations = new Map(); // id -> seconds (null when it can't be read), for videos and audio

// ---------------------------------------------------------------- uploads

function applyUploads(list, initial = false) {
  const done = list.filter((u) => u.status === 'done' && u.url);
  const ids = new Set(done.map((u) => u.id));
  for (const [id, t] of tiles) {
    if (!ids.has(id)) { tiles.delete(id); seen.unobserve(t.el); }
  }
  // The upload being previewed was deleted: move on to its neighbour, the way Quick Look does.
  const gone = pv.id && !ids.has(pv.id);
  const i = state.order.indexOf(pv.id);
  const next = gone && (state.order.slice(i + 1).find((id) => ids.has(id)) ?? state.order.slice(0, i).findLast((id) => ids.has(id)));
  state.uploads = done;
  render(!initial);
  if (gone) {
    if (next) { select([next]); showItem(tiles.get(next), true); } else closePreview();
  }
}

// ---------------------------------------------------------------- grid

const longDay = new Intl.DateTimeFormat(undefined, { weekday: 'long', day: 'numeric', month: 'long' });
const monthFmt = new Intl.DateTimeFormat(undefined, { month: 'long' });
const monthYearFmt = new Intl.DateTimeFormat(undefined, { month: 'long', year: 'numeric' });
const dayStart = (d, offset = 0) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + offset).getTime();

// "Today", "Yesterday", "Earlier in October", "September", "October 2025"
function groupOf(ms, now) {
  const n = new Date(now), d = new Date(ms);
  if (ms >= dayStart(n)) return { key: 'today', title: 'Today', detail: longDay.format(d) };
  if (ms >= dayStart(n, -1)) return { key: 'yesterday', title: 'Yesterday', detail: longDay.format(d) };
  const key = `${d.getFullYear()}-${d.getMonth()}`;
  const thisYear = d.getFullYear() === n.getFullYear();
  if (thisYear && d.getMonth() === n.getMonth()) return { key, title: `Earlier in ${monthFmt.format(d)}` };
  return { key, title: (thisYear ? monthFmt : monthYearFmt).format(d) };
}

const FILTERS = { image: 'images', video: 'videos', file: 'files' };
const matches = (u) => state.filter === 'all'
  || (state.filter === 'file' ? !['image', 'video'].includes(u.kind) : u.kind === state.filter);

function render(animate = false) {
  const q = state.query.toLowerCase();
  const shown = state.uploads.filter((u) => matches(u) && (!q || u.fileName.toLowerCase().includes(q)));
  state.order = shown.map((u) => u.id);

  const groups = [];
  const now = Date.now();
  for (const u of shown) {
    const g = groupOf(u.createdAt, now);
    if (groups.at(-1)?.key !== g.key) groups.push({ ...g, items: [] });
    groups.at(-1).items.push(u);
  }
  grid.replaceChildren(...groups.map((g) => h('section', { role: 'group', 'aria-label': g.title },
    h('div', { class: 'group-head', 'aria-hidden': 'true' }, h('h2', {}, g.title), g.detail && h('p', {}, g.detail)),
    h('div', { class: 'grid', role: 'presentation' }, ...g.items.map((u) => tileFor(u, animate))))));

  const total = shown.reduce((sum, u) => sum + u.size, 0);
  $('count').textContent = shown.length ? `${shown.length} ${shown.length === 1 ? 'upload' : 'uploads'} · ${formatSize(total)}` : '';
  renderEmpty(!shown.length);
  // Tiles that are no longer shown aren't selected.
  const ids = new Set(state.order);
  const keep = (id) => (ids.has(id) ? id : null);
  select([...state.selection].filter(keep), keep(state.current), { scroll: false, anchor: keep(state.anchor) });
}

function renderEmpty(empty) {
  $('empty').hidden = !empty;
  if (!empty) return;
  const [title, sub] = !state.uploads.length ? ['No uploads yet', 'Screenshots and files you upload appear here.']
    : state.query ? [`No results for “${state.query}”`, 'Check the spelling or try a different search.']
    : [`No ${FILTERS[state.filter]}`, `${FILTERS[state.filter].replace(/^./, (c) => c.toUpperCase())} you upload appear here.`];
  $('empty-title').textContent = title;
  $('empty-sub').textContent = sub;
}

function tileFor(u, animate) {
  let t = tiles.get(u.id);
  if (t) return t.el;
  t = { u };
  tiles.set(u.id, t);
  t.el = h('div', {
    class: `tile ${u.kind}`, role: 'option', id: `t-${u.id}`, 'data-id': u.id,
    'aria-selected': String(state.selection.has(u.id)), 'aria-label': u.fileName, title: u.fileName,
  });
  t.act = h('button', {
    class: 'act glass', type: 'button', tabindex: '-1', 'aria-hidden': 'true', title: 'Copy Link',
    onclick: () => copy(u.id, t.act),
  }, icon('link'));
  if (u.kind === 'image' || u.kind === 'video') {
    t.el.append(cover(t));
    if (u.kind === 'video') {
      t.badge = h('span', { class: 'badge num' }, icon('play'), h('span', {}, clockFor(u.id)));
      t.el.append(t.badge);
    }
  } else {
    t.el.append(doc(t));
  }
  t.el.append(t.act);
  if (media(u)) seen.observe(t.el);
  if (animate) fadeIn(t.el);
  return t.el;
}

// The preview core made at upload time; older uploads fall back to the image itself, a video's
// first frame, or a file tile.
function cover(t) {
  const { u } = t;
  const img = h('img', { class: 'cover', alt: '', loading: 'lazy', decoding: 'async', draggable: 'false' });
  img.addEventListener('load', () => img.classList.add('loaded'));
  img.addEventListener('error', () => {
    if (u.kind === 'image' && inline(u) && !img.dataset.remote) { img.dataset.remote = '1'; img.src = u.url; }
    else if (u.kind === 'video' && inline(u)) img.replaceWith(firstFrame(t));
    else toDoc(t);
  });
  img.src = previewUrl(u.id);
  return img;
}

function firstFrame(t) {
  const { u } = t;
  const v = h('video', { class: 'cover', muted: true, playsinline: true, preload: 'metadata', 'aria-hidden': 'true' });
  v.muted = true;
  v.addEventListener('loadedmetadata', () => setDuration(u.id, v.duration));
  v.addEventListener('loadeddata', () => v.classList.add('loaded'));
  v.addEventListener('error', () => toDoc(t));
  v.src = `${u.url}#t=0.1`;
  return v;
}

function toDoc(t) {
  t.badge = null;
  t.el.replaceChildren(doc(t), t.act);
}

// Files, audio, and anything without a picture: art, then name and details.
function doc(t) {
  const { u } = t;
  const art = h('span', { class: 'doc-art' }, u.kind === 'audio' ? wave(u.id, 22) : icon(kindIcon(u.kind)));
  if (u.kind === 'file') { // PDFs have a preview of their first page
    const img = h('img', { alt: '', decoding: 'async', draggable: 'false' });
    img.addEventListener('load', () => art.replaceChildren(img));
    img.src = previewUrl(u.id);
  }
  t.meta = h('span', { class: 'doc-meta num' }, docMeta(u));
  return h('span', { class: 'doc' }, art, h('span', { class: 'doc-name' }, u.fileName), t.meta);
}

// "2:07 · 1.6 MB" for videos and audio, "PDF · 2.1 MB" for the rest.
function docMeta(u) {
  const lead = media(u) ? clockFor(u.id) : extOf(u.fileName).toUpperCase();
  return [lead, formatSize(u.size)].filter(Boolean).join(' · ');
}

// A waveform drawn from the upload's id: decoration, the same every time, not the real audio.
function wave(id, n) {
  let s = [...id].reduce((a, c) => (Math.imul(a, 31) + c.charCodeAt(0)) >>> 0, 7);
  const bars = [];
  for (let i = 0; i < n; i++) {
    s = (Math.imul(s, 1103515245) + 12345) >>> 0;
    const envelope = Math.sin(((i + 0.5) / n) * Math.PI);
    bars.push(h('i', { style: `height:${Math.round(12 + envelope * (28 + ((s >>> 16) / 65535) * 60))}%` }));
  }
  return h('span', { class: 'wave', 'aria-hidden': 'true' }, ...bars);
}

// ---------------------------------------------------------------- durations

// Videos and audio report their length once their metadata loads from the link. Read lazily, as tiles
// scroll into view, a few at a time, and kept for as long as the window is open.
const probing = new Set();
const waiting = [];
const seen = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    seen.unobserve(e.target);
    const t = tiles.get(e.target.dataset.id);
    if (t) probe(t.u);
  }
}, { root: grid, rootMargin: '200px 0px' });

function probe(u) {
  if (durations.has(u.id) || !inline(u)) return;
  waiting.push(u);
  pump();
}

function pump() {
  while (probing.size < 3 && waiting.length) {
    const u = waiting.shift();
    if (durations.has(u.id) || probing.has(u.id)) continue;
    probing.add(u.id);
    const el = h(u.kind === 'audio' ? 'audio' : 'video', { preload: 'metadata' });
    el.muted = true;
    const done = (seconds) => {
      clearTimeout(timer);
      el.removeAttribute('src');
      el.load();
      probing.delete(u.id);
      setDuration(u.id, seconds);
      pump();
    };
    const timer = setTimeout(() => done(NaN), 15_000);
    el.addEventListener('loadedmetadata', () => done(el.duration), { once: true });
    el.addEventListener('error', () => done(NaN), { once: true });
    el.src = u.url;
  }
}

function setDuration(id, seconds) {
  if (durations.get(id)) return;
  durations.set(id, Number.isFinite(seconds) && seconds > 0 ? seconds : null);
  const t = tiles.get(id);
  if (t?.badge) t.badge.lastChild.textContent = clockFor(id);
  if (t?.meta) t.meta.textContent = docMeta(t.u);
  if (pv.id === id) $('pv-meta').textContent = pvMeta(t.u);
}

// "0:42", "1:02:07"
function clock(seconds) {
  const s = Math.max(0, Math.floor(seconds || 0));
  const hrs = Math.floor(s / 3600), mins = Math.floor(s / 60) % 60, secs = String(s % 60).padStart(2, '0');
  return hrs ? `${hrs}:${String(mins).padStart(2, '0')}:${secs}` : `${mins}:${secs}`;
}
const clockFor = (id) => (durations.get(id) ? clock(durations.get(id)) : '');

// ---------------------------------------------------------------- selection and keys

// `ids` become the selection; `current` is the tile the keys move from, scrolled into view.
function select(ids, current = [...ids].at(-1) ?? null, { scroll = true, anchor = current } = {}) {
  const next = new Set(ids);
  for (const id of new Set([...state.selection, ...next])) tiles.get(id)?.el.setAttribute('aria-selected', String(next.has(id)));
  state.selection = next;
  state.current = current;
  state.anchor = anchor;
  const t = tiles.get(current);
  if (t) {
    grid.setAttribute('aria-activedescendant', t.el.id);
    if (scroll) t.el.scrollIntoView({ block: 'nearest' });
  } else {
    grid.removeAttribute('aria-activedescendant');
  }
  renderBar();
}

// The tiles from `a` to `b`, in order, the way Photos' Shift-click does it.
function range(a, b) {
  const i = state.order.indexOf(a), j = state.order.indexOf(b);
  return i < 0 ? [b] : state.order.slice(Math.min(i, j), Math.max(i, j) + 1);
}

function onTileClick(e) {
  if (e.target.closest('.act')) return;
  const id = e.target.closest('.tile')?.dataset.id;
  if (!id) { select([]); return; }
  if (e.shiftKey && state.anchor) {
    select(range(state.anchor, id), id, { scroll: false, anchor: state.anchor });
  } else if (cmdKey(e)) {
    const picked = new Set(state.selection);
    if (!picked.delete(id)) picked.add(id);
    select(picked, id, { scroll: false });
  } else {
    select([id], id, { scroll: false });
  }
}

// Arrow keys move the way they do in Finder: left and right through the order, up and down to the
// nearest tile in the row above or below, across day groups. With Shift they extend the selection.
function move(key, extend = false) {
  const els = [...grid.querySelectorAll('.tile')];
  if (!els.length) return;
  const i = els.findIndex((el) => el.dataset.id === state.current);
  if (i < 0) { select([els[0].dataset.id]); return; }
  let j = i;
  if (key === 'ArrowLeft') j = Math.max(0, i - 1);
  else if (key === 'ArrowRight') j = Math.min(els.length - 1, i + 1);
  else {
    const down = key === 'ArrowDown';
    const r = els[i].getBoundingClientRect(), cx = r.left + r.width / 2;
    let best = -1, bestTop = 0, bestDx = 0;
    els.forEach((el, k) => {
      const q = el.getBoundingClientRect();
      if (down ? q.top < r.bottom : q.bottom > r.top) return;
      const dx = Math.abs(q.left + q.width / 2 - cx);
      const nearerRow = down ? q.top < bestTop - 1 : q.top > bestTop + 1;
      if (best < 0 || nearerRow || (Math.abs(q.top - bestTop) <= 1 && dx < bestDx)) { best = k; bestTop = q.top; bestDx = dx; }
    });
    if (best >= 0) j = best;
  }
  const id = els[j].dataset.id;
  if (extend) select(range(state.anchor ?? state.current, id), id, { anchor: state.anchor ?? state.current });
  else select([id]);
}

function onKey(e) {
  if (e.isComposing) return;
  const mod = cmdKey(e);
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const onButton = e.target.closest?.('button') && (key === ' ' || key === 'Enter');
  const copying = mod && key === 'c' && !getSelection().toString();
  if (pv.id) {
    if (!$('pv-confirm').hidden) {
      if (key === 'Escape') { e.preventDefault(); closeConfirm(); }
    } else if (copying) {
      e.preventDefault();
      copyFromPreview();
    } else if (mod || e.altKey || onButton) {
      // leave it
    } else if (key === 'Escape' || key === ' ') {
      e.preventDefault();
      closePreview();
    } else if (key.startsWith('Arrow')) {
      e.preventDefault();
      move(key);
      if (state.current !== pv.id) showItem(tiles.get(state.current), true);
    }
    return;
  }
  if (bulk.phase === 'asking') {
    if (key === 'Escape') { e.preventDefault(); closeBulk(); grid.focus(); }
    return;
  }
  const search = $('search');
  if (mod && key === 'f') { e.preventDefault(); search.focus(); search.select(); return; }
  if (e.target === search) {
    if (key === 'Escape') {
      e.preventDefault();
      if (search.value) { search.value = ''; onSearch(); } else grid.focus();
    } else if (key === 'ArrowDown' || key === 'Enter') {
      e.preventDefault();
      grid.focus();
      if (!state.current && state.order.length) select([state.order[0]]);
    }
    return;
  }
  if (copying && state.current) { e.preventDefault(); copy(state.current); return; }
  if (mod && key === 'a') { e.preventDefault(); grid.focus(); selectAll(); return; }
  if ((key === 'Backspace' || key === 'Delete') && !e.altKey && state.selection.size) { e.preventDefault(); askBulk(); return; }
  if (mod || e.altKey || onButton || (e.target !== grid && e.target !== document.body)) return;
  if (key.startsWith('Arrow')) { e.preventDefault(); grid.focus(); move(key, e.shiftKey); }
  else if ((key === ' ' || key === 'Enter') && state.current) { e.preventDefault(); openPreview(state.current); }
  else if (key === 'Escape') select([]);
}

function selectAll() {
  select(state.order, state.current ?? state.order[0] ?? null, { scroll: false, anchor: state.order[0] ?? null });
}

function onSearch() {
  const input = $('search');
  state.query = input.value.trim();
  render();
}

function setFilter(filter) {
  state.filter = filter;
  for (const b of $('filters').children) b.setAttribute('aria-checked', String(b.dataset.filter === filter));
  render();
}

// ---------------------------------------------------------------- copying

let toastTimer;
function toast(text, ok = true) {
  const el = $('toast');
  el.classList.toggle('err', !ok);
  el.querySelector('use').setAttribute('href', ok ? '#i-check' : '#i-circle-alert');
  $('toast-text').textContent = text;
  el.classList.add('on');
  announce(text);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('on'), ok ? 1500 : 4000);
}

async function copy(id, button) {
  try {
    await invoke('copy_link', { id });
  } catch (e) {
    toast(String(e), false);
    return;
  }
  toast('Link copied');
  if (!button) return;
  button.classList.add('done');
  button.querySelector('use').setAttribute('href', '#i-check');
  clearTimeout(button.timer);
  button.timer = setTimeout(() => {
    button.classList.remove('done');
    button.querySelector('use').setAttribute('href', '#i-link');
  }, 1500);
}

// ---------------------------------------------------------------- deleting several

// The bar at the bottom: shown for more than one selected tile, and for a delete to confirm or in progress.
const bulk = { phase: 'idle', done: 0, total: 0 }; // 'idle' | 'asking' | 'deleting'

function renderBar() {
  const bar = $('selbar');
  const n = state.selection.size;
  const on = bulk.phase !== 'idle' || n > 1;
  bar.classList.toggle('on', on);
  bar.inert = !on || !!pv.id;
  if (!on) return;
  let title, sub = '';
  if (bulk.phase === 'deleting') {
    title = `Deleting ${Math.min(bulk.done + 1, bulk.total)} of ${bulk.total}…`;
  } else if (bulk.phase === 'asking') {
    title = n === 1 ? 'Delete this upload?' : `Delete ${n} uploads?`;
    sub = n === 1 ? 'The link stops working.' : 'The links stop working.';
  } else {
    title = `${n} selected`;
    sub = `· ${formatSize([...state.selection].reduce((sum, id) => sum + (tiles.get(id)?.u.size ?? 0), 0))}`;
  }
  $('sel-title').textContent = title;
  $('sel-sub').textContent = sub;
  $('sel-all').textContent = n === state.order.length ? 'Deselect All' : 'Select All';
  $('sel-all').hidden = $('sel-delete').hidden = bulk.phase !== 'idle';
  $('sel-cancel').hidden = $('sel-really').hidden = bulk.phase !== 'asking';
}

function askBulk() {
  if (bulk.phase === 'deleting' || !state.selection.size) return;
  bulk.phase = 'asking';
  renderBar();
  $('sel-cancel').focus();
}

function closeBulk() {
  if (bulk.phase !== 'asking') return;
  bulk.phase = 'idle';
  renderBar();
}

// Deletes the selected uploads a few at a time; their tiles go as each one does. Stops at the first error
// (most are the server or the token) and leaves what's left selected.
async function deleteSelected() {
  const ids = state.order.filter((id) => state.selection.has(id));
  Object.assign(bulk, { phase: 'deleting', done: 0, total: ids.length });
  renderBar();
  grid.focus({ preventScroll: true });
  let error = null;
  const worker = async () => {
    while (ids.length && !error) {
      try {
        await invoke('delete_upload', { id: ids.shift() });
        bulk.done++;
      } catch (e) {
        error ??= String(e);
      }
      renderBar();
    }
  };
  await Promise.all(Array.from({ length: Math.min(4, bulk.total) }, worker));
  bulk.phase = 'idle';
  renderBar();
  if (!error) toast(bulk.total === 1 ? 'Upload deleted' : `${bulk.total} uploads deleted`);
  else toast(bulk.done ? `Deleted ${bulk.done} of ${bulk.total}. ${error}` : error, false);
}

// ---------------------------------------------------------------- preview

const EDGE = 480; // core's preview size (preview.rs): a smaller preview is the whole image
const pv = {
  id: null,
  size: [4, 3], // the content's natural size
  cap: Infinity, // how far it may be scaled up (1: not past its natural size)
  copyTimer: 0,
};

function openPreview(id) {
  const t = tiles.get(id);
  if (!t) return;
  // A multiple selection stays, so closing goes back to it; stepping through picks one at a time.
  if (state.selection.has(id)) select(state.selection, id, { anchor: state.anchor });
  else select([id]);
  pv.id = id;
  $('preview').hidden = false;
  $('bar').inert = grid.inert = true;
  renderBar();
  showItem(t);
  $('preview').focus({ preventScroll: true });
  zoom(t, true);
}

function closePreview() {
  if (!pv.id) return;
  const t = tiles.get(pv.id);
  pv.id = null;
  stopMedia();
  closeConfirm();
  $('bar').inert = grid.inert = false;
  renderBar();
  grid.focus({ preventScroll: true });
  t?.el.scrollIntoView({ block: 'nearest' });
  zoom(t, false).finished.then(() => {
    if (pv.id) return; // opened again meanwhile
    $('preview').hidden = true;
    for (const a of $('preview').getAnimations({ subtree: true })) a.cancel();
  });
}

function step(by) {
  const i = state.order.indexOf(pv.id) + by;
  if (i < 0 || i >= state.order.length) return;
  select([state.order[i]]);
  showItem(tiles.get(state.order[i]), true);
}

function showItem(t, fade = false) {
  const { u } = t;
  pv.id = u.id;
  closeConfirm();
  fitMiddle($('pv-name'), u.fileName);
  $('pv-meta').textContent = pvMeta(u);
  $('pv-url').textContent = shortUrl(u.url);
  clearTimeout(pv.copyTimer);
  $('pv-copy').replaceChildren('Copy Link');
  const i = state.order.indexOf(u.id);
  $('pv-prev').disabled = i <= 0;
  $('pv-next').disabled = i >= state.order.length - 1;
  buildMedia(t);
  if (fade) fadeIn($('pv-media'));
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const dateFmt = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'long', year: 'numeric' });
// "Today at 2:03 pm", "Yesterday at 9:41 am", "17 October 2026 at 4:20 pm"
function when(ms) {
  const d = new Date(ms), days = Math.round((dayStart(new Date()) - dayStart(d)) / 864e5);
  const day = days <= 0 ? 'Today' : days === 1 ? 'Yesterday' : dateFmt.format(d);
  return `${day} at ${timeFmt.format(d)}`;
}

// "0:42 · 24.8 MB · Today at 11:48", plus "Downloads from the link" when the browser won't show it.
function pvMeta(u) {
  return [clockFor(u.id), formatSize(u.size), when(u.createdAt), !inline(u) && 'Downloads from the link'].filter(Boolean).join(' · ');
}

// What the tile already shows, so the preview opens at the right shape and without waiting.
function tileImage(t) {
  const el = t.el.querySelector('.cover.loaded, .doc-art img');
  if (!el) return null;
  const video = el.tagName === 'VIDEO';
  const [w, hgt] = video ? [el.videoWidth, el.videoHeight] : [el.naturalWidth, el.naturalHeight];
  return w && hgt ? { size: [w, hgt], src: video ? null : el.currentSrc, remote: video || !!el.dataset.remote } : null;
}

function buildMedia(t) {
  const { u } = t;
  const box = $('pv-media');
  stopMedia();
  box.className = 'media';
  box.replaceChildren();
  const shown = tileImage(t);
  const src = shown?.src ?? previewUrl(u.id);
  // A preview at core's full size stands in for a bigger picture that replaces it, so until then it may be
  // scaled up; anything else isn't.
  const replaced = inline(u) && (u.kind === 'image' || u.kind === 'video');
  const capFor = (w, hgt, remote) => (remote || !replaced || Math.max(w, hgt) < EDGE ? 1 : Infinity);
  setSize(shown?.size ?? [4, 3], shown ? capFor(...shown.size, shown.remote) : Infinity);

  if (u.kind === 'audio') return audioCard(u, box);
  if (u.kind === 'video' && inline(u)) return videoPlayer(u, box, src);

  const img = h('img', { alt: '', draggable: 'false' });
  img.addEventListener('load', () => {
    if (pv.id === u.id) setSize([img.naturalWidth, img.naturalHeight], capFor(img.naturalWidth, img.naturalHeight, !!img.dataset.remote), true);
  });
  img.addEventListener('error', () => {
    if (pv.id !== u.id) return;
    if (u.kind === 'image' && inline(u) && !img.dataset.remote) { img.dataset.remote = '1'; img.src = u.url; }
    else fileCard(u, box);
  });
  if (shown?.remote) img.dataset.remote = '1';
  img.src = src;
  box.append(img);

  // The full image replaces the preview once it has loaded.
  if (u.kind === 'image' && inline(u) && !img.dataset.remote) {
    const full = h('img', { class: 'full', alt: '', draggable: 'false' });
    full.addEventListener('load', () => {
      if (pv.id !== u.id) return;
      full.classList.add('loaded');
      setSize([full.naturalWidth, full.naturalHeight], 1, true);
    });
    full.src = u.url;
    box.append(full);
  }
}

function fileCard(u, box) {
  setSize([300, 240], 1);
  box.replaceChildren(h('div', { class: 'card' }, icon(kindIcon(u.kind))));
}

function audioCard(u, box) {
  setSize([360, 232], 1);
  const bars = wave(u.id, 40);
  const card = h('div', { class: 'card' }, bars);
  box.append(card);
  if (!inline(u)) return;
  const el = h('audio', { preload: 'metadata' });
  el.src = u.url;
  card.append(el, player(u, el, box, [...bars.children]));
}

function videoPlayer(u, box, poster) {
  const v = h('video', { playsinline: true, preload: 'metadata' });
  v.poster = poster;
  v.addEventListener('loadedmetadata', () => {
    if (pv.id === u.id && v.videoWidth) setSize([v.videoWidth, v.videoHeight], 1, true);
  });
  v.src = u.url;
  const controls = player(u, v, box);
  v.addEventListener('click', () => controls.toggle());
  const big = h('button', { class: 'big-play', type: 'button', 'aria-label': 'Play', onclick: () => controls.toggle() }, icon('play'));
  box.append(v, big, controls);
}

// Play/pause, the time, a track to seek with, and the length. `bars`: an audio waveform that fills as it plays.
function player(u, el, box, bars) {
  const btn = h('button', { class: 'play', type: 'button', 'aria-label': 'Play' }, icon('play'));
  const now = h('span', { class: 'num' }, '0:00');
  const fill = h('i');
  const track = h('span', { class: 'track' }, fill);
  const total = h('span', { class: 'num' }, clockFor(u.id) || '0:00');
  let frame = 0;
  const update = () => {
    const f = el.duration ? el.currentTime / el.duration : 0;
    fill.style.width = `${f * 100}%`;
    now.textContent = clock(el.currentTime);
    bars?.forEach((b, i) => b.classList.toggle('played', (i + 0.5) / bars.length <= f));
  };
  const loop = () => { if (el.paused) return; update(); frame = requestAnimationFrame(loop); };
  const setIcon = (name, label) => {
    btn.querySelector('use').setAttribute('href', `#i-${name}`);
    btn.setAttribute('aria-label', label);
  };
  const toggle = () => (el.paused ? el.play().catch(() => {}) : el.pause());
  btn.addEventListener('click', toggle);
  el.addEventListener('play', () => {
    box.classList.add('playing', 'started');
    setIcon('pause', 'Pause');
    cancelAnimationFrame(frame);
    loop();
  });
  el.addEventListener('pause', () => {
    box.classList.remove('playing');
    setIcon('play', 'Play');
    cancelAnimationFrame(frame);
    update();
  });
  el.addEventListener('loadedmetadata', () => {
    setDuration(u.id, el.duration);
    if (Number.isFinite(el.duration)) total.textContent = clock(el.duration);
  });
  const seek = (e) => {
    const r = track.getBoundingClientRect();
    if (el.duration) el.currentTime = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) * el.duration;
    update();
  };
  track.addEventListener('pointerdown', (e) => { track.setPointerCapture(e.pointerId); seek(e); });
  track.addEventListener('pointermove', (e) => { if (track.hasPointerCapture(e.pointerId)) seek(e); });
  const bar = h('div', { class: 'controls' }, btn, now, track, total);
  bar.toggle = toggle;
  return bar;
}

function stopMedia() {
  for (const m of $('pv-media').querySelectorAll('video, audio')) {
    m.pause();
    m.removeAttribute('src');
    m.load();
  }
}

function setSize(size, cap, animate = false) {
  const box = $('pv-media');
  const before = animate && [box.offsetWidth, box.offsetHeight];
  pv.size = size;
  pv.cap = cap;
  fit();
  if (!before || reducedMotion.matches || (before[0] === box.offsetWidth && before[1] === box.offsetHeight)) return;
  const { easing, duration } = motion('smooth');
  box.animate([{ width: `${before[0]}px`, height: `${before[1]}px` }, { width: box.style.width, height: box.style.height }], { duration, easing });
}

// Fits the content in the stage, clear of the arrows, never past `cap` times its natural size.
function fit() {
  if (!pv.id) return;
  const stage = $('pv-stage'), box = $('pv-media');
  const [w, hgt] = pv.size;
  const k = Math.max(0, Math.min((stage.clientWidth - 144) / w, (stage.clientHeight - 24) / hgt, pv.cap));
  box.style.width = `${Math.round(w * k)}px`;
  box.style.height = `${Math.round(hgt * k)}px`;
}

// Opening grows the preview out of its tile, from the tile's square crop to the whole picture; closing
// shrinks it back. The bars and the dimmed grid fade.
function zoom(t, opening) {
  const fading = [$('pv-scrim'), $('pv-bar'), $('pv-foot'), ...[$('pv-prev'), $('pv-next')].filter((b) => !b.disabled)];
  for (const el of fading) {
    el.animate(opening ? [{ opacity: 0 }, { opacity: 1 }] : [{ opacity: 1 }, { opacity: 0 }],
      { duration: opening ? 220 : 200, easing: 'ease-out', fill: opening ? 'none' : 'forwards' });
  }
  const box = $('pv-media');
  const from = tileRect(t);
  let frames;
  if (from && !reducedMotion.matches) {
    const to = box.getBoundingClientRect();
    const s = Math.max(from.width / to.width, from.height / to.height);
    const dx = from.left + from.width / 2 - (to.left + to.width / 2);
    const dy = from.top + from.height / 2 - (to.top + to.height / 2);
    const ix = (to.width - from.width / s) / 2, iy = (to.height - from.height / s) / 2;
    frames = [
      { transform: `translate(${dx}px, ${dy}px) scale(${s})`, clipPath: `inset(${iy}px ${ix}px round ${8 / s}px)` },
      { transform: 'none', clipPath: 'inset(0 round 10px)' },
    ];
  } else {
    frames = [{ opacity: 0, transform: 'scale(.96)' }, { opacity: 1, transform: 'none' }];
  }
  const { easing, duration } = motion('smooth');
  return box.animate(opening ? frames : frames.reverse(),
    { duration: opening ? duration : duration * 0.75, easing, fill: opening ? 'none' : 'forwards' });
}

// The tile's place on screen, if it can be seen below the toolbar.
function tileRect(t) {
  if (!t?.el.isConnected) return null;
  const r = t.el.getBoundingClientRect(), g = grid.getBoundingClientRect();
  return r.bottom > g.top + 52 && r.top < g.bottom && r.width ? r : null;
}

function askDelete() {
  $('pv-actions').hidden = true;
  $('pv-confirm').hidden = false;
  $('pv-cancel').focus();
}

function closeConfirm() {
  const confirm = $('pv-confirm');
  if (confirm.hidden) return;
  confirm.hidden = true;
  $('pv-actions').hidden = false;
  for (const b of confirm.querySelectorAll('button')) b.disabled = false;
}

async function reallyDelete() {
  for (const b of $('pv-confirm').querySelectorAll('button')) b.disabled = true;
  try {
    await invoke('delete_upload', { id: pv.id }); // the uploads event moves on to the next one
  } catch (e) {
    closeConfirm();
    toast(String(e), false);
  }
}

async function copyFromPreview() {
  const id = pv.id;
  try {
    await invoke('copy_link', { id });
  } catch (e) {
    toast(String(e), false);
    return;
  }
  announce('Link copied');
  if (pv.id !== id) return;
  $('pv-copy').replaceChildren(icon('check'), 'Copied');
  clearTimeout(pv.copyTimer);
  pv.copyTimer = setTimeout(() => $('pv-copy').replaceChildren('Copy Link'), 1500);
}

// ---------------------------------------------------------------- start

function wire() {
  document.addEventListener('keydown', onKey);
  $('filters').addEventListener('click', (e) => {
    const b = e.target.closest('[data-filter]');
    if (b) setFilter(b.dataset.filter);
  });
  $('search').addEventListener('input', onSearch);
  grid.addEventListener('click', onTileClick);
  grid.addEventListener('dblclick', (e) => {
    const tile = e.target.closest('.tile');
    if (tile && !e.target.closest('.act')) openPreview(tile.dataset.id);
  });
  $('pv-close').addEventListener('click', closePreview);
  $('pv-open').addEventListener('click', () => invoke('open_link', { id: pv.id }).catch((e) => toast(String(e), false)));
  $('pv-delete').addEventListener('click', askDelete);
  $('pv-cancel').addEventListener('click', closeConfirm);
  $('pv-really').addEventListener('click', reallyDelete);
  $('pv-prev').addEventListener('click', () => step(-1));
  $('pv-next').addEventListener('click', () => step(1));
  $('pv-link').addEventListener('click', copyFromPreview);
  $('sel-all').addEventListener('click', () => {
    if (state.selection.size === state.order.length) select([]); else selectAll();
    grid.focus({ preventScroll: true });
  });
  $('sel-delete').addEventListener('click', askBulk);
  $('sel-cancel').addEventListener('click', () => { closeBulk(); grid.focus({ preventScroll: true }); });
  $('sel-really').addEventListener('click', deleteSelected);
  document.addEventListener('pointerdown', (e) => {
    if (!$('pv-confirm').contains(e.target)) closeConfirm();
    if (!$('selbar').contains(e.target)) closeBulk();
  });
  new ResizeObserver(() => {
    fit();
    const t = tiles.get(pv.id);
    if (t) fitMiddle($('pv-name'), t.u.fileName);
  }).observe($('pv-stage'));
  // Day groups ("Today") move on while the window sits open.
  document.addEventListener('visibilitychange', () => { if (!document.hidden) render(); });
}

async function init() {
  wire();
  // Subscribe before reading the first state; events that arrive before it are replayed on top of it.
  let early = [];
  await listen('uploads', (list) => (early ? early.push(list) : applyUploads(list)));
  const info = await invoke('get_info');
  setPlatform(info.platform);
  applyUploads(info.uploads, true);
  for (const list of early) applyUploads(list);
  early = null;
  grid.focus({ preventScroll: true });
}

init();
