// A stand-in for window.__TAURI__ with realistic fake data. Preview harness only, never shipped.
const MIN = 60e3, HR = 60 * MIN, DAY = 24 * HR;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clone = (v) => structuredClone(v);

const KINDS = {
  image: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'bmp', 'heic', 'tiff'],
  video: ['mp4', 'm4v', 'mov', 'webm', 'mkv', 'avi'],
  audio: ['mp3', 'm4a', 'wav', 'aac', 'flac', 'ogg'],
};
const kindOf = (ext) => Object.keys(KINDS).find((k) => KINDS[k].includes(ext)) ?? 'file';
const extOf = (name) => (name.match(/\.([A-Za-z0-9]{1,10})$/)?.[1] ?? 'bin').toLowerCase();

const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
const newId = () => Array.from(crypto.getRandomValues(new Uint8Array(10)), (b) => ALPHABET[b % 62]).join('');

// Fake screenshots, painted once on a canvas.
function paint(kind) {
  const c = document.createElement('canvas');
  c.width = kind === 'page' ? 340 : 480; c.height = kind === 'page' ? 440 : 300;
  const g = c.getContext('2d');
  const rr = (x, y, w, h, r, fill) => { g.fillStyle = fill; g.beginPath(); g.roundRect(x, y, w, h, r); g.fill(); };
  if (kind === 'ui') {
    const bg = g.createLinearGradient(0, 0, 480, 300); bg.addColorStop(0, '#c9d8f2'); bg.addColorStop(1, '#e7d4f0');
    g.fillStyle = bg; g.fillRect(0, 0, 480, 300);
    rr(60, 40, 360, 230, 18, '#fbfbfd');
    ['#ff5f57', '#febc2e', '#28c840'].forEach((col, i) => rr(80 + i * 18, 58, 11, 11, 6, col));
    rr(84, 98, 180, 14, 7, '#1d1d1f'); rr(84, 126, 300, 9, 5, '#c7c7cc'); rr(84, 144, 260, 9, 5, '#c7c7cc');
    rr(84, 172, 132, 72, 10, '#5b8cff'); rr(228, 172, 132, 72, 10, '#ffb340');
  } else if (kind === 'photo') {
    const sky = g.createLinearGradient(0, 0, 0, 300); sky.addColorStop(0, '#24366d'); sky.addColorStop(.55, '#f08a5d'); sky.addColorStop(1, '#f9c784');
    g.fillStyle = sky; g.fillRect(0, 0, 480, 300);
    g.fillStyle = '#ffe3a3'; g.beginPath(); g.arc(300, 190, 46, 0, Math.PI * 2); g.fill();
    g.fillStyle = '#2b2440'; g.beginPath(); g.moveTo(0, 230); g.lineTo(120, 170); g.lineTo(210, 215); g.lineTo(330, 150); g.lineTo(480, 220); g.lineTo(480, 300); g.lineTo(0, 300); g.fill();
    g.fillStyle = '#1b1730'; g.fillRect(0, 262, 480, 38);
  } else if (kind === 'aurora') {
    g.fillStyle = '#071425'; g.fillRect(0, 0, 480, 300);
    for (const [y, col] of [[120, '#3dfc9b'], [150, '#2bd4c9'], [95, '#7b6cff']]) {
      const glow = g.createRadialGradient(240, y, 10, 240, y, 230); glow.addColorStop(0, `${col}99`); glow.addColorStop(1, `${col}00`);
      g.fillStyle = glow; g.fillRect(0, 0, 480, 300);
    }
    g.fillStyle = '#030a14'; g.beginPath(); g.moveTo(0, 300); g.lineTo(0, 250); g.lineTo(90, 232); g.lineTo(200, 258); g.lineTo(330, 236); g.lineTo(480, 254); g.lineTo(480, 300); g.fill();
  } else if (kind === 'page') {
    g.fillStyle = '#fff'; g.fillRect(0, 0, 340, 440);
    rr(32, 36, 22, 22, 5, '#5b8cff'); rr(32, 80, 170, 16, 8, '#1d1d1f'); rr(32, 108, 230, 8, 4, '#c7c7cc');
    ['#ffb3a8', '#a8d8ff', '#b8f0c0', '#ffe08a', '#d6b8ff', '#ffb3a8'].forEach((col, i) => rr(32 + (i % 3) * 94, 140 + Math.floor(i / 3) * 94, 82, 82, 10, col));
    for (let i = 0; i < 6; i++) rr(32, 340 + i * 14, 276 - (i % 3) * 40, 7, 4, '#d1d1d6');
  } else if (kind === 'code') {
    g.fillStyle = '#1f2028'; g.fillRect(0, 0, 480, 300);
    rr(0, 0, 480, 34, 0, '#2a2b35');
    const cols = ['#ff7ab2', '#78c2b3', '#d9c97c', '#acf2e4', '#b281eb', '#ffffffb0'];
    for (let i = 0; i < 11; i++) {
      let x = 34 + (i % 4) * 22;
      for (let j = 0; j < 3; j++) { const w = 30 + ((i * 37 + j * 53) % 90); rr(x, 52 + i * 21, w, 9, 4, cols[(i + j) % cols.length]); x += w + 10; }
    }
  } else {
    g.fillStyle = '#fff'; g.fillRect(0, 0, 480, 300);
    rr(40, 34, 150, 14, 7, '#1d1d1f');
    [90, 140, 120, 190, 160, 220].forEach((v, i) => rr(56 + i * 66, 270 - v, 40, v, 6, i === 5 ? '#34c759' : '#0a84ff'));
  }
  return c.toDataURL('image/png');
}

