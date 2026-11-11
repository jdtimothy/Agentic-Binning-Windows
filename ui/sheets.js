// Every sheet: projects, item details, categories, Re-sort, filters, Bin Files, the report, settings.

import * as api from './api.js';
import { h, icon, toast, fail, openSheet, confirmSheet, duration, plural } from './dom.js';
import { S, project, running, refresh, reboot, show, render } from './main.js';

const rowBtn = (ic, title, sub, onclick, extra) => h('button.rowbtn', { onclick }, h('span.ico2', icon(ic)), h('span.tx', h('b', title), sub ? h('span', sub) : null), extra || null);


/** A row of the Bin Files sheets: an icon, a line with an optional second line, and a count. */
const fact = (ic, text, sub, n, tone) => h(`div.fact${tone ? '.' + tone : ''}`, h('span.ico2', icon(ic, 18)), h('span.tx', h('b', text), sub ? h('span', sub) : null), n === null || n === undefined ? null : h('span.n', typeof n === 'number' ? n.toLocaleString() : n));

/** "Left where they are": failed and missing items, with an icon and a pill each. */
function leftList(rows) {
  if (!rows.length) return null;
  return h('div.sect', h('h3', `Left where they are · ${rows.length}`),
    rows.slice(0, 8).map((l) => h('div.fact', h('span.ico2.quiet', icon(l.reason === 'Failed' ? 'warn' : 'image', 18)),
      h('span.tx', h('b', l.name), h('span', l.reason === 'Failed' ? (l.why || 'Couldn\'t be analyzed') : 'Not found')),
      h('span.pilltag.warn', l.reason === 'Failed' ? 'Failed' : 'Missing'))),
    rows.length > 8 ? h('p.hint', `and ${rows.length - 8} more`) : null);
}

const files = (n) => (n === 1 ? '1 file' : `${n.toLocaleString()} files`);
const folders = (n) => (n === 1 ? '1 folder' : `${n.toLocaleString()} folders`);
const baseName = (path) => path.replace(/\\/g, '/').split('/').pop();
const parentName = (path) => { const parts = path.replace(/\\/g, '/').split('/'); return parts[parts.length - 2] || ''; };

// ---------------------------------------------------------------------------------------------
// Projects and adding media
// ---------------------------------------------------------------------------------------------

export async function addFolder() {
  try {
    const dir = await api.pickFolder('Add a folder');
    if (!dir) return;
    S.registration = { done: 0, total: 0 };
    onRegistration(S.registration);
    await api.call('media.addFolder', { path: dir });
  } catch (e) { fail(e); }
}

export async function addFiles() {
  try {
    const picked = await api.pickFiles('Pick photos and videos');
    if (!picked.length) return;
    S.registration = { done: 0, total: picked.length };
    onRegistration(S.registration);
    await api.call('media.addFiles', { paths: picked });
  } catch (e) { fail(e); }
}

/**
 * Media another project holds: one notice naming the projects and counts, with Cancel (the rest
 * of the batch is already added) and Copy media files, which copies them, checked, into
 * "Copied from <project>" folders, with progress in the notice.
 */
export function heldNotice(summary) {
  const held = summary.held;
  const n = held.length;
  const by = [];
  for (const f of held) {
    let e = by.find((x) => x.project === f.project);
    if (!e) by.push(e = { project: f.project, listed: f.listed, count: 0 });
    e.count += 1;
  }
  const names = by.map((e) => e.project);
  const others = by.length === 1 ? 'The other project\'s' : 'The other projects\'';
  openSheet((close, entry) => {
    const body = h('div.sbody');
    const foot = h('div.sfoot');
    const ask = () => {
      const added = summary.added;
      body.replaceChildren(
        h('p', { style: 'margin:0;font-size:14.5px' },
          by.map((e, i) => [i ? ' ' : '', 'In ', h('b', e.project), e.listed ? '' : ' (not on your list)', `: ${files(e.count)}.`]),
          added ? (added === 1 ? ' The other file you picked was added.' : ` The other ${added.toLocaleString()} files you picked were added.`) : ''),
        h('p.hint', `Copy media files puts a copy of ${n === 1 ? 'it' : 'each'} in `, by.map((e, i) => [i ? ' and ' : '', h('b', `${i ? '' : project().name + '/'}Copied from ${e.project}/`)]),
          `. ${others} ${n === 1 ? 'file stays exactly where it is' : 'files stay exactly where they are'}.`));
      foot.replaceChildren(h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'),
        h('button.primary', { style: 'flex:none', onclick: copy }, icon('move'), 'Copy media files'));
    };
    const bar = h('i', { style: 'width:0%' });
    const copy = async () => {
      try {
        await api.call('media.copyHeld', { held });
      } catch (e) {
        body.replaceChildren(h('p.big', 'Nothing added'), h('p.hint', e.message));
        foot.replaceChildren(h('div.spacer'), h('button.primary', { style: 'flex:none', onclick: close }, 'OK'));
        return;
      }
      entry.sheet.querySelector('.shead h2').textContent = `Copying ${files(n)}`;
      body.replaceChildren(h('p.hint', 'Each copy is checked against its original before it\'s added.'), h('div.bar', bar));
      foot.replaceChildren(h('div.spacer'), h('button.primary.off', { style: 'flex:none', disabled: true }, 'Copying…'));
    };
    copyUi = (r) => {
      if (!r.finished) { bar.style.width = `${(100 * r.done) / Math.max(1, r.total)}%`; return; }
      const res = r.result;
      const total = summary.added + res.copied;
      entry.sheet.querySelector('.shead h2').textContent = res.copied ? `${total.toLocaleString()} added` : 'Nothing copied';
      body.replaceChildren(h('ul.list',
        summary.added ? h('li', `${summary.added.toLocaleString()} added from where they are`) : null,
        res.copied ? h('li', `${res.copied.toLocaleString()} copied into ${res.folders.join(' and ')}`) : null,
        res.alreadyHere ? h('li', res.alreadyHere === 1 ? '1 was already in this project' : `${res.alreadyHere} were already in this project`) : null,
        res.failed ? h('li', `${files(res.failed)} couldn't be copied; nothing was lost`) : null,
        res.notTried ? h('li', res.text.split(' · ').pop()) : null),
        res.copied ? h('p.hint', 'They\'re analyzed and sorted like any other item, and Bin Files moves them into their category folders.') : null);
      foot.replaceChildren(h('div.spacer'), h('button.primary', { style: 'flex:none', onclick: close }, 'OK'));
    };
    entry.onClose = () => { copyUi = null; };
    ask();
    return { title: n === 1 ? '1 file is already in another project' : `${n.toLocaleString()} files are already in another project`, body, foot };
  });
  void names;
}

let copyUi = null;
/** Progress and the result of copying held files, shown in the notice when it is open. */
export function onCopy(r) {
  if (copyUi) copyUi(r);
  else if (r.finished) toast(r.result.text);
}

