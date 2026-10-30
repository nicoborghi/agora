// What changed in each version, in plain words, for the "What's new" window shown once after
// an update (and from Settings → Advanced). Newest first; each item is [title, one line].

export const CHANGES = [
  {
    version: '0.3.0',
    items: [
      ['PDFs inside Agora', 'PDFs show their first page in the conversation and open in Agora with zoom and a map of the pages. Other files open in your usual app. No more web login.'],
      ['Thread replies you can reach', 'Popups and Home open a reply right inside its thread. Unread dots no longer come back after reconnecting.'],
      ['Replies in the main chat', 'Replies sent to the conversation quote the original once, and folded thread lines show the latest reply.'],
      ['New message menu', 'Copy a link to a message, mark it unread, or copy its text from the … button.'],
      ['Start a conversation', 'The Message button on anyone\'s profile opens a direct conversation, even a new one.'],
      ['Better thread replies', 'Formatting, emoji, attachments, a saved draft, and "Also send to the conversation" on Slack and Rocket.Chat.'],
      ['Workspace emoji', 'Your workspace\'s own emoji show in messages and reactions, with their own tab in the picker.'],
      ['Faster moving around', 'Alt+↑/↓ goes to the conversation above or below; add Shift to jump to the next one with news.'],
      ['Conversation details', 'The ⓘ button shows the description, Slack canvases, bookmarks, pinned messages, images and links.'],
      ['Emoji look', 'Google\'s Noto emoji, the same on every system, in their yellow version.'],
      ['One Agora at a time', 'Starting Agora again brings the open one to the front. "Start at login" now works on every system.'],
      ['Images load once', 'Pictures are kept on disk within the limit in Settings, so they are not downloaded again.'],
      ['Smaller things', 'Zoom in opened images with a map of where you are, shorter link previews, shared Slack messages shown, roomier Settings.'],
    ],
  },
  {
    version: '0.2.0',
    items: [
      ['macOS and Ubuntu', 'Agora now runs on macOS and Ubuntu as well as Windows.'],
      ['Updates by itself', 'New versions install with one click and restart Agora.'],
      ['Who reacted', 'Hover a reaction to see who; right-click it for the list with pictures.'],
      ['Thread notifications', 'Replies in threads you started or joined notify you like mentions.'],
      ['Tidier messages', 'Long code blocks fold after 10 lines; your four favourite emoji are one click away.'],
    ],
  },
];

/** -1, 0 or 1: version a before, equal to, or after b ("0.10.0" after "0.9.1"). */
export function compareVersions(a, b) {
  const pa = String(a).split('.').map((n) => parseInt(n, 10) || 0);
  const pb = String(b).split('.').map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] || 0) - (pb[i] || 0);
    if (d) return Math.sign(d);
  }
  return 0;
}
