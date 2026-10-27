// Minimal image editor for screenshots before sending: select/move, pen,
// highlighter, circle, text, crop, a few colors, undo. Plain canvas, no library.
// Annotations stay separate objects (movable, editable, deletable) and are only
// flattened into the PNG on Done. The caller keeps the returned `state` with
// the original image, so reopening the editor brings them all back editable.
// The canvas exists only while the editor is open and is released on close.
import { icon } from './api.js';

const COLORS = ['#e53935', '#fbc02d', '#43a047', '#1e88e5', '#111111', '#ffffff'];
const TOOLS = [
  ['select', 'Select', 'pointer', 'V'],
  ['brush', 'Pen', 'edit', 'P'],
  ['highlight', 'Highlighter', 'marker', 'H'],
  ['circle', 'Circle', 'circle', 'O'],
  ['text', 'Text', 'text', 'T'],
  ['crop', 'Crop', 'crop', 'C'],
];
/** Big screenshots are scaled down to this longest side, which bounds memory (about 4 bytes per pixel). */
const MAX_SIDE = 3200;

/**
 * Opens the editor on `blob` (always the original image). `state` is what a previous
 * session returned, so its annotations come back editable. Resolves with
 * `{ blob, state }` (the flattened PNG and the editable annotations) or null when cancelled.
 */
export function editImage(blob, state = null) {
  return new Promise((resolve) => {
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.onerror = () => { URL.revokeObjectURL(url); resolve(null); };
    img.onload = () => open(img, url, state, resolve);
    img.src = url;
  });
}

const clone = (v) => JSON.parse(JSON.stringify(v));