let regLine = null;
/** The one-line progress and summary under "Add a folder" in the project sheet. */
export function onRegistration(r) {
  if (!regLine || !document.contains(regLine)) return;
  if (!r) return regLine.replaceChildren();
  regLine.replaceChildren(r.summary ? r.summary.text : (r.total ? `Adding ${r.done.toLocaleString()} of ${r.total.toLocaleString()}…` : 'Adding…'));
}

export function projects() {
  openSheet((close) => {
    const body = h('div.sbody');
    let editing = false;
    const open = async (cmd, path) => {
      try {
        S.boot.projects = await api.call(cmd, { path });
        await refresh();
        close();
      } catch (e) { fail(e); }
    };
    const remove = async (p) => {
      // One confirmation only: when a run is in progress for the project being removed.
      if (p.open && running() && !await confirmSheet(`Stop analysis and remove ${p.name}?`, 'It comes off this list. Its files and its database stay where they are.', 'Remove', true)) return;
      try {
        const r = await api.call('project.remove', { path: p.path });
        S.boot.projects = r.projects;
        await refresh();
        draw();
        if (r.removed) toast(`Removed ${p.name}`, { label: 'Undo', run: async () => { try { S.boot.projects = await api.call('project.restore', r.removed); await refresh(); } catch (e) { fail(e); } } }, 5000);
      } catch (e) { fail(e); }
    };
    const draw = () => {
      const cur = project();
      const list = S.boot.projects.projects;
      const parts = [];
      if (cur) {
        regLine = h('p.hint', { 'aria-live': 'polite' });
        parts.push(h('div.sect', h('h3', 'Current project'),
          h('div.rowbtn.cur', h('span.ico2', icon('folder')), h('span.tx', h('b', cur.name), h('span', cur.displayPath))),
          h('div.two-up', rowBtn('folder', 'Add a folder', null, () => addFolder()), rowBtn('image', 'Pick photos and videos', null, () => addFiles())),
          regLine));
        onRegistration(S.registration);
      }
      parts.push(h('div.sect',
        h('div', { style: 'display:flex;align-items:center;gap:8px' }, h('h3', { style: 'flex:1' }, 'Current projects'),
          list.length ? h('button.mini', { onclick: () => { editing = !editing; draw(); } }, editing ? 'Done' : 'Edit') : null),
        h('div.two-up',
          rowBtn('open', 'Open project', null, async () => { const dir = await api.pickFolder('Open project'); if (dir) open('project.open', dir); }),
          rowBtn('plus', 'New project', null, () => { close(); newProject(); })),
        list.map((p) => h(`div.rowbtn${p.open ? '.cur' : ''}`, { style: 'padding:0;min-height:0' },
          h('button', { style: 'flex:1;min-width:0;display:flex;align-items:center;gap:12px;padding:12px 14px;text-align:left', disabled: p.missing, onclick: () => (p.open ? close() : open('project.switch', p.path)) },
            h('span.ico2', icon('folder')),
            h('span.tx', h('b', p.name), h('span', p.missing ? 'Folder not found' : p.displayPath)),
            p.open ? icon('check', 18) : null),
          editing ? h('button.ib.sm', { style: 'margin-right:10px', 'aria-label': `Remove ${p.name} from the list`, onclick: () => remove(p) }, icon('x', 16)) : null)),
        list.length ? null : h('p.hint', 'No projects yet')));
      body.replaceChildren(...parts);
    };
    draw();
    return { title: 'Projects', body };
  });
}

export function newProject() {
  openSheet((close) => {
    let pending = null; // {kind:'folder', path} or {kind:'files', paths}
    const name = h('input', { type: 'text', maxlength: 80, autofocus: true, spellcheck: false });
    const picked = h('p.hint');
    const create = h('button.primary', { style: 'flex:none' });
    const tiles = h('div.two-up');
    const sync = () => {
      create.replaceChildren(pending ? 'Create and analyze' : 'Create project');
      const chosen = (kind) => (pending && pending.kind === kind ? '.cur' : '');
      const tile = (kind, ic, label, pick) => h(`button.rowbtn${chosen(kind)}`, { onclick: pick }, h('span.ico2', icon(ic)), h('span.tx', h('b', label), pending && pending.kind === kind ? h('span', 'Change') : null));
      tiles.replaceChildren(
        tile('folder', 'folder', 'A folder', async () => { const d = await api.pickFolder('Add a folder'); if (d) { pending = { kind: 'folder', path: d }; sync(); } }),
        tile('files', 'image', 'Photos and videos', async () => { const f = await api.pickFiles('Pick photos and videos'); if (f.length) { pending = { kind: 'files', paths: f }; sync(); } }));
      picked.replaceChildren(pending ? (pending.kind === 'folder' ? pending.path : files(pending.paths.length)) : 'Optional. You can add media later.');
    };
    const go = async () => {
      if (!name.value.trim()) { name.focus(); return toast('Give the project a name'); }
      try {
        S.boot.projects = await api.call('project.create', { name: name.value });
        if (pending) {
          S.registration = { done: 0, total: 0 };
          if (pending.kind === 'folder') await api.call('media.addFolder', { path: pending.path });
          else await api.call('media.addFiles', { paths: pending.paths });
        }
        await refresh();
        close();
        show('analyze');
      } catch (e) { fail(e); }
    };
    create.addEventListener('click', go);
    name.addEventListener('keydown', (e) => { if (e.key === 'Enter') go(); });
    sync();
    return {
      title: 'New project',
      body: h('div.sbody', h('label.field', h('span.label', 'Project name'), name),
        h('p.hint', `Made in ${S.boot.projectsRoot}`),
        h('div.sect', tiles, picked)),
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'), create),
    };
  });
}

// ---------------------------------------------------------------------------------------------
// Item detail, the failed item, bulk edits
// ---------------------------------------------------------------------------------------------

/** A category picker: the existing ones plus "New category…", which shows a name field. */
function categoryPicker(categories, currentId) {
  const select = h('select', { 'aria-label': 'Category' },
    currentId ? null : h('option', { value: '' }, 'Choose a category'),
    categories.map((c) => h('option', { value: String(c.id), selected: c.id === currentId }, c.name)),
    h('option', { value: 'new' }, 'New category…'));
  const fresh = h('input', { type: 'text', placeholder: 'For example “Snorkeling”', maxlength: 40, hidden: true, spellcheck: false, 'aria-label': 'New category' });
  const note = h('p.hint', { hidden: true }, 'New · created when you save. Becomes a folder name when the files are binned.');
  const listeners = [];
  select.addEventListener('change', () => {
    fresh.hidden = note.hidden = select.value !== 'new';
    if (!fresh.hidden) fresh.focus();
    listeners.forEach((f) => f());
  });
  return {
    nodes: [select, fresh, note],
    onChange(f) { listeners.push(f); },
    changed() { return select.value === 'new' || (select.value !== '' && Number(select.value) !== currentId); },
    /** null = unchanged, or the choice to send; throws a sentence when a new name is missing. */
    value() {
      if (select.value === '') return null;
      if (select.value === 'new') {
        if (!fresh.value.trim()) { fresh.focus(); throw new Error('Type a name for the new category'); }
        return { new: fresh.value.trim() };
      }
      const id = Number(select.value);
      return id === currentId ? null : { existing: id };
    },
    required() {
      const v = this.value();
      if (v) return v;
      if (currentId) return { existing: currentId };
      throw new Error('Choose a category');
    },
  };
}

