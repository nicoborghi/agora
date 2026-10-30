// Safe Markdown renderer + tiny local syntax highlighter.
// Everything is HTML-escaped first; only a fixed set of tags is produced.
import { esc } from './api.js';
import { EMOJI_SOURCE } from './emoji-data.js';

/** Every standard shortcode (the names Slack and Mattermost use) to its character. */
export const EMOJI = {};
for (const entry of EMOJI_SOURCE.split(',')) {
  const [names, hex] = entry.split(':');
  const ch = String.fromCodePoint(...hex.split('-').map((h) => parseInt(h, 16)));
  for (const n of names.split('|')) EMOJI[n] = ch;
}
// Rocket.Chat's (EmojiOne) names where they differ.
for (const [alias, name] of [['slight_smile', 'slightly_smiling_face'], ['slight_frown', 'slightly_frowning_face'],
  ['frowning2', 'white_frowning_face'], ['thinking', 'thinking_face'], ['hugging', 'hugging_face'], ['upside_down', 'upside_down_face'],
  ['rolling_eyes', 'face_with_rolling_eyes'], ['nerd', 'nerd_face'], ['robot', 'robot_face'], ['hand_splayed', 'raised_hand_with_fingers_splayed']]) {
  EMOJI[alias] ||= EMOJI[name];
}

/** The picker's set: the ones people actually use. Every other name works when typed. */
export const COMMON_EMOJI = ['+1', '-1', 'heart', 'joy', 'smile', 'slightly_smiling_face', 'eyes', 'tada', 'white_check_mark',
  'heavy_check_mark', 'rocket', 'pray', 'fire', '100', 'thinking_face', 'ok_hand', 'clap', 'warning', 'x', 'question',
  'raised_hands', 'wave', 'sob', 'muscle', 'point_up', 'sweat_smile', 'grin', 'wink', 'star', 'bulb', 'memo', 'coffee',
  'bug', 'see_no_evil', 'face_palm', 'shrug', 'white_frowning_face', 'heart_eyes', 'sunglasses', 'partying_face',
  'zap', 'sparkles', 'chart_with_upwards_trend', 'calendar', 'pushpin', 'link', 'lock', 'telescope', 'ringed_planet'];
export const QUICK_REACTIONS = ['+1', 'heart', 'joy', 'eyes', 'tada', 'white_check_mark', 'rocket', 'pray',
  'fire', '100', 'thinking_face', 'ok_hand', 'clap', 'warning', 'x', 'question',
  'raised_hands', 'wave', 'sob', 'muscle', 'bulb', 'bug', 'coffee', 'telescope'];

/**
 * An emoji, skin tone or not (Slack writes `wave::skin-tone-3`). Agora's emoji font keeps only the
 * yellow version of each, so tones are left out and every emoji is drawn in that one font.
 */
function withTone(name, _tone) {
  return EMOJI[name] || customEmoji(name);
}

// Workspace (custom) emoji: the account being drawn, and how to find a picture for a name.
let drawingFor = null;
let customLookup = () => null;
export function setCustomEmoji(lookup) { customLookup = lookup; }
function customEmoji(name) {
  const url = drawingFor && customLookup(drawingFor, name);
  return url ? `<img class="cemoji" src="${esc(url)}" alt=":${esc(name)}:" title=":${esc(name)}:">` : null;
}
/** Skin-tone modifiers typed straight into a message. */
const SKIN_TONES = /[\u{1F3FB}-\u{1F3FF}]/gu;

/** `account`: whose workspace emoji may be used. */
export const emoji = (name, account = null) => {
  const [base, tone] = String(name).split('::skin-tone-');
  drawingFor = account;
  try { return withTone(base, tone) || `:${esc(name)}:`; } finally { drawingFor = null; }
};

// ------------------------------------------------------------ highlighter

