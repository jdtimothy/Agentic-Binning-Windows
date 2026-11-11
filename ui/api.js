// The bridge to the Rust core. Inside the Windows app it goes through Tauri; in a web browser
// (development, `binning-devserver`) the same commands go over HTTP on this computer.

const T = window.__TAURI__;
export const inApp = !!T;

/** Runs one core command; rejects with a sentence written for people. */
export async function call(cmd, args = {}) {
  if (inApp) {
    try {
      return await T.core.invoke('call', { cmd, args });
    } catch (e) {
      throw new Error(typeof e === 'string' ? e : (e && e.message) || 'Something went wrong');
    }
  }
  const r = await fetch('/api/' + cmd, { method: 'POST', body: JSON.stringify(args) });
  const body = await r.json();
  if (!body.ok) throw new Error(body.error || 'Something went wrong');
  return body.value;
}

const listeners = new Map();

/** Listens to a core event: `pipeline`, `data`, `projects`, `registration`, `copy`, `pull`. */
export function on(name, fn) {
  if (!listeners.has(name)) listeners.set(name, []);
  listeners.get(name).push(fn);
}

function deliver(name, payload) {
  for (const fn of listeners.get(name) || []) {
    try { fn(payload); } catch (e) { console.error(e); }
  }
}

export async function startEvents() {
  if (inApp) {
    await T.event.listen('binning', (e) => deliver(e.payload.name, e.payload.payload));
    await T.event.listen('tauri://drag-drop', (e) => deliver('drop', e.payload.paths || []));
    await T.event.listen('tauri://drag-enter', () => deliver('dragging', true));
    await T.event.listen('tauri://drag-leave', () => deliver('dragging', false));
    return;
  }
  let since = 0;
  try {
    since = (await (await fetch('/events?since=999999999999')).json()).last || 0;
  } catch { /* the first poll below retries */ }
  (async () => {
    for (;;) {
      try {
        const r = await (await fetch('/events?since=' + since)).json();
        since = r.last;
        for (const e of r.events) deliver(e.name, e.payload);
      } catch {
        await new Promise((ok) => setTimeout(ok, 1500));
      }
    }
  })();
}

/** A URL the window can load a photo, video or thumbnail from. */
export function fileUrl(path, version) {
  if (!path) return '';
  const v = version ? (inApp ? '?v=' : '&v=') + version : '';
  if (inApp) return T.core.convertFileSrc(path) + v;
  return '/file?path=' + encodeURIComponent(path) + v;
}

const MEDIA_EXTENSIONS = ['jpg', 'jpeg', 'png', 'heic', 'heif', 'webp', 'dng', 'mp4', 'mov', 'm4v', '3gp', 'mkv', 'webm'];

/** The Windows folder picker; null when cancelled. */
export async function pickFolder(title) {
  if (inApp) return (await T.dialog.open({ directory: true, multiple: false, title })) || null;
  const typed = window.prompt(title + '\n\nBrowser mode has no folder picker. Type the folder\'s full path:');
  return typed && typed.trim() ? typed.trim() : null;
}

/** The Windows file picker for photos and videos; an empty list when cancelled. */
export async function pickFiles(title) {
  if (inApp) {
    const picked = await T.dialog.open({ multiple: true, title, filters: [{ name: 'Photos and videos', extensions: MEDIA_EXTENSIONS }] });
    return picked ? [].concat(picked) : [];
  }
  const typed = window.prompt(title + '\n\nBrowser mode has no file picker. Type full paths, separated by semicolons:');
  return typed ? typed.split(';').map((s) => s.trim()).filter(Boolean) : [];
}

/** Shows a file selected in File Explorer. */
export async function reveal(path) {
  if (inApp) return T.core.invoke('reveal', { path });
  throw new Error('Showing a file in Explorer works in the Windows app');
}

/** Opens a web page in the default browser. */
export async function openExternal(url) {
  if (inApp) return T.core.invoke('open_url', { url });
  window.open(url, '_blank', 'noopener');
}

/** Keeps the computer awake while a run is in progress (Windows app only). */
export async function keepAwake(on) {
  if (inApp) { try { await T.core.invoke('keep_awake', { on }); } catch { /* not essential */ } }
}

/** Hands frames the window decoded back to the core (video without FFmpeg). */
export async function frameResult(payload) {
  if (inApp) return T.core.invoke('frame_result', { result: payload });
}