function applied(result, done) {
  if (result.ok) { done(); refresh(); } else toast(result.error);
}

function previewOf(d) {
  const item = d.item;
  const isVideo = item.kind === 'Video';
  const preview = h('div.preview');
  const showable = /\.(jpe?g|png|webp)$/i.test(item.currentPath);
  if (d.fileExists && isVideo) preview.append(h('video', { src: api.fileUrl(item.currentPath), controls: true, preload: 'metadata', poster: d.thumb ? api.fileUrl(d.thumb) : null }));
  else if (d.thumb || (d.fileExists && showable)) {
    const img = h('img', { src: api.fileUrl(d.fileExists && showable ? item.currentPath : d.thumb), alt: '' });
    img.addEventListener('error', () => { if (d.thumb && !img.dataset.fell) { img.dataset.fell = '1'; img.src = api.fileUrl(d.thumb); } });
    preview.append(img);
  } else preview.append(h('div', { style: 'color:#9aa3b2;padding:40px' }, icon(isVideo ? 'film' : 'image', 48)));
  return preview;
}

const extOf = (name) => { const i = name.lastIndexOf('.'); return i > 0 ? name.slice(i) : ''; };
const when = (ms) => new Date(ms).toLocaleString(undefined, { month: 'short', day: 'numeric', year: 'numeric', hour: 'numeric', minute: '2-digit' });

