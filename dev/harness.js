// Preview harness: paints a desktop and a stand-in for the native window material, loads the real
// ui/ page in an iframe and gives it the mock __TAURI__ (ui/lib.js picks up window.parent.__PUSH_DEV_TAURI__).
import { createMock } from './mock.js';
import { SCENARIOS } from './scenarios.js';

const q = new URLSearchParams(location.search);
const name = q.get('s') ?? 'list';
const scenario = SCENARIOS[name] ?? {};
const shot = q.has('shot');
const page = scenario.page ?? 'main';
const linux = scenario.info?.platform === 'linux';

const win = document.getElementById('win');
const mock = createMock({
  scenario,
  log: (...a) => q.has('log') && console.debug('[mock]', ...a),
  onHudDismiss: () => win.classList.add('dismissed'),
  hovered: () => win.matches(':hover'),
});
window.__PUSH_DEV_TAURI__ = mock.api;
window.mock = mock; // for poking at it from the console

const root = document.documentElement;
root.classList.toggle('shot', shot);
root.dataset.page = page;
root.classList.toggle('linux', linux);

const frame = document.getElementById('frame');

frame.src = `../ui/${{ hud: 'hud', library: 'library' }[page] ?? 'index'}.html`;
frame.addEventListener('load', async () => {
  const doc = frame.contentDocument;
  bridgeFileDrops(doc);
  await sleep(350); // let the page render from get_info
  if (scenario.hover) forcePseudo(doc, ':hover', scenario.hover);
  if (scenario.focus) forcePseudo(doc, ':focus-visible', scenario.focus);
  await scenario.act?.(doc, mock);
  await sleep(page === 'hud' ? 650 : 550);
  root.dataset.ready = '1';
});

buildBar();

function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

// Real files dragged from Finder become enter/over/leave/drop events plus fake uploads.
function bridgeFileDrops(doc) {
  let depth = 0;
  const isFiles = (e) => e.dataTransfer?.types.includes('Files');
  for (const target of [doc, document]) {
    target.addEventListener('dragenter', (e) => {
      if (!isFiles(e)) return;
      e.preventDefault();
      if (depth++ === 0) mock.drag('enter', Array.from({ length: e.dataTransfer.items.length }, (_, i) => `/file${i}`));
    });
    target.addEventListener('dragover', (e) => { if (isFiles(e)) { e.preventDefault(); mock.drag('over'); } });
    target.addEventListener('dragleave', (e) => { if (isFiles(e) && --depth === 0) mock.drag('leave'); });
    target.addEventListener('drop', async (e) => {
      if (!isFiles(e)) return;
      e.preventDefault();
      depth = 0;
      mock.drag('drop');
      const files = await Promise.all([...e.dataTransfer.files].map(async (f) => ({
        name: f.name, size: f.size, thumb: f.type.startsWith('image/') && f.size <= 8e6 ? await dataUrl(f) : null,
      })));
      mock.add(files);
    });
  }
}

function dataUrl(file) {
  return new Promise((resolve) => {
    const r = new FileReader();
    r.onload = () => resolve(r.result);
    r.readAsDataURL(file);
  });
}

// Screenshots can't hover or show keyboard focus, so copy those rules to a class and apply it.
function forcePseudo(doc, pseudo, selector) {
  const cls = `dev-${pseudo.slice(1)}`;
  const rules = [];
  const walk = (list, wrap) => {
    for (const rule of list) {
      if (rule.selectorText?.includes(pseudo)) rules.push(wrap(rule.cssText.replaceAll(pseudo, `.${cls}`)));
      else if (rule.cssRules) walk(rule.cssRules, (css) => wrap(`@media ${rule.conditionText ?? rule.media?.mediaText} { ${css} }`));
    }
  };
  for (const s of doc.styleSheets) walk(s.cssRules, (css) => css);
  const style = doc.createElement('style');
  style.textContent = rules.join('\n');
  doc.head.append(style);
  for (const el of doc.querySelectorAll(selector)) {
    if (pseudo !== ':hover') { el.classList.add(cls); continue; }
    for (let n = el; n && n !== doc.body; n = n.parentElement) n.classList.add(cls);
  }
}

function buildBar() {
  const bar = document.getElementById('bar');
  if (shot) { bar.remove(); return; }
  const select = document.createElement('select');
  for (const s of Object.keys(SCENARIOS)) select.add(new Option(s, s, false, s === name));
  select.addEventListener('change', () => { location.search = `?s=${select.value}`; });
  bar.append(select);
  const button = (label, fn) => {
    const b = document.createElement('button');
    b.textContent = label;
    b.addEventListener('click', fn);
    bar.append(b);
  };
  if (page === 'main') {
    button('Upload', () => mock.api.core.invoke('pick_files'));
    button('Drag in', () => mock.drag('enter', ['/a.png']));
    button('Drag out', () => mock.drag('leave'));
    button('Drop', () => { mock.drag('drop'); mock.add([{ name: 'Dropped Photo.jpg', size: 3_200_000, thumb: mock.fx.pics.photo }]); });
    button('Resume', () => mock.pump());
  } else if (page === 'library') {
    button('Replay', () => location.reload());
    button('Upload', () => mock.add([{ name: 'Screenshot 2026-10-21 at 14.12.40.png', size: 1_840_000 }]));
    return;
  } else {
    button('Replay', () => location.reload());
    button('Finish', () => { const u = mock.uploads().find((x) => x.status === 'uploading'); if (u) mock.complete(u.id); });
    button('Progress', () => mock.pump());
  }
  button('Settings', () => mock.emit('navigate', { view: 'settings' }));
}
