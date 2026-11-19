// The two screens (Analyze and Project Media) and the state they share.

import * as api from './api.js';
import { h, icon, toast, fail, closeTopSheet, sheetOpen, duration, plural } from './dom.js';
import * as sheets from './sheets.js';

export const S = {
  boot: null,
  pipeline: null,
  screen: 'analyze',
  all: null, // the open project's grid with no filter
  view: null, // the grid as Project Media filters it
  filter: { chip: 'all', kind: null, shotType: null, takenFrom: null, takenTo: null },
  query: '',
  selecting: false,
  selected: new Set(),
  index: 0, // the carousel's centre card
  follow: true, // the carousel follows the item being analyzed
  details: new Map(),
  registration: null, // {done,total} while files are being added, then {summary}
};

export const project = () => S.boot && S.boot.projects.current;
export const running = () => !!S.pipeline && !['Idle', 'Stopped'].includes(S.pipeline.phase);
export const moving = () => !!S.pipeline && ['Moving', 'MovingPaused'].includes(S.pipeline.phase);
const filtered = () => S.filter.chip !== 'all' || S.filter.kind || S.filter.shotType || S.filter.takenFrom || S.filter.takenTo || S.query.trim();

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

export async function reboot(boot) {
  S.boot = boot || await api.call('boot');
  S.pipeline = S.boot.pipeline;
  applyTheme();
  renderChrome();
}

