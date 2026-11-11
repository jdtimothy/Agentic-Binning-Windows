// Small DOM helpers: element builder, icons, toasts and the sheet stack.

/** h('div.cls#id', {attrs}, ...children). Children may be nodes, strings, arrays, null. */
export function h(spec, attrs, ...children) {
  const [tagAndId, ...classes] = spec.split('.');
  const [tag, id] = tagAndId.split('#');
  const el = document.createElement(tag || 'div');
  if (id) el.id = id;
  if (classes.length) el.className = classes.join(' ');
  if (attrs && (attrs instanceof Node || typeof attrs !== 'object' || Array.isArray(attrs))) {
    children.unshift(attrs);
    attrs = null;
  }
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'class') el.className += ' ' + v;
    else if (k === 'html') el.innerHTML = v;
    else if (k in el && k !== 'list' && typeof v !== 'string') el[k] = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  const add = (c) => {
    if (c === null || c === undefined || c === false) return;
    if (Array.isArray(c)) c.forEach(add);
    else el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  };
  children.forEach(add);
  return el;
}

const PATHS = {
  chip: '<rect x="7" y="7" width="10" height="10" rx="2"/><path d="M10 3v4M14 3v4M10 17v4M14 17v4M3 10h4M3 14h4M17 10h4M17 14h4"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2.5l1.6 2.6 3-.6.9 2.9 2.9.9-.6 3L22.4 12l-2.6 1.6.6 3-2.9.9-.9 2.9-3-.6L12 21.5l-1.6-2.6-3 .6-.9-2.9-2.9-.9.6-3L1.6 12l2.6-1.6-.6-3 2.9-.9.9-2.9 3 .6z"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="2" fill="currentColor" stroke="none"/>',
  pause: '<rect x="7" y="6" width="3.5" height="12" rx="1" fill="currentColor" stroke="none"/><rect x="13.5" y="6" width="3.5" height="12" rx="1" fill="currentColor" stroke="none"/>',
  play: '<path d="M8 5.5v13l11-6.5z" fill="currentColor" stroke="none"/>',
  grid: '<rect x="4" y="4" width="6.5" height="6.5" rx="1.5"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="1.5"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="1.5"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.5"/>',
  back: '<path d="M15 5l-7 7 7 7"/>',
  next: '<path d="M9 5l7 7-7 7"/>',
  search: '<circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
  shuffle: '<path d="M3 7h3.5c5 0 5.5 10 10.5 10H21M3 17h3.5c1.6 0 2.7-1 3.6-2.4M21 7h-4c-1.6 0-2.7 1-3.6 2.4M18 4l3 3-3 3M18 14l3 3-3 3"/>',
  edit: '<path d="M4 20h4L19 9l-4-4L4 16zM14 6l4 4"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
  checkbox: '<rect x="4" y="4" width="16" height="16" rx="4"/><path d="M8 12.5l3 3 5-6"/>',
  bin: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M9 13.5l2.2 2.2 4-4.4"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  image: '<rect x="3.5" y="5" width="17" height="14" rx="3"/><circle cx="9" cy="10" r="1.6"/><path d="M4.5 17l5-4.5 3.5 3 2.5-2 4 3.5"/>',
  film: '<rect x="3.5" y="5" width="17" height="14" rx="3"/><path d="M10 9.5v5l4.5-2.5z" fill="currentColor"/>',
  lock: '<rect x="5.5" y="11" width="13" height="9" rx="2.5"/><path d="M8.5 11V8a3.5 3.5 0 0 1 7 0v3"/>',
  unlock: '<rect x="5.5" y="11" width="13" height="9" rx="2.5"/><path d="M8.5 11V8a3.5 3.5 0 0 1 6.6-1.6"/>',
  warn: '<path d="M12 4l9 16H3z"/><path d="M12 10v4.5M12 17.2v.3"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.5M12 7.8v.3"/>',
  down: '<path d="M6 9.5l6 6 6-6"/>',
  open: '<path d="M14 4h6v6M20 4l-9 9M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>',
  download: '<path d="M12 4v11M7 11l5 5 5-5M5 20h14"/>',
  trash: '<path d="M5 7h14M10 7V4.5h4V7M7 7l1 13h8l1-13"/>',
  refresh: '<path d="M20 12a8 8 0 1 1-2.6-5.9M20 4v5h-5"/>',
  move: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M9.5 13h5M12.5 10.5L15 13l-2.5 2.5"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"/>',
  key: '<circle cx="8" cy="14" r="4"/><path d="M11 11l9-9M16 6l3 3M14 8l2 2"/>',
  terminal: '<rect x="3" y="4.5" width="18" height="15" rx="3"/><path d="M7 9.5l3 2.5-3 2.5M12.5 15H17"/>',
};