const KW = {
  python: 'and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield self',
  js: 'async await break case catch class const continue default delete do else export extends false finally for function if import in instanceof let new null return super switch this throw true try typeof undefined var void while yield of',
  rust: 'as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while Some None Ok Err',
  c: 'auto break case char const continue default do double else enum extern float for goto if int long register return short signed sizeof static struct switch typedef union unsigned void volatile while class namespace public private protected template typename virtual bool true false nullptr new delete include define',
  bash: 'if then else elif fi for in do done while until case esac function return local export echo exit set unset',
  sql: 'select from where and or not insert into values update set delete create table index join left right inner outer on group by order having limit as distinct null is like in',
  json: 'true false null',
};
const ALIAS = { py: 'python', python3: 'python', javascript: 'js', ts: 'js', typescript: 'js', rs: 'rust', cpp: 'c', 'c++': 'c', h: 'c', sh: 'bash', shell: 'bash', zsh: 'bash', powershell: 'bash', ps1: 'bash' };
const COMMENT = { python: '#', bash: '#', sql: '--', js: '//', rust: '//', c: '//', json: null };
const kwSets = {};

export function highlight(code, lang) {
  lang = (lang || 'python').toLowerCase();
  lang = ALIAS[lang] || lang;
  if (!KW[lang]) return esc(code);
  const kws = kwSets[lang] || (kwSets[lang] = new Set(KW[lang].split(' ')));
  const lineComment = COMMENT[lang];
  const ci = lang === 'sql';
  let out = '';
  let i = 0;
  const n = code.length;
  while (i < n) {
    const ch = code[i];
    if (lineComment && code.startsWith(lineComment, i)) {
      let j = code.indexOf('\n', i); if (j < 0) j = n;
      out += `<span class="tk-com">${esc(code.slice(i, j))}</span>`; i = j; continue;
    }
    if ((lang === 'js' || lang === 'rust' || lang === 'c') && code.startsWith('/*', i)) {
      let j = code.indexOf('*/', i + 2); j = j < 0 ? n : j + 2;
      out += `<span class="tk-com">${esc(code.slice(i, j))}</span>`; i = j; continue;
    }
    if (ch === '"' || ch === "'" || (ch === '`' && lang === 'js')) {
      const triple = lang === 'python' && code.startsWith(ch.repeat(3), i);
      const end = triple ? ch.repeat(3) : ch;
      let j = i + end.length;
      while (j < n && !code.startsWith(end, j)) { if (code[j] === '\\') j++; if (!triple && code[j] === '\n') break; j++; }
      j = Math.min(n, j + end.length);
      out += `<span class="tk-str">${esc(code.slice(i, j))}</span>`; i = j; continue;
    }
    if (/[0-9]/.test(ch) && !/[\w]/.test(code[i - 1] || '')) {
      const m = /^[0-9][0-9a-fA-FxXoObB_.eE]*/.exec(code.slice(i));
      out += `<span class="tk-num">${esc(m[0])}</span>`; i += m[0].length; continue;
    }
    if (/[A-Za-z_$]/.test(ch)) {
      const m = /^[A-Za-z_$][\w$]*/.exec(code.slice(i));
      const w = m[0];
      out += kws.has(ci ? w.toLowerCase() : w) ? `<span class="tk-kw">${esc(w)}</span>` : esc(w);
      i += w.length; continue;
    }
    out += esc(ch); i++;
  }
  return out;
}

// ------------------------------------------------------------ markdown

