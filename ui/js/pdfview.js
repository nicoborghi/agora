// The PDF viewer: pages drawn by PDF.js (ui/vendor/pdfjs, loaded on first use), zoom like the
// image viewer, and a map of the pages on the right with the part on screen outlined.
// Only pages near the screen keep a drawing, so a long PDF does not fill memory.

import { invoke, esc, icon, toast } from './api.js';

let lib = null;
async function pdfjs() {
  if (!lib) {
    lib = await import('../vendor/pdfjs/pdf.min.js');
    lib.GlobalWorkerOptions.workerSrc = new URL('../vendor/pdfjs/pdf.worker.min.js', import.meta.url).href;
  }
  return lib;
}

/**
 * The first page of a PDF as a small JPEG (data URL), for its card in the conversation.
 * `width`: in CSS pixels.
 */
export async function firstPage(a, path, width) {
  const bytes = await invoke('read_file', { account_id: a, path });
  const { getDocument } = await pdfjs();
  const task = getDocument({ data: new Uint8Array(bytes), isEvalSupported: false, useSystemFonts: true });
  const doc = await task.promise;
  try {
    const page = await doc.getPage(1);
    const vp = page.getViewport({ scale: 1 });
    const view = page.getViewport({ scale: (width / vp.width) * devicePixelRatio });
    const canvas = document.createElement('canvas');
    canvas.width = Math.ceil(view.width);
    canvas.height = Math.ceil(view.height);
    await page.render({ canvas, canvasContext: canvas.getContext('2d'), viewport: view }).promise;
    return canvas.toDataURL('image/jpeg', 0.82);
  } finally {
    task.destroy().catch(() => {});
  }
}

const GAP = 12; // between pages, at zoom 1
const MAP_W = 150; // map width in pixels