/** An icon as an SVG element. */
export function icon(name, size = 20) {
  const t = document.createElement('template');
  t.innerHTML = `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${PATHS[name] || ''}</svg>`;
  return t.content.firstChild;
}

/** A short note at the bottom of the window; `action` adds a button ({label, run}). */
export function toast(text, action, ms = 4200) {
  const box = document.getElementById('toasts');
  const el = h('div.toast', { role: 'status' }, h('span', text));
  if (action) el.append(h('button.link', { onclick: () => { el.remove(); action.run(); } }, action.label));
  box.append(el);
  while (box.children.length > 3) box.firstChild.remove();
  setTimeout(() => el.remove(), action ? Math.max(ms, 7000) : ms);
}

/** Shows an error from a rejected call as a toast. */
export function fail(e) {
  toast((e && e.message) || String(e));
}

const stack = [];

/**
 * Opens a sheet. `build(close)` returns {title, body, foot?, wide?, tall?}; the scrim, Escape and
 * the × close it. Returns the close function.
 */
export function openSheet(build) {
  const layer = h('div.layer');
  const close = () => {
    const i = stack.indexOf(entry);
    if (i < 0) return;
    stack.splice(i, 1);
    layer.remove();
    if (entry.onClose) entry.onClose();
    if (previous && previous.focus && document.contains(previous)) previous.focus();
  };
  const entry = { close, layer };
  const previous = document.activeElement;
  const spec = build(close, entry);
  const sheet = h(`div.sheet${spec.wide ? '.wide' : ''}${spec.tall ? '.tall' : ''}`, { role: 'dialog', 'aria-modal': 'true', 'aria-label': spec.title },
    h('div.shead', h('h2', spec.title), spec.headExtra || null, h('button.ib.sm', { 'aria-label': 'Close', onclick: close }, icon('x', 18))),
    spec.body,
    spec.foot || null,
  );
  entry.sheet = sheet;
  layer.append(h('div.scrim', { onclick: close }), sheet);
  document.getElementById('sheets').append(layer);
  stack.push(entry);
  const first = sheet.querySelector('[autofocus]') || sheet.querySelector('input, button.primary') || sheet.querySelector('button');
  if (first) setTimeout(() => first.focus(), 30);
  return close;
}

export function closeTopSheet() {
  const top = stack[stack.length - 1];
  if (top) top.close();
  return !!top;
}

export function sheetOpen() {
  return stack.length > 0;
}

/** A yes/no question as a small sheet. Resolves true when confirmed. */
export function confirmSheet(title, text, yes, danger) {
  return new Promise((resolve) => {
    let answered = false;
    openSheet((close, entry) => {
      entry.onClose = () => { if (!answered) resolve(false); };
      return {
        title,
        body: h('div.sbody', h('p.hint', { style: 'font-size:14.5px' }, text)),
        foot: h('div.sfoot', h('div.spacer'),
          h('button.secondary', { onclick: close }, 'Cancel'),
          h(`button.${danger ? 'secondary.danger' : 'primary'}`, { style: 'flex:none', onclick: () => { answered = true; resolve(true); close(); } }, yes)),
      };
    });
  });
}

export function duration(ms) {
  if (ms === null || ms === undefined) return '';
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

export function plural(n, one, many) {
  return `${n.toLocaleString()} ${n === 1 ? one : many || one + 's'}`;
}
