// Notification popup window. Lives only while it has notes to show:
// when the last one expires the window asks Rust to destroy it.
import { invoke, listen, esc } from './api.js';

const MAX = 3;
const box = document.getElementById('notes');

function fit() {
  requestAnimationFrame(() => invoke('popup_resize', { height: box.scrollHeight || 76 }));
}

function remove(el) {
  el.remove();
  if (!box.children.length) invoke('popup_closed');
  else fit();
}

function add(n) {
  const el = document.createElement('div');
  el.className = 'note';
  el.style.setProperty('--c', n.color);
  el.innerHTML = `<div class="meta">${esc(n.service)} &middot; ${esc(n.channel)}</div>
    <div class="who">${esc(n.author)}</div><div class="text">${esc(n.preview)}</div>`;
  let left = Math.max(2, n.seconds || 6) * 1000;
  let started = 0;
  let timer = 0;
  const arm = () => { started = Date.now(); timer = setTimeout(() => remove(el), left); };
  el.addEventListener('mouseenter', () => { clearTimeout(timer); left = Math.max(1500, left - (Date.now() - started)); });
  el.addEventListener('mouseleave', arm);
  el.addEventListener('click', () => {
    clearTimeout(timer);
    invoke('popup_open', { account_id: n.account_id, channel_id: n.channel_id });
    remove(el);
  });
  box.prepend(el);
  while (box.children.length > MAX) box.lastElementChild.remove();
  arm();
  fit();
}

await listen('popup-note', add);
(await invoke('popup_ready')).forEach(add);
