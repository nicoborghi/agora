// Main window. Plain DOM, no framework: the only state is `S`, and every
// render function rebuilds one region from it. Only the open conversation's
// messages live in memory (capped), everything else stays in the Rust archive.
import { invoke, listen, esc, initials, toast, icon } from './api.js';
import { renderMarkdown, emoji, EMOJI, COMMON_EMOJI, QUICK_REACTIONS, extractUrls } from './markdown.js';
import { editImage } from './editor.js';
import { openSettings, settingsOpen, closeSettings, settingsAccountsChanged } from './settings.js';

const PAGE = 50;
const MAX_LOADED = 400;
const TYPING_MS = 6000;

export const S = {
  accounts: [],
  conn: {},
  channels: new Map(),
  settings: null,
  rail: 'all',
  open: null, // { a, c }
  messages: [],
  hasOlder: true,
  loadingOlder: false,
  openToken: 0,
  panel: null, // { type: 'thread', a, c, root, messages, back } | { type: 'gallery' } | { type: 'search', ... }
  me: {}, // account id -> { user, presence } | null while loading
  typing: new Map(), // key -> Map(name -> expiry)
  editing: null, // { a, c, id }
  previewOn: false,
  newSince: null, // last-read time when the open conversation was opened with news
  stick: true, // the message list follows the bottom
};

const key = (a, c) => `${a}\u0001${c}`;
const $ = (sel) => document.querySelector(sel);
const account = (id) => S.accounts.find((a) => a.id === id);
const enabledAccounts = () => S.accounts.filter((a) => a.enabled);
const isDirect = (c) => c.kind === 'dm' || c.kind === 'group';
const isOpen = (a, c) => S.open && S.open.a === a && S.open.c === c;
// The workspace selected in the left bar, or null for Home and All.
const railAccount = () => (S.rail === 'all' || S.rail === 'home' ? null : S.rail);
const KIND_NAMES = { slack: 'Slack', mattermost: 'Mattermost', rocketchat: 'Rocket.Chat', demo: 'Demo' };
export const kindName = (k) => KIND_NAMES[k] || k;

// Keys saved under the app's earlier name (armos.) are still read until saved again.
function load(k) { try { return JSON.parse(localStorage.getItem(`agora.${k}`) ?? localStorage.getItem(`armos.${k}`)); } catch { return null; } }
function save(k, v) { try { localStorage.setItem(`agora.${k}`, JSON.stringify(v)); } catch { /* storage unavailable */ } }

// ------------------------------------------------------------ batched rendering

const RENDER = {};
const dirty = new Set();
let frame = 0;
function schedule(...parts) {
  parts.forEach((p) => dirty.add(p));
  if (frame) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    const todo = [...dirty];
    dirty.clear();
    todo.forEach((p) => RENDER[p]());
  });
}
export function refreshAll() { schedule('rail', 'sidebar', 'header', 'messages', 'panel', 'status', 'me', 'home'); }

// ------------------------------------------------------------ start-up

async function init() {
  restoreLayout();
  const b = await invoke('bootstrap');
  S.accounts = b.accounts;
  S.conn = b.connections;
  S.settings = b.settings;
  b.channels.forEach((c) => S.channels.set(key(c.account_id, c.id), c));
  S.rail = load('rail') || 'home';
  if (railAccount() && !account(S.rail)) S.rail = 'home';
  applyTheme();
  await listen('agora', onEvent);
  await listen('agora-sound', playSound);
  await listen('agora-open', (p) => openChannel(p.account_id, p.channel_id));
  bindUi();
  refreshAll();
  const last = load('last-open');
  if (last && S.channels.has(key(last.a, last.c))) openChannel(last.a, last.c);
}

export function applyTheme() {
  const t = S.settings?.theme;
  if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
  if (S.settings) {
    // Presets that no longer exist fall back to their nearest one.
    if (S.settings.sidebar_size === 'larger') S.settings.sidebar_size = 'large';
    if (S.settings.sidebar_width === 'wider') S.settings.sidebar_width = 'wide';
  }
  document.documentElement.dataset.sbWidth = S.settings?.sidebar_width || 'compact';
  if (S.settings?.font === 'lato') S.settings.font = 'system'; // Lato is the default now
  const font = FONTS.find(([id]) => id === S.settings?.font) || FONTS[0];
  document.documentElement.style.setProperty('--font', font[2]);
  const accent = ACCENTS.find(([id]) => id === S.settings?.accent) || ACCENTS[0];
  document.documentElement.style.setProperty('--accent-light', accent[2]);
  document.documentElement.style.setProperty('--accent-dark', accent[3]);
  // Text size zooms the whole window, so every part scales together.
  const zoom = TEXT_SIZES[S.settings?.sidebar_size] || TEXT_SIZES.normal;
  if (zoom !== appliedZoom) {
    appliedZoom = zoom;
    invoke('set_zoom', { scale: zoom }).catch(() => {});
  }
  paintTitlebar();
}

/** The Windows title bar takes the colour of the rail and sidebar below it. */
function paintTitlebar() {
  const css = getComputedStyle(document.documentElement);
  const v = (name) => css.getPropertyValue(name).trim();
  const bar = { dark: css.colorScheme === 'dark', caption: v('--bg-2'), text: v('--fg'), border: v('--line') };
  const sig = JSON.stringify(bar);
  if (sig === paintedBar) return;
  paintedBar = sig;
  invoke('set_titlebar', bar).catch(() => {});
}
let paintedBar = '';
// "As system" follows Windows switching between light and dark while Agora runs.
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', paintTitlebar);

// Every size is larger than the WebView's own 100%: Windows UI text at 13px reads small.
export const TEXT_SIZES = { small: 1.1, normal: 1.2, large: 1.3 };
let appliedZoom = 1;

/** A sidebar width preset replaces a width set by dragging the divider. */
export function resetSidebarWidth() {
  document.documentElement.style.removeProperty('--sb-w');
  const l = load('layout') || {};
  delete l.sb;
  save('layout', l);
}

// Fonts that ship with Windows (already loaded by the system and shared with every app, so they
// cost no memory), plus Lato, bundled as two small files that load only when it is chosen.
// "system" is the default id (older settings saved it for Segoe UI, now Lato).
export const FONTS = [
  // Bundled with Agora (two small files); a Windows-installed Lato wins.
  ['system', 'Lato (default)', 'Lato, "Segoe UI", sans-serif'],
  ['segoe', 'Segoe UI', '"Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif'],
  ['calibri', 'Calibri', 'Calibri, "Segoe UI", sans-serif'],
  ['arial', 'Arial', 'Arial, "Segoe UI", sans-serif'],
  ['verdana', 'Verdana (wide, very legible)', 'Verdana, "Segoe UI", sans-serif'],
  ['georgia', 'Georgia (serif)', 'Georgia, "Times New Roman", serif'],
];

// Accent colours: [id, label, light theme, dark theme].
export const ACCENTS = [
  ['blue', 'Blue', '#2f6fd6', '#5b8def'],
  ['indigo', 'Indigo', '#5048c8', '#8a84f2'],
  ['violet', 'Violet', '#7a45c9', '#a986f0'],
  ['teal', 'Teal', '#0e8282', '#3dbdbd'],
  ['green', 'Green', '#2c8450', '#5cbd82'],
  ['amber', 'Amber', '#b06a0a', '#e3a33f'],
  ['rose', 'Rose', '#c33561', '#ec7097'],
  ['graphite', 'Graphite', '#4a505c', '#a7abb5'],
];

// ------------------------------------------------------------ events from Rust

function onEvent(ev) {
  switch (ev.type) {
    case 'connection':
      S.conn[ev.account_id] = ev.state;
      schedule('rail', 'status', 'home');
      break;
    case 'message':
      onMessage(ev.message);
      break;
    case 'message_deleted': {
      for (const list of [S.messages, S.panel?.messages || []]) {
        const m = list.find((x) => x.account_id === ev.account_id && x.channel_id === ev.channel_id && x.id === ev.id);
        if (m) Object.assign(m, { deleted: true, text: '', reactions: [], attachments: [], links: [] });
      }
      schedule('messages', 'panel');
      break;
    }
    case 'channel':
      S.channels.set(key(ev.channel.account_id, ev.channel.id), ev.channel);
      schedule('sidebar', 'rail', 'home');
      if (isOpen(ev.channel.account_id, ev.channel.id)) schedule('header');
      break;
    case 'channels_reloaded':
      reloadChannels(ev.account_id);
      delete S.me[ev.account_id]; // profile may have changed on (re)connect
      schedule('me');
      break;
    case 'history':
      if (isOpen(ev.account_id, ev.channel_id)) reloadOpen();
      break;
    case 'typing': {
      const k = key(ev.account_id, ev.channel_id);
      if (!S.typing.has(k)) S.typing.set(k, new Map());
      S.typing.get(k).set(ev.user_name, Date.now() + TYPING_MS);
      schedule('typing');
      setTimeout(() => schedule('typing'), TYPING_MS + 50);
      break;
    }
    case 'presence':
      for (const c of S.channels.values()) {
        if (c.account_id !== ev.account_id) continue;
        for (const p of c.peers) if (p.id === ev.user_id) p.presence = ev.presence;
      }
      if (S.me[ev.account_id]?.user.id === ev.user_id) { S.me[ev.account_id].presence = ev.presence; schedule('me', 'panel'); }
      schedule('sidebar');
      break;
    case 'accounts':
      reloadAccounts();
      break;
  }
}

function onMessage(m) {
  S.typing.get(key(m.account_id, m.channel_id))?.delete(m.author_name);
  schedule('typing');
  if (isOpen(m.account_id, m.channel_id) && !m.thread_root) {
    const i = S.messages.findIndex((x) => x.id === m.id);
    if (i >= 0) S.messages[i] = m;
    else if (!S.messages.length || m.ts >= S.messages[0].ts || !S.hasOlder) {
      S.messages.push(m);
      S.messages.sort((x, y) => x.ts - y.ts);
      trimLoaded();
    }
    schedule('messages');
  }
  const p = S.panel;
  if (p?.type === 'thread' && p.a === m.account_id && p.c === m.channel_id && (m.id === p.root || m.thread_root === p.root)) {
    const i = p.messages.findIndex((x) => x.id === m.id);
    if (i >= 0) p.messages[i] = m;
    else p.messages.push(m);
    schedule('panel');
  }
}

/** Keeps the in-memory window bounded while the user sits at the bottom of a busy channel. */
function trimLoaded() {
  const box = $('#messages');
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  if (S.messages.length > MAX_LOADED && atBottom) {
    S.messages.splice(0, S.messages.length - (MAX_LOADED - 100));
    S.hasOlder = true;
  }
}

async function reloadChannels(accountId) {
  const list = await invoke('list_channels', { account_id: accountId });
  for (const [k, c] of S.channels) if (c.account_id === accountId) S.channels.delete(k);
  list.forEach((c) => S.channels.set(key(c.account_id, c.id), c));
  schedule('sidebar', 'rail', 'header', 'home');
}

async function reloadAccounts() {
  S.accounts = await invoke('list_accounts');
  const ids = new Set(S.accounts.map((a) => a.id));
  for (const [k, c] of S.channels) if (!ids.has(c.account_id)) S.channels.delete(k);
  if (railAccount() && !account(S.rail)?.enabled) S.rail = 'home';
  if (S.open && !account(S.open.a)?.enabled) closeChannel();
  refreshAll();
  settingsAccountsChanged();
}

async function reloadOpen() {
  const { a, c } = S.open;
  const msgs = await invoke('get_messages', { account_id: a, channel_id: c, limit: Math.min(200, Math.max(PAGE, S.messages.length)) });
  if (!isOpen(a, c)) return;
  S.messages = msgs;
  schedule('messages');
}

// ------------------------------------------------------------ rail

RENDER.rail = function renderRail() {
  // Same rule as the tray: conversations with notifications off (Silent) add no dot, unless
  // someone mentioned you and mentions override levels.
  const unread = new Set();
  const mentionsBreakThrough = S.settings?.notifications?.mentions_override !== false;
  for (const c of S.channels.values()) {
    const news = c.unread > 0 || c.mentions > 0;
    if (news && (!c.muted || (c.mentions > 0 && mentionsBreakThrough))) unread.add(c.account_id);
  }
  const inSettings = settingsOpen();
  const on = (id) => (!inSettings && S.rail === id ? 'active' : '');
  let html = `<div class="rail-item home ${on('home')}" data-id="home" title="Home (Ctrl+0)">${icon('grid')}<span class="num">0</span></div>
    <div class="rail-item all ${on('all')}" data-id="all" title="All conversations">${icon('chats')}</div><div class="rail-sep"></div>`;
  enabledAccounts().forEach((a, i) => {
    const st = S.conn[a.id];
    const cls = st?.state === 'auth_expired' ? 'expired' : st?.state === 'offline' ? 'offline' : '';
    const state = st?.state === 'auth_expired' ? ' - signed out' : st?.state === 'offline' ? ' - offline' : '';
    const title = `${a.name} (${kindName(a.kind)})${i < 9 ? ` - Ctrl+${i + 1}` : ''}${state}`;
    html += `<div class="rail-item ${cls} ${a.icon_image ? 'has-logo' : ''} ${on(a.id)}" data-id="${esc(a.id)}" draggable="true"
      style="--c:${esc(a.color)}" title="${esc(title)}">${accountIcon(a)}
      ${unread.has(a.id) ? '<span class="dot"></span>' : ''}${i < 9 ? `<span class="num">${i + 1}</span>` : ''}</div>`;
  });
  html += `<div class="rail-spacer"></div><div class="rail-item gear ${inSettings ? 'active' : ''}" data-act="settings"
    title="${inSettings ? 'Close settings (Esc)' : 'Settings (Ctrl+,)'}">${icon('gear')}</div>`;
  $('#rail').innerHTML = html;
};

/** Settings open and close from the gear, which stays highlighted while they are open. */
export function toggleSettings(section) {
  if (settingsOpen() && !section) closeSettings();
  else openSettings(section);
  schedule('rail');
}

// ------------------------------------------------------------ workspace menu (right click in the bar)

const SWATCHES = ['#2f6fd6', '#7a4fc4', '#d23c41', '#3b8f5a', '#d97a1e', '#c2408f', '#1f9bb0', '#6b7280'];
const LEVEL_CHOICES = [['', 'Default'], ['alert', 'Alert'], ['dot', 'Dot'], ['silent', 'Silent']];
const LEVEL_NAMES = { alert: 'Alert', dot: 'Dot only', silent: 'Silent' };

