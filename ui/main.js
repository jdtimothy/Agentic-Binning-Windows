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
  thumbVersion: 0,
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
    render();
    return;
  }
  try {
    S.all = await api.call('grid', {});
    S.view = filtered() ? await api.call('grid', { filter: S.filter, query: S.query }) : S.all;
    S.thumbVersion = S.all.rows.filter((r) => r.thumb).length;
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
  const rows = S.all.rows;
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
  if (screen === 'media') refresh();
  render();
}

function renderChrome() {
  const b = S.boot;
  const cur = project();
  const top = $('analyze-top');
  top.replaceChildren(
    h('button.pill', { title: 'Models and settings', onclick: () => sheets.settings() },
      icon('chip'),
      h('span.two', h('b', b.simulated ? 'Simulated' : b.models.vision.displayName), h('span', b.simulated ? 'GenieX not in use' : b.pulling === b.models.vision.id ? 'Downloading' : b.genie.visionReady || b.genie.error ? b.settings.compute.toUpperCase() : 'Not downloaded'))),
    h('button.pill.grow', { title: cur ? 'Add media or switch project' : 'Open projects', onclick: () => (cur || S.boot.projects.projects.length ? sheets.projects() : sheets.newProject()) },
      icon('folder'),
      h('span.name', cur ? cur.name : 'Create project'),
      cur && S.all ? h('span.count', S.all.total.toLocaleString()) : null),
    b.simulated ? h('span.simtag', { title: 'GenieX was not found, so analysis is simulated from each picture\'s colours.' }, 'Simulated') : null,
    h('button.ib', { 'aria-label': 'Settings', title: 'Settings', onclick: () => sheets.settings() }, icon('gear')),
  );
  const mtop = $('media-top');
  mtop.replaceChildren(
    h('button.ib', { 'aria-label': 'Back to Analyze', title: 'Back (Esc)', onclick: () => show('analyze') }, icon('back')),
    h('div.ht', h('h1', cur ? cur.name : ''), h('span', S.all ? `${plural(S.all.total, 'item')} · ${plural(S.all.categories.length, 'category', 'categories')}` : '')),
    b.simulated ? h('span.simtag', 'Simulated') : null,
    h('button.ib', { 'aria-label': 'Settings', title: 'Settings', onclick: () => sheets.settings() }, icon('gear')),
  );
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
    ? h('img', { src: api.fileUrl(row.thumb, row.status + S.thumbVersion), alt: '', loading: 'lazy', draggable: false })
    : icon(row.kind === 'Video' ? 'film' : 'image', 40);
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
  const rows = S.all ? S.all.rows : [];
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

function renderCarousel(stage, rows, p) {
  followCurrent();
  const at = S.index;
  const cards = [];
  for (let i = Math.max(0, at - 2); i <= Math.min(rows.length - 1, at + 2); i++) {
    const row = rows[i];
    const d = i - at;
    const cls = d === 0 ? '' : d === -1 ? '.prev' : d === 1 ? '.next' : d < 0 ? '.far-prev' : '.far-next';
    const isCurrent = p.currentItemId === row.id;
    // Every clip shows its frame dots; they light up as the frames are read, and stay lit once analyzed.
    let frames = null;
    if (row.kind === 'Video') {
      const live = isCurrent && running();
      const total = live && p.framesTotal ? p.framesTotal : Math.min(Math.max(1, Math.ceil((row.durationMs || 0) / 500)), S.boot.settings.maxFramesPerClip);
      const lit = live ? (p.phase === 'Describing' ? total : p.framesReady) : (row.hasAnalysis ? total : 0);
      frames = h('div.frames', Array.from({ length: total }, (_, k) => h(`i${k < lit ? '.on' : ''}`)));
    }
    const badge = row.status === 'Failed' ? h('span.bdg.st.warn', 'Failed') : row.status === 'Missing' ? h('span.bdg.st.warn', 'Missing') : row.status === 'Pending' ? h('span.bdg.st', 'Waiting') : null;
    cards.push(h(`div.card.glass.strong${cls}`, {
      'data-id': row.id,
      onclick: () => { if (d !== 0) { S.index = i; S.follow = false; render(); } else sheets.detail(row.id); },
      title: d === 0 ? 'Open details' : '',
    },
      h('div.shot', thumbImg(row),
        isCurrent && running() ? h('span.progress', { style: `width:${p.phase === 'Describing' ? 100 : p.framesTotal ? (100 * p.framesReady) / p.framesTotal : 30}%` }) : null,
        row.categoryName ? h('span.bdg.cat', row.categoryName) : null,
        row.kind === 'Video' ? h('span.bdg.dur', icon('play', 10), duration(row.durationMs)) : null,
        badge),
      h('h2', isCurrent || !row.friendlyName ? row.originalName : displayName(row)),
      h('p', cardSub(row)),
      frames));
  }
  const carousel = h('div.carousel', { tabindex: 0, 'aria-label': 'Items', role: 'group' },
    cards,
    h('button.ib.navbtn.l.glass', { 'aria-label': 'Previous', disabled: at <= 0, onclick: (e) => { e.stopPropagation(); step(-1); } }, icon('back')),
    h('button.ib.navbtn.r.glass', { 'aria-label': 'Next', disabled: at >= rows.length - 1, onclick: (e) => { e.stopPropagation(); step(1); } }, icon('next')));
  carousel.addEventListener('wheel', (e) => {
    if (Math.abs(e.deltaY) + Math.abs(e.deltaX) < 20) return;
    e.preventDefault();
    if (carousel.dataset.wheel) return;
    carousel.dataset.wheel = '1';
    step(e.deltaY + e.deltaX > 0 ? 1 : -1);
    setTimeout(() => { delete carousel.dataset.wheel; }, 220);
  }, { passive: false });
  const output = h('div.output.glass');
  stage.replaceChildren(carousel, output);
  renderOutput(output, rows[at], p);
  tintBackdrop(rows[at]);
}

export function step(by) {
  if (!S.all || !S.all.rows.length) return;
  const next = Math.max(0, Math.min(S.all.rows.length - 1, S.index + by));
  if (next === S.index) return;
  S.index = next;
  // Walking back to the item being analyzed picks the live view up again.
  S.follow = S.pipeline.currentItemId === S.all.rows[next].id;
  render();
}

function analysisBlocks(description, actions, tags, live) {
  const out = [];
  out.push(h('h3', 'Description'));
  out.push(h('p.desc.selectable', description || (live ? '' : '—'), live ? h('span.caret') : null));
  if (actions && actions.length) {
    out.push(h('h3', 'Actions'));
    out.push(h('ul.actions.selectable', actions.map((a) => h('li', h('time', a.t), h('span', a.text)))));
  }
  if (tags && tags.length) {
    out.push(h('h3', 'Tags'));
    out.push(h('div.tags', tags.map((t) => h('span.tag', t))));
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
    if (!S.all || S.all.rows[S.index] !== row) return; // the carousel moved on meanwhile
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

let tintFor = null;
/** Takes the backdrop's three blob colours from the picture in view. */
function tintBackdrop(row) {
  if (!row || !row.thumb || tintFor === row.thumb) return;
  tintFor = row.thumb;
  const img = new Image();
  img.crossOrigin = 'anonymous';
  img.onload = () => {
    try {
      const c = document.createElement('canvas');
      c.width = 3; c.height = 1;
      const g = c.getContext('2d', { willReadFrequently: true });
      g.drawImage(img, 0, 0, 3, 1);
      const px = g.getImageData(0, 0, 3, 1).data;
      document.querySelectorAll('#backdrop i').forEach((el, i) => { el.style.backgroundColor = `rgb(${px[i * 4]},${px[i * 4 + 1]},${px[i * 4 + 2]})`; });
    } catch { /* a picture the canvas may not read: keep the colours */ }
  };
  img.src = api.fileUrl(row.thumb);
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
    search = h('input#search-input', { type: 'search', placeholder: 'Search names, tags, descriptions', 'aria-label': 'Search this project', spellcheck: false, autocomplete: 'off' });
    search.addEventListener('input', () => {
      S.query = search.value;
      clearTimeout(searchTimer);
      searchTimer = setTimeout(refresh, 140);
    });
    $('media-search').replaceChildren(icon('search'), search, h('button#search-clear.ib.sm', { 'aria-label': 'Clear search', onclick: () => { search.value = ''; S.query = ''; refresh(); search.focus(); } }, icon('x', 16)));
  }
  if (search.value !== S.query && document.activeElement !== search) search.value = S.query;
  $('search-clear').style.visibility = S.query ? 'visible' : 'hidden';

  const busy = running();
  const cats = v.categories.length;
  const chipEl = (c) => h(`button.chip${c.chip === 'waiting' || c.chip === 'failed' ? '.attn' : ''}`, {
    'aria-pressed': String(S.filter.chip === c.chip),
    onclick: () => { S.filter.chip = S.filter.chip === c.chip && c.chip !== 'all' ? 'all' : c.chip; refresh(); },
  }, c.locked ? icon('lock', 13) : null, c.label, h('span.n', c.count.toLocaleString()));
  const kind = (label, value) => h('button', { 'aria-pressed': String(S.filter.kind === value), onclick: () => { S.filter.kind = value; refresh(); } }, label);
  const chipLabel = v.chips.find((c) => c.chip === S.filter.chip);
  const resultText = v.searching
    ? `${plural(v.rows.length, 'result')} for "${S.query.trim()}"`
    : `${chipLabel && S.filter.chip !== 'all' ? chipLabel.label + ' · ' : ''}${plural(v.rows.length, 'item')}`;

  const head = h('div', { style: 'display:flex;flex-direction:column;gap:10px;flex:none' },
    h('div.cathead',
      h('h3', 'Categories'),
      h('button.mini', { disabled: busy, title: busy ? 'Re-sort is available when analysis is stopped or done' : 'Sort everything again', onclick: () => sheets.resort() }, icon('shuffle', 16), 'Re-sort'),
      h('button.mini', { disabled: !cats, onclick: () => sheets.categoryEdit() }, icon('edit', 16), 'Edit')),
    h('div.chips', v.chips.map(chipEl)),
    h('div.filters',
      h('div.seg', { role: 'group', 'aria-label': 'Kind' }, kind('All', null), kind('Photos', 'Photo'), kind('Videos', 'Video')),
      h('button.fchip', { onclick: () => sheets.filters() }, 'Filters', icon('down', 14)),
      S.filter.shotType ? h('button.fchip.set', { title: 'Clear this filter', onclick: () => { S.filter.shotType = null; refresh(); } }, `${S.filter.shotType[0].toUpperCase()}${S.filter.shotType.slice(1)} shots`, icon('x', 13)) : null,
      S.filter.takenFrom || S.filter.takenTo ? h('button.fchip.set', { title: 'Clear this filter', onclick: () => { S.filter.takenFrom = null; S.filter.takenTo = null; refresh(); } },
        [S.filter.takenFrom, S.filter.takenTo].map((d) => (d ? new Date(d).toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) : '…')).join(' – '), icon('x', 13)) : null,
      h('span.result', resultText),
      S.selecting ? h('button.mini', { onclick: () => { v.rows.forEach((r) => S.selected.add(r.id)); render(); } }, 'Select all') : null,
      h('button.mini', { onclick: () => { S.selecting = !S.selecting; S.selected.clear(); render(); } }, icon('checkbox', 16), S.selecting ? 'Done' : 'Select')));

  const tiles = v.rows.map((row) => {
    const on = S.selected.has(row.id);
    const sub = row.matchedIn && row.matchedIn !== 'Name' ? `Matched in ${row.matchedIn.toLowerCase()}`
      : row.status === 'Failed' ? row.failure
      : row.status === 'Pending' || row.status === 'Analyzing' ? 'Waiting to analyze'
      : row.status === 'Analyzed' && !row.categoryId ? 'Waiting for a category'
      : S.filter.chip === 'all' && row.categoryName ? row.categoryName : null;
    const badge = row.status === 'Failed' ? h('span.bdg.st.warn', 'Failed') : row.status === 'Missing' ? h('span.bdg.st.warn', 'Missing') : null;
    return h(`button.tile${on ? '.on' : ''}`, {
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
  const wrap = h('div.gridwrap', tiles.length ? h('div.grid', tiles) : h('p.nomatch', v.total ? 'Nothing matches. Clear the search or the filters to see everything.' : 'This project has no photos or videos yet.'));
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
      h('button.primary', { 'aria-disabled': String(!n), disabled: !n, onclick: () => sheets.moveTo([...S.selected]) }, icon('move'), n ? `Move to category · ${n}` : 'Move to category'),
      h('button.secondary.danger', { disabled: !n, onclick: () => sheets.removeItems([...S.selected]) }, icon('trash'), 'Remove from project'))));
  } else {
    const st = v.status;
    const tone = { Running: 'run', Attention: 'warn', Ready: 'ok', Quiet: '' }[st.tone];
    const notice = !running() && p.sortNotice ? p.sortNotice : null;
    parts.push(h('div.dock.glass',
      h('div.status', h(`i.dot${tone ? '.' + tone : ''}`),
        h('span.stxt', st.text, notice ? h('span', { style: 'color:var(--sub)' }, ` · ${notice}`) : null),
        st.categorizeNow && !running() ? h('button.link', { onclick: () => api.call('analysis.categorizeNow').catch(fail) }, 'Categorize now') : null),
      h('div.dbtns',
        h('button.secondary', { title: 'The live analysis view', onclick: () => show('analyze') }, h('i', { style: running() ? 'background:var(--accent)' : '' }), 'Analyze'),
        h('button.secondary', { title: 'Add photos and videos', onclick: () => sheets.projects() }, icon('plus'), 'Add'),
        h(`button.primary${st.binFilesEnabled ? '' : '.off'}`, {
          'aria-disabled': String(!st.binFilesEnabled),
          onclick: () => (st.binFilesEnabled ? sheets.binFiles() : toast(st.blockedReason || 'Nothing to bin yet')),
        }, icon('bin'), 'Bin Files', st.readyToBin ? h('span.cnt', st.readyToBin.toLocaleString()) : null))));
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
    } else if (e.key === 'Enter' && S.all && S.all.rows[S.index] && document.activeElement.tagName !== 'BUTTON') sheets.detail(S.all.rows[S.index].id);
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
