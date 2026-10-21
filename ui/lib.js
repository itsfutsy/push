// Helpers shared by the main window (main.js), the Library (library.js) and the HUD (hud.js).

// Tauri's globals (withGlobalTauri). The preview harness in dev/ (never shipped) provides a stand-in.
const tauri = window.__TAURI__ ?? window.parent?.__PUSH_DEV_TAURI__;

export const invoke = (cmd, args) => tauri.core.invoke(cmd, args);
export const listen = (event, fn) => tauri.event.listen(event, (e) => fn(e.payload));
export const onDragDrop = (fn) => tauri.webview.getCurrentWebview().onDragDropEvent((e) => fn(e.payload));
// An upload's preview image, served by core (preview.rs). The harness has no such scheme.
export const previewUrl = (id) => tauri.previewUrl?.(id) ?? `preview://localhost/${id}`;

// Platform: a synchronous guess from the user agent, confirmed by get_info.
let mac = !/Linux|X11/.test(navigator.userAgent);
export const isMac = () => mac;
export function setPlatform(platform) {
  mac = platform !== 'linux';
  document.documentElement.classList.toggle('linux', !mac);
}
setPlatform(mac ? 'macos' : 'linux');

addEventListener('contextmenu', (e) => {
  if (!e.target.closest('input')) e.preventDefault();
});

export const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');

// Reads a motion token from CSS so JS animations use the same springs.
export function motion(name) {
  const css = getComputedStyle(document.documentElement);
  const easing = css.getPropertyValue(`--ease-${name}`).trim();
  return {
    // Older WebKitGTK lacks linear(); animate() would throw on it.
    easing: easing && CSS.supports('transition-timing-function', easing) ? easing : 'ease',
    duration: parseFloat(css.getPropertyValue(`--dur-${name}`)) || 200,
  };
}

// Cross-fades new content in, the way SwiftUI's opacity content transition does.
export function fadeIn(el) {
  el.animate(
    reducedMotion.matches ? [{ opacity: 0 }, { opacity: 1 }] : [{ opacity: 0, filter: 'blur(2px)' }, { opacity: 1, filter: 'blur(0)' }],
    { duration: 220, easing: 'ease-out' },
  );
}

/** The error message core sends when no token is saved yet (queue.rs NO_TOKEN). */
export const NO_TOKEN = 'Add your upload token in Settings.';

const SVG_NS = 'http://www.w3.org/2000/svg';

// A Lucide glyph from the page's sprite (<symbol id="i-name">).
export function icon(name, cls = '') {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', `i ${cls}`.trim());
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.append(use);
  return svg;
}

export const kindIcon = (kind) => ({ image: 'image', video: 'file-video', audio: 'file-music' })[kind] ?? 'file';

// Sizes in decimal units, like Finder: "812 bytes", "412 KB", "24.8 MB", "1.25 GB".
const UNITS = [[1e9, 'GB', 2], [1e6, 'MB', 1], [1e3, 'KB', 0]];
function unitFor(bytes) {
  return UNITS.find(([n]) => bytes >= n) ?? [1, 'bytes', 0];
}
function scaled(bytes, [n, , digits]) {
  return String(+(bytes / n).toFixed(digits));
}
export function formatSize(bytes) {
  const unit = unitFor(bytes);
  return `${scaled(bytes, unit)} ${unit[1]}`;
}
// "1.2 of 2.4 MB": both numbers in the total's unit, one decimal so the count moves smoothly.
export function formatProgress(sent, total) {
  const unit = unitFor(total);
  const fmt = (b) => (unit[0] === 1 ? String(b) : (b / unit[0]).toFixed(unit[0] >= 1e6 ? 1 : 0));
  return `${fmt(Math.min(sent, total))} of ${fmt(total)} ${unit[1]}`;
}
export const percent = (sent, total) => (total > 0 ? Math.min(100, Math.floor((sent / total) * 100)) : 0);

// "Just now", "3 min ago", "2 hr ago", "Yesterday", "Oct 18", "Oct 18, 2025" (hand-rolled on purpose:
// Intl.RelativeTimeFormat's short style adds periods).
const dayFmt = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
const yearFmt = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
export function relativeTime(ms, now = Date.now()) {
  const mins = Math.floor((now - ms) / 60000);
  if (mins < 1) return 'Just now';
  if (mins < 60) return `${mins} min ago`;
  if (mins < 24 * 60) return `${Math.floor(mins / 60)} hr ago`;
  const then = new Date(ms), today = new Date(now);
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1);
  if (then >= yesterday) return 'Yesterday';
  return (then.getFullYear() === today.getFullYear() ? dayFmt : yearFmt).format(then);
}