function showWorkspaceMenu(anchor, a) {
  const n = S.settings.notifications;
  const own = n.accounts?.[a.id] || {};
  const st = S.conn[a.id]?.state;
  const status = { online: 'Connected', offline: 'Offline, retrying', auth_expired: 'Signed out', connecting: 'Connecting' }[st] || 'Not connected';
  const seg = (k, label, global) => `<div class="seg-row"><span>${label}</span><div class="seg" data-k="${k}">
    ${LEVEL_CHOICES.map(([v, l]) => `<button data-v="${v}" class="${(own[`${k}_level`] || '') === v ? 'on' : ''}"
      title="${v ? LEVEL_NAMES[v] : `Same as the global setting: ${LEVEL_NAMES[global]}`}">${l}</button>`).join('')}</div></div>`;
  showPopover(anchor, `<div class="menu ws-menu">
      <div class="menu-head"><span class="rail-item ${a.icon_image ? 'has-logo' : ''}" style="--c:${esc(a.color)}">${accountIcon(a)}</span>
        <div><b>${esc(a.name)}</b><div class="muted">${esc(kindName(a.kind))} · ${status}</div></div></div>
      <button data-act="reload">${icon('refresh')}Reload</button>
      <div class="menu-label">Notifications in this workspace</div>
      ${seg('dm', 'Direct messages', n.dm_level)}${seg('channel', 'Channels', n.channel_level)}
      <div class="menu-label">Alert: popup and tray dot · Dot: tray dot only · Silent: nothing</div>
      <div class="menu-label">Color in the bar and the tray</div>
      <div class="swatches">${SWATCHES.map((c) => `<button data-color="${c}" style="--c:${c}" class="${a.color === c ? 'on' : ''}" title="${c}"></button>`).join('')}
        <label class="custom" title="Other color"><input type="color" value="${esc(a.color)}"></label></div>
      <hr><button data-act="ws-settings">${icon('gear')}Workspace settings...</button></div>`, (pop) => {
    const saveLevels = async () => {
      n.accounts = n.accounts || {};
      const levels = {};
      pop.querySelectorAll('.seg').forEach((s) => { levels[`${s.dataset.k}_level`] = s.querySelector('.on')?.dataset.v || null; });
      if (!levels.dm_level && !levels.channel_level) delete n.accounts[a.id];
      else n.accounts[a.id] = levels;
      try { await invoke('save_settings', { settings: S.settings }); } catch (e) { toast(String(e)); }
    };
    const setColor = async (color) => {
      try { await invoke('update_account', { account: { ...a, color } }); a.color = color; } catch (e) { toast(String(e)); }
      pop.querySelectorAll('.swatches button').forEach((b) => b.classList.toggle('on', b.dataset.color === color));
    };
    pop.addEventListener('click', async (e) => {
      const b = e.target.closest('button');
      if (!b) return;
      if (b.dataset.v !== undefined) {
        b.parentElement.querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === b));
        saveLevels();
      } else if (b.dataset.color) {
        setColor(b.dataset.color);
      } else if (b.dataset.act === 'reload') {
        closePopover();
        invoke('reconnect_account', { account_id: a.id }).then(() => toast(`${a.name}: reconnecting`)).catch((x) => toast(String(x)));
      } else if (b.dataset.act === 'ws-settings') {
        closePopover();
        toggleSettings('accounts');
      }
    });
    pop.querySelector('input[type="color"]').addEventListener('change', (e) => setColor(e.target.value));
  });
}

/** The workspace logo when there is one (from the service or chosen), else the initials. */
export const accountIcon = (a) => (a.icon_image
  ? `<img class="logo" src="${esc(a.icon_image)}" alt="">`
  : `<span class="ico">${a.icon ? esc(a.icon.slice(0, 2)) : initials(a.name)}</span>`);

function selectRail(id) {
  if (settingsOpen()) closeSettings();
  // The right-hand panel (thread, profile, members...) belongs to what was shown before.
  if (id !== S.rail && S.panel) { S.panel = null; schedule('panel', 'header'); }
  S.rail = id;
  save('rail', id);
  if (id === 'home') closeChannel();
  // A workspace never shows another one's chat: reopen its last conversation, else its newest.
  if (railAccount() && S.open?.a !== id) {
    const mine = [...S.channels.values()].filter((c) => c.account_id === id);
    const last = lastByAccount[id];
    const ch = (last && S.channels.get(key(id, last))) || mine.sort((x, y) => y.last_activity - x.last_activity)[0];
    if (ch) openChannel(id, ch.id); else closeChannel();
  }
  schedule('rail', 'sidebar', 'me', 'home');
}
// The conversation last open in each workspace (a per-viewer convenience, so localStorage).
const lastByAccount = load('last-by-account') || {};

// ------------------------------------------------------------ sidebar

function glyph(c) {
  if (!c) return '';
  if (c.kind === 'public') return '<span class="glyph">#</span>';
  if (c.kind === 'private') return `<span class="glyph" title="Private channel">${icon('lock')}</span>`;
  // People get their profile picture (initials until it loads, or when there is none).
  const face = (p, fallback) => (p?.avatar
    ? `<img src="${esc(p.avatar)}" alt="" loading="lazy" data-fallback="${initials(p?.name || fallback)}">`
    : initials(p?.name || fallback));
  if (c.kind === 'dm') {
    const p = c.peers[0];
    return `<span class="av sm">${face(p, c.display_name)}<i class="presence ${esc(p?.presence || '')}"></i></span>`;
  }
  const [p1, p2] = c.peers;
  return `<span class="av-stack"><span class="av">${face(p1, '?')}</span><span class="av">${face(p2, '+')}</span></span>`;
}

/** The workspace's logo in miniature (All view), instead of a plain colored dot. */
function serviceMark(acc) {
  if (!acc) return '';
  return acc.icon_image
    ? `<img class="svc-logo" src="${esc(acc.icon_image)}" alt="" title="${esc(acc.name)}">`
    : `<span class="svc" style="--c:${esc(acc.color)}" title="${esc(acc.name)}"></span>`;
}

const isUnread = (c) => c.unread > 0 || c.mentions > 0;
const count = (n) => (n > 99 ? '99+' : String(n));

/** Like Mattermost: a filled pill for mentions (and every DM message), bold name only for other unread. */
/** Text color that stays readable on a given background color (black on light colors). */
export function inkFor(hex) {
  const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex || '');
  if (!m) return '#fff';
  const [r, g, b] = m.slice(1).map((h) => parseInt(h, 16) / 255);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.6 ? '#111' : '#fff';
}

/** Badges use the workspace color, the same as its dot in the bar and on the taskbar. */
function badgeHtml(c) {
  const color = account(c.account_id)?.color || '';
  const style = color ? ` style="--c:${esc(color)};--cf:${inkFor(color)}"` : '';
  // Notifications off: only a mention can still show (when mentions override levels).
  const mentionsShow = !c.muted || S.settings?.notifications?.mentions_override !== false;
  if (c.mentions > 0 && mentionsShow) return `<span class="badge mention"${style} title="${c.mentions} mention${c.mentions === 1 ? '' : 's'}">${count(c.mentions)}</span>`;
  if (isDirect(c) && c.unread > 0 && !c.muted) return `<span class="badge mention"${style}>${count(c.unread)}</span>`;
  return '';
}

function rowHtml(c, showService) {
  const acc = account(c.account_id);
  return `<div class="row ${isOpen(c.account_id, c.id) ? 'active' : ''} ${isUnread(c) ? 'unread' : ''} ${c.muted ? 'muted' : ''}" data-a="${esc(c.account_id)}" data-c="${esc(c.id)}" draggable="true"
    title="${esc([c.group, acc?.name, c.muted ? 'notifications off' : ''].filter(Boolean).join(' · '))}">${glyph(c)}<span class="name">${esc(c.display_name)}</span>
    ${c.muted ? `<span class="muted-bell" title="Notifications off">${icon('bellOff')}</span>` : ''}
    ${showService ? serviceMark(acc) : ''}${badgeHtml(c)}</div>`;
}

function visibleChannels() {
  const enabled = new Set(enabledAccounts().map((a) => a.id));
  return [...S.channels.values()]
    .filter((c) => enabled.has(c.account_id) && (!railAccount() || c.account_id === S.rail))
    .sort((a, b) => b.last_activity - a.last_activity);
}

// Collapsed sections (per viewer convenience, so localStorage is enough).
const collapsed = new Set(load('collapsed') || []);
const byName = (x, y) => x.localeCompare(y, undefined, { sensitivity: 'base' });

/** A collapsible section. Collapsed, it still shows unread and open conversations, like Mattermost. */
function sectionHtml(id, title, rows, row, { group = false, body = null } = {}) {
  if (!rows.length && !group) return '';
  const shut = collapsed.has(id);
  const shown = shut ? rows.filter((c) => isUnread(c) || isOpen(c.account_id, c.id)) : rows;
  const hidden = rows.length - shown.length;
  const content = shut ? shown.map(row).join('') : body ?? rows.map(row).join('');
  const hint = group && !rows.length && !shut ? '<div class="empty-group">Drag a conversation here</div>' : '';
  return `<div class="section ${shut ? 'shut' : ''} ${group ? 'local' : ''}" data-sec="${esc(id)}">
      <span class="chev">${icon('chevron')}</span><span class="t">${title}</span>${shut && hidden ? `<span class="n">${hidden}</span>` : ''}</div>${content}${hint}`;
}

/** Channels of one team (or of every team-less account): each of the user's own sections
 *  (Slack sidebar sections, Mattermost categories) is a full section like any other, then the
 *  channels in none of them under the team's name. `tag` marks which team a section belongs to
 *  when several are shown. */
function teamSections(id, title, chans, row, tag = '') {
  const cats = [...new Set(chans.map((c) => c.category).filter(Boolean))].sort(byName);
  let html = '';
  for (const cat of cats) {
    html += sectionHtml(`${id}\u0002${cat}`, esc(cat) + tag, chans.filter((c) => c.category === cat), row);
  }
  return html + sectionHtml(id, title, chans.filter((c) => !c.category), row);
}

RENDER.sidebar = function renderSidebar() {
  const list = visibleChannels();
  const showService = !railAccount() && enabledAccounts().length > 1;
  const row = (c) => rowHtml(c, showService);
  const groups = S.settings?.groups || [];
  const groupIds = new Set(groups.map((g) => g.id));
  // Favorites first (just the conversation name), then Agora groups, then the service's own structure.
  const fav = list.filter((c) => c.starred);
  const rest = list.filter((c) => !c.starred);
  let html = sectionHtml('fav', 'Favorites', fav, row);
  for (const g of groups) html += sectionHtml(`g:${g.id}`, esc(g.name), rest.filter((c) => c.local_group === g.id), row, { group: true });
  const free = rest.filter((c) => !groupIds.has(c.local_group));
  const chans = free.filter((c) => !isDirect(c));
  // One section per team (Mattermost accounts with several teams); "Channels" for the rest.
  const teams = new Map();
  for (const c of chans) {
    const k = c.group ? `${c.account_id}\u0001${c.group}` : '';
    if (!teams.has(k)) teams.set(k, []);
    teams.get(k).push(c);
  }
  const named = [...teams.keys()].filter(Boolean).sort((x, y) => byName(x.split('\u0001')[1], y.split('\u0001')[1]));
  for (const k of named) {
    const [acc, name] = k.split('\u0001');
    const dot = showService ? ` <span class="svc" style="--c:${esc(account(acc)?.color || '')}"></span>` : '';
    // With several teams, a section of the user's own says which team it is in.
    const tag = named.length > 1 || teams.has('') ? ` <span class="sec-tag">${esc(name)}</span>` : '';
    html += teamSections(`t:${k}`, esc(name) + dot, teams.get(k), row, tag + dot);
  }
  if (teams.has('')) html += teamSections('chans', 'Channels', teams.get(''), row);
  html += sectionHtml('dm', 'Direct messages', free.filter(isDirect), row);
  if (!S.accounts.length) html = `<p class="section">No accounts yet. Add one in <a href="#" data-act="settings">Settings</a>.</p>`;
  $('#channel-list').innerHTML = html;
  $('#sidebar-title').textContent = S.rail === 'home' ? 'Home' : S.rail === 'all' ? 'All conversations' : account(S.rail)?.name || '';
};

function toggleSection(id) {
  if (collapsed.has(id)) collapsed.delete(id); else collapsed.add(id);
  save('collapsed', [...collapsed]);
  schedule('sidebar');
}

RENDER.status = function renderStatus() {
  const items = [];
  let offline = false;
  for (const a of enabledAccounts()) {
    const s = S.conn[a.id];
    if (s?.state === 'offline') {
      offline = true;
      items.push(`${esc(a.name)} offline, retrying in ${s.retry_in}s`);
    } else if (s?.state === 'auth_expired') {
      items.push(`${esc(a.name)}: ${esc(s.reason)} <button data-act="relogin" data-id="${esc(a.id)}">Sign in</button>`);
    }
  }
  const el = $('#status-line');
  el.hidden = !items.length;
  el.innerHTML = items.join('<br>') + (offline ? ` <button data-act="reconnect">Retry now</button>` : '');
};

// ------------------------------------------------------------ status bar (you)

/** The account the bar shows: the selected workspace, else the open conversation's, else the first. */
function meAccount() {
  if (railAccount()) return account(S.rail);
  if (S.open) return account(S.open.a);
  return enabledAccounts()[0];
}

const PRESENCE = [['online', 'Online'], ['away', 'Away'], ['dnd', 'Do not disturb'], ['offline', 'Offline']];
const presenceLabel = (p) => PRESENCE.find(([k]) => k === p)?.[1] || 'Status unknown';

function avatarHtml(user, fallback, presence, cls = 'md') {
  const pic = user?.avatar_url ? `<img src="${esc(user.avatar_url)}" alt="" data-fallback="${initials(fallback)}">` : initials(fallback);
  return `<span class="av ${cls}">${pic}${presence !== undefined ? `<i class="presence ${esc(presence || '')}"></i>` : ''}</span>`;
}

RENDER.me = function renderMe() {
  const el = $('#me-bar');
  const a = meAccount();
  if (!a) { el.hidden = true; return; }
  if (!(a.id in S.me)) {
    S.me[a.id] = null;
    invoke('get_me', { account_id: a.id }).then((v) => { S.me[a.id] = v; schedule('me'); }).catch(() => {});
  }
  const me = S.me[a.id];
  const u = me?.user;
  const name = u ? u.display_name || u.name : a.name;
  el.hidden = false;
  el.innerHTML = `<button class="me" data-act="me-menu" title="${esc(presenceLabel(me?.presence))} - status and account">
      ${avatarHtml(u, name, me?.presence || '')}
      <span class="me-text"><b>${esc(name)}</b><span>${u ? `@${esc(u.name)} · ` : ''}${esc(a.name)}</span></span>
      </button>`;
};

/** You on this workspace: status, and links for what Agora does not manage. Docked on the right. */
function showMeMenu() {
  const a = meAccount();
  if (!a) return;
  if (S.panel?.type === 'account' && S.panel.a === a.id) { S.panel = null; schedule('panel', 'header'); return; }
  const p = { type: 'account', a: a.id, profile: null };
  S.panel = p;
  schedule('panel', 'header');
  // Your own profile as the workspace shows it to others (title, time zone, custom status...).
  const me = a.self_user_id || S.me[a.id]?.user?.id;
  if (!me) return;
  invoke('get_profile', { account_id: a.id, user_id: me }).then((profile) => {
    p.profile = profile;
    if (S.panel === p) schedule('panel');
  }).catch(() => {});
}