export async function detail(id) {
  let d;
  try { d = await api.call('item.detail', { id }); } catch (e) { return fail(e); }
  if (!d) return toast('It isn\'t in the project any more');
  const item = d.item;
  if (!d.analysis && (item.status === 'Failed' || item.status === 'Missing')) return failedSheet(d);
  const cats = S.all ? S.all.categories : [];
  const binned = item.status === 'Binned';
  openSheet((close) => {
    const suggested = d.analysis ? d.analysis.suggestedName : '';
    const name = h('input', { type: 'text', value: item.friendlyName || suggested || '', maxlength: 60, spellcheck: false });
    const picker = categoryPicker(cats, item.categoryId);
    let unlockName = false;
    let unlockCategory = false;
    const nameLock = h('div.lockrow');
    const catLock = h('div.lockrow');
    const save = h('button.primary', { style: 'flex:none' });
    const fileLine = h('p.hint');
    const sync = () => {
      const nameChanged = name.value.trim() !== (item.friendlyName || suggested || '');
      const lockRow = (row, locked, changed, unlocked, onUnlock) => {
        if (unlocked) row.replaceChildren(h('span.pillnote', icon('unlock', 12), 'Sorting can change it after you save'));
        else if (changed) row.replaceChildren(h('span.pillnote', icon('lock', 12), 'Set by you when you save'));
        else if (locked) row.replaceChildren(h('span.pillnote', icon('lock', 12), 'Set by you'), h('button.link', { onclick: () => { onUnlock(); sync(); } }, 'Unlock'));
        else row.replaceChildren(h('span.pillnote', 'From sorting'));
      };
      lockRow(nameLock, item.nameSetByUser, nameChanged, unlockName, () => { unlockName = true; });
      if (item.categoryId || picker.changed()) lockRow(catLock, item.categorySetByUser, picker.changed(), unlockCategory, () => { unlockCategory = true; });
      else catLock.replaceChildren(h('span.pillnote', item.status === 'Analyzed' ? 'Waiting to be categorized' : 'Not placed yet'));
      const moves = binned && (nameChanged || picker.changed());
      save.replaceChildren(moves ? 'Save and move file' : 'Save');
      fileLine.replaceChildren(`File name when binned: ${name.value.trim() || '…'}${extOf(item.originalName)}`, moves ? ' · Saving moves or renames this file right away.' : '');
    };
    name.addEventListener('input', sync);
    picker.onChange(sync);
    sync();
    const where = binned
      ? `Binned in the ${parentName(item.currentPath)} folder as ${baseName(item.currentPath)}.`
      : `Not binned yet. The file stays in ${parentName(item.currentPath)} until Bin Files.`;
    const right = h('div',
      d.simulated ? h('div.note', icon('info', 18), h('span.grow', h('b', 'Simulated analysis. '), 'This text was not written by a vision model.')) : null,
      item.status === 'Missing' ? h('div.note', icon('warn', 18), h('span.grow', h('b', 'File missing. '), 'The file is no longer at this location.')) : null,
      item.status === 'Analyzed' && !item.categoryId ? h('div.note.info', icon('info', 18), h('span.grow', 'Sorting couldn\'t place this item. It tries again on the next run, or you can choose a category now. Bin Files waits until it has one.')) : null,
      h('label.field', h('span.label', 'Friendly name'), name), nameLock,
      suggested && suggested !== name.value ? h('p.hint', `The model suggested “${suggested}”`) : null,
      h('div.field', h('span.label', 'Category'), ...picker.nodes), catLock,
      d.category && d.category.createdBy !== 'model' ? h('p.hint', 'Yours: sorting never renames or merges it') : null,
      fileLine,
      h('div.sect', h('h3', 'Description'), h('p.selectable', { style: 'margin:0;font-size:14.5px;line-height:1.5' }, d.analysis.description)),
      d.analysis.actions.length ? h('div.sect', h('h3', 'Actions'), h('ul.actions.selectable', d.analysis.actions.map((a) => h('li', h('time', a.t), h('span', a.text))))) : null,
      d.tags.length ? h('div.sect', h('h3', 'Tags'), h('div.tags', d.tags.map((t) => h('button.tag', { title: `Search for ${t}`, onclick: () => { S.query = t; close(); show('media'); } }, t)))) : null);
    const facts = h('div.sect', h('h3', 'Details'), h('dl.kv',
      h('dt', 'Original file'), h('dd.selectable', item.originalName),
      h('dt', 'Location'), h('dd.selectable', where, h('br'), h('span', { style: 'color:var(--sub)' }, d.displayPath)),
      item.takenAt ? [h('dt', 'Taken'), h('dd', when(item.takenAt))] : null,
      item.kind === 'Video' && item.durationMs ? [h('dt', 'Length'), h('dd', duration(item.durationMs))] : null,
      item.width && item.height ? [h('dt', 'Size'), h('dd', `${item.width} × ${item.height}`)] : null,
      h('dt', 'Shot type'), h('dd', d.analysis.shotType),
      h('dt', 'Model'), h('dd', d.analysis.modelId)));
    const doSave = async () => {
      try { applied(await api.call('item.save', { id, name: name.value, category: picker.value(), unlockName, unlockCategory }), close); } catch (e) { fail(e); }
    };
    save.addEventListener('click', doSave);
    name.addEventListener('keydown', (e) => { if (e.key === 'Enter') doSave(); });
    return {
      title: 'Item details',
      wide: true,
      body: h('div.sbody', h('div.detail', h('div', previewOf(d), facts), right)),
      foot: h('div.sfoot',
        h('button.secondary.danger', { onclick: () => { close(); removeItems([id]); } }, icon('trash'), 'Remove from project'),
        d.fileExists ? h('button.secondary', { onclick: () => api.reveal(item.currentPath).catch(fail) }, icon('open'), 'Show in Explorer') : null,
        h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'), save),
    };
  });
}

/** A failed or missing item with no analysis: retry, set a name and category by hand, or remove. */
function failedSheet(d) {
  const item = d.item;
  const cats = S.all ? S.all.categories : [];
  const missing = item.status === 'Missing';
  const noun = item.kind === 'Video' ? 'video' : 'photo';
  const tries = item.attempts === 1 ? ' Tried once.' : item.attempts > 1 ? ` Tried ${item.attempts} times.` : '';
  const why = missing ? 'The file is no longer at this location, so it can\'t be analyzed.'
    : item.failureReason === 'decode' ? `This computer couldn't decode this ${noun}, so the model never saw it. The file may be damaged or use a format Windows can't read.${tries}`
    : `${d.failure}${tries}`;
  openSheet((close) => {
    const name = h('input', { type: 'text', placeholder: 'For example “Sunset Drive Home”', maxlength: 60, value: item.friendlyName || '', spellcheck: false });
    const picker = categoryPicker(cats, item.categoryId);
    const fileLine = h('p.hint');
    const sync = () => fileLine.replaceChildren(`File name when binned: ${name.value.trim() || '…'}${extOf(item.originalName)}`);
    name.addEventListener('input', sync);
    sync();
    const byHand = async () => {
      try {
        if (!name.value.trim()) { name.focus(); throw new Error('Type a friendly name'); }
        applied(await api.call('item.setByHand', { id: item.id, name: name.value, category: picker.required() }), close);
      } catch (e) { fail(e); }
    };
    return {
      title: missing ? 'File missing' : 'Couldn\'t analyze',
      wide: true,
      body: h('div.sbody', h('div.detail',
        h('div', previewOf(d), h('dl.kv', h('dt', 'Original file'), h('dd.selectable', item.originalName), h('dt', 'Location'), h('dd.selectable', d.displayPath))),
        h('div',
          h('div.note', icon('warn', 18), h('span.grow', h('b', missing ? 'Missing. ' : `Failed · ${item.failureReason || 'unknown'}. `), why, ' Not blocked. This item stays where it is.')),
          missing ? null : h('button.rowbtn', { onclick: async () => { try { applied(await api.call('item.retry', { id: item.id }), close); } catch (e) { fail(e); } } }, h('span.ico2', icon('refresh')), h('span.tx', h('b', 'Retry analysis'))),
          h('div.sect', h('h3', 'Set name and category by hand'),
            h('p.hint', `With a name and category the ${noun} is binned like any other at the next Bin Files. It has no description or tags, so search finds it only by name and category.`),
            h('label.field', h('span.label', 'Friendly name'), name),
            h('div.field', h('span.label', 'Category'), ...picker.nodes),
            fileLine)))),
      foot: h('div.sfoot',
        h('button.secondary.danger', { onclick: () => { close(); removeItems([item.id]); } }, icon('trash'), 'Remove from project'),
        h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'), h('button.primary', { style: 'flex:none', onclick: byHand }, 'Save')),
    };
  });
}

export function moveTo(ids) {
  const cats = S.all ? S.all.categories : [];
  const binnedN = S.all ? S.all.rows.filter((r) => ids.includes(r.id) && r.status === 'Binned').length : 0;
  openSheet((close) => {
    const picker = categoryPicker(cats, null);
    return {
      title: 'Move to category',
      body: h('div.sbody',
        h('div.field', h('span.label', 'Category'), ...picker.nodes),
        h('p.hint', binnedN ? `Saving moves ${binnedN === 1 ? '1 binned file' : `${binnedN} binned files`} right away, one at a time. Other files move at the next Bin Files.` : 'Only the database changes. Files move at the next Bin Files.')),
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'),
        h('button.primary', { style: 'flex:none', onclick: async () => {
          try { applied(await api.call('items.move', { ids, category: picker.required() }), () => { S.selecting = false; S.selected.clear(); close(); }); } catch (e) { fail(e); }
        } }, binnedN ? `Save · move ${binnedN}` : 'Save')),
    };
  });
}

export async function removeItems(ids) {
  const ok = await confirmSheet(ids.length === 1 ? 'Remove from project?' : `Remove ${ids.length.toLocaleString()} items from project?`,
    'Only the project forgets them. The files are never deleted and stay where they are.', 'Remove from project', true);
  if (!ok) return;
  try {
    await api.call('items.remove', { ids });
    S.selecting = false;
    S.selected.clear();
    refresh();
  } catch (e) { fail(e); }
}

// ---------------------------------------------------------------------------------------------
// Categories, Re-sort, filters
// ---------------------------------------------------------------------------------------------

/** Rename a category, or tick two to merge them; the user chooses the surviving name. */
export function categoryEdit() {
  const cats = S.all ? S.all.categories : [];
  const rowsAll = S.all ? S.all.rows : [];
  const binnedIn = (id) => rowsAll.filter((r) => r.categoryId === id && r.status === 'Binned').length;
  openSheet((close) => {
    const names = new Map(cats.map((c) => [c.id, c.name]));
    const ticked = [];
    let keep = null;
    const body = h('div.sbody');
    const save = h('button.primary', { style: 'flex:none' });
    const draw = () => {
      const merge = ticked.length === 2;
      if (!merge) keep = null; else if (!ticked.includes(keep)) keep = ticked[0];
      const renamed = cats.filter((c) => names.get(c.id).trim() !== c.name && !(merge && ticked.includes(c.id) && c.id !== keep));
      const changes = renamed.length + (merge ? 1 : 0);
      const dropId = merge ? ticked.find((t) => t !== keep) : null;
      const moving = renamed.reduce((n, c) => n + binnedIn(c.id), 0) + (merge ? binnedIn(dropId) : 0);
      save.replaceChildren(changes === 1 ? 'Save 1 change' : changes ? `Save ${changes} changes` : 'Save');
      save.classList.toggle('off', !changes);
      body.replaceChildren(
        h('p.hint', 'Rename a category, or tick two to merge them. Categories you rename or merge become yours, so sorting won\'t change them.'),
        h('ul.list.catedit', cats.map((c) => {
          const input = h('input.input', { type: 'text', value: names.get(c.id), maxlength: 40, 'aria-label': `Rename ${c.name}`, spellcheck: false, disabled: merge && c.id === dropId });
          input.addEventListener('change', () => { names.set(c.id, input.value); draw(); });
          const tick = h('input', { type: 'checkbox', checked: ticked.includes(c.id), 'aria-label': `Select ${c.name} to merge`, disabled: !ticked.includes(c.id) && ticked.length >= 2 });
          tick.addEventListener('change', () => { if (tick.checked) ticked.push(c.id); else ticked.splice(ticked.indexOf(c.id), 1); draw(); });
          const n = binnedIn(c.id);
          const hint = names.get(c.id).trim() !== c.name ? (n ? `Renames the ${c.name} folder when you save` : 'No folder yet; only the database changes') : '';
          return h('li', { style: 'flex-wrap:wrap' }, h('label.check', { style: 'margin:0' }, tick), input, hint ? h('span', { style: 'flex-basis:100%;margin:0 0 0 28px;white-space:normal;font-size:12.5px' }, hint) : null);
        })),
        merge ? h('div.sect', h('h3', `Merge ${names.get(ticked[0])} and ${names.get(ticked[1])}`),
          ticked.map((id) => h('label.check', h('input', { type: 'radio', name: 'keep', checked: keep === id, onchange: () => { keep = id; draw(); } }), h('span', `Keep the name “${names.get(id)}”`)))) : null,
        h('p.hint', !changes ? '' : moving ? `Saving moves ${moving === 1 ? '1 binned file' : `${moving} binned files`} right away, one at a time. Other files move at the next Bin Files.` : 'Only the database changes. Files move at the next Bin Files.'));
    };
    save.addEventListener('click', async () => {
      const merge = ticked.length === 2;
      const dropId = merge ? ticked.find((t) => t !== keep) : null;
      const merges = merge ? [[keep, dropId]] : [];
      const renames = cats.filter((c) => c.id !== dropId && names.get(c.id).trim() !== c.name).map((c) => [c.id, names.get(c.id)]);
      if (!merges.length && !renames.length) return close();
      try { applied(await api.call('categories.apply', { renames, merges }), close); } catch (e) { fail(e); }
    });
    draw();
    return { title: 'Edit categories', body, foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'), save) };
  });
}

export function resort() {
  const rowsAll = S.all ? S.all.rows : [];
  const cats = S.all ? S.all.categories : [];
  openSheet((close) => {
    const s = S.boot.settings;
    let model = s.sortingModel;
    let keep = true;
    const body = h('div.sbody');
    const go = h('button.primary', { style: 'flex:none' });
    const draw = () => {
      const analyzed = rowsAll.filter((r) => r.hasAnalysis && ['Analyzed', 'Sorted', 'Binned'].includes(r.status));
      const items = keep ? analyzed.filter((r) => !r.categorySetByUser) : analyzed;
      const n = items.length;
      const mineCats = cats.filter((c) => c.createdBy !== 'model').length;
      const mineItems = rowsAll.filter((r) => r.categorySetByUser || r.nameSetByUser).length;
      const failed = rowsAll.filter((r) => r.status === 'Failed').length;
      const local = S.boot.simulated ? 'Simulated sorter' : S.boot.models.text.displayName;
      const option = (value, title, sub) => h(`button.rowbtn${model === value ? '.cur' : ''}`, { onclick: () => { model = value; draw(); } },
        h('span.ico2', icon(value === 'Claude' ? 'open' : 'chip')), h('span.tx', h('b', title, s.sortingModel === value ? h('span.pillnote', { style: 'margin-left:8px' }, 'Settings choice') : null), h('span', { style: 'white-space:normal' }, sub)));
      go.replaceChildren(`Re-sort ${n === 1 ? '1 item' : `${n.toLocaleString()} items`}`);
      go.disabled = !n;
      go.classList.toggle('off', !n);
      body.replaceChildren(
        h('p.hint', { style: 'font-size:14px' }, n ? `Sorts ${n === 1 ? '1 item' : `${n.toLocaleString()} items`} again from scratch. The categories may change completely: new names, merges and splits.${failed ? (failed === 1 ? ' 1 failed item is left out.' : ` ${failed} failed items are left out.`) : ''}` : (analyzed.length ? 'Every analyzed item is in a category you set' : 'Nothing is analyzed yet')),
        h('div.sect', h('h3', 'Sorting model for this run'),
          option('Lfm3_1_7B', local, 'Runs on this computer\'s NPU. Analysis waits until it finishes.'),
          option('Claude', 'Claude', `Sends the text of ${n === 1 ? '1 item' : `${n.toLocaleString()} items`} (descriptions, actions, tags, suggested names) and the category list to Claude. Never photos.`)),
        h('label.check', h('input', { type: 'checkbox', checked: keep, onchange: (e) => { keep = e.target.checked; draw(); } }),
          h('span', 'Keep my edits', h('small', mineCats + mineItems ? `Keeps the ${mineCats === 1 ? '1 category' : `${mineCats} categories`} and ${mineItems === 1 ? '1 item' : `${mineItems} items`} you set. New items can still join your categories.` : 'Nothing is set by you yet.'))),
        h('div.two-up',
          h('div.sect', h('h3', 'Changes'), h('ul.list', h('li', 'Which items each category holds'), h('li', 'Category names and descriptions'), h('li', 'Friendly names that sorting chose'))),
          h('div.sect', h('h3', 'Doesn\'t change now'), h('ul.list', h('li', 'Any file on this computer. Only the database changes.'), h('li', 'Files move only when you press Bin Files.')))),
        h('div.note.info', icon('info', 18), h('span.grow', 'If it is stopped or can\'t finish, the items it cleared wait to be categorized, and Bin Files waits for them. Categorize now picks them up.')));
    };
    go.addEventListener('click', async () => {
      try {
        const r = await api.call('analysis.resort', { model, keepEdits: keep });
        if (r.ok) close(); else toast(r.error);
      } catch (e) { fail(e); }
    });
    draw();
    return { title: 'Re-sort project', body, foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'), go) };
  });
}

const dayStart = (ms) => { const d = new Date(ms); d.setHours(0, 0, 0, 0); return d.getTime(); };
/** "Jul 18", or "Jul 18, 2026" when the project spans more than one year. */
export function dayLabel(ms) {
  const years = new Set((S.all ? S.all.rows : []).filter((r) => r.takenAt).map((r) => new Date(r.takenAt).getFullYear()));
  return new Date(ms).toLocaleDateString('en-US', years.size > 1 ? { month: 'short', day: 'numeric', year: 'numeric' } : { month: 'short', day: 'numeric' });
}

/** Shot type and Date, each choice with the count the grid would show, then Show. */
export async function filters() {
  const f = S.filter;
  let found;
  try {
    // What the search, the chip and the kind toggle leave; the sheet's own two filters are counted here.
    found = (await api.call('grid', { filter: { chip: f.chip, kind: f.kind }, query: S.query })).rows;
  } catch (e) { return fail(e); }
  const all = S.all ? S.all.rows : [];
  const shotLabel = { wide: 'Wide', medium: 'Medium', close: 'Close-up' };
  const shots = ['wide', 'medium', 'close'].filter((t) => all.some((r) => r.shotType === t));
  const days = [...new Set(all.filter((r) => r.takenAt).map((r) => dayStart(r.takenAt)))].sort((a, b) => a - b);
  let shot = f.shotType;
  let day = f.takenFrom ? dayStart(f.takenFrom) : null;
  const count = (s, d) => found.filter((r) => (!s || r.shotType === s) && (!d || (r.takenAt && dayStart(r.takenAt) === d))).length;
  openSheet((close) => {
    const body = h('div.sbody');
    const foot = h('div.sfoot');
    const option = (label, n, on, pick) => h('button.optrow', { role: 'radio', 'aria-checked': String(on), onclick: () => { pick(); draw(); } }, h('i.radio'), h('span.grow', label), h('span.n', String(n)));
    const draw = () => {
      body.replaceChildren(
        h('span.label', 'Shot type'),
        option('Any shot type', count(null, day), !shot, () => { shot = null; }),
        ...shots.map((t) => option(shotLabel[t], count(t, day), shot === t, () => { shot = t; })),
        h('span.label', { style: 'margin-top:10px' }, 'Date'),
        option('Any date', count(shot, null), !day, () => { day = null; }),
        ...days.map((d) => option(dayLabel(d), count(shot, d), day === d, () => { day = d; })));
      const n = count(shot, day);
      foot.replaceChildren(h('button.primary', { onclick: () => {
        f.shotType = shot;
        f.takenFrom = day;
        f.takenTo = day ? day + 86400000 - 1 : null;
        close();
        refresh();
      } }, `Show ${n === 1 ? '1 item' : `${n} items`}`));
    };
    draw();
    return { title: 'Filters', body, foot };
  });
}

// ---------------------------------------------------------------------------------------------
// Bin Files
// ---------------------------------------------------------------------------------------------

async function startBin(after) {
  try {
    const r = await api.call('bin.start');
    if (r.ok) { if (after) after(); show('media'); } else toast(r.error);
  } catch (e) { fail(e); }
}

/** The confirm sheet before Bin Files. A check-only tap (nothing to move) skips it. */
export async function binFiles() {
  let data;
  try { data = await api.call('bin.preview'); } catch (e) { return fail(e); }
  if (data.unexpected.length) return unknownFiles(data);
  if (data.checkOnly) return startBin();
  const p = data.preview;
  openSheet((close) => {
    const body = h('div.sbody',
      h('p.big', p.files ? `Bin ${files(p.files)} into ${folders(p.folders)}` : 'Nothing to move'),
      h('p.hint', 'Into ', h('b.selectable', `${data.projectPath}\\`), ', one folder per category.'),
      h('div.facts',
        p.sources.map((s) => fact('image', `From ${s.folder}`, s.path, s.count)),
        p.otherStorage ? fact('download', 'From another drive', `Copied, checked, then deleted from that drive · needs ${data.copyBytesText} free, ${data.freeBytesText} free`, p.otherStorage) : null,
        p.shared ? fact('move', 'Also in another project', 'Copied into their folders; the other project keeps its file', p.shared) : null,
        fact('bin', 'Already in their folders', 'Stay as they are', p.inPlace, 'ok')),
      data.enoughSpace ? null : h('div.note', icon('warn', 18), h('span.grow', h('b', 'Storage is full. '), `The copies need ${data.copyBytesText}; ${data.freeBytesText} is free.`)),
      h('div.note', icon('lock', 18), h('span.grow', h('b', 'No undo. '), 'Each file moves into its category folder and is checked. Nothing is overwritten: a name that\'s taken gets a number.')),
      h('div.note.info', icon('info', 18), h('span.grow', 'In File Explorer they leave the folders they were in and show up in folders named after their categories.')),
      leftList(p.leftOut.map((l) => ({ ...l, why: 'Couldn\'t be analyzed' }))));
    return {
      title: 'Bin Files',
      body,
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'),
        h('button.primary', { style: 'flex:none', disabled: p.files > 0 && !data.enoughSpace, onclick: () => startBin(close) }, icon('bin'), p.files ? `Bin ${files(p.files)}` : 'Bin Files')),
    };
  });
}

/** The folder check found files in category folders that no item points to: nothing moves until answered. */
function unknownFiles(data) {
  openSheet((close) => {
    let rows = data.unexpected.slice();
    let addable = true; // After an add, what couldn't become an item stays with only Leave them.
    const body = h('div.sbody');
    const foot = h('div.sfoot');
    const leave = async () => {
      try {
        await api.call('bin.leave', { paths: rows.map((f) => f.path) });
        close();
        await refresh();
        binFiles();
      } catch (e) { fail(e); }
    };
    const add = async () => {
      try {
        const r = await api.call('bin.add', { paths: rows.map((f) => f.path) });
        const left = new Set(r.notAdded);
        rows = rows.filter((f) => left.has(f.path));
        addable = false;
        await refresh();
        if (!rows.length) { close(); return toast(`${files(r.added)} added to the project`); }
        toast(r.added ? `${files(r.added)} added. ${files(rows.length)} can't be added.` : `${rows.length === 1 ? 'This file' : 'These files'} can't be added to the project`);
        draw();
      } catch (e) { fail(e); }
    };
    const draw = () => {
      body.replaceChildren(
        h('div.note', icon('warn', 18), h('span.grow', h('b', 'Nothing was moved. '), `These files are in category folders, but they aren't in ${data.projectName}. Bin Files checks the folders before it moves anything.`)),
        h('div.sect', h('h3', `Not in the project · ${rows.length}`),
          rows.slice(0, 12).map((f) => fact(f.isMedia ? 'image' : 'info', `${f.folder}\\${f.name}`, `Added ${new Date(f.modified).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })} · ${f.sizeText}`)),
          rows.length > 12 ? h('p.hint', `and ${rows.length - 12} more`) : null),
        data.missing.length ? h('div.sect', h('h3', `Marked missing · ${data.missing.length}`),
          data.missing.slice(0, 6).map((f) => fact('warn', `${f.folder}\\${f.name}`, 'Binned, but no longer there. It is listed as Missing and doesn\'t block Bin Files.'))) : null,
        h('div.note.info', icon('info', 18), h('span.grow', addable ? [h('b', 'Add to project'), ' analyzes and sorts them, then the next Bin Files moves them into their own category. '] : null, h('b', 'Leave them'), ' keeps them where they are and the check skips them from now on.')));
      foot.replaceChildren(h('div.spacer'), h('button.secondary', { onclick: leave }, 'Leave them'),
        addable ? h('button.primary', { style: 'flex:none', onclick: add }, icon('plus'), 'Add to project') : null);
    };
    draw();
    return { title: 'Files Bin Files doesn\'t know', body, foot };
  });
}

let reportShown = null;
/** Shows the last run's report until it is closed, also after a restart. */
export async function reportIfUnread(state) {
  if (!project() || running()) return;
  if (state && state.moveOutcome === 'Blocked') {
    try { const d = await api.call('bin.preview'); if (d.unexpected.length) unknownFiles(d); } catch (e) { fail(e); }
    return;
  }
  let r;
  try { r = (await api.call('bin.report')).report; } catch { return; }
  if (!r || reportShown === r.runId) return;
  reportShown = r.runId;
  const save = r.kind === 'Save';
  const done = r.state === 'Done';
  const movedNow = r.moved + r.copiedShared;
  const reasons = { 'copy-mismatch': 'Its copy didn\'t match the original twice.', interrupted: 'The app closed twice while moving it.', io: 'Windows refused the move; it may be open in another program.' };
  openSheet((close, entry) => {
    entry.onClose = () => { api.call('bin.closeReport', { runId: r.runId }).catch(() => {}); refresh(); };
    const body = h('div.sbody');
    const foot = h('div.sfoot', h('div.spacer'));
    const check = r.checkOk === true ? fact('check', 'Folder check', 'Nothing missing or extra', 'OK', 'ok')
      : r.checkOk === false ? fact('warn', 'Folder check', 'Something is missing or extra. Bin Files shows what.', '!', 'warn') : null;
    if (done) {
      body.append(h('p.big', r.headline),
        h('p.hint', movedNow ? `Checked ${files(movedNow)} in ${folders(r.folders)}.${r.checkOk ? ' Nothing missing or extra.' : ''}` : (r.checkOk ? 'Checked the category folders. Nothing missing or extra.' : '')),
        h('div.facts',
          movedNow ? fact('move', 'Moved now', r.copiedShared ? `${r.moved} moved, ${r.copiedShared} copied from another project` : null, movedNow) : null,
          r.inPlace ? fact('bin', 'Already in place', 'Marked as binned', r.inPlace) : null,
          r.numbered.length ? fact('edit', 'Names that got a number', r.numbered.slice(0, 3).join(', ') + (r.numbered.length > 3 ? '…' : ''), r.numbered.length) : null,
          r.removedFolders.length ? fact('trash', 'Empty folders removed', r.removedFolders.join(', '), r.removedFolders.length) : null,
          check));
      foot.append(h('button.primary', { style: 'flex:none', onclick: close }, 'Done'));
    } else {
      const full = (r.message || '').startsWith('Storage is full');
      const title = save ? 'Saving stopped' : 'Bin Files stopped';
      const why = r.message || (r.lastMoved ? `You stopped it after ${r.lastMoved}. Press Bin Files to move the rest.` : 'You stopped it before anything moved. Press Bin Files to move the files.');
      body.append(
        h('div.note', icon(full ? 'download' : 'stop', 18), h('span.grow', h('b', `${title}${full ? ': storage is full' : ''}. `), why)),
        h('div.facts',
          fact('move', 'Moved', null, movedNow),
          fact('refresh', 'Still where they were', 'Moved by the next Bin Files', r.stillToMove, 'warn'),
          check));
      foot.append(h('button.secondary', { onclick: close }, 'Close'));
      // A stopped Bin Files carries on with no second confirm; a stopped Save covered only what was
      // edited, so its button opens the confirm sheet.
      if (r.state !== 'Failed') {
        foot.append(save || full
          ? h('button.primary', { style: 'flex:none', onclick: () => { close(); binFiles(); } }, icon(full ? 'refresh' : 'bin'), full ? 'Try again' : 'Bin Files')
          : h('button.primary', { style: 'flex:none', onclick: () => { close(); startBin(); } }, icon('bin'), `Bin ${files(r.stillToMove)}`));
      }
    }
    if (r.skipped.length) {
      body.append(h('div.sect', h('h3', `Couldn't move · ${r.skipped.length}`),
        r.skipped.slice(0, 10).map((sk) => h('div.fact', h('span.ico2.quiet', icon('warn', 18)), h('span.tx', h('b', sk.name), h('span', `${reasons[sk.reason] || 'It couldn\'t be moved.'} Nothing was lost; the next Bin Files tries again.`)), h('span.pilltag.warn', 'Skipped')))));
    }
    const left = leftList(r.leftOut);
    if (left) body.append(left);
    return { title: done ? (save ? 'Files saved' : 'Files binned') : (save ? 'Saving stopped' : 'Bin Files stopped'), body, foot };
  });
}

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

let pullUi = null;
export function onPull(r) {
  if (r.finished) {
    if (r.error) toast(`The download didn't finish: ${r.error}`);
    else toast('Model downloaded');
    reboot().then(() => { if (pullUi) pullUi(); render(); });
  } else if (pullUi) pullUi(r);
}

export function settings() {
  openSheet((close, entry) => {
    const body = h('div.sbody');
    const set = async (patch) => {
      try { await reboot(await api.call('settings.set', patch)); draw(); render(); } catch (e) { fail(e); }
    };
    const seg = (options, current, onPick) => h('div.seg', { role: 'group' }, options.map(([label, value]) => h('button', { 'aria-pressed': String(current === value), onclick: () => onPick(value) }, label)));
    let progress = null;
    const draw = (pull) => {
      if (pull) progress = pull;
      const b = S.boot;
      const s = b.settings;
      const g = b.genie;
      const modelRow = (spec, ready, what) => {
        const pulling = b.pulling === spec.id;
        return h('div.statusline',
          h(`i.dot${ready ? '.ok' : pulling ? '.run' : '.warn'}`),
          h('span.grow', h('b', spec.displayName), ` · ${what} · `, ready ? 'downloaded' : pulling ? `downloading${progress && progress.model === spec.id ? ` ${Math.round(progress.percent)}%` : '…'}` : `not downloaded (${spec.downloadSize})`),
          !ready && g.path && !pulling ? h('button.mini', { disabled: !!b.pulling, onclick: async () => { try { await api.call('genie.pull', { model: spec.id }); await reboot(); draw(); } catch (e) { fail(e); } } }, icon('download', 16), 'Download') : null,
          pulling ? h('button.mini', { onclick: () => api.call('genie.cancelPull').catch(fail) }, 'Cancel') : null);
      };
      const pathField = (key, placeholder) => {
        const input = h('input.input', { type: 'text', value: s[key], placeholder, spellcheck: false });
        input.addEventListener('change', () => set({ [key]: input.value.trim() }));
        return input;
      };
      const keyInput = h('input.input', { type: 'password', placeholder: 'sk-ant-…', autocomplete: 'off', spellcheck: false });
      const log = h('pre.logbox', 'Loading…');
      api.call('log').then((r) => { log.textContent = r.lines.length ? r.lines.join('\n') : 'Nothing yet.'; log.scrollTop = log.scrollHeight; }).catch(() => {});
      body.replaceChildren(
        h('div.sect', h('h3', 'Appearance'), seg([['Match Windows', 'System'], ['Dark', 'Dark'], ['Light', 'Light']], s.themeMode, (v) => set({ themeMode: v }))),

        h('div.sect', h('h3', 'GenieX'),
          h('div.statusline', h(`i.dot${g.path ? '.ok' : '.warn'}`), h('span.grow.selectable', g.path ? `${g.version || 'GenieX'} · ${g.path}` : 'GenieX was not found on this computer'),
            h('button.mini', { onclick: async () => { try { await reboot(await api.call('genie.status')); draw(); render(); toast(S.boot.genie.path ? 'GenieX found' : 'GenieX not found'); } catch (e) { fail(e); } } }, icon('refresh', 16), 'Check again')),
          g.error ? h('div.note', icon('warn', 18), h('span.grow', `GenieX answered with an error: ${g.error}`)) : null,
          g.path ? null : h('div.note.info', icon('info', 18), h('span.grow', 'Install the GenieX CLI for Windows on Snapdragon, then press Check again. Until then the app runs with a simulated model so you can try everything else. ',
            h('button.link', { onclick: () => api.openExternal('https://github.com/qualcomm/GenieX') }, 'GenieX on GitHub'))),
          modelRow(b.models.vision, g.visionReady, 'analyzes photos and videos'),
          modelRow(b.models.text, g.textReady, 'sorts into categories'),
          h('label.field', h('span.label', 'Run on'), seg([['NPU', 'npu'], ['GPU', 'gpu'], ['CPU', 'cpu']], s.compute, (v) => set({ compute: v }))),
          h('label.field', h('span.label', 'Use'), seg([['GenieX when installed', 'Auto'], ['GenieX always', 'GenieX'], ['Simulated', 'Simulated']], s.backend, (v) => set({ backend: v }))),
          h('label.field', h('span.label', 'Path to geniex.exe (only if it isn\'t found)'), pathField('geniexPath', 'C:\\…\\geniex.exe')),
          h('p.hint', 'The app runs GenieX as terminal commands: geniex list, geniex pull, and one geniex infer per photo, clip or sorting call. The exact commands are in the activity log below.')),

        h('div.sect', h('h3', 'Analysis'),
          h('label.field', h('span.label', 'Max frames per clip'), seg(b.frameCaps.map((n) => [String(n), n]), s.maxFramesPerClip, (v) => set({ maxFramesPerClip: v }))),
          h('p.hint', `${b.frameCapHint}. More frames see more of a long clip and take longer.`),
          h('div.statusline', h(`i.dot${b.ffmpeg.path ? '.ok' : '.warn'}`), h('span.grow.selectable', b.ffmpeg.path ? `FFmpeg · ${b.ffmpeg.path}` : 'FFmpeg not found: the app\'s own window reads video frames instead, for formats Windows can play')),
          h('label.field', h('span.label', 'Path to ffmpeg.exe (only if it isn\'t found)'), pathField('ffmpegPath', 'C:\\…\\ffmpeg.exe')),
          h('label.check', h('input', { type: 'checkbox', checked: s.keepAwake, onchange: (e) => set({ keepAwake: e.target.checked }) }), h('span', 'Keep the computer awake while a run is in progress'))),

        h('div.sect', h('h3', 'Sorting model'),
          seg([[S.boot.simulated ? 'Simulated sorter' : `${b.models.text.displayName} (on this computer)`, 'Lfm3_1_7B'], ['Claude (cloud)', 'Claude']], s.sortingModel, (v) => set({ sortingModel: v })),
          h('p.hint', s.sortingModel === 'Claude' ? (b.apiKey ? 'Sends descriptions and tags, never photos' : 'Add your API key to use Claude') : 'Runs on this computer'),
          h('span.label', 'Claude API key'),
          h('p.hint', 'Stored encrypted on this computer. Only used for sorting.'),
          b.apiKey
            ? h('div.statusline', h('i.dot.ok'), h('span.grow', `Key saved (${b.apiKey.replace('••••', '…')})`), h('button.mini', { onclick: async () => { try { await reboot(await api.call('apiKey.clear')); draw(); } catch (e) { fail(e); } } }, 'Remove'))
            : h('div', { style: 'display:flex;gap:8px' }, keyInput, h('button.mini', { style: 'height:46px', onclick: async () => { try { await reboot(await api.call('apiKey.set', { key: keyInput.value })); draw(); toast('Key saved'); } catch (e) { fail(e); } } }, icon('key', 16), 'Save key'))),

        h('div.sect', h('h3', 'Activity log'), log),
        h('div.sect', h('h3', 'About'), h('p.hint', `Agentic Binning for Windows ${b.version}. New projects are made in ${b.projectsRoot}.`)));
    };
    pullUi = draw;
    entry.onClose = () => { pullUi = null; };
    draw();
    return { title: 'Settings', body, tall: true };
  });
}

// ---------------------------------------------------------------------------------------------
// Reading video frames in the window (when FFmpeg is not installed)
// ---------------------------------------------------------------------------------------------

/**
 * The core asks the window to decode a clip: seek to each time, draw the frame padded onto a
 * black square, and hand the JPEGs back. `req`: {id, url, times|null, durationMs, maxFrames, edge}.
 */
export async function grabFrames(req) {
  const video = document.createElement('video');
  video.muted = true;
  video.preload = 'auto';
  video.crossOrigin = 'anonymous';
  const once = (name, ms) => new Promise((ok, no) => {
    const t = setTimeout(() => no(new Error('The video took too long to answer')), ms);
    video.addEventListener(name, () => { clearTimeout(t); ok(); }, { once: true });
    video.addEventListener('error', () => { clearTimeout(t); no(new Error('Windows could not play this video format')); }, { once: true });
  });
  try {
    video.src = api.fileUrl(req.path);
    await once('loadeddata', 20000);
    const durationMs = Math.round((Number.isFinite(video.duration) ? video.duration : 0) * 1000) || req.durationMs || 0;
    const step = 500;
    const atFps = Math.max(1, Math.ceil(durationMs / step));
    const times = durationMs <= 0 ? [0] : atFps <= req.maxFrames
      ? Array.from({ length: atFps }, (_, i) => i * step)
      : Array.from({ length: req.maxFrames }, (_, i) => Math.floor((durationMs * i) / req.maxFrames));
    const canvas = document.createElement('canvas');
    canvas.width = req.edge; canvas.height = req.edge;
    const g = canvas.getContext('2d');
    const jpeg = (c, q) => c.toDataURL('image/jpeg', q).split(',')[1];
    for (let i = 0; i < times.length; i++) {
      video.currentTime = Math.min(times[i] / 1000, Math.max(0, video.duration - 0.05));
      await once('seeked', 15000);
      const scale = req.edge / Math.max(video.videoWidth, video.videoHeight);
      const w = Math.max(1, Math.round(video.videoWidth * scale));
      const hgt = Math.max(1, Math.round(video.videoHeight * scale));
      g.fillStyle = '#000';
      g.fillRect(0, 0, req.edge, req.edge);
      g.drawImage(video, (req.edge - w) / 2, (req.edge - hgt) / 2, w, hgt);
      let thumb = null;
      if (i === 0) {
        const ts = Math.min(1, 512 / Math.max(video.videoWidth, video.videoHeight));
        const tc = document.createElement('canvas');
        tc.width = Math.max(1, Math.round(video.videoWidth * ts));
        tc.height = Math.max(1, Math.round(video.videoHeight * ts));
        tc.getContext('2d').drawImage(video, 0, 0, tc.width, tc.height);
        thumb = jpeg(tc, 0.85);
      }
      await api.frameResult({ id: req.id, timeMs: times[i], total: times.length, jpeg: jpeg(canvas, 0.9), thumb, done: false, error: null });
    }
    await api.frameResult({ id: req.id, timeMs: 0, total: times.length, jpeg: null, thumb: null, done: true, error: null });
  } catch (e) {
    await api.frameResult({ id: req.id, timeMs: 0, total: 0, jpeg: null, thumb: null, done: true, error: String(e.message || e) });
  } finally {
    video.removeAttribute('src');
    video.load();
  }
}