function applyTheme() {
  const mode = S.boot.settings.themeMode;
  const dark = mode === 'Dark' || (mode === 'System' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
}

let gridTimer = null;
/** Reloads the grid soon; many `data` events in a burst become one reload. */
export function refreshSoon(ms = 120) {
  clearTimeout(gridTimer);
  gridTimer = setTimeout(refresh, ms);
}

export async function refresh() {
  if (!project()) {
    S.all = null;
    S.view = null;
    S.queue = [];
    render();
    return;
  }
  try {
    S.all = await api.call('grid', {});
    // The Analyze cards stand in the order the run takes them (by id, as the Android carousel's
    // observeAll); Project Media keeps the grid's order by date taken.
    S.queue = [...S.all.rows].sort((a, b) => a.id - b.id);
    S.view = filtered() ? await api.call('grid', { filter: S.filter, query: S.query }) : S.all;
    const ids = new Set(S.all.rows.map((r) => r.id));
    for (const id of [...S.selected]) if (!ids.has(id)) S.selected.delete(id);
    for (const id of [...S.details.keys()]) S.details.delete(id);
    followCurrent();
  } catch (e) {
    fail(e);
  }
  render();
}

function followCurrent() {
  if (!S.all) return;
  const rows = S.queue;
  if (S.follow && S.pipeline && S.pipeline.currentItemId) {
    const i = rows.findIndex((r) => r.id === S.pipeline.currentItemId);
    if (i >= 0) S.index = i;
  }
  S.index = Math.max(0, Math.min(S.index, rows.length - 1));
}

// ---------------------------------------------------------------------------------------------
// Chrome and navigation
// ---------------------------------------------------------------------------------------------

export function show(screen) {
  S.screen = screen;
  $('analyze').hidden = screen !== 'analyze';
  $('media').hidden = screen !== 'media';
  if (screen === 'media') {
    // Project Media keeps the blobs, as on Android.
    tintBackdrop(null);
    $('backdrop').classList.remove('moving');
    refresh();
  }
  render();
}

function renderChrome() {
  const b = S.boot;
  const cur = project();
  const top = $('analyze-top');
  top.replaceChildren(
    h('button.pill', { title: 'Models and settings', onclick: () => sheets.settings() },
      icon('chip'),
      h('span.two', h('b', b.simulated ? 'Simulated' : b.models.vision.displayName), h('span', b.simulated ? 'GenieX not in use' : !b.genie.path ? 'GenieX not installed' : b.pulling === b.models.vision.id ? 'Downloading' : b.genie.visionReady || b.genie.error ? b.settings.compute.toUpperCase() : 'Not downloaded'))),
    h('button.pill.grow', { title: cur ? 'Add media or switch project' : 'Open projects', onclick: () => (cur || S.boot.projects.projects.length ? sheets.projects() : sheets.newProject()) },
      icon('folder'),
      h('span.name', cur ? cur.name : 'Create project'),
      cur && S.all ? h('span.count', S.all.total.toLocaleString()) : null),
    b.simulated ? h('span.simtag', { title: 'GenieX was not found, so analysis is simulated from each picture\'s colours.' }, 'Simulated') : null,
    h('button.ib', { 'aria-label': 'Settings', title: 'Settings', onclick: () => sheets.settings() }, icon('gear')),
  );
  const mtop = $('media-top');
  if (S.selecting) {
    const shown = S.view ? S.view.rows : [];
    const allOn = shown.length > 0 && shown.every((r) => S.selected.has(r.id));
    mtop.replaceChildren(
      h('button.ib', { 'aria-label': 'Leave select mode', title: 'Leave select mode (Esc)', onclick: () => { S.selecting = false; S.selected.clear(); render(); } }, icon('x')),
      h('div.ht', h('h1', `${S.selected.size} selected`), h('span', 'Click tiles to add or remove them')),
      h('button.txtbtn', { onclick: () => { if (allOn) shown.forEach((r) => S.selected.delete(r.id)); else shown.forEach((r) => S.selected.add(r.id)); render(); } }, allOn ? 'Clear' : 'Select all'),
    );
  } else {
    mtop.replaceChildren(
      h('button.ib', { 'aria-label': 'Back to Analyze', title: 'Back (Esc)', onclick: () => show('analyze') }, icon('back')),
      h('div.ht', h('h1', cur ? cur.name : ''), h('span', S.all ? `Project media · ${plural(S.all.total, 'item')}` : '')),
      b.simulated ? h('span.simtag', 'Simulated') : null,
      h('button.ib', { 'aria-label': 'Settings', title: 'Settings', onclick: () => sheets.settings() }, icon('gear')),
    );
  }
}

export function render() {
  if (!S.boot) return;
  renderChrome();
  if (S.screen === 'analyze') renderAnalyze();
  else renderMedia();
}

// ---------------------------------------------------------------------------------------------
// Analyze
// ---------------------------------------------------------------------------------------------

function phaseLine(p) {
  const row = S.all && S.all.rows.find((r) => r.id === p.currentItemId);
  switch (p.phase) {
    case 'LoadingModel': return p.downloadFraction !== null && p.downloadFraction !== undefined ? ['run', `Downloading model · ${Math.round(p.downloadFraction * 100)}%`] : ['run', 'Loading model'];
    case 'Preparing': return ['run', row && row.kind === 'Video' ? 'Encoding frames' : 'Reading image'];
    case 'Describing': return ['run', 'Describing'];
    case 'Paused': return ['warn', 'Paused'];
    case 'Releasing': return ['run', 'Finishing up'];
    case 'Categorizing': return ['run', 'Categorizing'];
    case 'Moving': return ['run', 'Binning files'];
    case 'MovingPaused': return ['warn', 'Bin Files paused'];
    case 'Stopped': return p.error ? ['warn', p.error] : ['', 'Stopped'];
    default:
      if (!project()) return ['', 'No project open'];
      if (p.sortNotice) return ['warn', p.sortNotice];
      if (!p.total) return ['', 'No media yet'];
      return p.pending === 0 ? ['ok', 'All caught up'] : ['', 'Waiting to analyze'];
  }
}

/** The right of the status line: "3 of 10 analyzed · 1 failed", or the count of the job in hand. */
function progressText(p) {
  if (p.phase === 'Categorizing' && p.sortTotal) return `${p.sortDone} of ${p.sortTotal}`;
  if ((p.phase === 'Moving' || p.phase === 'MovingPaused') && p.moveProgress) return `${p.moveProgress.done} of ${p.moveProgress.total}`;
  if (!p.total) return '';
  return `${p.analyzed} of ${p.total} analyzed${p.failed ? ` · ${p.failed} failed` : ''}`;
}

function cardSub(row) {
  if (row.kind === 'Photo') return row.status === 'Pending' ? 'Photo · waiting' : 'Photo';
  const cap = S.boot.settings.maxFramesPerClip;
  const ms = row.durationMs || 0;
  const atFps = Math.max(1, Math.ceil(ms / 500));
  const frames = Math.min(atFps, cap);
  return `${duration(ms)} · ${plural(frames, 'frame')}${atFps > cap ? ', evenly spaced' : ''}`;
}

const displayName = (row) => row.friendlyName || row.suggestedName || row.originalName;

function thumbImg(row) {
  return row.thumb
    ? h('img', { src: api.fileUrl(row.thumb, row.thumbAt), alt: '', loading: 'lazy', draggable: false })
    : icon(row.kind === 'Video' ? 'film' : 'image', 40);
}

// The cards are redrawn whenever the run reports progress (each video frame, each piece of the
// reply), and a new element would start its animation again from the top. The line takes its
// place in the sweep from the clock instead, so it carries on where the one before it was.
const SCAN_CYCLE_MS = 3600; // Down and back up: twice the 1.8 s in styles.css.
function scanLine() {
  return h('span.scanline', { style: `animation-delay:-${Math.round(performance.now() % SCAN_CYCLE_MS)}ms` });
}

function renderAnalyze() {
  const p = S.pipeline;
  const [tone, text] = phaseLine(p);
  const speed = running() && p.lastProfile && p.lastProfile.decodingSpeed ? `${Math.round(p.lastProfile.decodingSpeed)} tok/s` : '';
  $('runline').replaceChildren(
    h('div.phase', h(`i.dot${tone ? '.' + tone : ''}`), h('span', { title: text }, text)),
    h('div.mid', speed),
    h('div.right', progressText(p)),
  );

  const stage = $('stage');
  const rows = S.all ? S.queue : [];
  // The backdrop drifts only while items are analyzed: a moving backdrop is blurred again by every panel.
  $('backdrop').classList.toggle('moving', running() && !['Paused', 'Moving', 'MovingPaused'].includes(p.phase));
  if (!project() || !rows.length) tintBackdrop(null);
  if (!project()) {
    stage.replaceChildren(h('div.empty', { style: 'grid-column:1/-1;grid-row:1/-1' },
      h('h2', 'No project open'),
      h('p', 'Create a project, then add a folder or pick photos and videos from the project menu.'),
      h('button.primary', { style: 'flex:none', onclick: () => sheets.newProject() }, icon('plus'), 'Create project'),
      h('button.link', { onclick: () => sheets.projects() }, 'Open project')));
  } else if (!rows.length) {
    stage.replaceChildren(h('div.empty', { style: 'grid-column:1/-1;grid-row:1/-1' },
      h('h2', 'No media yet'),
      h('p', S.registration && !S.registration.summary ? (S.registration.total ? `Adding ${S.registration.done} of ${S.registration.total}…` : 'Adding…') : 'Add a folder or pick photos and videos from the project menu. You can also drop them on this window.'),
      h('div', { style: 'display:flex;gap:8px' },
        h('button.primary', { style: 'flex:none', onclick: () => sheets.addFolder() }, icon('folder'), 'Add a folder'),
        h('button.secondary', { onclick: () => sheets.addFiles() }, icon('image'), 'Pick photos and videos'))));
  } else {
    renderCarousel(stage, rows, p);
  }

  const paused = p.phase === 'Paused' || p.phase === 'MovingPaused';
  const canResume = paused || (!running() && project() && p.pending > 0);
  $('analyze-bottom').replaceChildren(
    h('button.round', { 'aria-label': 'Stop', title: 'Stop', disabled: !running(), onclick: () => api.call('analysis.stop').catch(fail) }, icon('stop')),
    canResume || !running()
      ? h('button.round', { 'aria-label': 'Resume', title: 'Resume (Space)', disabled: !canResume, onclick: () => api.call('analysis.resume').catch(fail) }, icon('play'))
      : h('button.round', { 'aria-label': 'Pause', title: 'Pause after this one (Space)', disabled: p.phase === 'Categorizing', onclick: () => api.call('analysis.pause').catch(fail) }, icon('pause')),
    h('button.primary', { disabled: !project(), onclick: () => show('media') }, icon('grid'), 'Project Media', S.all ? h('span.cnt', S.all.total.toLocaleString()) : null),
  );
}

// The carousel, its cards and the text panel are kept between draws. A card slides because its
// own element changes place; one built anew for every draw would only ever appear where it ends up.
let carouselEl = null;
let outputEl = null;
let navPrev = null;
let navNext = null;
const cardEls = new Map(); // item id -> its card

function buildCarousel() {
  navPrev = h('button.ib.navbtn.l.glass', { 'aria-label': 'Previous', onclick: (e) => { e.stopPropagation(); step(-1); } }, icon('back'));
  navNext = h('button.ib.navbtn.r.glass', { 'aria-label': 'Next', onclick: (e) => { e.stopPropagation(); step(1); } }, icon('next'));
  const carousel = h('div.carousel', { tabindex: 0, 'aria-label': 'Items', role: 'group' }, navPrev, navNext);
  carousel.addEventListener('wheel', (e) => {
    if (Math.abs(e.deltaY) + Math.abs(e.deltaX) < 20) return;
    e.preventDefault();
    if (carousel.dataset.wheel) return;
    carousel.dataset.wheel = '1';
    step(e.deltaY + e.deltaX > 0 ? 1 : -1);
    setTimeout(() => { delete carousel.dataset.wheel; }, 220);
  }, { passive: false });
  carouselEl = carousel;
  outputEl = h('div.output.glass');
}

function renderCarousel(stage, rows, p) {
  followCurrent();
  const at = S.index;
  if (!carouselEl) buildCarousel();
  // Back on the stage after an empty screen: the cards start afresh, with nothing to slide from.
  if (carouselEl.parentNode !== stage) {
    for (const el of cardEls.values()) el.remove();
    cardEls.clear();
    stage.replaceChildren(carouselEl, outputEl);
  }
  const wanted = new Set();
  const entering = [];
  const fresh = new Map();
  for (let i = Math.max(0, at - 2); i <= Math.min(rows.length - 1, at + 2); i++) {
    const row = rows[i];
    const d = i - at;
    const cls = d === 0 ? '' : d === -1 ? ' prev' : d === 1 ? ' next' : d < 0 ? ' far-prev' : ' far-next';
    const isCurrent = p.currentItemId === row.id;
    // Every clip shows its frame dots; they light up as the frames are read, and stay lit once analyzed.
    let frames = null;
    if (row.kind === 'Video') {
      const live = isCurrent && running();
      const total = live && p.framesTotal ? p.framesTotal : Math.min(Math.max(1, Math.ceil((row.durationMs || 0) / 500)), S.boot.settings.maxFramesPerClip);
      const lit = live ? (p.phase === 'Describing' ? total : p.framesReady) : (row.hasAnalysis ? total : 0);
      frames = h('div.frames', Array.from({ length: total }, (_, k) => h(`i${k < lit ? '.on' : ''}`)));
    }
    // Top corner: Analyzed, then the category once sorted; Failed or Missing; nothing while it waits.
    const badge = row.status === 'Analyzed' ? h('span.bdg.cat', icon('check', 11), 'Analyzed')
      : row.status === 'Sorted' || row.status === 'Binned' ? h('span.bdg.cat', icon('check', 11), h('span', row.categoryName || 'Sorted'))
      : row.status === 'Failed' || row.status === 'Missing' ? h('span.bdg.cat', icon('warn', 11), row.status) : null;
    const scanning = isCurrent && (p.phase === 'Preparing' || p.phase === 'Describing');
    if (row.kind !== 'Video') {
      const part = isCurrent && running() ? (p.phase === 'Describing' ? 0.7 : 0.3) : 0;
      frames = h('div.pbar', h('i', { style: `width:${row.hasAnalysis ? 100 : part * 100}%` }));
    }
    const className = `card glass strong${cls}`;
    wanted.add(row.id);
    let card = cardEls.get(row.id);
    if (!card) {
      // A card new to the window comes in from the far side it is on.
      card = h('div', { 'data-id': row.id });
      card.className = cardEls.size ? `card glass strong ${d < 0 ? 'far-prev' : 'far-next'}` : className;
      carouselEl.insertBefore(card, navPrev);
      if (card.className !== className) entering.push([card, className]);
    } else if (card.className !== className) card.className = className;
    card.onclick = () => { if (d !== 0) { S.index = i; S.follow = false; render(); } else sheets.detail(row.id); };
    card.title = d === 0 ? 'Open details' : '';
    // The picture stays as it is unless it changed: a new <img> would flash while it loads.
    const src = row.thumb ? api.fileUrl(row.thumb, row.thumbAt) : '';
    const shot = card.firstChild && card.firstChild.dataset.src === src ? card.firstChild : h('div.shot', { 'data-src': src }, thumbImg(row));
    while (shot.childNodes.length > 1) shot.lastChild.remove();
    shot.append(
      scanning ? scanLine() : null,
      row.kind === 'Video' ? h('span.bdg.dur', icon('play', 10), duration(row.durationMs)) : null,
      badge);
    card.replaceChildren(shot,
      h('h2', isCurrent || !row.friendlyName ? row.originalName : displayName(row)),
      h('p', cardSub(row)),
      frames);
    fresh.set(row.id, card);
  }
  for (const [id, card] of cardEls) {
    if (!wanted.has(id)) { card.remove(); cardEls.delete(id); }
  }
  for (const [id, card] of fresh) cardEls.set(id, card);
  if (entering.length) {
    void carouselEl.offsetWidth; // the starting place has to be laid out before the move to the real one
    for (const [card, className] of entering) card.className = className;
  }
  navPrev.disabled = at <= 0;
  navNext.disabled = at >= rows.length - 1;
  renderOutput(outputEl, rows[at], p);
  tintBackdrop(rows[at]);
}

export function step(by) {
  if (!S.all || !S.queue.length) return;
  const next = Math.max(0, Math.min(S.queue.length - 1, S.index + by));
  if (next === S.index) return;
  S.index = next;
  // Walking back to the item being analyzed picks the live view up again.
  S.follow = S.pipeline.currentItemId === S.queue[next].id;
  render();
}

function analysisBlocks(description, actions, tags, live) {
  const out = [];
  out.push(h('h3', 'Description'));
  out.push(h('p.desc.selectable', description || (live ? '' : '—'), live ? h('span.caret') : null));
  if (actions && actions.length) {
    out.push(h('h3', 'Action'));
    out.push(h('ul.actions.selectable', actions.map((a) => h('li', h('time', a.t), h('span', a.text)))));
  }
  if (tags && tags.length) {
    out.push(h('div.tags', tags.map((t) => h('span.tag', `#${t}`))));
  }
  return out;
}

async function renderOutput(box, row, p) {
  if (!row) return;
  if (p.currentItemId === row.id && running()) {
    const part = p.partial || { description: '', descriptionDone: false, actions: [], tags: [] };
    const waiting = p.phase === 'Preparing' || p.phase === 'LoadingModel';
    box.replaceChildren(...(waiting && !part.description
      ? [h('h3', 'Description'), h('p.desc', { style: 'color:var(--sub)' }, row.kind === 'Video' ? `Reading ${p.framesTotal || 'the'} frames on the NPU…` : 'Reading the image on the NPU…')]
      : analysisBlocks(part.description, part.actions, part.tags, p.phase === 'Describing')));
    box.scrollTop = box.scrollHeight;
    return;
  }
  let d = S.details.get(row.id);
  if (!d) {
    try {
      d = await api.call('item.detail', { id: row.id });
      S.details.set(row.id, d);
    } catch (e) { return fail(e); }
    if (!S.all || S.queue[S.index] !== row) return; // the carousel moved on meanwhile
  }
  if (!d) return;
  const blocks = [];
  if (d.simulated) blocks.push(h('div.note', icon('info', 18), h('span.grow', h('b', 'Simulated analysis. '), 'GenieX was not used for this item. Install GenieX and the model, then use Retry or re-add it for a real analysis.')));
  if (row.status === 'Failed') blocks.push(h('div.note', icon('warn', 18), h('span.grow', h('b', 'Analysis failed. '), d.failure), h('button.link', { onclick: () => sheets.detail(row.id) }, 'Fix')));
  if (row.status === 'Missing') blocks.push(h('div.note', icon('warn', 18), h('span.grow', 'The file is missing.')));
  if (d.analysis) blocks.push(...analysisBlocks(d.analysis.description, d.analysis.actions, d.tags, false));
  else if (row.status === 'Pending' || row.status === 'Analyzing') blocks.push(h('h3', 'Description'), h('p.desc', { style: 'color:var(--sub)' }, 'Waiting to be analyzed.'));
  box.replaceChildren(...blocks);
}

// The Analyze backdrop, as the Android app's ThumbnailBackdrop: the picture in view shrunk to 20 px
// on its long edge and box-blurred, so only soft colour fields are left, then stretched over the
// window. The numbers are Android's; the fade, drift and scrim are in styles.css.
const ABSTRACT_EDGE_PX = 20;
const BLUR_RADIUS_PX = 3;
const BLUR_PASSES = 3;
const CROSSFADE_MS = 2300;

/** One pass of a box blur along rows or columns, edges repeated. */
function boxBlur(px, w, h, horizontal) {
  const src = px.slice();
  const [len, lines] = horizontal ? [w, h] : [h, w];
  const at = (line, i) => 4 * (horizontal ? line * w + i : i * w + line);
  for (let line = 0; line < lines; line++) {
    for (let i = 0; i < len; i++) {
      for (let ch = 0; ch < 3; ch++) {
        let sum = 0;
        for (let k = -BLUR_RADIUS_PX; k <= BLUR_RADIUS_PX; k++) sum += src[at(line, Math.max(0, Math.min(len - 1, i + k))) + ch];
        px[at(line, i) + ch] = sum / (2 * BLUR_RADIUS_PX + 1);
      }
    }
  }
}

/** A tiny blurred copy of `img` as a data URL; the browser only has to stretch it. */
function abstractColour(img) {
  const scale = ABSTRACT_EDGE_PX / Math.max(img.naturalWidth, img.naturalHeight, 1);
  const c = document.createElement('canvas');
  c.width = Math.max(2, Math.round(img.naturalWidth * scale));
  c.height = Math.max(2, Math.round(img.naturalHeight * scale));
  const g = c.getContext('2d', { willReadFrequently: true });
  g.drawImage(img, 0, 0, c.width, c.height);
  const data = g.getImageData(0, 0, c.width, c.height);
  for (let pass = 0; pass < BLUR_PASSES; pass++) {
    boxBlur(data.data, c.width, c.height, true);
    boxBlur(data.data, c.width, c.height, false);
  }
  g.putImageData(data, 0, 0);
  return c.toDataURL('image/png');
}

let backdropFor = null;
/**
 * Puts the picture in view behind the Analyze screen and takes the three blob colours from it.
 * A row without a thumbnail yet keeps the last picture; no row at all (no project, no media, or
 * another screen) goes back to the blobs.
 */
function tintBackdrop(row) {
  const photo = $('backdrop-photo');
  if (!row) {
    backdropFor = null;
    photo.classList.remove('on');
    return;
  }
  const key = row.thumb ? `${row.thumb}|${row.thumbAt}` : null;
  if (!key || backdropFor === key) return;
  backdropFor = key;
  const img = new Image();
  img.crossOrigin = 'anonymous';
  img.onload = () => {
    if (backdropFor !== key) return; // the carousel moved on while it loaded
    try {
      const c = document.createElement('canvas');
      c.width = 3; c.height = 1;
      const g = c.getContext('2d', { willReadFrequently: true });
      g.drawImage(img, 0, 0, 3, 1);
      const px = g.getImageData(0, 0, 3, 1).data;
      document.querySelectorAll('#backdrop i').forEach((el, i) => { el.style.backgroundColor = `rgb(${px[i * 4]},${px[i * 4 + 1]},${px[i * 4 + 2]})`; });
      const layer = h('div.layer', { style: `background-image:url(${abstractColour(img)})` });
      const drift = photo.querySelector('.drift');
      const before = [...drift.children];
      drift.append(layer);
      layer.getBoundingClientRect(); // so the fade starts from clear
      layer.classList.add('in');
      photo.classList.add('on');
      // The pictures underneath are covered once the new one has faded in.
      setTimeout(() => before.forEach((old) => old.remove()), CROSSFADE_MS + 100);
    } catch { /* a picture the canvas may not read: keep what is shown */ }
  };
  img.src = api.fileUrl(row.thumb, row.thumbAt);
}

// ---------------------------------------------------------------------------------------------
// Project Media
// ---------------------------------------------------------------------------------------------

let searchTimer = null;

function renderMedia() {
  const v = S.view;
  const body = $('media-body');
  if (!project() || !v) {
    body.replaceChildren(h('div.empty', h('h2', 'No project open'), h('button.primary', { style: 'flex:none', onclick: () => sheets.projects() }, 'Projects')));
    $('media-dock').replaceChildren();
    return;
  }
  // The search field keeps its node so typing never loses focus or the caret.
  let search = $('search-input');
  if (!search) {
    search = h('input#search-input', { type: 'search', placeholder: 'Search “sea turtles on the sand”', 'aria-label': 'Search this project', spellcheck: false, autocomplete: 'off' });
    search.addEventListener('input', () => {
      S.query = search.value;
      clearTimeout(searchTimer);
      searchTimer = setTimeout(refresh, 140);
    });
    $('media-search').replaceChildren(icon('search'), search, h('button#search-clear.ib.sm', { 'aria-label': 'Clear search', onclick: () => { search.value = ''; S.query = ''; refresh(); search.focus(); } }, icon('x', 16)));
  }
  if (search.value !== S.query && document.activeElement !== search) search.value = S.query;
  $('search-clear').style.visibility = S.query ? 'visible' : 'hidden';
  search.disabled = !v.total;
  search.placeholder = v.total ? 'Search “sea turtles on the sand”' : 'Nothing to search yet';
  if (!v.total) {
    // A project with nothing in it: the two ways to add media, as in the project sheet.
    body.replaceChildren(h('div.emptycard.glass',
      h('span.ico3', icon('image', 30)),
      h('h2', 'No photos or clips yet'),
      h('p', 'Add media and the computer describes each item, then sorts everything into categories. Files stay where they are until you press Bin Files.'),
      h('button.primary', { onclick: () => sheets.addFolder() }, icon('folder'), 'Add a folder'),
      h('button.secondary', { onclick: () => sheets.addFiles() }, icon('plus'), 'Pick photos and videos')));
    renderDock(v);
    return;
  }

  const busy = running();
  const cats = v.categories.length;
  const chipEl = (c) => h(`button.chip${c.chip === 'waiting' || c.chip === 'failed' ? '.attn' : ''}`, {
    'aria-pressed': String(S.filter.chip === c.chip),
    onclick: () => { S.filter.chip = S.filter.chip === c.chip && c.chip !== 'all' ? 'all' : c.chip; refresh(); },
  }, c.chip === 'waiting' ? icon('clock', 13) : c.chip === 'failed' ? icon('warn', 13) : c.locked ? icon('lock', 12) : null, c.label, h('span.n', c.count.toLocaleString()));
  const kind = (label, value) => h('button', { 'aria-pressed': String(S.filter.kind === value), onclick: () => { S.filter.kind = value; refresh(); } }, label);
  const chipLabel = v.chips.find((c) => c.chip === S.filter.chip);
  const chipName = S.filter.chip === 'all' ? 'All categories' : S.filter.chip === 'waiting' ? 'Waiting to be categorized' : chipLabel ? chipLabel.label : 'Category';
  const resultText = v.searching
    ? `${plural(v.rows.length, 'result')} for “${S.query.trim()}”${S.filter.chip !== 'all' ? ` in ${chipName}` : ''}`
    : `${chipName} · ${plural(v.rows.length, 'item')}`;
  const shotLabel = { wide: 'Wide', medium: 'Medium', close: 'Close-up' };
  const anySet = S.filter.shotType || S.filter.takenFrom || S.filter.takenTo;

  const head = h('div', { style: 'display:flex;flex-direction:column;gap:10px;flex:none' },
    h('div.cathead',
      h('h3', `Categories · ${cats}`),
      h('button.mini', { disabled: busy, title: busy ? (moving() ? 'Edits wait until the files have moved' : 'Re-sort is available when analysis is stopped or done') : 'Sort everything again', onclick: () => sheets.resort() }, icon('shuffle', 15), 'Re-sort'),
      h('button.mini', { disabled: busy || !cats, title: busy ? (moving() ? 'Edits wait until the files have moved' : 'Edit is available when analysis is stopped or done') : '', onclick: () => sheets.categoryEdit() }, icon('edit', 15), 'Edit')),
    h('div.chips', v.chips.map(chipEl)),
    h('div.filters',
      h('div.seg', { role: 'group', 'aria-label': 'Kind' }, kind('All', null), kind('Photos', 'Photo'), kind('Videos', 'Video')),
      S.filter.shotType ? h('span.fchip.set', shotLabel[S.filter.shotType] || S.filter.shotType, h('button', { 'aria-label': 'Clear shot type', onclick: () => { S.filter.shotType = null; refresh(); } }, icon('x', 13))) : null,
      S.filter.takenFrom ? h('span.fchip.set', sheets.dayLabel(S.filter.takenFrom), h('button', { 'aria-label': 'Clear date', onclick: () => { S.filter.takenFrom = null; S.filter.takenTo = null; refresh(); } }, icon('x', 13))) : null,
      h(`button.fchip${anySet ? '.set' : ''}`, { onclick: () => sheets.filters() }, 'Filters', icon('down', 14))),
    h('div.resultline',
      h('span.result', resultText),
      S.selecting ? null : h('button.mini', { title: 'Select tiles (or Ctrl-click one)', onclick: () => { S.selecting = true; S.selected.clear(); render(); } }, icon('checkbox', 15), 'Select')));

  const tiles = v.rows.map((row) => {
    const on = S.selected.has(row.id);
    // Under the name only where a search matched; the picture carries the rest (Waiting, Failed, Analyzing).
    const sub = v.searching && row.matchedIn && row.matchedIn !== 'Name' ? `Matched in ${row.matchedIn.toLowerCase()}` : null;
    const waiting = row.status === 'Analyzed' && !row.categoryId && !row.categorySetByUser;
    const badge = row.status === 'Failed' ? h('span.bdg.st.warn', icon('warn', 11), 'Failed')
      : row.status === 'Missing' ? h('span.bdg.st.warn', 'Missing')
      : row.status === 'Analyzing' ? h('span.bdg.st', 'Analyzing…')
      : waiting ? h('span.bdg.st', icon('clock', 11), 'Waiting') : null;
    return h(`button.tile${on ? '.on' : ''}${row.status === 'Analyzing' ? '.dimmed' : ''}`, {
      'data-id': row.id,
      title: displayName(row),
      onclick: (e) => {
        if (S.selecting || e.ctrlKey || e.metaKey) {
          S.selecting = true;
          if (S.selected.has(row.id)) S.selected.delete(row.id); else S.selected.add(row.id);
          render();
        } else sheets.detail(row.id);
      },
    },
      h('span.art', thumbImg(row),
        S.selecting ? h('span.tick', icon('check', 14)) : null,
        row.status === 'Binned' ? h('span.bdg.bin', { title: 'In its category folder' }, icon('bin', 14)) : null,
        row.kind === 'Video' ? h('span.bdg.dur', icon('play', 10), duration(row.durationMs)) : null,
        badge),
      h('span.tname', displayName(row)),
      sub ? h('span.tsub', sub) : null);
  });
  const keepScroll = body.querySelector('.gridwrap');
  const scrollTop = keepScroll ? keepScroll.scrollTop : 0;
  const wrap = h('div.gridwrap', tiles.length ? h('div.grid', tiles) : h('p.nomatch', v.searching ? `Nothing in ${project().name} matches “${S.query.trim()}”.` : 'No items match these filters.'));
  body.replaceChildren(head, wrap);
  wrap.scrollTop = scrollTop;
  renderDock(v);
}

function renderDock(v) {
  const p = S.pipeline;
  const dock = $('media-dock');
  const parts = [];
  if (moving()) {
    const m = p.moveProgress;
    const paused = p.phase === 'MovingPaused';
    const save = m && m.kind === 'Save';
    const title = paused ? 'Paused' : save ? 'Saving' : 'Binning files';
    const rows = [];
    for (const f of (m && m.recent) || []) rows.push(h('li.done', h('span.ico', icon('check', 12)), h('span', h('b', f.name), h('small', f.folder)), h('span.ph2', 'Done')));
    parts.push(h('div.job.glass',
      h('div.top', h('b', title), h('span', m ? `${paused ? m.done : Math.min(m.done + 1, m.total)} of ${m.total}` : '')),
      h('div.bar', h('i', { style: `width:${m && m.total ? (100 * m.done) / m.total : 0}%` })),
      paused && m && m.next ? h('div.now', h('b', `Next: ${m.next}`), h('small', 'The file in progress finished. Nothing is half moved.'))
        : m && m.current ? h('div.now', h('b', m.current), h('small', `${m.from} → ${m.to}`))
        : h('div.now', h('small', m ? '' : 'Checking the folders')),
      rows.length ? h('ul.steps', rows) : null,
      h('div.row',
        h('span.wait', icon('info', 14), paused ? 'Other edits wait until it ends.' : 'Other edits wait. You can leave this screen.'),
        h('button.mini', { onclick: () => api.call(paused ? 'analysis.resume' : 'analysis.pause').catch(fail) }, icon(paused ? 'play' : 'pause', 16), paused ? 'Resume' : 'Pause'),
        h('button.mini', { onclick: () => api.call('analysis.stop').catch(fail) }, icon('stop', 16), 'Stop'))));
  }
  if (S.selecting) {
    const n = S.selected.size;
    parts.push(h('div.dock.glass', h('div.dbtns',
      h('button.primary', { 'aria-disabled': String(!n), disabled: !n, onclick: () => sheets.moveTo([...S.selected]) }, icon('move'), 'Move to category'),
      h('button.secondary.danger', { style: 'flex:1', disabled: !n, onclick: () => sheets.removeItems([...S.selected]) }, icon('trash'), 'Remove from project'))));
  } else {
    const st = v.status;
    const tone = { Running: 'run', Attention: 'warn', Ready: 'ok', Quiet: '' }[st.tone];
    const notice = !running() && p.sortNotice ? p.sortNotice : null;
    parts.push(h('div.dock.glass',
      h('div.status', h(`i.dot${tone ? '.' + tone : ''}`),
        h('span.stxt', st.text, notice ? h('span', { style: 'color:var(--sub)' }, ` · ${notice}`) : null),
        st.categorizeNow && !running() ? h('button.link', { onclick: () => api.call('analysis.categorizeNow').catch(fail) }, 'Categorize now') : null),
      h('div.dbtns',
        h('button.secondary', { title: 'The live analysis view', onclick: () => show('analyze') }, h('i', { style: 'background:var(--ok)' }), 'Analyze'),
        h(`button.primary${st.binFilesEnabled ? '' : '.off'}`, {
          'aria-disabled': String(!st.binFilesEnabled),
          onclick: () => (st.binFilesEnabled ? sheets.binFiles() : toast(st.blockedReason || 'Nothing to bin yet')),
        }, icon('bin'), 'Bin Files', st.binFilesEnabled && st.readyToBin ? h('span.cnt', st.readyToBin.toLocaleString()) : null))));
  }
  dock.replaceChildren(...parts);
}

// ---------------------------------------------------------------------------------------------
// Events from the core, keys, drag and drop
// ---------------------------------------------------------------------------------------------

let lastMoveEnded = null;
let pipelineFrame = 0;

function onPipeline(state) {
  const before = S.pipeline;
  S.pipeline = state;
  if (lastMoveEnded === null) lastMoveEnded = before ? before.moveEnded : state.moveEnded;
  if (state.moveEnded !== lastMoveEnded) {
    lastMoveEnded = state.moveEnded;
    refreshSoon(0);
    setTimeout(() => sheets.reportIfUnread(state), 150);
  }
  if (!before || before.phase !== state.phase) {
    api.keepAwake(S.boot.settings.keepAwake && running());
    if (before && ['Idle', 'Stopped'].includes(state.phase)) refreshSoon(0);
  }
  if (before && before.currentItemId !== state.currentItemId) {
    S.follow = S.follow || S.screen !== 'analyze';
    followCurrent();
  }
  // Streaming text arrives many times a second: draw once per frame.
  cancelAnimationFrame(pipelineFrame);
  pipelineFrame = requestAnimationFrame(() => { if (S.screen === 'analyze') renderAnalyze(); else if (S.view) renderDock(S.view); });
}

function onKey(e) {
  if (e.key === 'Escape') {
    if (closeTopSheet()) return;
    if (S.selecting) { S.selecting = false; S.selected.clear(); render(); return; }
    if (S.screen === 'media') show('analyze');
    return;
  }
  if (sheetOpen()) return;
  const typing = ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName);
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') {
    e.preventDefault();
    if (S.screen !== 'media' && project()) show('media');
    setTimeout(() => { const s = $('search-input'); if (s) { s.focus(); s.select(); } }, 60);
    return;
  }
  if (typing) return;
  if (S.screen === 'analyze') {
    if (e.key === 'ArrowLeft') step(-1);
    else if (e.key === 'ArrowRight') step(1);
    else if (e.key === ' ' && project()) {
      e.preventDefault();
      const paused = S.pipeline.phase === 'Paused' || S.pipeline.phase === 'MovingPaused' || !running();
      api.call(paused ? 'analysis.resume' : 'analysis.pause').catch(fail);
    } else if (e.key === 'Enter' && S.all && S.queue[S.index] && document.activeElement.tagName !== 'BUTTON') sheets.detail(S.queue[S.index].id);
    else if (e.key.toLowerCase() === 'm' && project()) show('media');
  } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a' && S.view) {
    e.preventDefault();
    S.selecting = true;
    S.view.rows.forEach((r) => S.selected.add(r.id));
    render();
  }
}

