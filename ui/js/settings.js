// Settings screen. Inputs carry `data-path` (a dotted path into S.settings) and
// are saved automatically; everything else is handled by `data-act`.
import { invoke, esc, initials, toast, icon, OS, KEYCHAIN } from './api.js';
import { S, applyTheme, refreshAll, kindName, playSound, relogin, accountIcon, FONTS, ACCENTS, TEXT_SIZES, resetSidebarWidth, checkForUpdate, installUpdate } from './app.js';

const SECTIONS = [
  ['accounts', 'Accounts', 'people'],
  ['notifications', 'Notifications', 'bell'],
  ['keywords', 'Mentions', 'at'],
  ['dnd', 'Do not disturb', 'moon'],
  ['appearance', 'Appearance', 'palette'],
  ['privacy', 'Privacy & storage', 'shield'],
  ['advanced', 'Advanced', 'sliders'],
];
const DAYS = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];
const LEVELS = [['alert', 'Alert'], ['dot', 'Dot'], ['silent', 'Silent']];
const SOUNDS = [['default', 'Gravitational wave'], ['beep', 'Beep'], ['none', 'None']];

let current = 'accounts';
let bound = false;
let saveTimer = 0;
// Sign-in form: which service, last server typed, waiting for the login window.
const signin = { kind: null, server: '', busy: false };
// Per service: the sign-in command and the token fallback (Rocket.Chat needs the user id too,
// Slack the web session's token and cookie).
const SIGNIN = {
  slack: {
    label: 'Slack',
    command: 'slack_sign_in',
    serverOptional: true,
    placeholder: 'Workspace (optional), e.g. acme',
    intro: `Opens Slack's own sign-in page in a separate window (Google, SSO, email code and passwords all work).
      Every workspace you sign in to there is added, each as its own account. Agora keeps the web session
      (token and cookie) in ${KEYCHAIN}; nothing has to be installed in the workspace.
      <b>To add another workspace</b>, type its name: if the Slack session Agora already has is signed in
      to it, it is added at once without a window.`,
    tokenTitle: 'Paste a web session instead',
    tokenFields: [['token', 'xoxc- token'], ['cookie', 'd cookie (xoxd-...)']],
    tokenHelp: 'From a browser signed in to Slack: see the README, section on the Slack probe.',
  },
  mattermost: {
    label: 'Mattermost',
    command: 'mattermost_sign_in',
    tokenFields: [['token', 'Token']],
    tokenHelp: 'Profile &rarr; Security &rarr; Personal access tokens, if your server allows them.',
  },
  rocketchat: {
    label: 'Rocket.Chat',
    command: 'rocketchat_sign_in',
    tokenFields: [['user_id', 'User ID'], ['token', 'Token']],
    tokenHelp: 'Avatar &rarr; Account &rarr; Personal Access Tokens: the page shows both values once.',
  },
};

function signinForm(k) {
  const off = signin.busy ? 'disabled' : '';
  return `<div class="signin">
    <div class="form-row" style="justify-content:flex-start">
      <input type="text" id="signin-server" placeholder="${esc(k.placeholder || 'chat.example.org')}" value="${esc(signin.server)}" style="width:300px" ${off}>
      <button class="btn primary" data-act="signin" ${off}>${signin.busy ? 'Waiting for sign-in...' : `Sign in to ${k.label}`}</button>
    </div>
    <p class="muted">${k.intro || `Opens your server's own login page in a separate window, so SSO, two-factor and passwords
      work as in the browser. Agora keeps only the session token, in ${KEYCHAIN}.
      A link to a channel or the login page works too.`}</p>
    <details><summary class="muted">${k.tokenTitle || 'Use a personal access token instead'}</summary>
      <div class="form-row" style="justify-content:flex-start">
        ${k.tokenFields.map(([f, label]) => `<input type="${f === 'token' ? 'password' : 'text'}" data-token-field="${f}" placeholder="${label}" style="width:220px" ${off}>`).join('')}
        <button class="btn" data-act="signin-token" ${off}>Add</button>
      </div>
      <p class="muted">${k.tokenHelp}</p>
    </details></div>`;
}

