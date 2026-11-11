// The gallery: every image of a conversation over the conversation, by month, newest first.
// Tiles grow and shrink (slider, Ctrl+wheel); the timeline on the right shows the months and
// where you are, and a click or drag on it scrolls there. A tile opens the image viewer, which
// can then step through the whole gallery.

import { esc, icon } from './api.js';

const SIZE_KEY = 'agora.gallery.tile';
const SIZES = { min: 72, max: 360, step: 24, start: 150 };
/** Timeline labels closer than this (px) to the one above are left out. */
const LABEL_GAP = 16;

/**
 * One image of the gallery, from a message (see `galleryItems`).
 * @typedef {{ src: string, full: string, name: string, ts: number, author: string, message: object }} Item
 */

/** The images of `messages` (newest first), one item per image file. */
export function galleryItems(messages) {
  const out = [];
  for (const m of messages) {
    for (const f of m.attachments || []) {
      if (!(f.mime || '').startsWith('image/')) continue;
      out.push({ src: f.thumb_url || f.url, full: f.url, name: f.name || '', ts: m.ts, author: m.author_name || '', message: m });
    }
  }
  return out.sort((a, b) => b.ts - a.ts);
}

const monthKey = (ts) => { const d = new Date(ts); return `${d.getFullYear()}-${d.getMonth()}`; };
const monthName = (ts) => new Date(ts).toLocaleDateString([], { month: 'long', year: 'numeric' });
const shortMonth = (ts) => new Date(ts).toLocaleDateString([], { month: 'short' });
export const itemDate = (ts) => new Date(ts).toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' });

function savedSize() {
  try {
    const n = Number(localStorage.getItem(SIZE_KEY));
    if (n >= SIZES.min && n <= SIZES.max) return n;
  } catch {}
  return SIZES.start;
}

/**
 * Opens the gallery over the window from `left` (px) to the right edge, so it covers the
 * conversation and its side panel. `open(items, index)` shows one image (the viewer).
 * Returns the gallery element; it removes itself on close.
 */