/** Files and folders dropped on the window are added to the open project. */
async function onDrop(paths) {
  $('drop').classList.remove('on');
  if (!paths.length) return;
  if (!project()) return toast('Make or open a project first, then drop the files again');
  try {
    S.registration = { done: 0, total: 0 };
    const looksLikeFolder = (p) => !/\.[A-Za-z0-9]{2,5}$/.test(p);
    const folders = paths.filter(looksLikeFolder);
    const files = paths.filter((p) => !looksLikeFolder(p));
    for (const f of folders) await api.call('media.addFolder', { path: f });
    if (files.length) await api.call('media.addFiles', { paths: files });
  } catch (e) { fail(e); }
}

async function start() {
  await reboot();
  api.on('pipeline', onPipeline);
  api.on('data', () => refreshSoon());
  api.on('projects', async () => { S.boot.projects = await api.call('projects.list'); S.follow = true; S.index = 0; await refresh(); });
  api.on('registration', (r) => {
    S.registration = r.finished ? { summary: r.summary } : r;
    sheets.onRegistration(S.registration);
    if (r.finished) {
      if (r.summary.held.length) sheets.heldNotice(r.summary); else toast(r.summary.text);
      refreshSoon(0);
    } else if (S.screen === 'analyze' && S.all && !S.all.rows.length) renderAnalyze();
  });
  api.on('copy', (r) => { sheets.onCopy(r); if (r.finished) refreshSoon(0); });
  api.on('pull', (r) => sheets.onPull(r));
  api.on('install', (r) => sheets.onInstall(r));
  api.on('drop', onDrop);
  api.on('dragging', (on) => $('drop').classList.toggle('on', !!on));
  api.on('grab', (req) => sheets.grabFrames(req));
  await api.startEvents();
  document.addEventListener('keydown', onKey);
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', applyTheme);
  await refresh();
  sheets.reportIfUnread(S.pipeline);
}

start().catch((e) => {
  document.body.append(h('div.empty', { style: 'position:fixed;inset:0;z-index:50;display:grid;place-content:center' }, h('h2', 'Agentic Binning could not start'), h('p', String(e.message || e))));
});