function accountPanelHtml(a) {
  const me = S.me[a.id];
  const canSetStatus = a.kind !== 'demo' && S.conn[a.id]?.state === 'online';
  const web = /^https?:\/\//.test(a.server_url);
  return `<div class="side-head">${avatarHtml(me?.user, me?.user?.display_name || a.name, me?.presence || '', 'lg')}
      <div><b>${esc(me?.user?.display_name || me?.user?.name || a.name)}</b>
      <div class="muted">${me?.user ? `@${esc(me.user.name)}` : ''}</div>
      <div class="muted">${esc(a.name)} · ${esc(kindName(a.kind))}</div></div></div>
    ${S.panel?.type === 'account' && S.panel.profile ? profileFactsHtml(S.panel.profile) : ''}
    ${canSetStatus ? `<div class="side-label">Status</div><div class="menu">${PRESENCE.map(([k, l]) =>
      `<button data-presence="${k}" class="${me?.presence === k ? 'on' : ''}"><i class="presence ${k}"></i>${l}</button>`).join('')}</div>` : ''}
    <div class="side-label">More</div><div class="menu">
      ${web ? `<button data-act="open-web">${icon('external')}Open ${esc(kindName(a.kind))} (profile, preferences)</button>` : ''}
      <button data-act="acc-settings">${icon('gear')}Account settings in Agora</button></div>`;
}

export async function relogin(id) {
  const a = account(id);
  const command = { mattermost: 'mattermost_sign_in', rocketchat: 'rocketchat_sign_in', slack: 'slack_sign_in' }[a?.kind];
  if (!command) { toast('Sign-in is not available for this account.'); return; }
  toast('Finish signing in in the window that opened.', 4000);
  try {
    await invoke(command, { server: a.server_url, token: null, user_id: null, cookie: null, account_id: id });
    toast(`${a.name}: signed in`);
  } catch (e) {
    toast(String(e), 5000);
  }
}

// ------------------------------------------------------------ conversation

async function openChannel(a, c) {
  const ch = S.channels.get(key(a, c));
  if (!ch) return;
  if (S.rail !== 'all' && S.rail !== a) S.rail = a;
  if (!isOpen(a, c)) { clearPending(); closeAutocomplete(); }
  S.open = { a, c };
  S.messages = [];
  S.hasOlder = true;
  S.editing = null;
  if (S.panel?.type === 'thread') S.panel = null;
  save('last-open', { a, c });
  lastByAccount[a] = c;
  save('last-by-account', lastByAccount);
  $('#empty').hidden = true;
  $('#composer').hidden = false;
  hideInfo();
  const input = $('#input');
  input.placeholder = `Message ${isDirect(ch) ? ch.display_name : '#' + ch.display_name}`;
  // With news, open at the first unread message (everything after it is highlighted);
  // otherwise at the newest. Read before open_channel, which marks the conversation read.
  S.newSince = isUnread(ch) && ch.last_read > 0 ? ch.last_read : null;
  schedule('header', 'sidebar', 'rail', 'panel', 'typing', 'messages', 'me', 'home');
  invoke('open_channel', { account_id: a, channel_id: c });
  const token = ++S.openToken;
  try {
    const msgs = await invoke('get_messages', { account_id: a, channel_id: c, limit: PAGE });
    if (token !== S.openToken) return;
    S.messages = msgs;
    S.hasOlder = msgs.length >= PAGE;
    renderMessages('open');
  } catch (e) {
    toast(String(e));
  }
  input.focus();
}

function closeChannel() {
  S.open = null;
  S.messages = [];
  S.panel = null;
  invoke('open_channel', {});
  $('#empty').hidden = S.rail === 'home';
  $('#composer').hidden = true;
  refreshAll();
}

// ------------------------------------------------------------ home

