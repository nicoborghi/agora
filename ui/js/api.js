// Thin wrappers over the Tauri globals (withGlobalTauri), so no bundler is needed.
const T = window.__TAURI__;

// `args` may be raw bytes (Uint8Array), sent as the request body; `options.headers` then carries the rest.
export const invoke = (cmd, args = {}, options) => T.core.invoke(cmd, args, options);
export const listen = (event, cb) => T.event.listen(event, (e) => cb(e.payload));

export function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}

export function initials(name) {
  const parts = String(name || '?').trim().split(/\s+/).filter(Boolean);
  const s = parts.length > 1 ? parts[0][0] + parts[parts.length - 1][0] : (parts[0] || '?').slice(0, 2);
  return esc(s.toUpperCase());
}

/** One toast at a time: a new one replaces the current, and each fades out. */
export function toast(text, ms = 2500) {
  document.querySelectorAll('.toast').forEach((t) => t.remove());
  const el = document.createElement('div');
  el.className = 'toast';
  el.setAttribute('role', 'status');
  el.textContent = text;
  document.body.appendChild(el);
  setTimeout(() => {
    el.classList.add('out');
    setTimeout(() => el.remove(), 200);
  }, ms);
}

// Minimal stroke icons (no icon font, no images).
const PATHS = {
  star: '<path d="M8 1.8l1.9 3.9 4.3.6-3.1 3 .7 4.3L8 11.6l-3.8 2 .7-4.3-3.1-3 4.3-.6z"/>',
  bell: '<path d="M4 11V7a4 4 0 0 1 8 0v4l1.2 1.5H2.8z"/><path d="M6.5 14a1.6 1.6 0 0 0 3 0"/>',
  // The bell above with one clean slash across it.
  bellOff: '<path d="M4 11V7a4 4 0 0 1 8 0v4l1.2 1.5H2.8z"/><path d="M6.5 14a1.6 1.6 0 0 0 3 0"/><path d="M2.6 2.4 13.4 13.6"/>',
  image: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><circle cx="6" cy="6.5" r="1.2"/><path d="M14 11l-3.5-3.5L4 13"/>',
  close: '<path d="M4 4l8 8M12 4l-8 8"/>',
  // Same weight as home/grid: a plain ring with eight short, rounded teeth and a hub.
  gear: '<circle cx="8" cy="8" r="4.1"/><circle cx="8" cy="8" r="1.6"/><path d="M8 1.9v1.6M8 12.5v1.6M1.9 8h1.6M12.5 8h1.6M3.7 3.7l1.1 1.1M11.2 11.2l1.1 1.1M3.7 12.3l1.1-1.1M11.2 4.8l1.1-1.1" stroke-width="2"/>',
  home: '<path d="M2.5 7.3L8 2.8l5.5 4.5"/><path d="M4 6.2V13.2h3V10h2v3.2h3V6.2"/>',
  clip: '<path d="M10.5 5L6 9.5a1.4 1.4 0 0 0 2 2L13 6.5a2.8 2.8 0 0 0-4-4L4 7.5a4.2 4.2 0 0 0 6 6l3-3"/>',
  link: '<path d="M7 9a2.6 2.6 0 0 0 3.7 0l2.1-2.1a2.6 2.6 0 0 0-3.7-3.7L8.3 4"/><path d="M9 7a2.6 2.6 0 0 0-3.7 0L3.2 9.1a2.6 2.6 0 0 0 3.7 3.7l.8-.8"/>',
  marker: '<path d="M9.5 2.5l4 4-6 6H3.5v-4z"/><path d="M2 14h12"/>',
  circle: '<ellipse cx="8" cy="8" rx="6" ry="4.6"/>',
  text: '<path d="M3 3.5h10M8 3.5v9.5M6 13h4"/>',
  crop: '<path d="M4.5 1.5v10h10"/><path d="M1.5 4.5h10v10"/>',
  pointer: '<path d="M4 2.5l8.5 5.2-3.8 1 2.2 4.1-1.6.8-2.2-4.1L4 12.4z"/>',
  more: '<circle cx="3.5" cy="8" r=".8"/><circle cx="8" cy="8" r=".8"/><circle cx="12.5" cy="8" r=".8"/>',
  undo: '<path d="M5 3.5L2 6.5l3 3"/><path d="M2 6.5h7.5a4 4 0 0 1 0 8H7"/>',
  list: '<path d="M6 4h8M6 8h8M6 12h8"/><circle cx="2.8" cy="4" r=".6"/><circle cx="2.8" cy="8" r=".6"/><circle cx="2.8" cy="12" r=".6"/>',
  olist: '<path d="M6.5 4h7.5M6.5 8h7.5M6.5 12h7.5"/><path d="M2.3 2.8h.9v2.6M2 10.6c.4-.6 1.6-.6 1.6.3 0 .6-1.6 1.2-1.6 2.1h1.7"/>',
  quote: '<path d="M3 4.5h4v4H4.6c0 1.4.6 2.4 2 3M9 4.5h4v4h-2.4c0 1.4.6 2.4 2 3"/>',
  code: '<path d="M5.5 4.5L2 8l3.5 3.5M10.5 4.5L14 8l-3.5 3.5"/>',
  codeblock: '<rect x="2" y="2.5" width="12" height="11" rx="1.5"/><path d="M6 6.5L4.5 8 6 9.5M10 6.5L11.5 8 10 9.5"/>',
  grid: '<rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1"/><rect x="9" y="2.5" width="4.5" height="4.5" rx="1"/><rect x="2.5" y="9" width="4.5" height="4.5" rx="1"/><rect x="9" y="9" width="4.5" height="4.5" rx="1"/>',
  smile: '<circle cx="8" cy="8" r="6"/><path d="M5.5 9.5a3 3 0 0 0 5 0"/><path d="M6 6.3v.1M10 6.3v.1"/>',
  reply: '<path d="M6.5 4L3 7.5 6.5 11"/><path d="M3 7.5h6a4 4 0 0 1 4 4V13"/>',
  edit: '<path d="M10.5 2.5l3 3L6 13H3v-3z"/>',
  trash: '<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.7 9h5.6l.7-9"/>',
  lock: '<rect x="3.5" y="7" width="9" height="7" rx="1.2"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/>',
  refresh: '<path d="M13 8a5 5 0 1 1-1.5-3.6"/><path d="M13 2.5v3h-3"/>',
  chevron: '<path d="M4.5 6.5L8 10l3.5-3.5"/>',
  search: '<circle cx="7" cy="7" r="4.2"/><path d="M10.2 10.2L14 14"/>',
  external: '<path d="M9 2.5h4.5V7"/><path d="M13.5 2.5L7.5 8.5"/><path d="M12 9.5v4H2.5V4h4"/>',
  back: '<path d="M10 3.5L5.5 8l4.5 4.5"/>',
  folder: '<path d="M2 4.5h4.5l1.5 1.5h6v7.5H2z"/>',
  chats: '<path d="M2 3.5h8.5v5.5H5.5L3 11V9H2z"/><path d="M12.5 6h1.5v5.5h-1V13.5l-2.5-2H6.5V10"/>',
  at: '<circle cx="8" cy="8" r="2.6"/><path d="M10.6 8v1.1c0 1.2 1.9 1.6 2.6.3.9-2.1.2-5.4-2.6-6.5-2.9-1.2-6.6.6-7.1 4-.5 3.4 2.2 6.2 5.6 5.9 1-.1 1.8-.4 2.5-.9"/>',
  moon: '<path d="M13 9.8A5.5 5.5 0 0 1 6.2 3a5.5 5.5 0 1 0 6.8 6.8z"/>',
  palette: '<path d="M8 2.2c-3.4 0-6 2.5-6 5.6 0 3.2 2.6 5.9 5.6 5.9 1.1 0 1.5-.8 1.2-1.6-.4-.9.2-1.8 1.2-1.8h1.6c1.5 0 2.6-1.1 2.6-2.6 0-3.1-2.8-5.5-6.2-5.5z"/><circle cx="5.2" cy="7.2" r=".7"/><circle cx="7.6" cy="5" r=".7"/><circle cx="10.4" cy="5.6" r=".7"/>',
  shield: '<path d="M8 1.8l5 1.9v3.9c0 3.1-2.1 5.5-5 6.6-2.9-1.1-5-3.5-5-6.6V3.7z"/>',
  sliders: '<path d="M2.5 4.5h4M10 4.5h3.5M2.5 11.5h7M13 11.5h.5"/><circle cx="8.2" cy="4.5" r="1.6"/><circle cx="11.3" cy="11.5" r="1.6"/>',
  person: '<circle cx="8" cy="5.5" r="2.7"/><path d="M2.8 14c.6-2.8 2.7-4.3 5.2-4.3s4.6 1.5 5.2 4.3"/>',
  people: '<circle cx="6" cy="6" r="2.4"/><path d="M1.6 13.5c.5-2.4 2.2-3.7 4.4-3.7s3.9 1.3 4.4 3.7"/><path d="M10.4 3.8a2.3 2.3 0 0 1 0 4.4M12 9.9c1.3.5 2.1 1.7 2.4 3.6"/>',
  copy: '<rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M3.5 10.5h-1v-8h8v1"/>',
  mail: '<rect x="2" y="3.5" width="12" height="9" rx="1.5"/><path d="M2.5 4.5 8 9l5.5-4.5"/>',
};
export const icon = (name) => `<svg class="i" viewBox="0 0 16 16">${PATHS[name] || ''}</svg>`;