function open(img, url, state, resolve) {
  const scale = Math.min(1, MAX_SIDE / Math.max(img.naturalWidth, img.naturalHeight));
  const W = Math.round(img.naturalWidth * scale);
  const H = Math.round(img.naturalHeight * scale);
  // Stroke sizes follow the image size, so annotations look the same on any screenshot.
  const unit = Math.max(2, Math.round(Math.max(W, H) / 400));

  const root = document.createElement('div');
  root.className = 'img-editor';
  root.innerHTML = `<div class="ie-bar">
      ${TOOLS.map(([id, label, ic, k]) => `<button data-tool="${id}" title="${label} (${k})">${icon(ic)}<span>${label}</span></button>`).join('')}
      <span class="ie-sep"></span>
      ${COLORS.map((c) => `<button class="ie-color" data-color="${c}" style="--c:${c}" title="${c}"></button>`).join('')}
      <span class="ie-sep"></span>
      <label class="ie-range" title="Line width (pen, highlighter, circle)">Width
        <input type="range" data-set="width" min="1" max="10" value="2"><output>2</output></label>
      <label class="ie-range" title="Text size">Text
        <input type="range" data-set="font" min="8" max="48" value="18"><output>18</output></label>
      <button data-act="shadow" title="Drop shadow under strokes and text">Shadow</button>
      <span class="ie-sep"></span>
      <button data-act="delete" title="Delete the selected item (Del)">${icon('trash')}</button>
      <button data-act="undo" title="Undo (Ctrl+Z)">${icon('undo')}<span>Undo</span></button>
      <span class="ie-grow"></span>
      <button data-act="cancel">Cancel</button>
      <button data-act="done" class="primary">Done</button>
    </div>
    <div class="ie-stage"><canvas></canvas></div>
    <div class="ie-hint"></div>`;
  document.body.appendChild(root);
  const canvas = root.querySelector('canvas');
  const hint = root.querySelector('.ie-hint');
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext('2d');

  // Editable state: annotation objects (drawn in order) and the crop rectangle.
  let items = clone(state?.items || []);
  let crop = state?.crop ? { ...state.crop } : null;
  const history = []; // snapshots for undo
  const snapshot = () => history.push(clone({ items, crop }));

  let live = null; // the object being drawn
  let selected = -1; // index in `items`
  let drag = null; // { start: [x, y], before, moved }
  let tool = 'brush';
  let color = COLORS[0];
  // Width 1–10 and text 8–48 are relative steps; `unit` turns them into image pixels.
  let width = 2;
  let font = 18;
  let shadow = false;
  const strokeWidth = () => width * unit;
  const fontSize = () => (font * unit) / 2;
  const textFont = (size) => `600 ${size}px "Segoe UI", sans-serif`;

  const HINTS = {
    select: 'Click an item to select it, drag to move it, double-click text to change it, Del to delete.',
    brush: 'Drag to draw.',
    highlight: 'Drag over what matters.',
    circle: 'Drag to circle something.',
    text: 'Click where the text goes, type, then Enter. Click existing text to change it, drag it to move it.',
    crop: 'Drag the area to keep; click once to remove the crop. Done applies it.',
  };

  function setTool(t) {
    tool = t;
    root.querySelectorAll('[data-tool]').forEach((b) => b.classList.toggle('on', b.dataset.tool === t));
    hint.textContent = HINTS[t];
    canvas.style.cursor = t === 'text' ? 'text' : t === 'select' ? 'default' : 'crosshair';
    if (t !== 'select' && t !== 'text') selectItem(-1);
  }

  /** Selecting shows the item's own style in the toolbar, so it can be restyled. */
  function selectItem(i) {
    selected = i;
    const it = items[i];
    root.querySelector('[data-act="delete"]').disabled = !it;
    if (it) {
      showColor(it.color);
      root.querySelector('[data-act="shadow"]').classList.toggle('on', !!it.shadow);
      if (it.type === 'text') setRange('font', Math.round((it.size * 2) / unit));
      else setRange('width', Math.max(1, Math.round(it.width / unit / (it.type === 'highlight' ? 4.5 : 1))));
    }
    render();
  }

  function setRange(which, v) {
    const r = root.querySelector(`[data-set="${which}"]`);
    r.value = v;
    r.nextElementSibling.value = v;
    if (which === 'width') width = v; else font = v;
  }

  function showColor(c) {
    color = c;
    root.querySelectorAll('[data-color]').forEach((b) => b.classList.toggle('on', b.dataset.color === c));
  }

  /** Applies a style change to the selected item (with undo), if any. */
  function restyle(change) {
    const it = items[selected];
    if (!it) return;
    snapshot();
    change(it);
    render();
  }

  function draw(op) {
    if (op.hidden) return; // text being retyped in its box
    ctx.save();
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.strokeStyle = op.color;
    ctx.fillStyle = op.color;
    if (op.shadow) {
      ctx.shadowColor = 'rgba(0, 0, 0, 0.6)';
      ctx.shadowBlur = unit * 3;
      ctx.shadowOffsetX = unit;
      ctx.shadowOffsetY = unit;
    }
    if (op.type === 'brush' || op.type === 'highlight') {
      if (op.type === 'highlight') {
        ctx.globalAlpha = 0.35;
        ctx.lineCap = 'square';
      }
      ctx.lineWidth = op.width;
      ctx.beginPath();
      op.points.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      if (op.points.length === 1) ctx.lineTo(op.points[0][0] + 0.1, op.points[0][1]);
      ctx.stroke();
    } else if (op.type === 'circle') {
      ctx.lineWidth = op.width;
      ctx.beginPath();
      ctx.ellipse((op.x0 + op.x1) / 2, (op.y0 + op.y1) / 2, Math.abs(op.x1 - op.x0) / 2, Math.abs(op.y1 - op.y0) / 2, 0, 0, Math.PI * 2);
      ctx.stroke();
    } else if (op.type === 'text') {
      ctx.font = textFont(op.size);
      ctx.textBaseline = 'top';
      // A thin outline in the opposite tone keeps text readable on any background.
      ctx.lineWidth = Math.max(2, op.size / 8);
      ctx.strokeStyle = op.color === '#ffffff' || op.color === '#fbc02d' ? '#000000' : '#ffffff';
      ctx.strokeText(op.text, op.x, op.y);
      // The shadow belongs under the outline only, or the letters would get two.
      ctx.shadowColor = 'transparent';
      ctx.fillText(op.text, op.x, op.y);
    }
    ctx.restore();
  }

  function rectOf(c) {
    return { x: Math.min(c.x0, c.x1), y: Math.min(c.y0, c.y1), w: Math.abs(c.x1 - c.x0), h: Math.abs(c.y1 - c.y0) };
  }

  /** Bounding box of an annotation, in image pixels. */
  function bounds(op) {
    if (op.points) {
      const xs = op.points.map((p) => p[0]);
      const ys = op.points.map((p) => p[1]);
      const pad = op.width / 2;
      return { x: Math.min(...xs) - pad, y: Math.min(...ys) - pad, w: Math.max(...xs) - Math.min(...xs) + pad * 2, h: Math.max(...ys) - Math.min(...ys) + pad * 2 };
    }
    if (op.type === 'circle') {
      const r = rectOf(op);
      return { x: r.x - op.width / 2, y: r.y - op.width / 2, w: r.w + op.width, h: r.h + op.width };
    }
    ctx.font = textFont(op.size);
    return { x: op.x, y: op.y, w: ctx.measureText(op.text).width, h: op.size * 1.25 };
  }

  /** Topmost item under a point, or -1. `types` limits what can be picked. */
  function hit(x, y, types = null) {
    const slack = unit * 3;
    for (let i = items.length - 1; i >= 0; i--) {
      if (types && !types.includes(items[i].type)) continue;
      const b = bounds(items[i]);
      if (x >= b.x - slack && x <= b.x + b.w + slack && y >= b.y - slack && y <= b.y + b.h + slack) return i;
    }
    return -1;
  }

  function moveBy(op, dx, dy) {
    if (op.points) op.points = op.points.map(([x, y]) => [x + dx, y + dy]);
    else if (op.type === 'circle') { op.x0 += dx; op.x1 += dx; op.y0 += dy; op.y1 += dy; }
    else { op.x += dx; op.y += dy; }
  }

  function render() {
    ctx.clearRect(0, 0, W, H);
    ctx.drawImage(img, 0, 0, W, H);
    items.forEach(draw);
    if (live && live.type !== 'crop') draw(live);
    const area = live?.type === 'crop' ? rectOf(live) : crop;
    if (area && area.w > 2 && area.h > 2) {
      ctx.save();
      ctx.fillStyle = 'rgba(0, 0, 0, 0.55)';
      ctx.beginPath();
      ctx.rect(0, 0, W, H);
      ctx.rect(area.x, area.y, area.w, area.h);
      ctx.fill('evenodd');
      ctx.strokeStyle = '#ffffff';
      ctx.setLineDash([unit * 3, unit * 2]);
      ctx.lineWidth = Math.max(1, unit / 2);
      ctx.strokeRect(area.x, area.y, area.w, area.h);
      ctx.restore();
    }
    if (items[selected]) {
      const b = bounds(items[selected]);
      const pad = unit * 2;
      ctx.save();
      ctx.strokeStyle = '#1e88e5';
      ctx.lineWidth = Math.max(1, unit / 2);
      ctx.setLineDash([unit * 2, unit * 1.5]);
      ctx.strokeRect(b.x - pad, b.y - pad, b.w + pad * 2, b.h + pad * 2);
      ctx.restore();
    }
  }

  /** Pointer position in image pixels (the canvas is shown scaled to fit). */
  function at(e) {
    const r = canvas.getBoundingClientRect();
    return [((e.clientX - r.left) * W) / r.width, ((e.clientY - r.top) * H) / r.height];
  }

  /** Screen position of an image point (to place the text box over existing text). */
  function toScreen(x, y) {
    const r = canvas.getBoundingClientRect();
    return [r.left + (x * r.width) / W, r.top + (y * r.height) / H];
  }

  let textBox = null;
  /** Types a new text at (x, y), or changes item `index` when given. */
  function editText(x, y, index = -1) {
    textBox?.remove();
    const existing = items[index];
    const size = existing ? existing.size : fontSize();
    const [sx, sy] = toScreen(x, y);
    const box = document.createElement('input');
    box.className = 'ie-text';
    box.style.left = `${sx}px`;
    box.style.top = `${sy - 4}px`;
    box.style.color = existing ? existing.color : color;
    // Same size on screen as the text will have in the (scaled) image.
    box.style.fontSize = `${Math.max(11, (size * canvas.getBoundingClientRect().width) / W)}px`;
    box.placeholder = 'Type, then Enter';
    box.value = existing ? existing.text : '';
    root.appendChild(box);
    textBox = box;
    // Hide the original while it is being changed.
    if (existing) { existing.hidden = true; render(); }
    setTimeout(() => { box.focus(); box.select(); }, 0);
    let finished = false;
    const finish = (keep) => {
      if (finished) return;
      finished = true;
      if (existing) delete existing.hidden;
      const text = box.value;
      box.remove();
      textBox = null;
      if (keep && existing && text !== existing.text) {
        snapshot();
        if (text.trim()) existing.text = text;
        else { items.splice(index, 1); selected = -1; }
      } else if (keep && !existing && text.trim()) {
        snapshot();
        items.push({ type: 'text', text, x, y, size, color, shadow });
        selectItem(items.length - 1);
      }
      render();
    };
    box.addEventListener('keydown', (ev) => {
      ev.stopPropagation();
      if (ev.key === 'Enter') finish(true);
      if (ev.key === 'Escape') finish(false);
    });
    box.addEventListener('blur', () => finish(true));
  }

  canvas.addEventListener('pointerdown', (e) => {
    const [x, y] = at(e);
    // Select (and Text, over existing text): pick an item and get ready to move it.
    if (tool === 'select' || tool === 'text') {
      const i = hit(x, y, tool === 'text' ? ['text'] : null);
      if (i >= 0) {
        selectItem(i);
        canvas.setPointerCapture(e.pointerId);
        drag = { start: [x, y], last: [x, y], before: clone({ items, crop }), moved: false, index: i };
        return;
      }
      if (tool === 'select') { selectItem(-1); return; }
      editText(x, y);
      return;
    }
    canvas.setPointerCapture(e.pointerId);
    if (tool === 'brush') live = { type: 'brush', points: [[x, y]], color, width: strokeWidth(), shadow };
    else if (tool === 'highlight') live = { type: 'highlight', points: [[x, y]], color, width: strokeWidth() * 4.5, shadow: false };
    else live = { type: tool, x0: x, y0: y, x1: x, y1: y, color, width: strokeWidth(), shadow };
    render();
  });
  canvas.addEventListener('pointermove', (e) => {
    const [x, y] = at(e);
    if (drag) {
      const [lx, ly] = drag.last;
      if (!drag.moved && Math.hypot(x - drag.start[0], y - drag.start[1]) < unit * 2) return;
      drag.moved = true;
      moveBy(items[drag.index], x - lx, y - ly);
      drag.last = [x, y];
      render();
      return;
    }
    if (!live) return;
    if (live.points) live.points.push([x, y]);
    else { live.x1 = x; live.y1 = y; }
    render();
  });
  const finish = () => {
    if (drag) {
      if (drag.moved) history.push(drag.before);
      // A plain click on text with the Text tool opens it for editing.
      else if (tool === 'text' && items[drag.index]?.type === 'text') editText(items[drag.index].x, items[drag.index].y, drag.index);
      drag = null;
      return;
    }
    if (!live) return;
    if (live.type === 'crop') {
      const r = rectOf(live);
      snapshot();
      crop = r.w > 8 && r.h > 8 ? r : null; // a click without dragging removes the crop
    } else {
      snapshot();
      items.push(live);
    }
    live = null;
    render();
  };
  canvas.addEventListener('pointerup', finish);
  canvas.addEventListener('pointercancel', finish);
  canvas.addEventListener('dblclick', (e) => {
    const [x, y] = at(e);
    const i = hit(x, y, ['text']);
    if (i >= 0) editText(items[i].x, items[i].y, i);
  });

  function undo() {
    const prev = history.pop();
    if (!prev) return;
    ({ items, crop } = prev);
    selectItem(-1);
  }

  function removeSelected() {
    if (!items[selected]) return;
    snapshot();
    items.splice(selected, 1);
    selectItem(-1);
  }

  function close(result) {
    window.removeEventListener('keydown', onKey, true);
    canvas.width = 0; // release the bitmap now rather than at garbage collection
    root.remove();
    URL.revokeObjectURL(url);
    resolve(result);
  }

  function done() {
    textBox?.blur();
    const area = crop || { x: 0, y: 0, w: W, h: H };
    const out = document.createElement('canvas');
    out.width = Math.round(area.w);
    out.height = Math.round(area.h);
    const octx = out.getContext('2d');
    // Re-render without the crop overlay and the selection, then copy the kept area.
    const saved = crop;
    crop = null;
    selected = -1;
    render();
    octx.drawImage(canvas, area.x, area.y, area.w, area.h, 0, 0, out.width, out.height);
    crop = saved;
    const editable = clone({ items, crop });
    out.toBlob((b) => { out.width = 0; close(b ? { blob: b, state: editable } : null); }, 'image/png');
  }

  const KEYS = Object.fromEntries(TOOLS.map(([id, , , k]) => [k.toLowerCase(), id]));
  function onKey(e) {
    if (e.target?.classList?.contains('ie-text')) return;
    const k = e.key.toLowerCase();
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (items[selected]) selectItem(-1); else close(null);
    } else if (e.ctrlKey && k === 'z') { e.preventDefault(); undo(); }
    else if ((e.key === 'Delete' || e.key === 'Backspace') && items[selected]) { e.preventDefault(); removeSelected(); }
    else if (e.key === 'Enter') { e.preventDefault(); done(); }
    else if (!e.ctrlKey && !e.altKey && KEYS[k]) { e.preventDefault(); setTool(KEYS[k]); }
  }
  window.addEventListener('keydown', onKey, true);

  root.querySelector('.ie-bar').addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b) return;
    if (b.dataset.tool) setTool(b.dataset.tool);
    else if (b.dataset.color) { showColor(b.dataset.color); restyle((it) => { it.color = b.dataset.color; }); }
    else if (b.dataset.act === 'shadow') {
      shadow = !shadow;
      b.classList.toggle('on', shadow);
      restyle((it) => { it.shadow = shadow && it.type !== 'highlight'; });
    } else if (b.dataset.act === 'delete') removeSelected();
    else if (b.dataset.act === 'undo') undo();
    else if (b.dataset.act === 'cancel') close(null);
    else if (b.dataset.act === 'done') done();
  });
  // Width and text size apply to what is drawn next, and to the selected item.
  root.querySelector('.ie-bar').addEventListener('change', (e) => {
    const r = e.target.closest('input[type="range"]');
    const it = items[selected];
    if (!r || !it) return;
    if (r.dataset.set === 'width' && it.type !== 'text') {
      restyle((x) => { x.width = strokeWidth() * (x.type === 'highlight' ? 4.5 : 1); });
    } else if (r.dataset.set === 'font' && it.type === 'text') {
      restyle((x) => { x.size = fontSize(); });
    }
  });
  root.querySelector('.ie-bar').addEventListener('input', (e) => {
    const r = e.target.closest('input[type="range"]');
    if (!r) return;
    const v = Number(r.value);
    r.nextElementSibling.value = v;
    if (r.dataset.set === 'width') width = v;
    else {
      font = v;
      if (textBox) textBox.style.fontSize = `${Math.max(11, (fontSize() * canvas.getBoundingClientRect().width) / W)}px`;
    }
  });

  // Reopened with earlier edits: start with Select, so they can be picked right away.
  setTool(items.length ? 'select' : 'brush');
  showColor(COLORS[0]);
  selectItem(-1);
}