/** Plain one-line text of a Markdown message. */
const plain = (md, max = 140) => {
  const t = String(md || '').replace(/```[\s\S]*?```/g, '[code]').replace(/[*_~`>#]/g, '').replace(/\[([^\]]+)\]\([^)]+\)/g, '$1').replace(/\s+/g, ' ').trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
};

let homeTimer = 0;
RENDER.home = function renderHome() {
  const show = S.rail === 'home' && !S.open;
  // Home is its own page: the cards replace the conversation list.
  document.body.classList.toggle('home-mode', show);
  $('#home').hidden = !show;
  if (!S.open) $('#empty').hidden = show;
  if (!show) return;
  // Bursts of events (catch-up after connecting) reload the view once.
  clearTimeout(homeTimer);
  homeTimer = setTimeout(loadHome, 200);
};

async function loadHome() {
  let items = [];
  try { items = await invoke('home'); } catch (e) { toast(String(e)); }
  if (S.rail !== 'home' || S.open) return;
  const accounts = enabledAccounts();
  const totals = { mentions: 0, unread: 0 };
  const stats = new Map(accounts.map((a) => [a.id, { mentions: 0, unread: 0 }]));
  for (const c of S.channels.values()) {
    const s = stats.get(c.account_id);
    if (!s || !isUnread(c)) continue;
    s.mentions += c.mentions;
    s.unread += 1;
    totals.mentions += c.mentions;
    totals.unread += 1;
  }
  const hour = new Date().getHours();
  const hello = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening';
  const summary = !accounts.length ? 'Add a workspace in Settings to get started.'
    : !totals.unread ? 'You are all caught up.'
    : `${totals.unread} ${totals.unread === 1 ? 'conversation' : 'conversations'} with news${totals.mentions ? `, ${totals.mentions} ${totals.mentions === 1 ? 'mention' : 'mentions'}` : ''}.`;
  const STATUS = { online: ['ok', 'Connected'], offline: ['warn', 'Offline'], auth_expired: ['bad', 'Signed out'], connecting: ['warn', 'Connecting'] };
  const card = (a) => {
    const s = stats.get(a.id);
    const [cls, label] = STATUS[S.conn[a.id]?.state] || ['warn', 'Not connected'];
    const mine = items.filter((i) => i.channel.account_id === a.id);
    // Conversations as small threads: name, then the latest lines as speech bubbles.
    const body = mine.length ? mine.map((i) => `<button class="home-item" data-a="${esc(a.id)}" data-c="${esc(i.channel.id)}">
        <div class="hi-where">${glyph(i.channel)}<b>${esc(i.channel.display_name)}</b>${i.channel.group ? `<span class="muted">${esc(i.channel.group)}</span>` : ''}${badgeHtml(i.channel)}</div>
        ${i.messages.slice(-2).map((m) => `<div class="hi-msg">${messageAvatar(m, 'xs')}<div class="bubble"><span class="who">${esc(m.author_name)}</span>
          <span class="txt">${esc(plain(m.text) || (m.attachments?.length ? 'sent a file' : ''))}</span></div><time>${fmtTime(m.ts)}</time></div>`).join('')}
      </button>`).join('')
      : `<div class="all-read">${s.unread ? 'News only in conversations kept quiet' : 'Nothing new here'}</div>`;
    const ink = inkFor(a.color);
    return `<section class="ws-card ${mine.length ? '' : 'calm'}" style="--c:${esc(a.color)};--cf:${ink}">
      <header data-rail="${esc(a.id)}" title="Open ${esc(a.name)}">
        <span class="ws-mark">${accountIcon(a)}</span>
        <div class="ws-title"><b>${esc(a.name)}</b><span><i class="conn ${cls}"></i>${esc(kindName(a.kind))} · ${label}</span></div>
        <div class="ws-counts">
          <span class="count ${s.mentions ? 'hot' : ''}"><b>${count(s.mentions)}</b><small>mentions</small></span>
          <span class="count"><b>${count(s.unread)}</b><small>with news</small></span>
        </div>
      </header>
      <div class="ws-body">${body}</div>
      </section>`;
  };
  $('#home').innerHTML = `<div class="home-head"><h1>${hello}</h1><p class="muted">${esc(summary)}</p></div>
    <div class="home-grid">${accounts.map(card).join('')}</div>`;
}

/** Channel headers are Markdown too (links to agendas, minutes...): render them on one line. */
function inlineMarkdown(text) {
  if (!text.trim()) return '';
  return renderMarkdown(text.replace(/\s*\n+\s*/g, '  ·  '))
    .replace(/<\/?(p|ul|ol|li|blockquote|h\d)[^>]*>/g, ' ')
    .replace(/<br\s*\/?>/g, ' ');
}

RENDER.header = function renderHeader() {
  const h = $('#ch-header');
  const c = S.open && S.channels.get(key(S.open.a, S.open.c));
  if (!c) { h.innerHTML = ''; return; }
  const acc = account(c.account_id);
  const where = [c.group, acc?.name].filter(Boolean).join(' · ');
  h.innerHTML = `<div class="hdr-title">${glyph(c)}<b>${esc(c.display_name)}</b>
      <span class="svc-name">${esc(where)}</span><span class="topic" title="${esc(c.topic || '')}">${inlineMarkdown(c.topic || '')}</span></div>
    <button class="icon-btn ${c.starred ? 'on' : ''}" data-act="star" title="${c.starred ? 'Remove from favorites' : 'Add to favorites'}">${icon('star')}</button>
    <button class="icon-btn" data-act="prefs" title="Notifications for this conversation${c.muted ? ' (off)' : ''}">${icon(c.muted ? 'bellOff' : 'bell')}</button>
    <button class="icon-btn ${S.panel?.type === 'gallery' ? 'on' : ''}" data-act="gallery" title="Images and links">${icon('image')}</button>
    ${peopleButton(c)}`;
};

/** A direct message's person, or a conversation's members. */
function peopleButton(c) {
  const peer = c.kind === 'dm' ? c.peers[0] : null;
  if (peer) {
    const on = S.panel?.type === 'person' && S.panel.u === peer.id;
    return `<button class="icon-btn ${on ? 'on' : ''}" data-act="peer" title="Profile">${icon('person')}</button>`;
  }
  const on = S.panel?.type === 'members' && S.panel.c === c.id;
  return `<button class="icon-btn ${on ? 'on' : ''}" data-act="members" title="Members">${icon('people')}</button>`;
}

const fmtTime = (ts) => new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
function fmtDay(ts) {
  const d = new Date(ts);
  const today = new Date();
  const yesterday = new Date(Date.now() - 864e5);
  if (d.toDateString() === today.toDateString()) return 'Today';
  if (d.toDateString() === yesterday.toDateString()) return 'Yesterday';
  return d.toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'long', year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric' });
}

function reactionsHtml(m) {
  if (!m.reactions?.length) return '';
  const me = account(m.account_id)?.self_user_id;
  return `<div class="reactions">${m.reactions.map((r) => `<button class="reaction ${r.users.includes(me) ? 'mine' : ''}"
    data-act="toggle-reaction" data-emoji="${esc(r.emoji)}" title=":${esc(r.emoji)}:">${emoji(r.emoji)} ${r.users.length}</button>`).join('')}</div>`;
}

function attachmentsHtml(m) {
  if (!m.attachments?.length) return '';
  return `<div class="attachments">${m.attachments.map((f) => (f.mime || '').startsWith('image/')
    ? `<img src="${esc(f.thumb_url || f.url)}" data-full="${esc(f.url)}" alt="${esc(f.name)}" loading="lazy">`
    : `<a class="file" href="${esc(f.url)}" data-url="${esc(f.url)}">${esc(f.name)}</a>`).join('')}</div>`;
}

function linksHtml(m) {
  if (!S.settings?.link_previews || !m.links?.length) return '';
  return m.links.map((l) => `<div class="link-card"><a class="t" href="${esc(l.url)}" data-url="${esc(l.url)}">${esc(l.title || l.url)}</a>
    ${l.description ? `<div class="d">${esc(l.description)}</div>` : ''}${l.image_url ? `<img src="${esc(l.image_url)}" data-full="${esc(l.image_url)}" loading="lazy" alt="">` : ''}</div>`).join('');
}

function toolsHtml(m, inThread) {
  if (m.deleted) return '';
  return `<div class="msg-tools">
    <button class="icon-btn" data-act="react" title="Add reaction">${icon('smile')}</button>
    ${inThread ? '' : `<button class="icon-btn" data-act="thread" title="Reply in thread">${icon('reply')}</button>`}
    ${m.is_self ? `<button class="icon-btn" data-act="edit" title="Edit">${icon('edit')}</button>
      <button class="icon-btn" data-act="delete" title="Delete (click twice)">${icon('trash')}</button>` : ''}</div>`;
}

/** The author's picture (lazy, falls back to initials when it cannot load). */
function messageAvatar(m, size = '') {
  const fallback = initials(m.author_name);
  const who = personAttrs(m);
  return m.author_avatar
    ? `<span class="av ${size}"${who}><img src="${esc(m.author_avatar)}" alt="" loading="lazy" data-fallback="${fallback}"></span>`
    : `<span class="av ${size}"${who}>${fallback}</span>`;
}

/** Makes an author's picture or name open their profile. */
const personAttrs = (m) => (m.author_id ? ` data-person="${esc(m.author_id)}" data-pa="${esc(m.account_id)}" title="View profile"` : '');

/** `#C06D36MQYVA` left by older Slack messages: shown as the conversation's name when known. */
function namedChannelRefs(html, acc) {
  return html.replace(/<span class="chan-ref" data-chan="([CG][A-Z0-9]{6,})">#\1<\/span>/g, (all, id) => {
    const c = S.channels.get(key(acc, id));
    return c ? `<span class="chan-ref" data-chan="${esc(c.name)}">#${esc(c.display_name)}</span>` : all;
  });
}

function messageHtml(m, grouped, inThread) {
  let body = m.deleted ? '<span class="deleted">Message deleted</span>' : namedChannelRefs(renderMarkdown(m.text), m.account_id);
  let edited = m.edited_at && !m.deleted ? '<span class="edited">(edited)</span>' : '';
  // Keep the label on the last line of text instead of a line of its own.
  if (edited && body.endsWith('</p>')) { body = `${body.slice(0, -4)}${edited}</p>`; edited = ''; }
  const replies = !inThread && m.reply_count
    ? `<button class="replies" data-act="thread">${m.reply_count} ${m.reply_count === 1 ? 'reply' : 'replies'}</button>` : '';
  return `<div class="msg ${grouped ? 'cont' : ''} ${S.editing?.id === m.id ? 'editing' : ''}" data-id="${esc(m.id)}">
    ${grouped ? `<span class="gutter-time">${fmtTime(m.ts)}</span>` : messageAvatar(m)}
    <div class="msg-main">
      ${grouped ? '' : `<div class="msg-head"><b class="author"${personAttrs(m)}>${esc(m.author_name)}</b><time title="${esc(new Date(m.ts).toLocaleString())}">${fmtTime(m.ts)}</time></div>`}
      <div class="msg-body">${body}${edited}</div>
      ${attachmentsHtml(m)}${linksHtml(m)}${reactionsHtml(m)}${replies}
    </div>${toolsHtml(m, inThread)}</div>`;
}

/** `newSince` (main view only): messages from others after this time are new since the last visit. */
function messagesHtml(list, inThread, newSince = null) {
  let out = '';
  let prev = null;
  let day = '';
  let dividerShown = false;
  for (const m of list) {
    const d = new Date(m.ts).toDateString();
    if (d !== day) {
      // Each day is a group, so its date line stays pinned at the top only while that day is
      // on screen. Today's line scrolls normally.
      if (!inThread) {
        const today = d === new Date().toDateString();
        out += `${day ? '</section>' : ''}<section class="day-group"><div class="day ${today ? 'today' : ''}"><span>${fmtDay(m.ts)}</span></div>`;
      }
      day = d;
      prev = null;
    }
    const fresh = newSince !== null && m.ts > newSince;
    if (fresh && !dividerShown && !m.is_self) {
      out += '<div class="new-divider"><span>New messages</span></div>';
      dividerShown = true;
      prev = null;
    }
    const grouped = prev && prev.author_id === m.author_id && m.ts - prev.ts < 5 * 60e3 && !prev.deleted && !m.deleted;
    const html = messageHtml(m, grouped, inThread);
    out += dividerShown && fresh ? html.replace('class="msg ', 'class="msg fresh ') : html;
    prev = m;
  }
  return !inThread && day ? `${out}</section>` : out;
}

/**
 * `mode`: 'open' places the view at the first new message (or the bottom); true forces the
 * bottom; false keeps the position, following the bottom when the reader is there.
 */
function renderMessages(mode = false) {
  const box = $('#messages');
  if (!S.open) { box.innerHTML = ''; return; }
  box.innerHTML = (S.hasOlder && S.messages.length ? '<div class="loading-older">Scroll up for older messages</div>' : '') + messagesHtml(S.messages, false, S.newSince);
  if (mode === 'open') {
    const divider = box.querySelector('.new-divider');
    if (divider && divider.offsetTop > box.clientHeight / 2) {
      box.scrollTop = Math.max(0, divider.offsetTop - 48);
      S.stick = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
      return;
    }
    S.stick = true;
  }
  if (mode === true) S.stick = true;
  if (S.stick) box.scrollTop = box.scrollHeight;
}

/** Keeps the view at the bottom while late content (pictures, previews) grows the list. */
function followBottom() {
  if (S.stick) $('#messages').scrollTop = $('#messages').scrollHeight;
}
RENDER.messages = () => renderMessages(false);

async function loadOlder() {
  const box = $('#messages');
  if (box.scrollTop > 80 || !S.open || !S.hasOlder || S.loadingOlder || !S.messages.length) return;
  S.loadingOlder = true;
  const { a, c } = S.open;
  try {
    const older = await invoke('get_messages', { account_id: a, channel_id: c, before: S.messages[0].ts, limit: PAGE });
    if (!isOpen(a, c)) return;
    S.hasOlder = older.length >= PAGE;
    const height = box.scrollHeight;
    S.messages = older.concat(S.messages);
    renderMessages();
    box.scrollTop += box.scrollHeight - height;
  } catch (e) {
    toast(String(e));
  } finally {
    S.loadingOlder = false;
  }
}

RENDER.typing = function renderTyping() {
  const el = $('#typing');
  const names = [];
  if (S.open) {
    const now = Date.now();
    for (const [name, until] of S.typing.get(key(S.open.a, S.open.c)) || []) if (until > now) names.push(name);
  }
  el.textContent = !names.length ? '' : names.length === 1 ? `${names[0]} is typing...`
    : names.length === 2 ? `${names[0]} and ${names[1]} are typing...` : 'Several people are typing...';
};

// ------------------------------------------------------------ composer

function autosize(el) {
  el.style.height = 'auto';
  el.style.height = `${Math.min(el.scrollHeight, window.innerHeight * 0.4)}px`;
}
function showInfo(html) { const el = $('#compose-info'); el.innerHTML = html; el.hidden = false; }
function hideInfo() { const el = $('#compose-info'); el.hidden = true; el.innerHTML = ''; S.previewOn = false; }

function startEdit(m) {
  S.editing = { a: m.account_id, c: m.channel_id, id: m.id };
  const input = $('#input');
  input.value = m.text;
  autosize(input);
  input.focus();
  showInfo('Editing message - <kbd>Enter</kbd> to save, <kbd>Esc</kbd> to cancel');
  schedule('messages');
}

function cancelEdit() {
  if (!S.editing) return;
  S.editing = null;
  $('#input').value = '';
  autosize($('#input'));
  hideInfo();
  schedule('messages');
}

async function submit() {
  const input = $('#input');
  const text = input.value.trim();
  const editing = S.editing;
  if ((!text && (editing || !pending.length)) || !S.open) return;
  const { a, c } = S.open;
  const files = editing ? [] : pending.splice(0);
  input.value = '';
  autosize(input);
  S.editing = null;
  hideInfo();
  renderTray();
  try {
    if (editing) await invoke('edit_message', { account_id: editing.a, channel_id: editing.c, message_id: editing.id, text });
    else if (text) await invoke('send_message', { account_id: a, channel_id: c, text, thread_root: null });
  } catch (e) {
    toast(String(e));
    input.value = text;
    autosize(input);
  }
  await uploadFiles(a, c, files);
}

// ------------------------------------------------------------ composer: formatting, autocomplete, attachments

const FORMATS = [
  ['bold', 'Bold (Ctrl+B)', '<b>B</b>'],
  ['italic', 'Italic (Ctrl+I)', '<i>I</i>'],
  ['strike', 'Strikethrough (Ctrl+Shift+X)', '<s>S</s>'],
  null,
  ['code', 'Code (Ctrl+E)', icon('code')],
  ['codeblock', 'Code block (Ctrl+Shift+E)', icon('codeblock')],
  ['link', 'Link (Ctrl+Alt+K)', icon('link')],
  null,
  ['ul', 'Bulleted list', icon('list')],
  ['ol', 'Numbered list', icon('olist')],
  ['quote', 'Quote', icon('quote')],
];

function renderFormatBar() {
  $('#fmt-bar').innerHTML = FORMATS.map((f) => (f ? `<button data-fmt="${f[0]}" title="${f[1]}" tabindex="-1">${f[2]}</button>` : '<span class="sep"></span>')).join('')
    + `<span class="grow"></span>
      <button data-act="emoji" title="Emoji (type : for suggestions)" tabindex="-1">${icon('smile')}</button>
      <button data-act="attach" title="Attach files (or paste / drop an image)" tabindex="-1">${icon('clip')}</button>
      <button id="preview-btn" class="ghost" title="Preview as others see it (Ctrl+Shift+P)" tabindex="-1">Preview</button>`;
}

/** Wraps the selection in Markdown markers (or removes them when already there). */
function wrapSelection(input, before, after = before, placeholder = '') {
  const { selectionStart: s, selectionEnd: e, value: v } = input;
  if (v.slice(s - before.length, s) === before && v.slice(e, e + after.length) === after) {
    input.setRangeText(v.slice(s, e), s - before.length, e + after.length, 'select');
  } else {
    const sel = v.slice(s, e) || placeholder;
    input.setRangeText(before + sel + after, s, e, 'end');
    input.setSelectionRange(s + before.length, s + before.length + sel.length);
  }
  input.focus();
  input.dispatchEvent(new Event('input'));
}

/** Puts a list or quote marker in front of every selected line. */
function prefixLines(input, kind) {
  const v = input.value;
  const s = v.lastIndexOf('\n', input.selectionStart - 1) + 1;
  let e = v.indexOf('\n', input.selectionEnd);
  if (e < 0) e = v.length;
  const lines = v.slice(s, e).split('\n').map((l, i) => `${kind === 'ul' ? '- ' : kind === 'ol' ? `${i + 1}. ` : '> '}${l}`);
  input.setRangeText(lines.join('\n'), s, e, 'end');
  input.focus();
  input.dispatchEvent(new Event('input'));
}

function applyFormat(input, f) {
  switch (f) {
    case 'bold': return wrapSelection(input, '**', '**', 'bold');
    case 'italic': return wrapSelection(input, '_', '_', 'italic');
    case 'strike': return wrapSelection(input, '~~', '~~', 'text');
    case 'code': return wrapSelection(input, '`', '`', 'code');
    case 'codeblock': return wrapSelection(input, '```\n', '\n```', '');
    case 'link': {
      const { selectionStart: s, selectionEnd: e, value: v } = input;
      const label = v.slice(s, e) || 'text';
      input.setRangeText(`[${label}](https://)`, s, e, 'end');
      const urlStart = s + label.length + 3;
      input.setSelectionRange(urlStart, urlStart + 8);
      input.focus();
      return input.dispatchEvent(new Event('input'));
    }
    default: return prefixLines(input, f);
  }
}

/** Formatting shortcuts shared by the composer and the thread reply box. */
function formatShortcut(e, input) {
  if (!e.ctrlKey) return false;
  const k = e.key.toLowerCase();
  const f = !e.shiftKey && !e.altKey && k === 'b' ? 'bold'
    : !e.shiftKey && !e.altKey && k === 'i' ? 'italic'
    : e.shiftKey && k === 'x' ? 'strike'
    : !e.shiftKey && k === 'e' ? 'code'
    : e.shiftKey && k === 'e' ? 'codeblock'
    : e.altKey && k === 'k' ? 'link' : null;
  if (!f) return false;
  e.preventDefault();
  applyFormat(input, f);
  return true;
}

// @mention and :emoji: completion, for any textarea.
const AC = { input: null, items: [], sel: 0, start: 0, end: 0 };

function closeAutocomplete() {
  AC.input = null;
  AC.items = [];
  $('#ac-pop').hidden = true;
}

async function updateAutocomplete(input, accountId) {
  const pos = input.selectionStart;
  const before = input.value.slice(0, pos);
  const m = /(^|[\s(])([@:])([\w.\-+]*)$/.exec(before);
  if (!m || (m[2] === ':' && m[3].length < 2)) { closeAutocomplete(); return; }
  const q = m[3].toLowerCase();
  let items;
  if (m[2] === '@') {
    const users = accountId ? await invoke('find_users', { account_id: accountId, query: q }).catch(() => []) : [];
    items = users.map((u) => ({
      html: `${avatarHtml(u, u.display_name || u.name, undefined, 'xs')}<b>${esc(u.display_name || u.name)}</b><span class="muted">@${esc(u.name)}</span>`,
      insert: `@${u.name} `,
    }));
    items.push(...['here', 'channel', 'all'].filter((w) => w.startsWith(q) && q.length > 0)
      .map((w) => ({ html: `<b>@${w}</b><span class="muted">notify ${w === 'here' ? 'everyone online' : 'everyone'}</span>`, insert: `@${w} ` })));
  } else {
    items = Object.keys(EMOJI).filter((n) => n.includes(q))
      .sort((x, y) => Number(!x.startsWith(q)) - Number(!y.startsWith(q)) || x.length - y.length).slice(0, 8)
      .map((n) => ({ html: `<span class="em">${EMOJI[n]}</span><span>:${esc(n)}:</span>`, insert: `:${n}: ` }));
  }
  // The text may have changed while people were looked up.
  if (input.value.slice(0, input.selectionStart) !== before) return;
  if (!items.length) { closeAutocomplete(); return; }
  Object.assign(AC, { input, items, sel: 0, start: pos - m[3].length - 1, end: pos });
  drawAutocomplete();
}

function drawAutocomplete() {
  const pop = $('#ac-pop');
  pop.innerHTML = AC.items.map((it, i) => `<button data-i="${i}" class="${i === AC.sel ? 'sel' : ''}">${it.html}</button>`).join('')
    + '<div class="ac-hint"><kbd>Tab</kbd> or <kbd>Enter</kbd> to insert</div>';
  pop.hidden = false;
  const r = AC.input.getBoundingClientRect();
  pop.style.left = `${r.left}px`;
  pop.style.width = `${Math.min(360, r.width)}px`;
  pop.style.top = `${Math.max(8, r.top - pop.offsetHeight - 6)}px`;
}

function acceptAutocomplete(i = AC.sel) {
  const it = AC.items[i];
  const input = AC.input;
  if (!it || !input) return;
  input.setRangeText(it.insert, AC.start, AC.end, 'end');
  closeAutocomplete();
  input.focus();
  autosize(input);
}

/** Handles keys while suggestions are shown; true when the key was used. */
function autocompleteKey(e) {
  if (!AC.items.length || e.target !== AC.input) return false;
  const n = AC.items.length;
  if (e.key === 'ArrowDown') AC.sel = (AC.sel + 1) % n;
  else if (e.key === 'ArrowUp') AC.sel = (AC.sel - 1 + n) % n;
  else if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) { e.preventDefault(); acceptAutocomplete(); return true; }
  else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeAutocomplete(); return true; }
  else return false;
  e.preventDefault();
  drawAutocomplete();
  return true;
}

function showEmojiPicker(anchor, input) {
  showPopover(anchor, `<div class="emoji-grid wide">${COMMON_EMOJI.filter((n) => EMOJI[n])
    .map((n) => `<button data-emoji="${n}" title=":${n}:">${EMOJI[n]}</button>`).join('')}</div>`, (pop) => {
    pop.addEventListener('click', (e) => {
      const b = e.target.closest('[data-emoji]');
      if (!b) return;
      closePopover();
      const s = input.selectionStart;
      const pad = s > 0 && !/\s/.test(input.value[s - 1]) ? ' ' : '';
      input.setRangeText(`${pad}:${b.dataset.emoji}: `, s, input.selectionEnd, 'end');
      input.focus();
      autosize(input);
    });
  });
}

// Files waiting to be sent with the next message (pasted, dropped or attached).
// Only references to the browser's own Blob objects; nothing is copied until sending.
const pending = [];
const MAX_FILE = 25 * 1024 * 1024;
const MAX_PENDING = 10;
let pendingSeq = 0;

function addFiles(list) {
  for (const f of list) {
    if (pending.length >= MAX_PENDING) { toast(`At most ${MAX_PENDING} files per message`); break; }
    if (f.size > MAX_FILE) { toast(`${f.name || 'File'} is larger than 25 MB`); continue; }
    const image = (f.type || '').startsWith('image/');
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
    const name = f.name && f.name !== 'image.png' ? f.name : `screenshot-${stamp}.png`;
    pending.push({ id: ++pendingSeq, name, blob: f, image, url: image ? URL.createObjectURL(f) : null });
  }
  renderTray();
}

function renderTray() {
  const tray = $('#attach-tray');
  tray.hidden = !pending.length;
  tray.innerHTML = pending.map((p) => `<div class="att" data-id="${p.id}">
      ${p.image ? `<img src="${esc(p.url)}" alt="" title="Double-click to edit">` : `<span class="file-ico">${icon('clip')}</span>`}
      <span class="nm" title="${esc(p.name)}">${esc(p.name)}</span>
      ${p.image ? `<button data-att="edit" title="Draw, highlight, add text, crop">${icon('edit')}</button>` : ''}
      <button data-att="remove" title="Remove">${icon('close')}</button></div>`).join('');
}

function clearPending() {
  pending.splice(0).forEach((p) => p.url && URL.revokeObjectURL(p.url));
  renderTray();
}

async function editPending(id) {
  const p = pending.find((x) => x.id === id);
  if (!p?.image) return;
  // Always edit the original with the previous annotations, which stay movable and editable.
  p.original ||= p.blob;
  const out = await editImage(p.original, p.edits);
  if (!out) return;
  if (p.url) URL.revokeObjectURL(p.url);
  Object.assign(p, { blob: out.blob, edits: out.state, url: URL.createObjectURL(out.blob), name: p.name.replace(/\.[^.]+$/, '') + '.png' });
  renderTray();
}

/** Sends files one by one as raw bytes (headers carry account, conversation and name). */
async function uploadFiles(a, c, files, threadRoot = null) {
  for (const p of files) {
    try {
      const bytes = new Uint8Array(await p.blob.arrayBuffer());
      const headers = { 'x-account': encodeURIComponent(a), 'x-channel': encodeURIComponent(c), 'x-name': encodeURIComponent(p.name) };
      if (threadRoot) headers['x-thread'] = encodeURIComponent(threadRoot);
      await invoke('upload_file', bytes, { headers });
    } catch (e) {
      toast(`${p.name}: ${e}`, 6000);
    } finally {
      if (p.url) URL.revokeObjectURL(p.url);
    }
  }
}

async function updatePreview() {
  if (!S.previewOn || !S.open) return;
  const text = $('#input').value;
  const remote = await invoke('preview_remote', { account_id: S.open.a, text });
  const kind = kindName(account(S.open.a)?.kind);
  const note = remote === text ? `${kind} receives your Markdown as-is` : `${kind} receives`;
  showInfo(`${esc(note)}:<pre>${esc(remote || ' ')}</pre>`);
  S.previewOn = true;
}

function togglePreview() {
  if (S.previewOn) { hideInfo(); return; }
  S.previewOn = true;
  updatePreview();
}

// ------------------------------------------------------------ right panel

/** `back` is the panel to return to (search results). */
async function openThread(rootId, back = null) {
  if (!S.open) return;
  const { a, c } = S.open;
  S.panel = { type: 'thread', a, c, root: rootId, messages: S.messages.filter((m) => m.id === rootId), back };
  renderPanel(true);
  try {
    const msgs = await invoke('get_thread', { account_id: a, channel_id: c, root_id: rootId });
    if (S.panel?.root !== rootId) return;
    S.panel.messages = msgs;
    renderPanel();
  } catch (e) {
    toast(String(e));
  }
}

function renderPanel(rebuild = false) {
  const el = $('#panel');
  const p = S.panel;
  el.hidden = !p;
  document.querySelector('.resizer[data-target="panel"]').hidden = !p;
  if (!p) { el.innerHTML = ''; el.dataset.view = ''; return; }
  if (p.type === 'thread') {
    const view = `thread:${p.root}`;
    if (rebuild || el.dataset.view !== view) {
      el.dataset.view = view;
      el.innerHTML = `<div class="panel-head">${p.back ? `<button class="icon-btn" data-act="panel-back" title="Back to search results">${icon('back')}</button>` : ''}<span>Thread</span><button class="icon-btn" data-act="close-panel" title="Close">${icon('close')}</button></div>
        <div class="panel-body" id="thread-body"></div>
        <div class="composer"><div class="compose-row"><textarea id="thread-input" rows="1" placeholder="Reply..."></textarea></div></div>`;
    }
    const [root, ...replies] = p.messages;
    const body = $('#thread-body');
    const atBottom = body.scrollHeight - body.scrollTop - body.clientHeight < 60;
    body.innerHTML = (root ? messagesHtml([root], true) : '')
      + (replies.length ? `<div class="thread-sep">${replies.length} ${replies.length === 1 ? 'reply' : 'replies'}</div>` : '')
      + messagesHtml(replies, true);
    if (atBottom || rebuild) body.scrollTop = body.scrollHeight;
  } else if (p.type === 'search') {
    renderSearch(el, rebuild);
  } else if (p.type === 'account' || p.type === 'notify') {
    const a = account(p.a);
    const body = !a ? '' : p.type === 'account' ? accountPanelHtml(a) : notifyPanelHtml(p);
    if (!body) { S.panel = null; renderPanel(); return; }
    // The notification form is rebuilt only when it is first shown, so a select being used is not reset.
    const view = `${p.type}:${p.a}:${p.c || ''}`;
    if (p.type === 'account' || rebuild || el.dataset.view !== view) {
      el.dataset.view = view;
      el.innerHTML = `<div class="panel-head"><span>${p.type === 'account' ? 'You' : 'Notifications'}</span>
          <button class="icon-btn" data-act="close-panel" title="Close (Esc)">${icon('close')}</button></div>
        <div class="panel-body side-panel">${body}</div>`;
    }
  } else if (p.type === 'person') {
    el.dataset.view = `person:${p.a}:${p.u}`;
    const back = p.back ? `<button class="icon-btn" data-act="panel-back" title="Back">${icon('back')}</button>` : '';
    const body = p.profile ? personHtml(p.profile)
      : p.error ? `<p class="muted">${esc(p.error)}</p>` : '<p class="muted">Loading...</p>';
    el.innerHTML = `<div class="panel-head">${back}<span>Profile</span>
        <button class="icon-btn" data-act="close-panel" title="Close (Esc)">${icon('close')}</button></div>
      <div class="panel-body side-panel">${body}</div>`;
  } else if (p.type === 'members') {
    // The filter box is built once, so typing in it is not interrupted; the list is redrawn.
    const view = `members:${p.a}:${p.c}`;
    if (rebuild || el.dataset.view !== view) {
      el.dataset.view = view;
      el.innerHTML = `<div class="panel-head"><span>Members <span class="muted" id="member-total"></span></span>
          <button class="icon-btn" data-act="close-panel" title="Close (Esc)">${icon('close')}</button></div>
        <div class="member-filter"><input type="text" id="member-filter" placeholder="Filter by name" value="${esc(p.filter)}"></div>
        <div class="panel-body" id="member-list"></div>`;
    }
    drawMembers(p);
  } else {
    el.dataset.view = 'gallery';
    const images = [];
    const links = new Set();
    for (const m of S.messages) {
      for (const f of m.attachments || []) if ((f.mime || '').startsWith('image/')) images.push(f);
      for (const l of m.links || []) links.add(l.url);
      for (const u of extractUrls(m.text)) links.add(u);
    }
    el.innerHTML = `<div class="panel-head"><span>Images and links</span><button class="icon-btn" data-act="close-panel" title="Close">${icon('close')}</button></div>
      <div class="panel-body">
        <div class="section">Images</div>
        ${images.length ? `<div class="gallery">${images.map((f) => `<img src="${esc(f.thumb_url || f.url)}" data-full="${esc(f.url)}" alt="${esc(f.name)}" loading="lazy">`).join('')}</div>` : '<div class="section muted">None in the loaded messages.</div>'}
        <div class="section">Links</div>
        <div class="link-list">${[...links].reverse().map((u) => `<div><a href="${esc(u)}" data-url="${esc(u)}">${esc(u)}</a></div>`).join('') || '<span class="muted">None in the loaded messages.</span>'}</div>
      </div>`;
  }
}
RENDER.panel = () => renderPanel(false);

// ------------------------------------------------------------ people

/** A person's profile in the side panel. `back` is the panel to return to (members, a thread). */
async function openPerson(a, u, back = null) {
  const p = { type: 'person', a, u, back, profile: null, error: null };
  S.panel = p;
  schedule('panel', 'header');
  try { p.profile = await invoke('get_profile', { account_id: a, user_id: u }); } catch (e) { p.error = String(e); }
  if (S.panel === p) schedule('panel');
}

/** Their time now, from a zone name or a UTC offset. */
function localTime(p) {
  const now = new Date();
  if (p.timezone) {
    try {
      return `${now.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', timeZone: p.timezone })} · ${p.timezone.replace(/_/g, ' ')}`;
    } catch { /* a zone name the browser does not know: use the offset */ }
  }
  if (p.utc_offset_min == null) return '';
  const there = new Date(now.getTime() + (p.utc_offset_min + now.getTimezoneOffset()) * 60000);
  const h = p.utc_offset_min / 60;
  return `${fmtTime(there)} · UTC${h >= 0 ? '+' : '−'}${Math.abs(h)}`;
}

const copyable = (v) => (v ? `<span class="fact-v">${esc(v)}</span><button class="icon-btn" data-copy="${esc(v)}" title="Copy">${icon('copy')}</button>` : '');

/** Status line and the facts the service shares (title, contact, time, custom fields). */
function profileFactsHtml(p) {
  const rows = [
    ['Title', esc(p.title)],
    ['Role', esc(p.roles.join(', '))],
    ['Local time', esc(localTime(p))],
    ['Email', copyable(p.email)],
    ['Phone', copyable(p.phone)],
    ...p.extra.map(([k, v]) => [esc(k), esc(v)]),
  ].filter(([, v]) => v);
  const status = p.status_text || p.status_emoji
    ? `<div class="custom-status">${p.status_emoji ? emoji(p.status_emoji) : ''}<span>${esc(p.status_text)}</span></div>` : '';
  return status + (rows.length ? `<dl class="facts">${rows.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join('')}</dl>` : '');
}

function personHtml(p) {
  const name = p.display_name || p.name;
  const dm = !p.is_self && [...S.channels.values()].find((c) => c.account_id === p.account_id && c.kind === 'dm' && c.peers[0]?.id === p.id);
  const tags = [p.is_self && 'You', p.is_bot && 'Bot', p.inactive && 'Deactivated'].filter(Boolean)
    .map((t) => `<span class="tag">${t}</span>`).join('');
  const facts = profileFactsHtml(p);
  return `<div class="side-head">${avatarHtml({ avatar_url: p.avatar_url }, name, p.presence, 'lg')}
      <div class="who"><b>${esc(name)}</b>${tags}
        <div class="muted">@${esc(p.name)}</div>
        ${p.presence !== 'unknown' ? `<div class="muted">${presenceLabel(p.presence)}</div>` : ''}</div></div>
    ${p.is_self ? '' : `<div class="person-actions">
      ${dm ? `<button class="btn" data-act="person-dm" data-c="${esc(dm.id)}">${icon('reply')}Message</button>` : ''}
      <button class="btn" data-act="person-mention">@ Mention</button></div>`}
    ${facts || '<p class="muted">The service shares nothing more about this person.</p>'}`;
}

/** Inserts @handle in the composer of the open conversation (same workspace only). */
function mentionPerson(p) {
  const input = $('#input');
  if (!S.open || S.open.a !== p.account_id || $('#composer').hidden) { toast('Open a conversation of this workspace first'); return; }
  const at = input.selectionStart ?? input.value.length;
  const before = input.value.slice(0, at);
  const ins = `${before && !/\s$/.test(before) ? ' ' : ''}@${p.name} `;
  input.value = before + ins + input.value.slice(at);
  input.focus();
  input.selectionStart = input.selectionEnd = at + ins.length;
  input.dispatchEvent(new Event('input'));
}

async function openMembers(a, c) {
  const p = { type: 'members', a, c, list: [], next: null, total: null, loading: false, error: null, filter: '' };
  S.panel = p;
  schedule('panel', 'header');
  await loadMembers(p);
}

/** The next page of members (the first when the list is empty). */
async function loadMembers(p) {
  if (p.loading) return;
  p.loading = true;
  if (S.panel === p) drawMembers(p);
  try {
    const r = await invoke('channel_members', { account_id: p.a, channel_id: p.c, cursor: p.next });
    p.list.push(...r.members);
    p.next = r.next;
    if (r.total != null) p.total = r.total;
  } catch (e) {
    p.error = String(e);
  }
  p.loading = false;
  if (S.panel === p) drawMembers(p);
}

function drawMembers(p) {
  const list = $('#member-list');
  if (!list) return;
  const total = $('#member-total');
  if (total) total.textContent = p.total != null ? String(p.total) : p.list.length ? String(p.list.length) : '';
  const q = p.filter.trim().toLowerCase();
  const shown = q ? p.list.filter((m) => `${m.display_name} ${m.name}`.toLowerCase().includes(q)) : p.list;
  const me = account(p.a)?.self_user_id;
  list.innerHTML = shown.map((m) => `<button class="member" data-u="${esc(m.id)}">
      ${avatarHtml(m, m.display_name || m.name, m.presence, 'md mem')}
      <span class="nm">${esc(m.display_name || m.name)}${m.id === me ? ' <span class="muted">(you)</span>' : ''}</span>
      <span class="muted handle">@${esc(m.name)}</span>${m.is_bot ? '<span class="tag">Bot</span>' : ''}</button>`).join('')
    + (p.loading ? '<p class="muted pad">Loading...</p>' : '')
    + (p.error ? `<p class="muted pad">${esc(p.error)}</p>` : '')
    + (!p.loading && p.next ? `<button class="btn more" data-act="members-more">Show more${q ? ' (filter covers loaded members)' : ''}</button>` : '')
    + (!p.loading && !p.error && !shown.length ? `<p class="muted pad">${q ? 'Nobody loaded matches.' : 'No members.'}</p>` : '');
}

// ------------------------------------------------------------ search

const SEARCH_KEYS = ['from:', 'in:', 'on:', 'before:', 'after:'];
const today = () => new Date().toLocaleDateString('sv'); // YYYY-MM-DD in local time
const SEARCH_HINTS = [
  ['from:', 'from:', 'a person'],
  ['in:', 'in:', 'a conversation'],
  ['on:', () => `on:${today()}`, 'a day, YYYY-MM-DD'],
  ['before:', 'before:', 'a day'],
  ['after:', 'after:', 'a day'],
  ['"', '""', 'exact phrase'],
];
const quoteIfSpaced = (s) => (/\s/.test(s) ? `"${s}"` : s);

/** Plain words and phrases of a query, for highlighting. */
function searchTerms(q) {
  const out = [];
  for (const [, phrase, word] of q.matchAll(/"([^"]+)"|(\S+)/g)) {
    if (phrase) out.push(phrase);
    else if (!SEARCH_KEYS.some((k) => word.toLowerCase().startsWith(k))) out.push(word.replace(/\*$/, ''));
  }
  return out.filter((t) => t.length > 1);
}

