// HUD: a small always-on-top panel at the bottom-right of the screen. It owns its lifecycle:
// success dismisses itself after 3 s (paused while hovered); failures and the permission notice stay.
import {
  $, invoke, listen, setPlatform, icon, kindIcon, formatProgress, percent, shortUrl, failureTitle, setRing, fadeIn, motion,
  reducedMotion, announce, fitMiddle, isActive,
} from './lib.js';

const root = $('hud');

let shown = null;        // { kind: 'upload', upload } | { kind: 'permission' } while visible
let thumb = null;        // local data URL of the shown upload, kept after it finishes
let serverUrl = '';
let timer = 0;

const openMain = (view) => () => { invoke('show_main', { view }); dismiss(); };

function view() {
  if (shown.kind === 'permission') {
    return { state: 'permission', title: 'Allow screen recording', sub: 'Click to open System Settings', glyph: 'push',
      click: () => { invoke('open_permission_settings'); dismiss(); } };
  }
  const u = shown.upload;
  if (isActive(u)) {
    // The ring says "uploading"; the title names the file, middle-truncated like Finder.
    return { state: 'uploading', title: u.fileName, sub: u.status === 'uploading' ? formatProgress(u.sent, u.size) : 'Waiting…',
      click: openMain('main') };
  }
  if (u.status === 'done') {
    return { state: 'done', title: 'Link copied', sub: shortUrl(u.url), click: () => { invoke('open_link', { id: u.id }); dismiss(); } };
  }
  const title = failureTitle(u.error, serverUrl);
  switch (u.error?.kind) {
    case 'auth': return { state: 'failed', title, sub: 'Click to open Settings', click: openMain('settings') };
    case 'unsupported': case 'missing': return { state: 'failed', title, sub: u.error.message, click: dismiss };
    default: return { state: 'failed', title, sub: 'Click to try again', click: () => invoke('retry_upload', { id: u.id }).catch(dismiss) };
  }
}

let last = {};
function render() {
  const v = view();
  const changed = v.state !== last.state || v.title !== last.title;
  last = v;
  root.dataset.state = v.state;
  if (changed) fitMiddle($('hud-title'), v.title);
  $('hud-sub').textContent = v.sub;
  renderThumb(v);
  if (v.state === 'uploading') setRing($('hud-ring'), shown.upload.status === 'uploading' ? percent(shown.upload.sent, shown.upload.size) : 0);
  if (changed) {
    announce(v.state === 'uploading' ? `Uploading ${v.title}` : `${v.title}. ${v.sub}`);
    if (v.state === 'done') armTimer();
  }
  return changed;
}

let thumbKey = null;
function renderThumb(v) {
  const u = shown.upload;
  const src = u && (thumb || (u.kind === 'image' && u.status === 'done' && u.url) || null);
  const key = src || v.glyph || (u ? kindIcon(u.kind) : '');
  if (key === thumbKey) return;
  thumbKey = key;
  const box = $('hud-thumb');
  if (src) {
    const img = document.createElement('img');
    img.alt = '';
    img.addEventListener('error', () => { box.className = 'hud-thumb'; box.replaceChildren(icon(kindIcon(u.kind))); });
    img.src = src;
    box.className = 'hud-thumb has-img';
    box.replaceChildren(img);
  } else {
    box.className = 'hud-thumb';
    box.replaceChildren(v.glyph ? icon(v.glyph, 'mark') : icon(kindIcon(u.kind)));
  }
}

function show(payload) {
  const visible = !!shown && !document.hidden;
  // Core sends the shown upload again when its status changes, as `uploads` already did: just update.
  if (visible && payload.kind === 'upload' && payload.upload.id === shown.upload?.id) return update(payload.upload);
  clearTimeout(timer);
  if (payload.kind === 'upload' && payload.upload.id !== shown?.upload?.id) thumb = null;
  shown = payload;
  if (payload.upload?.thumb) thumb = payload.upload.thumb;
  last = {};
  render();
  for (const a of root.getAnimations()) a.cancel();
  if (visible) {
    fadeIn(root);
  } else {
    const { easing, duration } = motion('snappy');
    root.animate(
      reducedMotion.matches ? [{ opacity: 0 }, { opacity: 1 }] : [{ opacity: 0, transform: 'scale(.96)' }, { opacity: 1, transform: 'none' }],
      { duration, easing },
    );
  }
}

function dismiss() {
  if (!shown) return;
  shown = null;
  clearTimeout(timer);
  const anim = root.animate(
    reducedMotion.matches ? [{ opacity: 1 }, { opacity: 0 }] : [{ opacity: 1 }, { opacity: 0, transform: 'translateX(8px)' }],
    { duration: reducedMotion.matches ? 150 : 200, easing: 'cubic-bezier(.4,0,1,1)', fill: 'forwards' },
  );
  // A covered or hidden window never finishes its animation, so don't wait on it forever.
  const done = document.hidden ? Promise.resolve() : Promise.race([anim.finished, new Promise(r => setTimeout(r, 400))]);
  done.then(() => {
    if (shown) return; // replaced while fading out
    invoke('hud_dismiss');
    root.dataset.state = '';
    thumb = null;
    thumbKey = null;
  });
}

// The HUD is never the key window, so WebKit sends it no hover events while another app is active.
// When the time is up, core says whether the pointer rests on the HUD; if so, look again shortly.
function armTimer(ms = 3000) {
  clearTimeout(timer);
  timer = setTimeout(async () => {
    const was = shown;
    const hovered = await invoke('hud_hovered').catch(() => false);
    if (shown !== was) return; // replaced or dismissed meanwhile
    if (hovered) armTimer(1000);
    else dismiss();
  }, ms);
}

function update(u) {
  if (u.thumb) thumb = u.thumb;
  shown.upload = u;
  if (render()) fadeIn($('hud-title').parentElement);
}

function onUploads(list) {
  if (shown?.kind !== 'upload') return;
  const u = list.find((x) => x.id === shown.upload.id);
  if (u) update(u);
  else dismiss(); // cancelled or deleted elsewhere
}

function onProgress({ id, sent, total }) {
  if (shown?.kind !== 'upload' || shown.upload.id !== id) return;
  shown.upload.sent = sent;
  shown.upload.size = total;
  if (shown.upload.status === 'queued') shown.upload.status = 'uploading';
  render();
}

async function init() {
  $('hud-hit').addEventListener('click', () => shown && last.click());
  $('hud-close').addEventListener('click', dismiss);
  await Promise.all([
    listen('hud', show),
    listen('uploads', onUploads),
    listen('upload-progress', onProgress),
    listen('settings', (s) => { serverUrl = s.serverUrl; }),
  ]);
  // The server URL names the host in "Can’t reach share.example.com".
  const [info, payload] = await Promise.all([invoke('get_info'), invoke('hud_ready')]);
  setPlatform(info.platform);
  serverUrl = info.settings.serverUrl;
  if (payload) show(payload);
}

init();