/** Opens a PDF over the app. `path` is the service path of the file (see `filePath`). */
export async function openPdf(a, path, name) {
  document.querySelector('.viewer')?.remove();
  const v = document.createElement('div');
  v.className = 'viewer pdf-viewer';
  v.innerHTML = `<div class="pdf-bar"><span class="pdf-name">${icon('page')}${esc(name)}</span>
      <span class="pdf-page"></span>
      <span class="pdf-zoom"><button data-z="out" title="Zoom out (-)">−</button><span class="viewer-pct">100%</span>
        <button data-z="in" title="Zoom in (+)">+</button><button data-z="fit" title="Fit the width (0)">Fit</button></span>
      <button class="btn" data-pdf="app">${icon('external')}Open in app</button>
      <button class="icon-btn" data-pdf="close" title="Close (Esc)">${icon('close')}</button></div>
    <div class="pdf-main"><div class="pdf-scroll"><div class="pdf-pages"></div></div>
      <div class="pdf-map" title="Click or drag to move"><div class="pm-pages"><i class="vm-box"></i></div></div></div>
    <div class="pdf-loading">Loading ${esc(name)}...</div>`;
  document.body.appendChild(v);
  const scroll = v.querySelector('.pdf-scroll');
  const pagesEl = v.querySelector('.pdf-pages');
  const map = v.querySelector('.pdf-map');
  const mapPages = v.querySelector('.pm-pages');
  const box = v.querySelector('.vm-box');
  const pct = v.querySelector('.viewer-pct');
  const pageLabel = v.querySelector('.pdf-page');

  let doc = null;
  let task = null;
  let closed = false;
  const close = () => {
    closed = true;
    // The viewer goes first: nothing below may keep it on screen.
    Element.prototype.remove.call(v);
    try {
      observer?.disconnect();
      mapObserver?.disconnect();
      task?.destroy().catch(() => {});
    } catch { /* already freed */ }
  };
  v.remove = close; // Esc and other closers call remove()
  v.addEventListener('click', (e) => {
    const b = e.target.closest('[data-pdf], [data-z]');
    if (b?.dataset.pdf === 'close') close();
    else if (b?.dataset.pdf === 'app') invoke('open_attachment', { account_id: a, path, name }).catch((x) => toast(String(x)));
    else if (b?.dataset.z) v.zoom(b.dataset.z);
  });

  let observer = null;
  let mapObserver = null;
  try {
    const bytes = await invoke('read_file', { account_id: a, path });
    if (closed) return;
    const { getDocument } = await pdfjs();
    task = getDocument({ data: new Uint8Array(bytes), isEvalSupported: false, useSystemFonts: true });
    doc = await task.promise;
  } catch (e) {
    if (!closed) v.querySelector('.pdf-loading').textContent = `Could not open ${name}: ${e}`;
    return;
  }
  if (closed) return;

  // Page sizes at scale 1, read once (cheap: no drawing).
  const sizes = [];
  for (let n = 1; n <= doc.numPages; n++) {
    const vp = (await doc.getPage(n)).getViewport({ scale: 1 });
    sizes.push([vp.width, vp.height]);
    if (closed) return;
  }
  v.querySelector('.pdf-loading').remove();
  const widest = Math.max(...sizes.map(([w]) => w));

  // zoom 1 = the widest page fits the width; `px` = screen pixels per PDF point.
  let zoom = 1;
  const px = () => ((scroll.clientWidth - 2 * GAP) / widest) * zoom;
  const slots = sizes.map((_, i) => {
    const s = document.createElement('div');
    s.className = 'pdf-slot';
    s.dataset.n = i + 1;
    pagesEl.appendChild(s);
    const t = document.createElement('div');
    t.className = 'pm-slot';
    t.dataset.n = i + 1;
    mapPages.insertBefore(t, box);
    return [s, t];
  });

  // Draws page n into a slot at `scale` (device pixels for a sharp result).
  const drawn = new Map(); // slot -> scale it was drawn at
  const draw = async (slot, n, scale) => {
    if (drawn.get(slot) === scale) return;
    drawn.set(slot, scale);
    const page = await doc.getPage(n);
    const vp = page.getViewport({ scale: scale * devicePixelRatio });
    const canvas = document.createElement('canvas');
    canvas.width = Math.ceil(vp.width);
    canvas.height = Math.ceil(vp.height);
    try {
      await page.render({ canvas, canvasContext: canvas.getContext('2d'), viewport: vp }).promise;
    } catch { drawn.delete(slot); return; }
    if (closed || drawn.get(slot) !== scale) return;
    slot.replaceChildren(canvas);
  };
  const forget = (slot) => { drawn.delete(slot); slot.replaceChildren(); };

  const layout = () => {
    const k = px();
    sizes.forEach(([w, h], i) => {
      Object.assign(slots[i][0].style, { width: `${w * k}px`, height: `${h * k}px` });
      const mk = MAP_W / widest * 0.9;
      Object.assign(slots[i][1].style, { width: `${w * mk}px`, height: `${h * mk}px` });
    });
    pct.textContent = `${Math.round(zoom * 100)}%`;
    redrawVisible();
    drawMap();
  };
  let redrawTimer = 0;
  const redrawVisible = () => {
    clearTimeout(redrawTimer);
    redrawTimer = setTimeout(() => {
      const k = px();
      for (const [s] of slots) if (s.dataset.near) draw(s, Number(s.dataset.n), k);
    }, 120);
  };

  // Pages near the screen are drawn; far ones forget their drawing.
  observer = new IntersectionObserver((entries) => {
    for (const e of entries) {
      const s = e.target;
      if (e.isIntersecting) { s.dataset.near = '1'; draw(s, Number(s.dataset.n), px()); }
      else { delete s.dataset.near; forget(s); }
    }
  }, { root: scroll, rootMargin: '150% 50%' });
  slots.forEach(([s]) => observer.observe(s));
  // Map pictures, drawn small when they come into view in the map.
  mapObserver = new IntersectionObserver((entries) => {
    for (const e of entries) if (e.isIntersecting) draw(e.target, Number(e.target.dataset.n), MAP_W / widest * 0.9);
  }, { root: map, rootMargin: '100%' });
  slots.forEach(([, t]) => mapObserver.observe(t));

  // The map: the pages column, scaled; the box is the part of it on screen.
  const scaleToMap = () => mapPages.scrollHeight / Math.max(1, pagesEl.scrollHeight);
  function drawMap() {
    const k = scaleToMap();
    const kx = mapPages.clientWidth / Math.max(1, pagesEl.scrollWidth);
    Object.assign(box.style, {
      top: `${scroll.scrollTop * k}px`, height: `${Math.min(scroll.clientHeight, pagesEl.scrollHeight) * k}px`,
      left: `${scroll.scrollLeft * kx}px`, width: `${Math.min(scroll.clientWidth, pagesEl.scrollWidth) * kx}px`,
    });
    // Keep the box in view in a long map.
    const top = scroll.scrollTop * k;
    if (top < map.scrollTop || top > map.scrollTop + map.clientHeight - 30) map.scrollTop = top - map.clientHeight / 3;
    // Current page: the one at the middle of the screen.
    const mid = scroll.scrollTop + scroll.clientHeight / 2;
    const cur = slots.findIndex(([s]) => s.offsetTop + s.offsetHeight + GAP > mid);
    pageLabel.textContent = `${Math.max(1, cur + 1)} / ${doc.numPages}`;
  }
  scroll.addEventListener('scroll', drawMap, { passive: true });

  const moveTo = (e) => {
    const r = mapPages.getBoundingClientRect();
    const k = scaleToMap();
    const kx = mapPages.clientWidth / Math.max(1, pagesEl.scrollWidth);
    scroll.scrollTop = (e.clientY - r.top) / k - scroll.clientHeight / 2;
    scroll.scrollLeft = (e.clientX - r.left) / kx - scroll.clientWidth / 2;
  };
  map.addEventListener('pointerdown', (e) => { map.setPointerCapture(e.pointerId); moveTo(e); });
  map.addEventListener('pointermove', (e) => { if (map.hasPointerCapture(e.pointerId)) moveTo(e); });

  // Zoom around a point of the screen, keeping it where it is.
  const zoomTo = (next, cx = scroll.clientWidth / 2, cy = scroll.clientHeight / 2) => {
    next = Math.min(6, Math.max(0.3, next));
    const fx = (scroll.scrollLeft + cx) / Math.max(1, pagesEl.scrollWidth);
    const fy = (scroll.scrollTop + cy) / Math.max(1, pagesEl.scrollHeight);
    zoom = next;
    layout();
    scroll.scrollLeft = fx * pagesEl.scrollWidth - cx;
    scroll.scrollTop = fy * pagesEl.scrollHeight - cy;
  };
  v.zoom = (how) => {
    if (how === 'in') zoomTo(zoom * 1.25);
    else if (how === 'out') zoomTo(zoom / 1.25);
    else if (how === 'fit') zoomTo(1);
  };
  // Ctrl+wheel zooms (the wheel alone scrolls the pages); drag moves the pages.
  scroll.addEventListener('wheel', (e) => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    const r = scroll.getBoundingClientRect();
    zoomTo(zoom * (e.deltaY < 0 ? 1.15 : 1 / 1.15), e.clientX - r.left, e.clientY - r.top);
  }, { passive: false });
  let drag = null;
  scroll.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    drag = { x: e.clientX, y: e.clientY, l: scroll.scrollLeft, t: scroll.scrollTop };
    scroll.setPointerCapture(e.pointerId);
    scroll.classList.add('dragging');
  });
  scroll.addEventListener('pointermove', (e) => {
    if (!drag) return;
    scroll.scrollLeft = drag.l - (e.clientX - drag.x);
    scroll.scrollTop = drag.t - (e.clientY - drag.y);
  });
  scroll.addEventListener('pointerup', () => { drag = null; scroll.classList.remove('dragging'); });
  new ResizeObserver(() => { if (!closed) layout(); }).observe(scroll);
  layout();
}