function highlight(html, terms) {
  if (!terms.length) return html;
  const re = new RegExp(`(${terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'gi');
  return html.split(/(<[^>]+>)/).map((part) => (part.startsWith('<') ? part : part.replace(re, '<mark>$1</mark>'))).join('');
}

/** Suggestions for the word being typed: modifiers, then conversations after in:, people after from:. */
function searchSuggestions(value) {
  const token = value.slice(value.lastIndexOf(' ') + 1);
  const low = token.toLowerCase();
  const visible = visibleChannels();
  if (low.startsWith('in:')) {
    const q = low.slice(3).replace(/^["#~]/, '');
    return visible.filter((c) => c.display_name.toLowerCase().includes(q)).slice(0, 8)
      .map((c) => ({ html: `${glyph(c)}<span>${esc(c.display_name)}</span>`, insert: `in:${quoteIfSpaced(c.display_name)} ` }));
  }
  if (low.startsWith('from:')) {
    const q = low.slice(5).replace(/^["@]/, '');
    const people = new Map();
    for (const c of visible) for (const p of c.peers) people.set(p.name, p);
    return [...people.values()].filter((p) => p.name.toLowerCase().includes(q)).slice(0, 8)
      .map((p) => ({ html: `<span class="av sm">${initials(p.name)}</span><span>${esc(p.name)}</span>`, insert: `from:${quoteIfSpaced(p.name)} ` }));
  }
  return SEARCH_HINTS.filter(([k]) => !low || k.startsWith(low))
    .map(([k, ins, label]) => ({ html: `<b>${esc(k)}</b><span class="muted">${esc(label)}</span>`, insert: typeof ins === 'function' ? ins() : ins }));
}

const searchUi = { items: [], sel: -1 };

function drawSuggestions() {
  const input = $('#search');
  const pop = $('#search-pop');
  searchUi.items = document.activeElement === input ? searchSuggestions(input.value) : [];
  searchUi.sel = Math.min(searchUi.sel, searchUi.items.length - 1);
  pop.hidden = !searchUi.items.length;
  pop.innerHTML = searchUi.items.map((s, i) => `<button data-i="${i}" class="${i === searchUi.sel ? 'sel' : ''}">${s.html}</button>`).join('')
    + '<div class="search-hint"><kbd>Enter</kbd> search · <kbd>Tab</kbd> complete</div>';
}

function applySuggestion(i) {
  const input = $('#search');
  const s = searchUi.items[i];
  if (!s) return;
  const v = input.value;
  input.value = v.slice(0, v.lastIndexOf(' ') + 1) + s.insert;
  // Leave the caret inside the quotes of an empty phrase.
  const caret = s.insert === '""' ? input.value.length - 1 : input.value.length;
  input.setSelectionRange(caret, caret);
  input.focus();
  searchUi.sel = -1;
  drawSuggestions();
}

// Date filter of the search panel: written into the query as on:/after:/before:, which both the
// archive and the servers understand. `after:` and `before:` exclude the day they name.
const DATE_TOKEN = /(^|\s)(on|after|before):\S+/gi;
const DATE_PRESETS = [['any', 'Any time'], ['today', 'Today'], ['week', 'Last 7 days'], ['month', 'Last 30 days'], ['custom', 'Dates...']];
const isoDay = (d) => d.toLocaleDateString('sv');
const shiftDay = (iso, n) => { const d = new Date(`${iso}T12:00`); d.setDate(d.getDate() + n); return isoDay(d); };
const withoutDates = (q) => q.replace(DATE_TOKEN, ' ').replace(/\s+/g, ' ').trim();

function dateTokens(range) {
  const t = isoDay(new Date());
  switch (range.kind) {
    case 'today': return `on:${t}`;
    case 'week': return `after:${shiftDay(t, -7)}`;
    case 'month': return `after:${shiftDay(t, -31)}`;
    case 'custom': return [range.from && `after:${shiftDay(range.from, -1)}`, range.to && `before:${shiftDay(range.to, 1)}`].filter(Boolean).join(' ');
    default: return '';
  }
}

/** Reruns the open search with another date range. */
function setSearchDates(range) {
  const p = S.panel;
  if (p?.type !== 'search') return;
  const words = withoutDates(p.q);
  const q = `${words} ${dateTokens(range)}`.trim();
  if (!q) { p.range = range; renderPanel(true); return; }
  runSearch(q, p.scope, range);
}

async function runSearch(q, scope = railAccount(), range = null) {
  q = q.trim();
  if (!q) return;
  // Dates typed by hand (on:, after:...) count as a custom range.
  range ||= /(^|\s)(on|after|before):\S+/i.test(q) ? { kind: 'typed' } : { kind: 'any' };
  const p = { type: 'search', q, scope, range, hits: [], loading: true, remote: '' };
  S.panel = p;
  renderPanel(true);
  try {
    p.hits = await invoke('search_messages', { query: q, account_id: scope, remote: false });
  } catch (e) {
    toast(String(e));
  }
  p.loading = false;
  if (S.panel === p) renderPanel(true);
}

/** Accounts in scope that can search on their server (not the offline demo). */
const remoteTargets = (p) => (p.scope ? [account(p.scope)] : enabledAccounts())
  .filter((a) => a && a.kind !== 'demo' && S.conn[a.id]?.state === 'online');

async function searchRemote(p) {
  p.remote = 'loading';
  renderPanel(true);
  try {
    const more = await invoke('search_messages', { query: p.q, account_id: p.scope, remote: true });
    const seen = new Set(p.hits.map((h) => key(h.account_id, h.id)));
    const added = more.filter((h) => !seen.has(key(h.account_id, h.id)));
    p.hits = p.hits.concat(added).sort((x, y) => y.ts - x.ts);
    p.remote = `done:${added.length}`;
  } catch (e) {
    p.remote = '';
    toast(String(e), 5000);
  }
  if (S.panel === p) renderPanel(true);
}

function renderSearch(el) {
  const p = S.panel;
  el.dataset.view = 'search';
  const terms = searchTerms(p.q);
  let body = '';
  if (p.loading) body = '<div class="search-empty">Searching...</div>';
  else if (!p.hits.length) body = '<div class="search-empty">No matches in the archive.</div>';
  let day = '';
  for (const m of p.loading ? [] : p.hits) {
    const d = new Date(m.ts).toDateString();
    if (d !== day) { body += `<div class="day">${fmtDay(m.ts)}</div>`; day = d; }
    const ch = S.channels.get(key(m.account_id, m.channel_id));
    const acc = account(m.account_id);
    body += `<div class="hit" data-a="${esc(m.account_id)}" data-c="${esc(m.channel_id)}" data-id="${esc(m.id)}">
      <div class="hit-where">${ch ? glyph(ch) : ''}<span class="ch">${esc(ch?.display_name || 'Unknown conversation')}</span>
        ${ch?.group ? `<span class="muted">${esc(ch.group)}</span>` : ''}${m.thread_root ? '<span class="muted">in a thread</span>' : ''}
        <span class="svc" style="--c:${esc(acc?.color || '')}" title="${esc(acc?.name || '')}"></span><span class="jump">Jump</span></div>
      ${highlight(messageHtml(m, false, true), terms)}</div>`;
  }
  const targets = remoteTargets(p);
  const n = p.remote.startsWith('done:') ? Number(p.remote.slice(5)) : 0;
  const remote = p.remote === 'loading' ? '<span class="muted">Asking the server...</span>'
    : p.remote ? `<span class="muted">${n ? `${n} more from the server` : 'Nothing more on the server'}</span>`
    : targets.length && !p.loading ? `<button class="btn" data-act="search-remote">Search on ${targets.length === 1 ? esc(targets[0].name) : 'the servers'}</button>
      <div class="muted">For older messages Agora has not archived.</div>` : '';
  const scopeAcc = p.scope ? account(p.scope) : null;
  const scopes = [`<button class="${p.scope ? '' : 'on'}" data-scope="">All accounts</button>`]
    .concat((scopeAcc ? [scopeAcc] : railAccount() ? [account(S.rail)] : []).filter(Boolean)
      .map((a) => `<button class="${p.scope === a.id ? 'on' : ''}" data-scope="${esc(a.id)}">${esc(a.name)}</button>`));
  const r = p.range || { kind: 'any' };
  const kind = r.kind === 'typed' ? '' : r.kind;
  const dates = `<div class="date-filter">${DATE_PRESETS.map(([k, l]) => `<button class="${kind === k ? 'on' : ''}" data-range="${k}">${l}</button>`).join('')}</div>
    ${r.kind === 'custom' ? `<div class="date-range"><label>From <input type="date" data-edge="from" value="${esc(r.from || '')}" max="${isoDay(new Date())}"></label>
      <label>To <input type="date" data-edge="to" value="${esc(r.to || '')}" max="${isoDay(new Date())}"></label></div>` : ''}`;
  const shown = r.kind === 'typed' ? p.q : withoutDates(p.q);
  el.innerHTML = `<div class="panel-head"><span>Search</span><button class="icon-btn" data-act="close-panel" title="Close">${icon('close')}</button></div>
    <div class="search-meta"><div class="q">${icon('search')}<span>${esc(shown || 'Any message')}</span></div>
      <div class="scope">${scopes.join('')}<span class="muted">${p.loading ? '' : `${p.hits.length}${p.hits.length >= 50 ? '+' : ''} ${p.hits.length === 1 ? 'result' : 'results'}`}</span></div>
      ${dates}</div>
    <div class="panel-body" id="search-body">${body}<div class="search-foot">${remote}</div></div>`;
}

/** Opens the conversation at a search hit: loads history back to it, or opens its thread. */
async function jumpTo(a, c, id) {
  const p = S.panel;
  const m = p?.hits?.find((h) => h.account_id === a && h.id === id);
  if (!m) return;
  if (!S.channels.has(key(a, c))) { toast('This conversation is not in your sidebar any more.'); return; }
  if (!isOpen(a, c)) await openChannel(a, c);
  if (m.thread_root) { openThread(m.thread_root, p); return; }
  for (let i = 0; i < 20 && isOpen(a, c) && S.hasOlder && S.messages.length && S.messages[0].ts > m.ts; i++) {
    const older = await invoke('get_messages', { account_id: a, channel_id: c, before: S.messages[0].ts, limit: 100 });
    if (!isOpen(a, c)) return;
    S.hasOlder = older.length >= 100;
    S.messages = older.concat(S.messages);
  }
  renderMessages();
  const el = $('#messages').querySelector(`.msg[data-id="${CSS.escape(id)}"]`);
  if (!el) { toast('That message is too far back to show here.'); return; }
  el.scrollIntoView({ block: 'center' });
  el.classList.add('flash');
  setTimeout(() => el.classList.remove('flash'), 2200);
}

// ------------------------------------------------------------ favorites and Agora groups

async function setStarred(c, starred) {
  try {
    await invoke('set_starred', { account_id: c.account_id, channel_id: c.id, starred });
  } catch (e) {
    toast(String(e), 5000);
  }
}

async function saveGroups(groups) {
  S.settings.groups = groups;
  schedule('sidebar');
  try { await invoke('save_settings', { settings: S.settings }); } catch (e) { toast(String(e)); }
}

async function moveToGroup(c, group) {
  try {
    await invoke('set_local_group', { account_id: c.account_id, channel_id: c.id, group });
    if (group && c.starred) toast('It stays under Favorites while it is a favorite.', 3500);
  } catch (e) {
    toast(String(e));
  }
}

const newGroupField = (label) => `<div class="menu-new"><input placeholder="New group name" maxlength="40"><button class="btn" data-act="new-group">${label}</button></div>`;

/** Reads the "new group" field; returns the created group or null. */
function createGroup(pop) {
  const name = pop.querySelector('.menu-new input').value.trim();
  if (!name) return null;
  const g = { id: `g${Date.now().toString(36)}`, name };
  saveGroups([...(S.settings.groups || []), g]);
  return g;
}

/** Off = level Silent. On = back to the default, or Alert when the default itself is Silent. */
async function toggleMute(c) {
  try {
    const p = await invoke('get_channel_prefs', { account_id: c.account_id, channel_id: c.id });
    p.level = !c.muted ? 'silent' : defaultLevel(c) === 'silent' ? 'alert' : null;
    await invoke('set_channel_prefs', { account_id: c.account_id, channel_id: c.id, prefs: p });
  } catch (e) {
    toast(String(e));
  }
}

function showRowMenu(anchor, c) {
  const groups = S.settings.groups || [];
  showPopover(anchor, `<div class="menu">
      <button data-act="star">${icon('star')}${c.starred ? 'Remove from favorites' : 'Add to favorites'}</button>
      <button data-act="mute">${icon(c.muted ? 'bell' : 'bellOff')}${c.muted ? 'Turn notifications on' : 'Turn notifications off'}</button>
      <button data-act="prefs">${icon('bell')}Notification settings...</button>
      <hr><div class="menu-label">Agora group (only in this app)</div>
      ${groups.map((g) => `<button data-group="${esc(g.id)}" class="${c.local_group === g.id ? 'on' : ''}">${icon('folder')}${esc(g.name)}</button>`).join('')}
      ${c.local_group ? `<button data-group="">${icon('close')}Remove from group</button>` : ''}
      ${newGroupField('Create and move')}</div>`, (pop) => {
    const create = () => { const g = createGroup(pop); if (g) { closePopover(); moveToGroup(c, g.id); } };
    pop.addEventListener('click', (e) => {
      const b = e.target.closest('button');
      if (!b) return;
      if (b.dataset.act === 'new-group') { create(); return; }
      if (b.dataset.act === 'prefs') { showPrefs(c.account_id, c.id); return; }
      closePopover();
      if (b.dataset.act === 'mute') toggleMute(c);
      else if (b.dataset.act === 'star') setStarred(c, !c.starred);
      else if (b.dataset.group !== undefined) moveToGroup(c, b.dataset.group || null);
    });
    pop.querySelector('.menu-new input').addEventListener('keydown', (e) => { if (e.key === 'Enter') create(); });
  });
}

function showSectionMenu(anchor, sectionId) {
  const groups = S.settings.groups || [];
  const gid = sectionId.startsWith('g:') ? sectionId.slice(2) : null;
  const g = groups.find((x) => x.id === gid);
  const html = g ? `<div class="menu">
      <div class="menu-new"><input value="${esc(g.name)}" maxlength="40"><button class="btn" data-act="rename">Rename</button></div>
      <button data-act="up">Move up</button><button data-act="down">Move down</button><hr>
      <button data-act="delete" class="danger">${icon('trash')}Delete group</button>
      <div class="menu-label">Its conversations go back to their usual section.</div></div>`
    : `<div class="menu"><div class="menu-label">Agora groups collect conversations from any account.</div>${newGroupField('Create')}</div>`;
  showPopover(anchor, html, (pop) => {
    const input = pop.querySelector('.menu-new input');
    const rename = () => {
      const name = input.value.trim();
      if (name) saveGroups(groups.map((x) => (x.id === gid ? { ...x, name } : x)));
      closePopover();
    };
    const create = () => { if (createGroup(pop)) closePopover(); };
    pop.addEventListener('click', (e) => {
      const b = e.target.closest('button');
      if (!b) return;
      const i = groups.indexOf(g);
      const list = [...groups];
      switch (b.dataset.act) {
        case 'rename': rename(); return;
        case 'new-group': create(); return;
        case 'up': if (i > 0) { [list[i - 1], list[i]] = [list[i], list[i - 1]]; saveGroups(list); } break;
        case 'down': if (i < list.length - 1) { [list[i + 1], list[i]] = [list[i], list[i + 1]]; saveGroups(list); } break;
        case 'delete': saveGroups(groups.filter((x) => x.id !== gid)); break;
        default: return;
      }
      closePopover();
    });
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') (g ? rename : create)(); });
    input.focus();
  });
}

async function sendThreadReply() {
  const input = $('#thread-input');
  const p = S.panel;
  const text = input?.value.trim();
  if (!text || p?.type !== 'thread') return;
  input.value = '';
  autosize(input);
  try {
    await invoke('send_message', { account_id: p.a, channel_id: p.c, text, thread_root: p.root });
  } catch (e) {
    toast(String(e));
    input.value = text;
  }
}

// ------------------------------------------------------------ popovers

let popoverCleanup = null;
function showPopover(anchor, html, setup) {
  closePopover();
  // A fresh element each time: listeners added by an earlier popover must not fire in this one.
  const old = $('#popover');
  const pop = old.cloneNode(false);
  old.replaceWith(pop);
  pop.innerHTML = html;
  pop.hidden = false;
  const r = anchor.getBoundingClientRect();
  const w = pop.offsetWidth;
  const h = pop.offsetHeight;
  pop.style.left = `${Math.max(8, Math.min(r.right - w, window.innerWidth - w - 8))}px`;
  pop.style.top = `${r.bottom + 4 + h > window.innerHeight ? Math.max(8, r.top - h - 4) : r.bottom + 4}px`;
  setup?.(pop);
  const outside = (e) => { if (!pop.contains(e.target) && e.target !== anchor) closePopover(); };
  setTimeout(() => document.addEventListener('mousedown', outside), 0);
  popoverCleanup = () => document.removeEventListener('mousedown', outside);
}

function closePopover() {
  popoverCleanup?.();
  popoverCleanup = null;
  const pop = $('#popover');
  pop.hidden = true;
  pop.innerHTML = '';
}

function showReactionPicker(anchor, m) {
  showPopover(anchor, `<div class="emoji-grid">${QUICK_REACTIONS.map((n) => `<button data-emoji="${n}" title=":${n}:">${emoji(n)}</button>`).join('')}</div>`, (pop) => {
    pop.addEventListener('click', (e) => {
      const b = e.target.closest('[data-emoji]');
      if (!b) return;
      closePopover();
      toggleReaction(m, b.dataset.emoji, true);
    });
  });
}

async function toggleReaction(m, name, forceAdd = false) {
  const me = account(m.account_id)?.self_user_id;
  const mine = m.reactions?.find((r) => r.emoji === name)?.users.includes(me);
  if (forceAdd && mine) return;
  try {
    await invoke('react', { account_id: m.account_id, channel_id: m.channel_id, message_id: m.id, emoji: name, add: !mine });
  } catch (e) {
    toast(String(e));
  }
}

/** The level a conversation gets without one of its own: workspace override, else global. */
function defaultLevel(ch) {
  const n = S.settings.notifications;
  const k = isDirect(ch) ? 'dm_level' : 'channel_level';
  return n.accounts?.[ch.account_id]?.[k] || n[k];
}

/** Notification settings of one conversation (header bell, or right-click), docked on the right. */
async function showPrefs(a = S.open?.a, c = S.open?.c) {
  if (!S.channels.has(key(a, c))) return;
  if (S.panel?.type === 'notify' && S.panel.a === a && S.panel.c === c) { S.panel = null; schedule('panel', 'header'); return; }
  closePopover();
  const prefs = await invoke('get_channel_prefs', { account_id: a, channel_id: c });
  S.panel = { type: 'notify', a, c, prefs };
  renderPanel(true);
  schedule('header');
}

function notifyPanelHtml(p) {
  const ch = S.channels.get(key(p.a, p.c));
  if (!ch) return '';
  const NAMES = { alert: 'Alert', dot: 'Dot', silent: 'Silent' };
  const sel = (k, value, opts) => `<select data-k="${k}">${opts.map(([v, l]) => `<option value="${v}" ${String(value ?? '') === v ? 'selected' : ''}>${l}</option>`).join('')}</select>`;
  const pr = p.prefs;
  return `<div class="side-head">${glyph(ch)}<div><b>${esc(ch.display_name)}</b><div class="muted">${esc(account(p.a)?.name || '')}</div></div></div>
    <div class="side-label">Level</div>
    <div class="seg big lv" data-k="level">${[['', `Default (${NAMES[defaultLevel(ch)]})`], ['alert', 'Alert'], ['dot', 'Dot'], ['silent', 'Off']]
      .map(([v, l]) => `<button data-v="${v}" class="${(pr.level || '') === v ? 'on' : ''}">${l}</button>`).join('')}</div>
    <p class="side-note">Alert: popup, sound and dot. Dot: tray and taskbar dot only. Off: nothing; the conversation is dimmed in the sidebar and left out of Home.</p>
    <div class="side-label">Fine-tune</div>
    <div class="form-row"><label>Tray and taskbar dot</label>${sel('tray', pr.tray, [['', 'From level'], ['true', 'On'], ['false', 'Off']])}</div>
    <div class="form-row"><label>Popup</label>${sel('popup', pr.popup, [['', 'From level'], ['true', 'On'], ['false', 'Off']])}</div>
    <div class="form-row"><label>Sound</label>${sel('sound', pr.sound, [['', 'Default'], ['default', 'Gravitational wave'], ['beep', 'Beep'], ['none', 'None']])}</div>
    <div class="form-row"><label>Popup color</label><span><input type="checkbox" data-k="color-on" ${pr.color ? 'checked' : ''}>
      <input type="color" data-k="color" value="${esc(pr.color || account(p.a)?.color || '#888888')}"></span></div>`;
}

/** Reads the notification panel back into prefs and saves them. */
async function saveNotifyPanel() {
  const p = S.panel;
  if (p?.type !== 'notify') return;
  const el = $('#panel');
  const v = (k) => el.querySelector(`[data-k="${k}"]`).value;
  const bool = (s) => (s === '' ? null : s === 'true');
  p.prefs = {
    level: el.querySelector('.seg[data-k="level"] .on')?.dataset.v || null,
    tray: bool(v('tray')),
    popup: bool(v('popup')),
    sound: v('sound') || null,
    color: el.querySelector('[data-k="color-on"]').checked ? v('color') : null,
  };
  try { await invoke('set_channel_prefs', { account_id: p.a, channel_id: p.c, prefs: p.prefs }); } catch (e) { toast(String(e)); }
}

// ------------------------------------------------------------ quick switcher, image viewer

function openSwitcher() {
  const ov = $('#overlay');
  ov.className = '';
  ov.hidden = false;
  ov.innerHTML = '<div class="switcher"><input placeholder="Jump to a conversation..." spellcheck="false"><div class="results"></div></div>';
  const input = ov.querySelector('input');
  const results = ov.querySelector('.results');
  let items = [];
  let sel = 0;
  const draw = () => {
    const q = input.value.trim().toLowerCase();
    items = [...S.channels.values()]
      .filter((c) => account(c.account_id)?.enabled && (!q || c.display_name.toLowerCase().includes(q)))
      .sort((x, y) => y.last_activity - x.last_activity)
      .slice(0, 30);
    sel = Math.min(sel, Math.max(0, items.length - 1));
    results.innerHTML = items.map((c, i) => rowHtml(c, true).replace('class="row', `class="row ${i === sel ? 'sel' : ''}`)).join('');
  };
  input.addEventListener('input', () => { sel = 0; draw(); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { sel = Math.min(sel + 1, items.length - 1); draw(); e.preventDefault(); }
    if (e.key === 'ArrowUp') { sel = Math.max(sel - 1, 0); draw(); e.preventDefault(); }
    if (e.key === 'Enter' && items[sel]) { closeOverlay(); openChannel(items[sel].account_id, items[sel].id); }
  });
  results.addEventListener('click', (e) => {
    const r = e.target.closest('.row');
    if (r) { closeOverlay(); openChannel(r.dataset.a, r.dataset.c); }
  });
  ov.addEventListener('mousedown', (e) => { if (e.target === ov) closeOverlay(); }, { once: true });
  draw();
  input.focus();
}