export function openGallery({ items, title, open, left = 0 }) {
  document.querySelector('#gallery-view')?.close();
  const view = document.createElement('div');
  view.id = 'gallery-view';
  view.style.left = `${left}px`;
  let size = savedSize();
  view.style.setProperty('--tile', `${size}px`);

  const months = [];
  for (const [i, it] of items.entries()) {
    const k = monthKey(it.ts);
    if (months.at(-1)?.key !== k) months.push({ key: k, ts: it.ts, items: [] });
    months.at(-1).items.push(i);
  }
  const tile = (i) => {
    const it = items[i];
    const tip = [it.name, it.author, itemDate(it.ts)].filter(Boolean).join(' · ');
    return `<button class="gal-tile" data-i="${i}" title="${esc(tip)}"><img src="${esc(it.src)}" alt="${esc(it.name)}" loading="lazy" draggable="false"></button>`;
  };
  view.innerHTML = `<div class="gal-bar">
      <span class="gal-title">${icon('image')}<b>${esc(title)}</b><span class="muted">${items.length} image${items.length === 1 ? '' : 's'}</span></span>
      <div class="gal-size" title="Tile size (Ctrl+wheel)">
        <button data-gs="-1" title="Smaller">${icon('grid')}</button>
        <input type="range" min="${SIZES.min}" max="${SIZES.max}" step="2" value="${size}" aria-label="Tile size">
        <button data-gs="1" title="Larger">${icon('image')}</button>
      </div>
      <button class="icon-btn" data-act="gal-close" title="Close (Esc)">${icon('close')}</button>
    </div>
    <div class="gal-main">
      <div class="gal-body">${months.map((m) => `<section data-ts="${m.ts}">
          <h3>${esc(monthName(m.ts))}<span class="muted">${m.items.length}</span></h3>
          <div class="gal-grid">${m.items.map(tile).join('')}</div></section>`).join('')
        || '<p class="muted gal-empty">No images in this conversation yet.</p>'}</div>
      <div class="gal-time" title="Click or drag to move through time"><i class="gal-thumb"></i><span class="gal-tip" hidden></span></div>
    </div>`;
  document.body.appendChild(view);

  const body = view.querySelector('.gal-body');
  const time = view.querySelector('.gal-time');
  const thumb = time.querySelector('.gal-thumb');
  const tip = time.querySelector('.gal-tip');
  const range = view.querySelector('input[type=range]');
  const sections = [...body.querySelectorAll('section')];

  // Month labels where each month starts, as a share of the whole scroll height.
  const drawLabels = () => {
    time.querySelectorAll('.gal-mark').forEach((el) => el.remove());
    const total = body.scrollHeight, h = time.clientHeight;
    if (!total || !h) return;
    let last = -Infinity, year = null;
    for (const s of sections) {
      const top = (s.offsetTop / total) * h;
      const ts = Number(s.dataset.ts);
      const y = new Date(ts).getFullYear();
      if (top - last < LABEL_GAP && year === y) continue;
      const mark = document.createElement('span');
      mark.className = `gal-mark${y !== year ? ' year' : ''}`;
      mark.style.top = `${top}px`;
      mark.textContent = y !== year ? String(y) : shortMonth(ts);
      time.appendChild(mark);
      last = top;
      year = y;
    }
    drawThumb();
  };
  // The part on screen.
  const drawThumb = () => {
    const total = body.scrollHeight || 1, h = time.clientHeight;
    thumb.style.top = `${(body.scrollTop / total) * h}px`;
    thumb.style.height = `${Math.max(14, (body.clientHeight / total) * h)}px`;
    thumb.hidden = body.scrollHeight <= body.clientHeight;
  };
  // The month at a height of the timeline.
  const monthAt = (frac) => {
    const y = frac * body.scrollHeight;
    let found = sections[0];
    for (const s of sections) if (s.offsetTop <= y) found = s; else break;
    return found;
  };
  const scrollToFrac = (frac) => { body.scrollTop = frac * body.scrollHeight - body.clientHeight / 2; };
  const fracOf = (e) => { const r = time.getBoundingClientRect(); return Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)); };
  const showTip = (e) => {
    const s = monthAt(fracOf(e));
    if (!s) return;
    tip.hidden = false;
    tip.textContent = monthName(Number(s.dataset.ts));
    tip.style.top = `${e.clientY - time.getBoundingClientRect().top}px`;
  };
  time.addEventListener('pointerdown', (e) => { time.setPointerCapture(e.pointerId); scrollToFrac(fracOf(e)); showTip(e); });
  time.addEventListener('pointermove', (e) => { showTip(e); if (time.hasPointerCapture(e.pointerId)) scrollToFrac(fracOf(e)); });
  time.addEventListener('pointerleave', () => { tip.hidden = true; });
  body.addEventListener('scroll', drawThumb, { passive: true });

  // A new size keeps the image at the middle of the screen where it is.
  const resize = (next) => {
    next = Math.round(Math.min(SIZES.max, Math.max(SIZES.min, next)));
    if (next === size) return;
    const mid = body.scrollTop + body.clientHeight / 2;
    const frac = body.scrollHeight ? mid / body.scrollHeight : 0;
    size = next;
    range.value = String(size);
    view.style.setProperty('--tile', `${size}px`);
    body.scrollTop = frac * body.scrollHeight - body.clientHeight / 2;
    drawLabels();
    try { localStorage.setItem(SIZE_KEY, String(size)); } catch {}
  };
  range.addEventListener('input', () => resize(Number(range.value)));
  body.addEventListener('wheel', (e) => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    resize(size * (e.deltaY < 0 ? 1.12 : 1 / 1.12));
  }, { passive: false });

  view.addEventListener('click', (e) => {
    if (e.target.closest('[data-act="gal-close"]')) { view.close(); return; }
    const step = e.target.closest('[data-gs]');
    if (step) { resize(size + Number(step.dataset.gs) * SIZES.step); return; }
    const t = e.target.closest('.gal-tile');
    if (t) open(items, Number(t.dataset.i));
  });

  // Tiles are square, so heights change only with the tile size or the window: the labels are
  // redrawn then, the thumb on every scroll.
  const ro = new ResizeObserver(drawLabels);
  ro.observe(body);
  view.close = () => { ro.disconnect(); view.remove(); };
  /** Scrolls a tile into view (when the viewer moved past the screen). */
  view.reveal = (i) => {
    const t = body.querySelector(`.gal-tile[data-i="${i}"]`);
    if (t) t.scrollIntoView({ block: 'nearest' });
  };
  drawLabels();
  return view;
}