function fixtures(now) {
  const today = new Date(now);
  const at = (daysAgo, h, m) => new Date(today.getFullYear(), today.getMonth(), today.getDate() - daysAgo, h, m).getTime();
  const pics = { ui: paint('ui'), photo: paint('photo'), code: paint('code'), chart: paint('chart'), aurora: paint('aurora'), page: paint('page') };
  const make = (id, fileName, size, createdAt, pic) => {
    const ext = extOf(fileName);
    return { id, name: `${id}.${ext}`, url: `https://share.example.com/i/${id}.${ext}`, fileName, size, kind: kindOf(ext), createdAt,
      status: 'done', sent: size, error: null, thumb: pic ? pics[pic] : null };
  };
  const done = [
    make('Xc3nzK9qPa', 'Screenshot 2026-10-21 at 14.03.22.png', 412_300, now - 2 * MIN, 'ui'),
    make('p2Lk9vR4tz', 'Screen Recording 2026-10-21 at 11.48.09.mov', 24_812_000, now - 3 * HR - 12 * MIN),
    make('Qm7aTe1Bxw', 'Sunset at Bondi.jpg', 3_412_000, at(1, 18, 20), 'photo'),
    make('Zr8Hy2Kd0c', 'design-review-v3.pdf', 2_140_000, at(3, 10, 5)),
    make('Lw4Nb6Pq1s', 'Screenshot 2026-10-14 at 09.41.10.png', 1_105_000, at(7, 9, 41), 'code'),
    make('Ue5Jc3Ma8v', 'Voice Memo 14.m4a', 1_604_000, at(9, 16, 2)),
    make('Hb1Wx7Ys3n', 'IMG_4821.HEIC', 2_903_000, at(12, 8, 30)),
    make('Tk2Vd9Rf6g', 'Q3 revenue.png', 268_000, at(400, 12, 0), 'chart'),
  ];
  // No server in the harness: this one points at a local 404 to exercise the glyph fallback.
  done[6].url = 'http://127.0.0.1:5174/i/Hb1Wx7Ys3n.heic';
  // More, for the Library.
  const more = [
    make('Rk4Tn8Wq2e', 'Screenshot 2026-10-21 at 10.12.40.png', 1_102_000, now - 5 * HR, 'code'),
    make('Gh3Jk5Lp7q', 'Aurora Timelapse.mp4', 38_400_000, at(1, 21, 4)),
    make('Mb7Xc1Vz5a', 'brand-assets.zip', 12_800_000, at(5, 15, 30)),
    make('Pn2Bv6Cx9m', 'Blue Mountains.jpg', 2_480_000, at(6, 7, 12), 'photo'),
  ];
  // Library previews core made at upload time (preview.rs). Q3 revenue.png predates them: it falls back.
  const previews = {
    Xc3nzK9qPa: pics.ui, p2Lk9vR4tz: pics.ui, Qm7aTe1Bxw: pics.photo, Zr8Hy2Kd0c: pics.page, Lw4Nb6Pq1s: pics.code,
    Hb1Wx7Ys3n: pics.photo, Rk4Tn8Wq2e: pics.code, Gh3Jk5Lp7q: pics.aurora, Pn2Bv6Cx9m: pics.photo,
  };
  const shot = make('Fv6Rb2Nq8e', 'Screenshot 2026-10-21 at 14.05.51.png', 2_412_000, now - 5000, 'ui');
  const uploading = { ...shot, url: '', status: 'uploading', sent: 1_206_000 };
  const queued = { ...make('Ao9Ks1Lm4d', 'Product Demo.mp4', 48_600_000, now - 4000), url: '', status: 'queued', sent: 0 };
  const failed = (kind, message) => ({ ...make('Ye3Pc5Wv7j', 'Keynote Export.mp4', 18_220_000, now - 40_000), url: '', status: 'failed',
    sent: 6_100_000, error: { kind, message } });
  return { done, more, previews, uploading, queued, failed, pics };
}