function closeOverlay() {
  const ov = $('#overlay');
  ov.hidden = true;
  ov.innerHTML = '';
  ov.className = '';
}

function openViewer(src) {
  const v = document.createElement('div');
  v.className = 'viewer';
  v.innerHTML = `<img src="${esc(src)}" alt="">`;
  v.addEventListener('click', () => v.remove());
  document.body.appendChild(v);
}

// ------------------------------------------------------------ sound

let audio = null;
// The default sound: a binary black-hole merger chirp (0.6 s, about 54 KB), loaded on first use.
let chirp = null;

/** `name`: 'default' (gravitational-wave chirp), 'beep' or 'none'. Volume from Settings. */
export function playSound(name) {
  if (!name || name === 'none') return;
  const volume = Math.max(0, Math.min(100, S.settings?.notifications?.volume ?? 70)) / 100;
  if (!volume) return;
  try {
    if (name !== 'beep') {
      chirp ||= new Audio('sounds/gw-chirp.wav');
      chirp.volume = volume;
      chirp.currentTime = 0;
      chirp.play().catch(() => {});
      return;
    }
    audio = audio || new AudioContext();
    audio.resume();
    const t = audio.currentTime;
    const osc = audio.createOscillator();
    const gain = audio.createGain();
    osc.frequency.setValueAtTime(880, t);
    osc.frequency.exponentialRampToValueAtTime(660, t + 0.12);
    gain.gain.setValueAtTime(0.0001, t);
    gain.gain.exponentialRampToValueAtTime(0.2 * volume, t + 0.01);
    gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.25);
    osc.connect(gain).connect(audio.destination);
    osc.start(t);
    osc.stop(t + 0.3);
  } catch { /* no audio device */ }
}