const URL_RE = /\bhttps?:\/\/[^\s<>"']*[^\s<>"'.,;:!?)\]]/g;

function inline(src) {
  const slots = [];
  const keep = (html) => `\u0000${slots.push(html) - 1}\u0000`;
  let s = src.replace(/`([^`\n]+)`/g, (_, c) => keep(`<code>${esc(c)}</code>`));
  s = s.replace(/\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g, (_, t, u) => keep(`<a href="${esc(u)}" data-url="${esc(u)}">${esc(t)}</a>`));
  s = s.replace(URL_RE, (u) => keep(`<a href="${esc(u)}" data-url="${esc(u)}">${esc(u)}</a>`));
  s = esc(s);
  s = s.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>');
  s = s.replace(/(^|[^\w*])\*([^*\n]+)\*(?![\w*])/g, '$1<em>$2</em>');
  s = s.replace(/(^|[^\w])_([^_\n]+)_(?!\w)/g, '$1<em>$2</em>');
  s = s.replace(/~~([^~\n]+)~~/g, '<del>$1</del>');
  // @someone opens their profile; @here/@channel/@all address everyone. #name opens a conversation.
  s = s.replace(/(^|[^\w@])@(here|channel|all|everyone)\b/g, '$1<span class="mention all">@$2</span>');
  // Any language's letters ("@Hervé"); a no-break space joins multi-word names (Slack sends them so).
  s = s.replace(/(^|[^\p{L}\p{N}_@>])@([\p{L}\p{N}_](?:[\p{L}\p{N}_.\-\u{a0}]*[\p{L}\p{N}_])?)/gu,
    (_, pre, name) => `${pre}<span class="mention" data-handle="${name.replace(/\u00a0/g, ' ')}">@${name}</span>`);
  s = s.replace(/(^|[\s(])#([A-Za-z][\w.\-]*\w|[A-Za-z])/g, '$1<span class="chan-ref" data-chan="$2">#$2</span>');
  s = s.replace(/:([a-z0-9_+\-]+):(?::skin-tone-([2-6]):)?/g, (m, name, tone) => withTone(name, tone) || m);
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => slots[+i]);
}

/** `account`: whose workspace emoji may be used. */
export function renderMarkdown(src, account = null) {
  drawingFor = account;
  try { return renderBlocks(src); } finally { drawingFor = null; }
}

function renderBlocks(src) {
  const lines = String(src || '').replace(/\r\n?/g, '\n').split('\n');
  let html = '';
  let para = [];
  const flush = () => { if (para.length) { html += `<p>${para.map(inline).join('<br>')}</p>`; para = []; } };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = /^\s*```\s*([\w+#.\-]*)\s*$/.exec(line);
    if (fence) {
      flush();
      const body = [];
      for (i++; i < lines.length && !/^\s*```\s*$/.test(lines[i]); i++) body.push(lines[i]);
      // Over 10 lines a block shows its start; the button opens it (see `.code-open`).
      const long = body.length > 10;
      html += `<pre${long ? ' class="long"' : ''}><code>${highlight(body.join('\n'), fence[1])}</code></pre>`;
      if (long) html += `<button class="code-more" data-act="code-more"><span class="more">Show all ${body.length} lines</span><span class="less">Show less</span></button>`;
      continue;
    }
    if (/^\s*>/.test(line)) {
      flush();
      const body = [];
      for (; i < lines.length && /^\s*>/.test(lines[i]); i++) body.push(lines[i].replace(/^\s*>\s?/, ''));
      i--;
      html += `<blockquote>${renderMarkdown(body.join('\n'))}</blockquote>`;
      continue;
    }
    const list = /^\s*([-*+]|\d+[.)])\s+/.exec(line);
    if (list) {
      flush();
      const ordered = /\d/.test(list[1]);
      const items = [];
      for (; i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i]); i++) items.push(lines[i].replace(/^\s*([-*+]|\d+[.)])\s+/, ''));
      i--;
      const tag = ordered ? 'ol' : 'ul';
      html += `<${tag}>${items.map((t) => `<li>${inline(t)}</li>`).join('')}</${tag}>`;
      continue;
    }
    const heading = /^\s*#{1,6}\s+(.*)$/.exec(line);
    if (heading) { flush(); html += `<p class="h">${inline(heading[1])}</p>`; continue; }
    if (/^\s*\|.*\|\s*$/.test(line)) {
      // Tables render as aligned monospace text: every service can show that.
      flush();
      const rows = [];
      for (; i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i]); i++) rows.push(lines[i].trim());
      i--;
      html += `<pre class="md-table">${esc(rows.filter((r) => !/^\|[\s:|-]+\|$/.test(r)).join('\n'))}</pre>`;
      continue;
    }
    if (!line.trim()) { flush(); continue; }
    para.push(line);
  }
  flush();
  return html.replace(SKIN_TONES, '');
}

export function extractUrls(text) {
  return String(text || '').match(URL_RE) || [];
}
