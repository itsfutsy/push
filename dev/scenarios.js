// Preview scenarios, selected with ?s=<name>. Each sets up the mock and optionally drives the UI.
//   info / settings / uploads / hud / connection: initial mock state
//   act(doc, mock): runs once the page has rendered; hover: a selector shown in its :hover state
//   onInfo(mock): runs while the page waits for get_info (events the page must not miss)
const settingsView = (doc, mock) => mock.emit('navigate', { view: 'settings' });
const later = (ms) => new Promise((r) => setTimeout(r, ms));
const key = (el, init) => el.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init }));

const library = (extra = {}) => ({ page: 'library', uploads: (fx) => [...fx.done, ...fx.more], ...extra });
const preview = (tile) => async (doc) => {
  doc.querySelector(tile).dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
  await later(600); // let the zoom finish
};

// Clicks tiles the way the user would: the first plainly, the rest with ⌘ held.
const pick = (...nths) => (doc) => {
  doc.getElementById('grid').focus();
  nths.forEach((n, i) => doc.querySelectorAll('.tile')[n].dispatchEvent(new MouseEvent('click', { bubbles: true, metaKey: i > 0 })));
};

const hudUpload = (pick) => ({ page: 'hud', uploads: (fx) => [pick(fx), ...fx.done], hud: (fx) => ({ kind: 'upload', upload: pick(fx) }) });

export const SCENARIOS = {
  list: {},
  empty: { uploads: () => [] },
  'list-hover': { hover: '#list li:nth-child(2)' },
  copied: { act: (doc) => doc.querySelector('#list .row-hit').click() },
  confirm: { act: (doc) => doc.querySelectorAll('#list li')[1].querySelector('.del').click() },
  uploading: { uploads: (fx) => [fx.uploading, fx.queued, ...fx.done] },
  focus: { focus: '#list li:nth-child(2) .row-hit, #zone-hit' },
  'settings-focus': { act: settingsView, focus: '#done, #login' },
  'uploading-hover': { uploads: (fx) => [fx.uploading, fx.queued, ...fx.done], hover: '#zone' },
  success: {
    uploads: (fx) => [{ ...fx.uploading, sent: fx.uploading.size }, ...fx.done],
    act: (doc, mock) => mock.complete(mock.fx.uploading.id),
  },
  dragover: { act: (doc, mock) => mock.drag('enter', ['/Users/me/Desktop/Screenshot.png']) },
  'dragover-many': { uploads: () => [], act: (doc, mock) => mock.drag('enter', ['/a.png', '/b.png', '/c.mov']) },
  error: { uploads: (fx) => [fx.failed('network', 'Can’t reach the server.'), ...fx.done] },
  'error-auth': { uploads: (fx) => [fx.failed('auth', 'The token wasn’t accepted.'), ...fx.done] },
  noToken: { info: { hasToken: false }, uploads: () => [] },
  firstRun: { info: { hasToken: false }, settings: { serverUrl: '' }, uploads: () => [] },
  noShortcut: { settings: { shortcut: null } },
  bothCommands: { settings: { shortcut: 'LeftCommand+RightCommand' } },
  'settings-commands': { settings: { shortcut: 'LeftCommand+RightCommand' }, act: settingsView },
  settings: { act: settingsView },
  'settings-error': {
    connection: { status: 'unauthorized', message: 'The token wasn’t accepted.' },
    act: async (doc, mock) => {
      settingsView(doc, mock);
      await later(50);
      const url = doc.getElementById('server-url');
      url.value = 'share.example.com';
      url.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    },
  },
  recording: {
    act: async (doc, mock) => {
      settingsView(doc, mock);
      await later(250);
      doc.getElementById('rec-key').click();
    },
  },
  'recording-held': {
    act: async (doc, mock) => {
      settingsView(doc, mock);
      await later(250);
      const el = doc.getElementById('rec-key');
      el.click();
      key(el, { key: 'Shift', shiftKey: true, metaKey: true });
    },
  },
  'shortcut-error': {
    act: async (doc, mock) => {
      settingsView(doc, mock);
      await later(250);
      const el = doc.getElementById('rec-key');
      el.click();
      key(el, { key: '#', code: 'Digit3', shiftKey: true, metaKey: true });
    },
  },
  // Cold start: an upload finishes and the tray asks for Settings while the window reads its first state.
  'cold-start': {
    uploads: (fx) => [{ ...fx.uploading, sent: fx.uploading.size }, ...fx.done],
    onInfo: (mock) => { mock.complete(mock.fx.uploading.id); mock.emit('navigate', { view: 'settings' }); },
  },
  linux: { info: { platform: 'linux' } },
  wayland: { info: { platform: 'linux', shortcutSupported: false }, act: settingsView },
  'wayland-main': { info: { platform: 'linux', shortcutSupported: false } },

  library: library(),
  'library-hover': library({ hover: 'section:first-child .tile:nth-child(2)', act: (doc) => doc.querySelectorAll('.tile')[1].click() }),
  'library-preview': library({ act: preview('.tile') }),
  'library-video': library({ act: preview('.tile.video') }),
  'library-audio': library({ act: preview('.tile.audio') }),
  'library-confirm': library({ act: async (doc) => { await preview('.tile')(doc); doc.getElementById('pv-delete').click(); } }),
  'library-select': library({ act: pick(0, 2, 3, 6) }),
  'library-select-confirm': library({ act: (doc) => { pick(0, 2, 3, 6)(doc); doc.getElementById('sel-delete').click(); } }),
  'library-images': library({ act: (doc) => doc.querySelector('[data-filter="image"]').click() }),
  'library-search': library({ act: (doc) => { const s = doc.getElementById('search'); s.value = 'zebra'; s.dispatchEvent(new Event('input')); } }),
  'library-empty': { page: 'library', uploads: () => [] },
  'library-linux': library({ info: { platform: 'linux' } }),

  'hud-uploading': hudUpload((fx) => fx.uploading),
  'hud-file': hudUpload((fx) => ({ ...fx.queued, status: 'uploading', sent: 31_200_000 })),
  'hud-done': {
    ...hudUpload((fx) => ({ ...fx.uploading, sent: fx.uploading.size })),
    act: (doc, mock) => mock.complete(mock.fx.uploading.id),
  },
  'hud-failed': hudUpload((fx) => ({ ...fx.uploading, status: 'failed', thumb: null, error: { kind: 'network', message: 'Can’t reach the server.' } })),
  'hud-auth': hudUpload((fx) => ({ ...fx.uploading, status: 'failed', thumb: null, error: { kind: 'auth', message: 'The token wasn’t accepted.' } })),
  'hud-permission': { page: 'hud', hud: () => ({ kind: 'permission' }) },
  'hud-hover': { ...hudUpload((fx) => fx.uploading), hover: '#hud-hit' },
};

// What dev/shots.sh renders, in light and dark.
export const SHOTS = [
  'list', 'list-hover', 'focus', 'empty', 'copied', 'confirm', 'uploading', 'uploading-hover', 'success', 'dragover', 'error',
  'error-auth', 'noToken', 'firstRun', 'noShortcut', 'settings', 'settings-focus', 'settings-error', 'recording', 'recording-held', 'shortcut-error',
  'linux', 'wayland', 'library', 'library-hover', 'library-preview', 'library-video', 'library-audio', 'library-confirm',
  'library-select', 'library-select-confirm', 'library-images', 'library-search', 'library-empty', 'library-linux', 'hud-uploading', 'hud-file', 'hud-done', 'hud-failed', 'hud-auth', 'hud-permission', 'hud-hover',
];