// ------------------------------------------------------------ layout

function restoreLayout() {
  const l = load('layout') || {};
  if (l.sb) document.documentElement.style.setProperty('--sb-w', l.sb);
  if (l.panel) document.documentElement.style.setProperty('--panel-w', l.panel);
}

function initResizers() {
  document.querySelectorAll('.resizer').forEach((r) => r.addEventListener('mousedown', (e) => {
    e.preventDefault();
    const isSidebar = r.dataset.target === 'sidebar';
    const target = isSidebar ? $('#sidebar') : $('#panel');
    const prop = isSidebar ? '--sb-w' : '--panel-w';
    const [min, max] = isSidebar ? [170, 480] : [260, 760];
    const startX = e.clientX;
    const startW = target.getBoundingClientRect().width;
    r.classList.add('dragging');
    const move = (ev) => {
      const w = isSidebar ? startW + ev.clientX - startX : startW - (ev.clientX - startX);
      document.documentElement.style.setProperty(prop, `${Math.max(min, Math.min(max, w))}px`);
    };
    const up = () => {
      r.classList.remove('dragging');
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      const st = document.documentElement.style;
      save('layout', { sb: st.getPropertyValue('--sb-w'), panel: st.getPropertyValue('--panel-w') });
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  }));
}

// ------------------------------------------------------------ wiring

function findMessage(id, inPanel) {
  const list = inPanel ? S.panel?.messages || S.panel?.hits || [] : S.messages;
  return list.find((m) => m.id === id);
}

/** `#name` in a message: that conversation, in the message's workspace first. */
function openChannelRef(acc, name) {
  const low = name.toLowerCase();
  const all = [...S.channels.values()].filter((c) => !isDirect(c)
    && [c.name, c.display_name, c.id].some((n) => (n || '').toLowerCase() === low));
  const c = all.find((x) => x.account_id === acc) || all[0];
  if (c) openChannel(c.account_id, c.id);
  else toast(`#${name} is not one of your conversations`);
}

/** `@handle` in a message: that person's profile. The message's mention list settles who it is. */
async function openMention(acc, handle, ids = [], back = null) {
  const people = (ids || []).filter((id) => id !== '!all');
  if (people.length === 1) { openPerson(acc, people[0], back); return; }
  const low = handle.toLowerCase();
  const found = await invoke('find_users', { account_id: acc, query: handle }).catch(() => []);
  const u = found.find((x) => people.includes(x.id)) || found.find((x) => x.name.toLowerCase() === low)
    || found.find((x) => (x.display_name || '').toLowerCase().startsWith(low));
  if (u) openPerson(acc, u.id, back);
  else toast(`No one called @${handle} found in this workspace`);
}

function onMessageAreaClick(e, inPanel) {
  const link = e.target.closest('a[data-url]');
  if (link) { e.preventDefault(); invoke('open_url', { url: link.dataset.url }).catch((x) => toast(String(x))); return; }
  const img = e.target.closest('img[data-full]');
  if (img) { openViewer(img.dataset.full); return; }
  const who = e.target.closest('[data-person]');
  if (who) { openPerson(who.dataset.pa, who.dataset.person, inPanel ? S.panel : null); return; }
  const ref = e.target.closest('.chan-ref, .mention[data-handle]');
  const refMsg = ref && findMessage(e.target.closest('.msg')?.dataset.id, inPanel);
  if (ref && refMsg) {
    if (ref.dataset.chan) openChannelRef(refMsg.account_id, ref.dataset.chan);
    else openMention(refMsg.account_id, ref.dataset.handle, refMsg.mentions, inPanel ? S.panel : null);
    return;
  }
  const btn = e.target.closest('[data-act]');
  const row = e.target.closest('.msg');
  if (!btn || !row) return;
  const m = findMessage(row.dataset.id, inPanel);
  if (!m) return;
  switch (btn.dataset.act) {
    case 'react': showReactionPicker(btn, m); break;
    case 'toggle-reaction': toggleReaction(m, btn.dataset.emoji); break;
    case 'thread': openThread(m.id); break;
    case 'edit': startEdit(m); break;
    case 'delete':
      if (btn.dataset.armed) {
        invoke('delete_message', { account_id: m.account_id, channel_id: m.channel_id, message_id: m.id }).catch((x) => toast(String(x)));
      } else {
        btn.dataset.armed = '1';
        btn.style.color = 'var(--danger)';
        btn.title = 'Click again to delete';
        setTimeout(() => { delete btn.dataset.armed; btn.style.color = ''; }, 3000);
      }
      break;
  }
}

function bindUi() {
  initResizers();

  const rail = $('#rail');
  rail.addEventListener('click', (e) => {
    if (e.target.closest('[data-act="settings"]')) return toggleSettings();
    const item = e.target.closest('.rail-item');
    if (item) selectRail(item.dataset.id);
  });
  rail.addEventListener('contextmenu', (e) => {
    const a = account(e.target.closest('.rail-item')?.dataset.id);
    if (!a) return;
    e.preventDefault();
    showWorkspaceMenu(e.target.closest('.rail-item'), a);
  });
  // Workspaces reorder by dragging (Home and All stay on top).
  const workspaceAt = (e) => e.target.closest('.rail-item:not(.all):not(.home):not(.gear)');
  let dragged = null;
  rail.addEventListener('dragstart', (e) => {
    dragged = workspaceAt(e)?.dataset.id || null;
    if (!dragged) { e.preventDefault(); return; }
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', dragged);
  });
  rail.addEventListener('dragover', (e) => {
    const item = workspaceAt(e);
    if (!item || !dragged) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    rail.querySelectorAll('.drag-over').forEach((x) => x.classList.remove('drag-over'));
    item.classList.add('drag-over');
  });
  rail.addEventListener('drop', (e) => {
    e.preventDefault();
    const target = workspaceAt(e)?.dataset.id;
    const ids = enabledAccounts().map((a) => a.id);
    if (!dragged || !target || dragged === target) return;
    // Dropping onto an icon takes its slot: before it when moving up, after it when moving down.
    const to = ids.indexOf(target);
    ids.splice(ids.indexOf(dragged), 1);
    ids.splice(to, 0, dragged);
    const disabled = S.accounts.filter((a) => !a.enabled).map((a) => a.id);
    invoke('reorder_accounts', { ids: ids.concat(disabled) }).catch((x) => toast(String(x)));
  });
  rail.addEventListener('dragend', () => { dragged = null; rail.querySelectorAll('.drag-over').forEach((x) => x.classList.remove('drag-over')); });

  const list = $('#channel-list');
  list.addEventListener('click', (e) => {
    if (e.target.closest('[data-act="settings"]')) { e.preventDefault(); return toggleSettings(); }
    const sec = e.target.closest('.section[data-sec]');
    if (sec) { toggleSection(sec.dataset.sec); return; }
    const r = e.target.closest('.row');
    if (r) openChannel(r.dataset.a, r.dataset.c);
  });
  list.addEventListener('contextmenu', (e) => {
    const r = e.target.closest('.row');
    const sec = e.target.closest('.section[data-sec]');
    if (!r && !sec) return;
    e.preventDefault();
    if (r) {
      const c = S.channels.get(key(r.dataset.a, r.dataset.c));
      if (c) showRowMenu(r, c);
    } else {
      showSectionMenu(sec, sec.dataset.sec);
    }
  });

  // Drag a conversation onto Favorites, an Agora group, or any other section (which takes it
  // out of its group / favorites). Rows inside a section count as that section.
  const sectionAt = (el) => {
    for (let n = el?.closest('.section[data-sec], .row, .empty-group'); n; n = n.previousElementSibling) {
      if (n.matches?.('.section[data-sec]')) return n;
    }
    return null;
  };
  let draggedRow = null;
  list.addEventListener('dragstart', (e) => {
    const r = e.target.closest('.row');
    if (!r) return;
    draggedRow = { a: r.dataset.a, c: r.dataset.c, from: sectionAt(r)?.dataset.sec };
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', r.querySelector('.name')?.textContent || '');
    r.classList.add('dragging');
  });
  list.addEventListener('dragover', (e) => {
    const sec = draggedRow && sectionAt(e.target);
    if (!sec) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    list.querySelectorAll('.drop-target').forEach((x) => x !== sec && x.classList.remove('drop-target'));
    sec.classList.add('drop-target');
  });
  list.addEventListener('drop', (e) => {
    const sec = draggedRow && sectionAt(e.target);
    if (!sec) return;
    e.preventDefault();
    const c = S.channels.get(key(draggedRow.a, draggedRow.c));
    const to = sec.dataset.sec;
    if (!c || to === draggedRow.from) return;
    if (to === 'fav') { if (!c.starred) setStarred(c, true); return; }
    // Leaving Favorites by dragging means un-starring (synced with the service).
    if (draggedRow.from === 'fav' && c.starred) setStarred(c, false);
    const now = { ...c, starred: draggedRow.from === 'fav' ? false : c.starred };
    if (to.startsWith('g:')) moveToGroup(now, to.slice(2));
    else if (c.local_group) moveToGroup(now, null);
  });
  list.addEventListener('dragend', () => {
    draggedRow = null;
    list.querySelectorAll('.drop-target, .dragging').forEach((x) => x.classList.remove('drop-target', 'dragging'));
  });

  $('#home').addEventListener('click', (e) => {
    const item = e.target.closest('.home-item');
    if (item) { openChannel(item.dataset.a, item.dataset.c); return; }
    const head = e.target.closest('[data-rail]');
    if (head) selectRail(head.dataset.rail);
  });

  // Workspace numbers in the bar show only while Ctrl is held (Ctrl+0..9 jump there).
  const ctrl = (on) => document.body.classList.toggle('ctrl-held', on);
  document.addEventListener('keydown', (e) => { if (e.key === 'Control') ctrl(true); });
  document.addEventListener('keyup', (e) => { if (e.key === 'Control') ctrl(false); });
  window.addEventListener('blur', () => ctrl(false));

  $('#me-bar').addEventListener('click', (e) => {
    const b = e.target.closest('[data-act="me-menu"]');
    if (b) showMeMenu();
  });

  // Broken avatar (offline, removed user): show the initials instead.
  document.addEventListener('error', (e) => {
    const img = e.target;
    if (img.tagName === 'IMG' && img.dataset.fallback !== undefined) img.replaceWith(document.createTextNode(img.dataset.fallback));
  }, true);

  const search = $('#search');
  search.addEventListener('focus', drawSuggestions);
  search.addEventListener('blur', () => setTimeout(() => { $('#search-pop').hidden = true; }, 150));
  search.addEventListener('input', () => { searchUi.sel = -1; drawSuggestions(); });
  search.addEventListener('keydown', (e) => {
    const n = searchUi.items.length;
    if (e.key === 'ArrowDown' && n) { searchUi.sel = (searchUi.sel + 1) % n; drawSuggestions(); e.preventDefault(); }
    else if (e.key === 'ArrowUp' && n) { searchUi.sel = (searchUi.sel - 1 + n) % n; drawSuggestions(); e.preventDefault(); }
    else if (e.key === 'Tab' && n) { applySuggestion(Math.max(0, searchUi.sel)); e.preventDefault(); }
    else if (e.key === 'Enter') {
      e.preventDefault();
      if (searchUi.sel >= 0) { applySuggestion(searchUi.sel); return; }
      $('#search-pop').hidden = true;
      runSearch(search.value);
    } else if (e.key === 'Escape') { e.stopPropagation(); search.blur(); }
  });
  $('#search-pop').addEventListener('mousedown', (e) => {
    const b = e.target.closest('button[data-i]');
    if (!b) return;
    e.preventDefault(); // keep focus in the input
    applySuggestion(Number(b.dataset.i));
  });

  $('#status-line').addEventListener('click', (e) => {
    const b = e.target.closest('[data-act]');
    if (b?.dataset.act === 'reconnect') invoke('reconnect');
    if (b?.dataset.act === 'relogin') relogin(b.dataset.id);
  });

  $('#ch-header').addEventListener('click', (e) => {
    const link = e.target.closest('a[data-url]');
    if (link) { e.preventDefault(); invoke('open_url', { url: link.dataset.url }).catch((x) => toast(String(x))); return; }
    const b = e.target.closest('[data-act]');
    if (!b || !S.open) return;
    const c = S.channels.get(key(S.open.a, S.open.c));
    if (b.dataset.act === 'star') setStarred(c, !c.starred);
    if (b.dataset.act === 'prefs') showPrefs();
    if (b.dataset.act === 'gallery') { S.panel = S.panel?.type === 'gallery' ? null : { type: 'gallery' }; schedule('panel', 'header'); }
    if (b.dataset.act === 'members') {
      if (S.panel?.type === 'members' && S.panel.c === c.id) { S.panel = null; schedule('panel', 'header'); } else openMembers(c.account_id, c.id);
    }
    if (b.dataset.act === 'peer') {
      const u = c.peers[0]?.id;
      if (S.panel?.type === 'person' && S.panel.u === u) { S.panel = null; schedule('panel', 'header'); } else if (u) openPerson(c.account_id, u);
    }
  });

  const box = $('#messages');
  box.addEventListener('click', (e) => onMessageAreaClick(e, false));
  box.addEventListener('scroll', loadOlder, { passive: true });
  // Following the bottom stops when the reader scrolls up, and resumes at the bottom.
  box.addEventListener('scroll', () => { S.stick = box.scrollHeight - box.scrollTop - box.clientHeight < 60; }, { passive: true });
  box.addEventListener('load', followBottom, true);

  const panel = $('#panel');
  panel.addEventListener('click', (e) => {
    if (e.target.closest('[data-act="close-panel"]')) { S.panel = null; schedule('panel', 'header'); return; }
    if (e.target.closest('[data-act="panel-back"]') && S.panel?.back) { S.panel = S.panel.back; renderPanel(true); schedule('header'); return; }
    // People
    const copy = e.target.closest('[data-copy]');
    if (copy) { navigator.clipboard.writeText(copy.dataset.copy).then(() => toast('Copied'), () => toast('Could not copy')); return; }
    const member = e.target.closest('.member[data-u]');
    if (member && S.panel?.type === 'members') { openPerson(S.panel.a, member.dataset.u, S.panel); return; }
    if (e.target.closest('[data-act="members-more"]') && S.panel?.type === 'members') { loadMembers(S.panel); return; }
    const dm = e.target.closest('[data-act="person-dm"]');
    if (dm && S.panel?.type === 'person') { openChannel(S.panel.a, dm.dataset.c); return; }
    if (e.target.closest('[data-act="person-mention"]') && S.panel?.profile) { mentionPerson(S.panel.profile); return; }
    // Account panel
    const presence = e.target.closest('[data-presence]');
    if (presence && S.panel?.type === 'account') {
      invoke('set_presence', { account_id: S.panel.a, presence: presence.dataset.presence }).catch((x) => toast(String(x)));
      return;
    }
    if (e.target.closest('[data-act="open-web"]') && S.panel?.type === 'account') {
      invoke('open_url', { url: account(S.panel.a)?.server_url }).catch((x) => toast(String(x)));
      return;
    }
    if (e.target.closest('[data-act="acc-settings"]')) { S.panel = null; schedule('panel'); toggleSettings('accounts'); return; }
    // Notification panel: the level buttons
    const lv = e.target.closest('.seg[data-k="level"] button');
    if (lv && S.panel?.type === 'notify') {
      lv.parentElement.querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === lv));
      saveNotifyPanel();
      return;
    }
    const scope = e.target.closest('[data-scope]');
    if (scope && S.panel?.type === 'search') { runSearch(S.panel.q, scope.dataset.scope || null, S.panel.range); return; }
    const preset = e.target.closest('[data-range]');
    if (preset && S.panel?.type === 'search') {
      const kind = preset.dataset.range;
      // "Dates..." only opens the two fields; the search reruns once a date is picked.
      if (kind === 'custom') { S.panel.range = { kind: 'custom', ...(S.panel.range?.kind === 'custom' ? S.panel.range : {}) }; renderPanel(true); return; }
      setSearchDates({ kind });
      return;
    }
    if (e.target.closest('[data-act="search-remote"]') && S.panel?.type === 'search') { searchRemote(S.panel); return; }
    const hit = e.target.closest('.hit');
    if (hit && !e.target.closest('a[data-url], img[data-full], [data-act="toggle-reaction"], [data-person]')) {
      jumpTo(hit.dataset.a, hit.dataset.c, hit.dataset.id);
      return;
    }
    const link = e.target.closest('a[data-url]');
    if (link && !e.target.closest('.msg')) { e.preventDefault(); invoke('open_url', { url: link.dataset.url }); return; }
    const img = e.target.closest('img[data-full]');
    if (img && !e.target.closest('.msg')) { openViewer(img.dataset.full); return; }
    onMessageAreaClick(e, true);
  });
  panel.addEventListener('change', (e) => {
    if (S.panel?.type === 'notify' && e.target.closest('[data-k]')) saveNotifyPanel();
    if (S.panel?.type === 'search' && e.target.dataset.edge) {
      const r = { ...S.panel.range, kind: 'custom', [e.target.dataset.edge]: e.target.value };
      setSearchDates(r);
    }
  });
  panel.addEventListener('input', (e) => {
    if (e.target.id === 'member-filter' && S.panel?.type === 'members') { S.panel.filter = e.target.value; drawMembers(S.panel); }
  });
  panel.addEventListener('keydown', (e) => {
    if (e.target.id !== 'thread-input') return;
    if (autocompleteKey(e) || formatShortcut(e, e.target)) return;
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); sendThreadReply(); }
  });
  panel.addEventListener('input', (e) => {
    if (e.target.id !== 'thread-input') return;
    autosize(e.target);
    updateAutocomplete(e.target, S.panel?.a);
  });

  const input = $('#input');
  renderFormatBar();
  let previewTimer = 0;
  input.addEventListener('input', () => {
    autosize(input);
    updateAutocomplete(input, S.open?.a);
    if (S.previewOn) { clearTimeout(previewTimer); previewTimer = setTimeout(updatePreview, 250); }
  });
  input.addEventListener('blur', () => setTimeout(() => { if (document.activeElement !== AC.input) closeAutocomplete(); }, 150));
  input.addEventListener('keydown', (e) => {
    if (autocompleteKey(e) || formatShortcut(e, input)) return;
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); submit(); return; }
    if (e.key === 'Escape') {
      // Cancelling an edit or closing the preview uses up Esc; otherwise it closes the thread panel.
      if (S.editing || S.previewOn) { e.preventDefault(); if (S.editing) cancelEdit(); else hideInfo(); }
      return;
    }
    if (e.key === 'ArrowUp' && !input.value) {
      const mine = [...S.messages].reverse().find((m) => m.is_self && !m.deleted);
      if (mine) { e.preventDefault(); startEdit(mine); }
    }
  });
  // Ctrl+V of a screenshot (or copied files): attach instead of pasting nothing.
  input.addEventListener('paste', (e) => {
    const files = [...(e.clipboardData?.files || [])];
    if (!files.length || e.clipboardData.getData('text/plain')) return;
    e.preventDefault();
    addFiles(files);
  });
  $('#fmt-bar').addEventListener('mousedown', (e) => { if (e.target.closest('button')) e.preventDefault(); }); // keep the selection
  $('#fmt-bar').addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b) return;
    if (b.dataset.fmt) applyFormat(input, b.dataset.fmt);
    else if (b.dataset.act === 'emoji') showEmojiPicker(b, input);
    else if (b.dataset.act === 'attach') {
      const pick = document.createElement('input');
      pick.type = 'file';
      pick.multiple = true;
      pick.addEventListener('change', () => addFiles([...pick.files]));
      pick.click();
    } else if (b.id === 'preview-btn') togglePreview();
  });
  $('#attach-tray').addEventListener('click', (e) => {
    const att = e.target.closest('.att');
    const b = e.target.closest('[data-att]');
    if (!att || !b) return;
    const id = Number(att.dataset.id);
    if (b.dataset.att === 'edit') editPending(id);
    else {
      const i = pending.findIndex((p) => p.id === id);
      if (i >= 0) { const [p] = pending.splice(i, 1); if (p.url) URL.revokeObjectURL(p.url); renderTray(); }
    }
  });
  $('#attach-tray').addEventListener('dblclick', (e) => {
    const att = e.target.closest('.att');
    if (att) editPending(Number(att.dataset.id));
  });
  $('#ac-pop').addEventListener('mousedown', (e) => {
    const b = e.target.closest('button[data-i]');
    if (!b) return;
    e.preventDefault();
    acceptAutocomplete(Number(b.dataset.i));
  });

  // Drag and drop files onto the conversation to attach them. Elsewhere a dropped file
  // must not make the window navigate to it.
  const main = $('#main');
  const hasFiles = (e) => [...(e.dataTransfer?.types || [])].includes('Files');
  let dragDepth = 0;
  main.addEventListener('dragenter', (e) => { if (hasFiles(e) && S.open) { dragDepth++; $('#drop-hint').hidden = false; } });
  main.addEventListener('dragleave', (e) => { if (hasFiles(e) && --dragDepth <= 0) { dragDepth = 0; $('#drop-hint').hidden = true; } });
  main.addEventListener('dragover', (e) => { if (hasFiles(e) && S.open) { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; } });
  main.addEventListener('drop', (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    dragDepth = 0;
    $('#drop-hint').hidden = true;
    if (S.open) { addFiles([...e.dataTransfer.files]); input.focus(); }
  });
  document.addEventListener('dragover', (e) => { if (hasFiles(e)) e.preventDefault(); });
  document.addEventListener('drop', (e) => { if (hasFiles(e)) e.preventDefault(); });

  document.addEventListener('keydown', (e) => {
    if (e.ctrlKey && !e.shiftKey && !e.altKey && /^[0-9]$/.test(e.key)) {
      e.preventDefault();
      const n = Number(e.key);
      if (n === 0) selectRail('home');
      else if (enabledAccounts()[n - 1]) selectRail(enabledAccounts()[n - 1].id);
      return;
    }
    if (e.ctrlKey && e.key.toLowerCase() === 'k') { e.preventDefault(); openSwitcher(); return; }
    if (e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      // Home has no sidebar (where the search box is): go to All first.
      if (S.rail === 'home') { selectRail('all'); document.body.classList.remove('home-mode'); }
      $('#search').focus();
      $('#search').select();
      return;
    }
    if (e.ctrlKey && e.key === ',') { e.preventDefault(); toggleSettings(); return; }
    if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'p') { e.preventDefault(); togglePreview(); return; }
    if (e.key === 'Escape') {
      const viewer = document.querySelector('.viewer');
      if (viewer) { viewer.remove(); return; }
      if (!$('#popover').hidden) closePopover();
      else if (settingsOpen()) { closeSettings(); schedule('rail'); }
      else if (!$('#overlay').hidden) closeOverlay();
      else if (S.panel && !e.defaultPrevented && !document.querySelector('.img-editor')) { S.panel = null; schedule('panel', 'header'); }
    }
  });
}

init().catch((e) => {
  document.body.innerHTML = `<pre style="padding:20px;white-space:pre-wrap">Agora failed to start:\n${esc(e)}</pre>`;
});