const overlay = () => document.getElementById('overlay');
export const settingsOpen = () => overlay().classList.contains('full') && !overlay().hidden;

export function openSettings(section) {
  if (section) current = section;
  const ov = overlay();
  ov.className = 'full';
  ov.hidden = false;
  if (!bound) bind(ov);
  render();
}

export function closeSettings() {
  const ov = overlay();
  ov.hidden = true;
  ov.innerHTML = '';
  ov.className = '';
}

function persist() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try { await invoke('save_settings', { settings: S.settings }); } catch (e) { toast(String(e)); }
  }, 300);
}

const getPath = (path) => path.split('.').reduce((o, k) => o?.[k], S.settings);
function setPath(path, value) {
  const keys = path.split('.');
  const last = keys.pop();
  keys.reduce((o, k) => o[k], S.settings)[last] = value;
}

const toggle = (path, disabled = false) =>
  `<input type="checkbox" class="switch" data-path="${path}" ${getPath(path) ? 'checked' : ''} ${disabled ? 'disabled' : ''}>`;
const opt = (label, hint, control, extra = '') =>
  `<div class="opt"><div class="lbl">${label}${hint ? `<div class="hint">${hint}</div>` : ''}${extra}</div><div>${control}</div></div>`;
const select = (path, options) =>
  `<select data-path="${path}">${options.map(([v, l]) => `<option value="${v}" ${getPath(path) === v ? 'selected' : ''}>${l}</option>`).join('')}</select>`;
/** One radio choice drawn as a card with a live preview (`path` is a dotted settings path). */
const choice = (path, value, label, preview) => `<label class="choice ${getPath(path) === value ? 'on' : ''}">
    <input type="radio" name="${path}" data-path="${path}" value="${value}" ${getPath(path) === value ? 'checked' : ''}>
    ${preview}<span class="lbl">${label}</span></label>`;