export function createMock({ scenario, log, onHudDismiss, hovered }) {
  const now = Date.now();
  const fx = fixtures(now);
  const info = {
    settings: { serverUrl: 'https://share.example.com', shortcut: 'CommandOrControl+Shift+Digit2', launchAtLogin: false },
    hasToken: true, platform: 'macos', shortcutSupported: true, version: '1.0.0', uploads: [],
    ...scenario.info,
  };
  if (scenario.settings) Object.assign(info.settings, scenario.settings);
  let uploads = scenario.uploads ? scenario.uploads(fx) : fx.done;
  const listeners = new Map();
  let dragHandler = null;
  let hud = scenario.hud?.(fx) ?? null; // what the HUD shows, like core's

  // Like core: in flight and failed first, then done; newest first, and the later of a batch first.
  const ORDER = { uploading: 0, queued: 0, failed: 0, done: 1 };
  const sorted = () => [...uploads].sort((a, b) =>
    ORDER[a.status] - ORDER[b.status] || b.createdAt - a.createdAt || uploads.indexOf(b) - uploads.indexOf(a));
  const find = (id) => uploads.find((u) => u.id === id);

  function emit(event, payload) {
    if (event === 'hud') hud = payload;
    for (const fn of listeners.get(event) ?? []) fn({ event, id: 0, payload: clone(payload) });
  }
  function emitUploads() {
    emit('uploads', sorted());
    // Core then tells the HUD about the upload it shows (hud::update).
    const u = hud?.kind === 'upload' && find(hud.upload.id);
    if (u) emit('hud', { kind: 'upload', upload: u });
  }

  // Upload simulation: one at a time, FIFO, progress at ~10 events/s.
  let ticking = null;
  function pump() {
    if (ticking) return;
    const next = uploads.find((u) => u.status === 'uploading') ?? sorted().filter((u) => u.status === 'queued').at(-1);
    if (!next) return;
    if (next.status !== 'uploading') { next.status = 'uploading'; emitUploads(); }
    const step = Math.max(next.size / 28, 96_000);
    ticking = setInterval(() => {
      if (!uploads.includes(next) || next.status !== 'uploading') { stop(); return; }
      next.sent = Math.min(next.size, Math.round(next.sent + step * (0.5 + Math.random())));
      if (next.failAt && next.sent >= next.size * next.failAt) {
        Object.assign(next, { status: 'failed', thumb: null, error: { kind: 'network', message: 'Can’t reach the server.' } });
        stop(); emitUploads(); return;
      }
      emit('upload-progress', { id: next.id, sent: next.sent, total: next.size });
      if (next.sent >= next.size) complete(next.id);
    }, 100);
    function stop() { clearInterval(ticking); ticking = null; pump(); }
  }
  function complete(id) {
    const u = find(id);
    clearInterval(ticking); ticking = null;
    Object.assign(u, { status: 'done', sent: u.size, url: `https://share.example.com/i/${u.name}`, thumb: null });
    emitUploads();
    pump();
  }
  function add(files) {
    const createdAt = Date.now();
    for (const f of files) {
      const id = newId(), ext = extOf(f.name);
      uploads.push({ id, name: `${id}.${ext}`, url: '', fileName: f.name, size: f.size, kind: kindOf(ext), createdAt,
        status: f.size ? 'queued' : 'failed', sent: 0, thumb: f.thumb ?? null, failAt: f.failAt ?? 0,
        error: f.size ? null : { kind: 'unsupported', message: 'Folders and empty files can’t be uploaded.' } });
    }
    emitUploads();
    pump();
  }

  let pickCount = 0;
  const commands = {
    get_info() {
      const snapshot = clone({ ...info, uploads: sorted() });
      scenario.onInfo?.(mock); // things that happen while the page waits for the answer
      return snapshot;
    },
    set_server_url({ url }) {
      url = url.trim().replace(/\/+$/, '');
      if (!/^https:\/\/[^\s/]+$/.test(url) && !/^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/.test(url)) {
        throw 'Enter an address that starts with https://';
      }
      info.settings.serverUrl = url;
      return info.settings;
    },
    set_token({ token }) {
      if (token.trim().length < 32) throw 'That token looks too short';
      info.hasToken = true;
      return null;
    },
    async test_connection() {
      await sleep(600);
      if (!info.hasToken || !info.settings.serverUrl) return { status: 'unconfigured', message: 'Enter a server URL and token.' };
      return scenario.connection ?? { status: 'connected', message: '' };
    },
    set_shortcut({ accelerator }) {
      if (accelerator && /(Command|Super)\+Shift\+Digit[345]$/.test(accelerator)) throw 'That shortcut is in use. Try another.';
      info.settings.shortcut = accelerator;
      return info.settings;
    },
    pause_shortcut: () => null,
    set_launch_at_login({ enabled }) { info.settings.launchAtLogin = enabled; return info.settings; },
    pick_files() {
      const picks = [
        { name: 'Screenshot 2026-10-21 at 14.12.40.png', size: 1_840_000, thumb: fx.pics.code },
        { name: 'Brand Guidelines.pdf', size: 6_200_000 },
        { name: 'Flaky Upload.mov', size: 9_400_000, failAt: 0.45 },
      ];
      setTimeout(() => add([picks[pickCount++ % picks.length]]), 350);
      return null;
    },
    copy_link: ({ id }) => { if (!find(id)) throw 'That upload is gone.'; return null; },
    open_link: ({ id }) => { log('open', find(id)?.url); return null; },
    async delete_upload({ id }) {
      await sleep(450);
      uploads = uploads.filter((u) => u.id !== id);
      emitUploads();
      return null;
    },
    cancel_upload({ id }) { uploads = uploads.filter((u) => u.id !== id); emitUploads(); pump(); return null; },
    retry_upload({ id }) {
      const u = find(id);
      if (u?.status !== 'failed') throw 'Nothing to retry.';
      Object.assign(u, { status: 'queued', error: null, sent: 0, failAt: 0 });
      emitUploads();
      pump();
      return null;
    },
    show_main({ view }) { emit('navigate', { view }); return null; },
    show_library: () => { log('show library'); return null; },
    hud_ready: () => hud,
    hud_dismiss() { hud = null; onHudDismiss(); return null; },
    hud_hovered: () => hovered(),
    open_permission_settings: () => null,
  };

  const api = {
    // core serves these as preview://localhost/<id>; `data:,` fails to load, like a missing one.
    previewUrl: (id) => fx.previews[id] ?? 'data:,',
    core: {
      async invoke(cmd, args = {}) {
        log('invoke', cmd, args);
        await sleep(20);
        if (!commands[cmd]) throw `Unknown command ${cmd}`;
        return clone(await commands[cmd](args));
      },
    },
    event: {
      async listen(event, fn) {
        if (!listeners.has(event)) listeners.set(event, new Set());
        listeners.get(event).add(fn);
        return () => listeners.get(event).delete(fn);
      },
    },
    webview: {
      getCurrentWebview: () => ({
        async onDragDropEvent(fn) { dragHandler = fn; return () => { dragHandler = null; }; },
      }),
    },
  };

  const mock = {
    api, emit, add, complete, pump, fx, uploads: () => uploads,
    drag(type, paths = []) {
      dragHandler?.({ event: `tauri://drag-${type}`, id: 0, payload: { type, paths, position: { x: 190, y: 120 } } });
    },
  };
  return mock;
}