// Finder-style middle truncation, so the end of a file name (time stamp, extension) stays visible:
// "Screenshot 2026-10…14.03.22.png". The element must be laid out; CSS end ellipsis is the fallback.
// Measured in a hidden span rather than a canvas: WebKit applies SF's tracking to DOM text only.
let probe;
const cut = (text, n) => `${text.slice(0, Math.ceil(n / 2)).trimEnd()}…${text.slice(text.length - Math.floor(n / 2)).trimStart()}`;
export function fitMiddle(el, text) {
  el.textContent = text;
  const room = el.getBoundingClientRect().width - 0.5; // fractional: clientWidth rounds
  if (room <= 0) return;
  if (!probe) {
    probe = document.createElement('span');
    probe.setAttribute('aria-hidden', 'true');
    probe.style.cssText = 'position:absolute;left:-10000px;top:0;visibility:hidden;white-space:pre';
    document.body.append(probe);
  }
  const cs = getComputedStyle(el);
  for (const p of ['fontFamily', 'fontSize', 'fontWeight', 'fontVariantNumeric']) probe.style[p] = cs[p];
  const fits = (s) => { probe.textContent = s; return probe.getBoundingClientRect().width <= room; };
  if (fits(text)) return;
  let lo = 0, hi = text.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (fits(cut(text, mid))) lo = mid;
    else hi = mid - 1;
  }
  el.textContent = cut(text, lo);
}

export const shortUrl = (url) => url.replace(/^https?:\/\//, '');
export function hostOf(url) {
  try { return new URL(url).host; } catch { return ''; }
}

// Accelerators ("CommandOrControl+Shift+Digit2") for display: "⇧⌘2" on macOS (Apple order ⌃⌥⇧⌘),
// "Ctrl+Shift+2" on Linux.
const MODS = {
  commandorcontrol: 'cmdOrCtrl', cmdorctrl: 'cmdOrCtrl', commandorctrl: 'cmdOrCtrl', cmdorcontrol: 'cmdOrCtrl',
  command: 'cmd', cmd: 'cmd', super: 'cmd', meta: 'cmd',
  control: 'ctrl', ctrl: 'ctrl', alt: 'alt', option: 'alt', shift: 'shift',
};
const KEY_NAMES = {
  Space: 'Space', Enter: '↩', Tab: '⇥', Backspace: '⌫', Delete: '⌦', Escape: '⎋',
  ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→',
  Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']', Backslash: '\\', Semicolon: ';',
  Quote: '\'', Comma: ',', Period: '.', Slash: '/', Backquote: '`',
  Home: '↖', End: '↘', PageUp: '⇞', PageDown: '⇟',
};
const LINUX_KEY_NAMES = { Enter: 'Enter', Tab: 'Tab', Backspace: 'Backspace', Delete: 'Delete', Escape: 'Esc',
  ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right', Home: 'Home', End: 'End', PageUp: 'PgUp', PageDown: 'PgDn' };

export function keyName(code) {
  return (!mac && LINUX_KEY_NAMES[code]) || KEY_NAMES[code] || code.replace(/^(Key|Digit|Numpad)/, '');
}

export function formatModifiers(set) {
  if (mac) return ['ctrl', 'alt', 'shift', 'cmd'].filter((m) => set.has(m)).map((m) => ({ ctrl: '⌃', alt: '⌥', shift: '⇧', cmd: '⌘' })[m]).join('');
  return ['ctrl', 'alt', 'shift', 'cmd'].filter((m) => set.has(m)).map((m) => ({ ctrl: 'Ctrl', alt: 'Alt', shift: 'Shift', cmd: 'Super' })[m]).join('+');
}

// Both Command keys held together (macOS). The app watches for it itself; it isn't a real accelerator.
export const BOTH_COMMANDS = 'LeftCommand+RightCommand';

export function formatShortcut(accel) {
  if (!accel) return '';
  if (accel === BOTH_COMMANDS) return '⌘ + ⌘';
  const mods = new Set();
  let key = '';
  for (const part of accel.split('+')) {
    const m = MODS[part.toLowerCase()];
    if (m === 'cmdOrCtrl') mods.add(mac ? 'cmd' : 'ctrl');
    else if (m) mods.add(m);
    else key = part;
  }
  const m = formatModifiers(mods);
  return mac ? m + keyName(key) : [m, keyName(key)].filter(Boolean).join('+');
}

// Polite screen reader announcements ("Link copied").
export function announce(text) {
  const el = document.getElementById('announcer');
  if (!el) return;
  el.textContent = '';
  requestAnimationFrame(() => { el.textContent = text; });
}

// Sets a progress ring (<svg class="ring">) to a percentage.
export function setRing(svg, pct) {
  const bar = svg.querySelector('.bar');
  bar.style.strokeDashoffset = String(100 - pct);
  bar.style.opacity = pct > 0 ? '1' : '0';
  svg.setAttribute('aria-valuenow', String(pct));
}