// The open panel under one workspace card: 'logo', 'more' or 'remove'.
let accPanel = { id: null, kind: null };
const CONN = { online: ['ok', 'Connected'], offline: ['warn', 'Offline'], auth_expired: ['bad', 'Signed out'], connecting: ['warn', 'Connecting'] };
const hostOf = (url) => String(url || '').replace(/^\w+:\/\//, '').split('/')[0];

/** One workspace: logo (click to change), name, color, on/off, and a menu for the rest. */
function accountCard(a) {
  const [cls, status] = a.enabled ? CONN[S.conn[a.id]?.state] || ['warn', 'Not connected'] : ['', 'Disabled'];
  const open = accPanel.id === a.id ? accPanel.kind : null;
  let panel = '';
  if (open === 'logo') {
    const initialsValue = a.icon || initials(a.name).replace(/&[^;]+;/g, '');
    panel = `<div class="acc-panel logo-choices">
      <button class="pick" data-act="logo-pick">${icon('image')}<span>Upload image</span></button>
      ${a.kind === 'demo' ? '' : `<button class="pick" data-act="logo-service">${icon('refresh')}<span>Workspace logo</span></button>`}
      <label class="pick initials ${a.icon_image === '' ? 'on' : ''}"><span class="rail-item" style="--c:${esc(a.color)}"><span class="ico">${esc(initialsValue.slice(0, 2))}</span></span>
        <input type="text" data-acc="initials" maxlength="2" value="${esc(initialsValue)}" title="Initials: one or two characters"></label>
    </div>`;
  } else if (open === 'more') {
    panel = `<div class="acc-panel">
      ${a.kind === 'demo' ? '' : `<button class="btn" data-act="relogin">${icon('refresh')} Sign in again</button>`}
      <button class="btn danger" data-act="remove">${icon('trash')} Remove...</button></div>`;
  } else if (open === 'remove') {
    panel = `<div class="acc-panel"><span class="muted">Remove ${esc(a.name)}?</span>
      <button class="btn" data-act="remove-keep">Keep its archive</button>
      <button class="btn danger" data-act="remove-purge">Delete archive too</button>
      <button class="btn" data-act="panel-close">Cancel</button></div>`;
  }
  return `<div class="acc-card ${a.enabled ? '' : 'off'} ${open ? 'open' : ''}" data-id="${esc(a.id)}">
    <div class="acc-line">
      <button class="logo-btn" data-act="logo-menu" title="Change logo">
        <span class="rail-item ${a.icon_image ? 'has-logo' : ''}" style="--c:${esc(a.color)}">${accountIcon(a)}</span><span class="pen">${icon('edit')}</span></button>
      <div class="acc-main">
        <input class="acc-name" type="text" data-acc="name" value="${esc(a.name)}" title="Rename" spellcheck="false">
        <div class="acc-sub"><i class="conn ${cls}"></i>${esc(status)} · ${esc(kindName(a.kind))}${a.kind === 'demo' ? '' : ` · ${esc(hostOf(a.server_url))}`}</div>
      </div>
      <label class="color-chip" style="--c:${esc(a.color)}" title="Color: left bar, badges, taskbar"><input type="color" data-acc="color" value="${esc(a.color)}"></label>
      <input type="checkbox" class="switch" data-acc="enabled" ${a.enabled ? 'checked' : ''} title="${a.enabled ? 'Enabled' : 'Disabled'}">
      <button class="icon-btn" data-act="acc-more" title="More">${icon('more')}</button>
    </div>${panel}</div>`;
}

const hhmm = (min) => `${String(Math.floor(min / 60)).padStart(2, '0')}:${String(min % 60).padStart(2, '0')}`;

const VIEWS = {
  accounts: () => `<h2>Accounts</h2>
    <div class="add-bar"><span>${icon('plus')}Add a workspace</span>
      ${Object.entries(SIGNIN).map(([kind, k]) => `<button class="btn ${signin.kind === kind ? 'primary' : ''}" data-act="add-account" data-kind="${kind}">${k.label}</button>`).join('')}
      <button class="btn" data-act="add-demo" title="Offline fake data for trying out the interface, notifications and tray">Demo</button>
    </div>
    ${signin.kind ? signinForm(SIGNIN[signin.kind]) : ''}
    <h3>Your workspaces</h3>
    ${S.accounts.map(accountCard).join('') || '<p class="muted">No accounts yet: add one above.</p>'}
    <p class="muted">Drag icons in the left bar to reorder them; this list follows. Ctrl+1 to Ctrl+9 jump to them.</p>`,

  notifications: () => {
    const n = S.settings.notifications;
    const seg = (attrs, current, withDefault) => `<div class="seg big" ${attrs}>${(withDefault ? [['', 'Default']] : []).concat(LEVELS)
      .map(([v, l]) => `<button data-v="${v}" class="${(current || '') === v ? 'on' : ''}">${l}</button>`).join('')}</div>`;
    return `<h2>Notifications</h2>
    <table class="levels-table">
      <thead><tr><th></th><th>Popup</th><th>Sound</th><th>Tray &amp; taskbar</th><th>Home</th><th>Sidebar</th></tr></thead>
      <tbody>
        <tr><th>Alert</th><td>Shown</td><td>Plays</td><td>Dot</td><td>Listed</td><td>Bold</td></tr>
        <tr><th>Dot</th><td class="no">–</td><td class="no">–</td><td>Dot</td><td>Listed</td><td>Bold</td></tr>
        <tr><th>Silent</th><td class="no">–</td><td class="no">–</td><td class="no">–</td><td class="no">–</td><td>Bold, dimmed</td></tr>
      </tbody>
    </table>
    <p class="muted">Mentions and keywords can raise any level to Alert (Settings → Mentions). Quiet hours hold popups back (Do not disturb).</p>
    <h3>Popups</h3>
    ${opt('Popup duration', 'Hovering a popup keeps it open.', `<input type="number" min="2" max="60" style="width:60px" data-path="notifications.popup_seconds" value="${getPath('notifications.popup_seconds')}"> s`)}
    ${opt('Sound', 'Played with popups. Can be overridden per conversation.', `${select('notifications.sound', SOUNDS)} <button class="btn" data-act="test-sound">Test</button>`)}
    ${opt('Volume', '', `<span class="vol"><input type="range" min="0" max="100" step="5" data-path="notifications.volume" value="${n.volume ?? 70}">
      <output>${n.volume ?? 70}%</output></span>`)}
    <h3>Global defaults</h3>
    <div class="lv-row"><span>Direct and group messages</span>${seg('data-level="dm_level"', n.dm_level, false)}</div>
    <div class="lv-row"><span>Channels</span>${seg('data-level="channel_level"', n.channel_level, false)}</div>
    <h3>Per workspace</h3>
    ${S.accounts.map((a) => {
      const own = n.accounts?.[a.id] || {};
      return `<div class="lv-ws"><span class="ws"><i class="svc" style="--c:${esc(a.color)}"></i>${esc(a.name)}</span>
        <div class="lv-row"><span>Direct</span>${seg(`data-ws="${esc(a.id)}" data-level="dm_level"`, own.dm_level, true)}</div>
        <div class="lv-row"><span>Channels</span>${seg(`data-ws="${esc(a.id)}" data-level="channel_level"`, own.channel_level, true)}</div></div>`;
    }).join('') || '<p class="muted">No accounts yet.</p>'}
    <p class="muted">Default follows the global setting. Single conversations: bell icon in the conversation header (it wins over both).</p>`;
  },

  keywords: () => {
    const kws = S.settings.notifications.keywords;
    const scope = (k, i) => `<select data-kw="${i}"><option value="">All accounts</option>${S.accounts.map((a) =>
      `<option value="${esc(a.id)}" ${k.accounts[0] === a.id ? 'selected' : ''}>${esc(a.name)}</option>`).join('')}</select>`;
    return `<h2>Mentions</h2>
      ${opt('Mentions override levels', '@mentions and @channel/@here alert you even in Dot-only or Silent conversations.', toggle('notifications.mentions_override'))}
      ${opt('Keywords override levels', 'A keyword match alerts you even in Dot-only or Silent conversations.', toggle('notifications.keywords_override'))}
      <h3>Keywords</h3>
      ${kws.map((k, i) => `<div class="form-row"><span class="chip">${esc(k.word.toLowerCase())}<button data-act="kw-del" data-i="${i}" title="Remove">&times;</button></span>${scope(k, i)}</div>`).join('') || '<p class="muted">No keywords. Your name or words like "urgent" are good candidates.</p>'}
      <div class="form-row" style="justify-content:flex-start"><input type="text" id="kw-new" placeholder="Add keyword"><button class="btn" data-act="kw-add">Add</button></div>
      <div class="example">Whole words, case-insensitive: <b>urgent</b> matches "URGENT:" but not "urgently".</div>`;
  },

  dnd: () => {
    const d = S.settings.notifications.dnd;
    return `<h2>Do not disturb</h2>
      ${opt('Quiet hours', 'During these periods there are no popups and no sounds. Tray dots still appear.', toggle('notifications.dnd.enabled'))}
      <h3>Periods</h3>
      ${d.ranges.map((r, i) => `<div class="form-row" style="justify-content:flex-start">
        <span class="days">${DAYS.map((l, day) => `<button class="${r.days.includes(day) ? 'on' : ''}" data-act="day" data-r="${i}" data-day="${day}">${l}</button>`).join('')}</span>
        <input type="time" data-range="${i}" data-edge="start" value="${hhmm(r.start)}"> to
        <input type="time" data-range="${i}" data-edge="end" value="${hhmm(r.end)}">
        <button class="btn" data-act="range-del" data-r="${i}">Remove</button></div>`).join('')}
      <button class="btn" data-act="range-add">Add period</button>
      <p class="muted">A period that ends earlier than it starts runs past midnight.</p>
      <h3>Let through</h3>
      ${opt('Direct messages', '', toggle('notifications.dnd.allow_dm'))}
      ${opt('Mentions', '', toggle('notifications.dnd.allow_mentions'))}
      ${opt('Keywords', '', toggle('notifications.dnd.allow_keywords'))}`;
  },

  appearance: () => {
    const rows = `<div class="row unread"><span class="glyph">#</span><span class="name">general</span><span class="badge mention">3</span></div>
      <div class="row"><span class="glyph">#</span><span class="name">project-updates</span></div>
      <div class="row unread"><span class="av sm">AM</span><span class="name">Alex Morgan</span><span class="badge mention">1</span></div>`;
    // A miniature window: rail, sidebar, a message. `mode` is light, dark or split (system).
    const mini = (mode) => `<div class="theme-preview ${mode}">${['light', 'dark'].filter((m) => mode === m || mode === 'split')
      .map((m) => `<div class="tp ${m}"><i class="tp-rail"></i><i class="tp-side"><b></b><b></b><b class="on"></b></i>
        <i class="tp-main"><b></b><b class="short"></b><b class="bubble"></b></i></div>`).join('')}</div>`;
    return `<h2>Appearance</h2>
    <h3>Theme</h3>
    <div class="choices">${[['system', 'As system', 'split'], ['light', 'Light', 'light'], ['dark', 'Dark', 'dark']]
      .map(([v, l, m]) => choice('theme', v, l, mini(m))).join('')}</div>
    <h3>Accent colour</h3>
    <div class="accents">${ACCENTS.map(([v, l, light, dark]) => {
      const on = (S.settings.accent || 'blue') === v;
      return `<label class="accent-choice ${on ? 'on' : ''}" style="--sw-l:${light};--sw-d:${dark}">
        <input type="radio" name="accent" data-path="accent" value="${v}" ${on ? 'checked' : ''}><i></i><span>${l}</span></label>`;
    }).join('')}</div>
    <p class="muted">Used for links, selections, buttons and mentions. Workspace colours stay as they are.</p>
    <h3>Text size</h3>
    <div class="choices">${[['small', 'Small'], ['normal', 'Medium (default)'], ['large', 'Large']].map(([v, l]) => choice('sidebar_size', v, l,
      // The window is already zoomed to the current size: scale relative to it, so each
      // preview shows that size exactly as it will look.
      `<div class="sb-preview zoomed" style="zoom:${(TEXT_SIZES[v] / (TEXT_SIZES[S.settings.sidebar_size] || TEXT_SIZES.normal)).toFixed(3)}">${rows}
        <div class="msg-preview"><b>Alex Morgan</b> The draft is ready for review.</div></div>`)).join('')}</div>
    <p class="muted">Everything scales together: sidebar, messages, settings and your profile.</p>
    <h3>Spacing</h3>
    <div class="choices true-width">${[['compact', 'Compact (default)'], ['wide', 'Wide']].map(([v, l]) => choice('sidebar_width', v, l,
      `<div class="sb-preview" data-sb-width="${v}">${rows}</div>`)).join('')}</div>
    <h3>Font</h3>
    <div class="choices">${FONTS.map(([v, l, stack]) => choice('font', v, l,
      `<div class="font-preview" style="font-family:${esc(stack)}"><b>Aa Bb 0123</b><br>The draft is ready for review.</div>`)).join('')}</div>
    ${opt('Link previews', 'Title, description and image under messages with links, when the service provides them.', toggle('link_previews'),
      `<div class="example"><div class="link-card" style="margin:0"><span class="t" style="color:var(--accent)">Title of the linked page</span>
       <div class="d">Short description supplied by the service.</div><div class="mock-shot" aria-hidden="true"><i></i><b></b><b></b></div></div></div>`)}`;
  },

  privacy: () => {
    const s = S.settings;
    return `<h2>Privacy & storage</h2>
      ${opt('Encrypt the archive', s.cipher_available
        ? `SQLCipher, with the key kept in ${KEYCHAIN}. Switching converts the existing archive.`
        : 'This build was compiled without SQLCipher.',
        `<input type="checkbox" class="switch" data-act="encrypt" ${s.db_encrypted ? 'checked' : ''} ${s.cipher_available ? '' : 'disabled'}>`)}
      ${opt('Keep versions before edits', 'Stores the old text when a message is edited. Off by default: check your workspace policy first.', toggle('keep_edit_history'))}
      ${opt('Image cache limit', 'Downloaded images, avatars and custom emoji.', `<input type="number" min="0.1" step="0.1" style="width:70px" data-gb value="${(s.image_cache_mb / 1024).toFixed(1)}"> GB`)}
      ${opt('Data folder', esc(s.data_dir), '')}
      <div class="example">The archive keeps every message received since installation, including other people's messages.
        Follow each workspace's retention and confidentiality rules before keeping it.</div>`;
  },

  advanced: () => `<h2>Advanced</h2>
    ${opt(OS === 'windows' ? 'Start with Windows' : 'Start at login', 'Off by default.', toggle('autostart'))}
    <h3>Updates</h3>
    ${opt(`Agora ${esc(S.settings.version)}`, S.update.available
      ? `Version ${esc(S.update.available.version)} is available.`
      : S.update.checking ? 'Checking...' : S.update.error ? esc(S.update.error) : S.update.checked ? 'You have the latest version.' : 'Agora checks for updates by itself, at start and every few hours.',
      S.update.available ? '<button class="btn primary" data-act="install-update">Update and restart</button>'
        : `<button class="btn" data-act="check-update" ${S.update.checking ? 'disabled' : ''}>Check for updates</button>`)}
    ${opt('Update feed', 'GitHub Releases <code>latest.json</code>. Updates are signed: only installers signed with Agora\'s key are accepted.', `<input type="url" data-path="update_url" style="width:360px" value="${esc(S.settings.update_url)}">`)}
    <h3>Build</h3>
    ${opt('Pre-publication build', 'Reminder to review sensitive features before any public release: Slack web-session tokens and the local message archive.', toggle('prepublication'))}
    ${opt('Version', esc(S.settings.version), '')}`,
};

function render() {
  if (!VIEWS[current]) current = 'accounts';
  overlay().innerHTML = `<div class="settings">
    <nav><div class="title">Settings</div>
      ${SECTIONS.map(([id, label, ic]) => `<button data-sec="${id}" class="${id === current ? 'active' : ''}">${icon(ic)}${label}</button>`).join('')}
    </nav>
    <main>${VIEWS[current]()}</main></div>`;
}

/** Reads an image file and turns it into a 64x64 PNG data URL (cropped to a square). */
function pickLogo() {
  return new Promise((resolve) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = 'image/*';
    input.addEventListener('change', () => {
      const file = input.files?.[0];
      if (!file) { resolve(null); return; }
      const img = new Image();
      const url = URL.createObjectURL(file);
      img.onload = () => {
        const size = 64;
        const canvas = document.createElement('canvas');
        canvas.width = canvas.height = size;
        const side = Math.min(img.naturalWidth, img.naturalHeight);
        canvas.getContext('2d').drawImage(img, (img.naturalWidth - side) / 2, (img.naturalHeight - side) / 2, side, side, 0, 0, size, size);
        URL.revokeObjectURL(url);
        resolve(canvas.toDataURL('image/png'));
      };
      img.onerror = () => { URL.revokeObjectURL(url); toast('That file is not an image'); resolve(null); };
      img.src = url;
    });
    input.click();
  });
}

/** `choice`: 'pick' (upload), 'service' (workspace logo), or 'initials' with `text`. */
async function changeLogo(card, choice, text = '') {
  const a = S.accounts.find((x) => x.id === card.dataset.id);
  if (!a) return;
  try {
    if (choice === 'service') {
      await invoke('use_service_icon', { account_id: a.id });
    } else if (choice === 'initials') {
      await invoke('update_account', { account: { ...a, icon: text.trim() || null, icon_image: '' } });
    } else {
      const icon_image = await pickLogo();
      if (icon_image === null) return;
      await invoke('update_account', { account: { ...a, icon_image } });
    }
    accPanel = { id: null, kind: null };
  } catch (e) {
    toast(String(e));
  }
}

async function saveAccount(card) {
  const a = S.accounts.find((x) => x.id === card.dataset.id);
  if (!a) return;
  const next = {
    ...a,
    name: card.querySelector('[data-acc="name"]').value.trim() || a.name,
    color: card.querySelector('[data-acc="color"]').value,
    enabled: card.querySelector('[data-acc="enabled"]').checked,
  };
  try {
    await invoke('update_account', { account: next });
    Object.assign(a, next);
    refreshAll();
  } catch (e) {
    toast(String(e));
  }
}

function bind(ov) {
  bound = true;
  ov.addEventListener('change', async (e) => {
    if (!settingsOpen()) return;
    const t = e.target;
    if (t.dataset.path) {
      const v = t.type === 'checkbox' ? t.checked : t.type === 'number' || t.type === 'range' ? Number(t.value) : t.value;
      setPath(t.dataset.path, v);
      if (t.dataset.path === 'sidebar_width') resetSidebarWidth();
      if (['theme', 'accent', 'sidebar_size', 'sidebar_width', 'font'].includes(t.dataset.path)) applyTheme();
      if (t.type === 'radio') render();
      if (t.dataset.path === 'link_previews') refreshAll();
      // Hear the new volume (or sound) right away.
      if (t.dataset.path === 'notifications.volume' || t.dataset.path === 'notifications.sound') playSound(S.settings.notifications.sound);
      persist();
      return;
    }
    if (t.dataset.gb !== undefined) {
      S.settings.image_cache_mb = Math.max(100, Math.round(Number(t.value) * 1024));
      persist();
      return;
    }
    if (t.dataset.acc === 'initials') return changeLogo(t.closest('.acc-card'), 'initials', t.value);
    if (t.dataset.acc) return saveAccount(t.closest('.acc-card'));
    if (t.dataset.kw !== undefined) {
      S.settings.notifications.keywords[Number(t.dataset.kw)].accounts = t.value ? [t.value] : [];
      persist();
      return;
    }
    if (t.dataset.range !== undefined) {
      const [h, m] = t.value.split(':').map(Number);
      S.settings.notifications.dnd.ranges[Number(t.dataset.range)][t.dataset.edge] = h * 60 + m;
      persist();
      return;
    }
    if (t.dataset.act === 'encrypt') {
      t.disabled = true;
      toast(t.checked ? 'Encrypting the archive...' : 'Decrypting the archive...');
      try {
        S.settings.db_encrypted = await invoke('set_db_encryption', { enabled: t.checked });
        toast(S.settings.db_encrypted ? 'Archive encrypted' : 'Archive stored unencrypted');
      } catch (err) {
        toast(String(err), 5000);
        t.checked = !t.checked;
      } finally {
        t.disabled = false;
      }
    }
  });

  ov.addEventListener('click', async (e) => {
    if (!settingsOpen()) return;
    const sec = e.target.closest('[data-sec]');
    if (sec) { current = sec.dataset.sec; render(); return; }
    // Level buttons: global defaults, or per workspace (where "Default" clears the override).
    const lv = e.target.closest('.seg[data-level] button');
    if (lv) {
      const { level, ws } = lv.parentElement.dataset;
      const n = S.settings.notifications;
      if (!ws) {
        n[level] = lv.dataset.v;
      } else {
        n.accounts = n.accounts || {};
        const own = { dm_level: null, channel_level: null, ...n.accounts[ws], [level]: lv.dataset.v || null };
        if (!own.dm_level && !own.channel_level) delete n.accounts[ws];
        else n.accounts[ws] = own;
      }
      persist();
      render();
      return;
    }
    const b = e.target.closest('[data-act]');
    if (!b) return;
    const n = S.settings.notifications;
    switch (b.dataset.act) {
      case 'close': closeSettings(); refreshAll(); break;
      case 'test-sound': playSound(S.settings.notifications.sound); break;
      case 'add-demo':
        try { await invoke('add_demo_account'); } catch (err) { toast(String(err)); }
        break;
      case 'add-account':
        signin.kind = signin.kind === b.dataset.kind ? null : b.dataset.kind;
        render();
        document.getElementById('signin-server')?.focus();
        break;
      case 'signin': signIn(null); break;
      case 'signin-token': {
        const fields = {};
        ov.querySelectorAll('[data-token-field]').forEach((i) => { fields[i.dataset.tokenField] = i.value.trim(); });
        signIn(fields);
        break;
      }
      case 'logo-menu':
      case 'acc-more': {
        const id = b.closest('.acc-card').dataset.id;
        const kind = b.dataset.act === 'logo-menu' ? 'logo' : 'more';
        accPanel = accPanel.id === id && accPanel.kind === kind ? { id: null, kind: null } : { id, kind };
        render();
        break;
      }
      case 'panel-close': accPanel = { id: null, kind: null }; render(); break;
      case 'logo-pick': changeLogo(b.closest('.acc-card'), 'pick'); break;
      case 'logo-service': changeLogo(b.closest('.acc-card'), 'service'); break;
      case 'relogin': relogin(b.closest('.acc-card').dataset.id); break;
      case 'remove': accPanel = { id: b.closest('.acc-card').dataset.id, kind: 'remove' }; render(); break;
      case 'remove-keep':
      case 'remove-purge': {
        const id = b.closest('.acc-card').dataset.id;
        accPanel = { id: null, kind: null };
        try {
          await invoke('remove_account', { account_id: id, purge: b.dataset.act === 'remove-purge' });
        } catch (err) { toast(String(err)); }
        break;
      }
      case 'check-update':
        checkForUpdate(true).then(render);
        render();
        break;
      case 'install-update':
        installUpdate();
        break;
      case 'kw-add': {
        const input = document.getElementById('kw-new');
        const word = input.value.trim().toLowerCase(); // matching ignores case anyway (text::contains_word)
        if (word && !n.keywords.some((k) => k.word.toLowerCase() === word.toLowerCase())) {
          n.keywords.push({ word, accounts: [] });
          persist();
          render();
        }
        break;
      }
      case 'kw-del': n.keywords.splice(Number(b.dataset.i), 1); persist(); render(); break;
      case 'day': {
        const r = n.dnd.ranges[Number(b.dataset.r)];
        const day = Number(b.dataset.day);
        r.days = r.days.includes(day) ? r.days.filter((d) => d !== day) : [...r.days, day].sort();
        persist();
        render();
        break;
      }
      case 'range-add': n.dnd.ranges.push({ days: [0, 1, 2, 3, 4], start: 12 * 60, end: 13 * 60 }); persist(); render(); break;
      case 'range-del': n.dnd.ranges.splice(Number(b.dataset.r), 1); persist(); render(); break;
    }
  });

  ov.addEventListener('keydown', (e) => {
    if (e.target.id === 'kw-new' && e.key === 'Enter') ov.querySelector('[data-act="kw-add"]').click();
    if (e.target.id === 'signin-server' && e.key === 'Enter') signIn(null);
  });
  ov.addEventListener('input', (e) => {
    if (e.target.id === 'signin-server') signin.server = e.target.value;
    // The percentage next to the volume slider follows it live.
    if (e.target.dataset.path === 'notifications.volume') e.target.nextElementSibling.value = `${e.target.value}%`;
  });
}

/** `tokens` (personal access token fields) skips the login window. */
async function signIn(tokens) {
  const k = SIGNIN[signin.kind];
  if (!k || signin.busy) return;
  if (tokens && Object.values(tokens).some((v) => !v)) { toast('Fill in every field first'); return; }
  const server = signin.server.trim();
  if (!server && !k.serverOptional) { toast('Enter the server address'); return; }
  signin.busy = true;
  render();
  try {
    const r = await invoke(k.command, { server: server || null, account_id: null, token: null, user_id: null, cookie: null, ...tokens });
    // Slack can add several workspaces at once.
    const list = Array.isArray(r) ? r : [r];
    toast(list.length > 1
      ? `Signed in to ${list.map((x) => x.account.name).join(', ')}`
      : `Signed in as @${list[0].username}`, 4000);
    Object.assign(signin, { kind: null, server: '' });
  } catch (err) {
    toast(String(err), 6000);
  } finally {
    signin.busy = false;
    if (settingsOpen() && current === 'accounts') render();
  }
}

// Re-render the accounts list when accounts change while settings are open.
export function settingsAccountsChanged() {
  if (settingsOpen() && current === 'accounts') render();
}
