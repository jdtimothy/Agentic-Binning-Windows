// Every sheet: projects, item details, categories, Re-sort, filters, Bin Files, the report, settings.

import * as api from './api.js';
import { h, icon, toast, fail, openSheet, confirmSheet, duration, plural } from './dom.js';
import { S, project, running, refresh, reboot, show, render } from './main.js';

const rowBtn = (ic, title, sub, onclick, extra) => h('button.rowbtn', { onclick }, h('span.ico2', icon(ic)), h('span.tx', h('b', title), sub ? h('span', sub) : null), extra || null);

// ---------------------------------------------------------------------------------------------
// Projects and adding media
// ---------------------------------------------------------------------------------------------

export async function addFolder() {
  try {
    const dir = await api.pickFolder('Add a folder of photos and videos');
    if (!dir) return;
    S.registration = { done: 0, total: 0 };
    onRegistration(S.registration);
    await api.call('media.addFolder', { path: dir });
  } catch (e) { fail(e); }
}

export async function addFiles() {
  try {
    const files = await api.pickFiles('Pick photos and videos');
    if (!files.length) return;
    S.registration = { done: 0, total: files.length };
    onRegistration(S.registration);
    await api.call('media.addFiles', { paths: files });
  } catch (e) { fail(e); }
}

export async function copyHeld(held) {
  const names = [...new Set(held.map((f) => f.project))].join(', ');
  const ok = await confirmSheet('Copy them in?',
    `${plural(held.length, 'file')} belong${held.length === 1 ? 's' : ''} to ${names}. Copies go into a "Copied from" folder in this project and are checked against the originals, which stay where they are.`, 'Copy them in');
  if (ok) api.call('media.copyHeld', { held }).catch(fail);
}

let regLine = null;
/** The one-line progress and summary under "Add a folder" in the project sheet. */
export function onRegistration(r) {
  if (!regLine || !document.contains(regLine)) return;
  if (!r) return regLine.replaceChildren();
  if (r.summary) {
    regLine.replaceChildren(h('span', r.summary.text),
      r.summary.held.length ? h('button.link', { style: 'margin-left:8px', onclick: () => copyHeld(r.summary.held) }, 'Copy them in') : null);
  } else {
    regLine.replaceChildren(r.total ? `Adding ${r.done.toLocaleString()} of ${r.total.toLocaleString()}…` : 'Looking for photos and videos…');
  }
}

export function projects() {
  openSheet((close) => {
    const body = h('div.sbody');
    let editing = false;
    const draw = () => {
      const cur = project();
      const list = S.boot.projects.projects;
      const parts = [];
      if (cur) {
        regLine = h('p.hint', { 'aria-live': 'polite' });
        parts.push(h('div.sect',
          h('h3', 'Current project'),
          h('div.two-up',
            rowBtn('folder', 'Add a folder', 'Everything inside it', () => addFolder()),
            rowBtn('image', 'Pick photos and videos', 'Choose files', () => addFiles())),
          regLine,
          h('p.hint', 'Files stay where they are until you press Bin Files. You can also drop them on the window.')));
        onRegistration(S.registration);
      }
      parts.push(h('div.sect',
        h('div', { style: 'display:flex;align-items:center;gap:8px' },
          h('h3', { style: 'flex:1' }, 'Current projects'),
          list.length ? h('button.mini', { onclick: () => { editing = !editing; draw(); } }, editing ? 'Done' : 'Edit') : null),
        h('div.two-up',
          rowBtn('open', 'Open project', 'A folder that already holds one', async () => {
            try {
              const dir = await api.pickFolder('Open a project folder');
              if (!dir) return;
              S.boot.projects = await api.call('project.open', { path: dir });
              await refresh();
              close();
            } catch (e) { fail(e); }
          }),
          rowBtn('plus', 'New project', S.boot.projectsRoot, () => { close(); newProject(); })),
        list.map((p) => h(`div.rowbtn${p.open ? '.cur' : ''}`, { style: 'padding:0;min-height:0' },
          h('button', { style: 'flex:1;min-width:0;display:flex;align-items:center;gap:12px;padding:12px 14px;text-align:left', disabled: p.missing, onclick: async () => {
            if (p.open) return close();
            try {
              S.boot.projects = await api.call('project.switch', { path: p.path });
              await refresh();
              close();
            } catch (e) { fail(e); }
          } },
            h('span.ico2', icon('folder')),
            h('span.tx', h('b', p.name), h('span', p.missing ? 'Folder not found' : p.displayPath)),
            p.open ? h('span.pillnote', 'Open') : null),
          editing ? h('button.ib.sm', { style: 'margin-right:10px', 'aria-label': `Take ${p.name} off the list`, title: 'Takes it off this list only. Its files and database stay.', onclick: async () => {
            try {
              const r = await api.call('project.remove', { path: p.path });
              S.boot.projects = r.projects;
              await refresh();
              draw();
              if (r.removed) toast(`${p.name} is off the list. Its files are untouched.`, { label: 'Undo', run: async () => { S.boot.projects = await api.call('project.restore', r.removed); await refresh(); } });
            } catch (e) { fail(e); }
          } }, icon('x', 16)) : null)),
        list.length ? null : h('p.hint', 'No projects yet. A project is a folder: your media goes in it, sorted into category folders.')));
      body.replaceChildren(...parts);
    };
    draw();
    return { title: 'Projects', body };
  });
}

export function newProject() {
  openSheet((close) => {
    let pending = null; // {kind:'folder', path} or {kind:'files', paths}
    const name = h('input', { type: 'text', placeholder: 'Maui 2026', maxlength: 80, autofocus: true, spellcheck: false });
    const picked = h('p.hint');
    const create = h('button.primary', { style: 'flex:none' }, 'Create');
    const sync = () => {
      create.replaceChildren(pending ? 'Create and analyze' : 'Create');
      picked.replaceChildren(pending ? (pending.kind === 'folder' ? `Will add everything in ${pending.path}` : `Will add ${plural(pending.paths.length, 'file')}`) : 'Optional. You can add media later too.');
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
      body: h('div.sbody',
        h('label.field', h('span.label', 'Name'), name),
        h('p.hint', `A folder with this name is made in ${S.boot.projectsRoot}. Bin Files moves your media into category folders inside it.`),
        h('div.sect', h('h3', 'Start with'),
          h('div.two-up',
            rowBtn('folder', 'A folder', 'Everything inside it', async () => { const d = await api.pickFolder('Add a folder of photos and videos'); if (d) { pending = { kind: 'folder', path: d }; sync(); } }),
            rowBtn('image', 'Photos and videos', 'Choose files', async () => { const f = await api.pickFiles('Pick photos and videos'); if (f.length) { pending = { kind: 'files', paths: f }; sync(); } })),
          picked)),
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'), create),
    };
  });
}

// ---------------------------------------------------------------------------------------------
// Item detail, bulk edits
// ---------------------------------------------------------------------------------------------

/** A category picker: the existing ones plus "New category…", which shows a name field. */
function categoryPicker(categories, currentId, allowKeep) {
  const select = h('select', { 'aria-label': 'Category' },
    allowKeep ? h('option', { value: '' }, currentId ? 'Keep' : 'No category yet') : null,
    categories.map((c) => h('option', { value: String(c.id), selected: c.id === currentId }, c.name)),
    h('option', { value: 'new' }, 'New category…'));
  const fresh = h('input', { type: 'text', placeholder: 'One word, like Sunsets', maxlength: 40, hidden: true, spellcheck: false });
  select.addEventListener('change', () => { fresh.hidden = select.value !== 'new'; if (!fresh.hidden) fresh.focus(); });
  if (!categories.length && !allowKeep) { select.value = 'new'; fresh.hidden = false; }
  return {
    nodes: [select, fresh],
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
      throw new Error('Pick a category');
    },
  };
}

function applied(result, done) {
  if (result.ok) { done(); refresh(); } else toast(result.error);
}

export async function detail(id) {
  let d;
  try { d = await api.call('item.detail', { id }); } catch (e) { return fail(e); }
  if (!d) return toast('It isn\'t in the project any more');
  const item = d.item;
  const failed = item.status === 'Failed' && !d.analysis;
  const cats = S.all ? S.all.categories : [];
  openSheet((close) => {
    const isVideo = item.kind === 'Video';
    const preview = h('div.preview');
    if (d.fileExists && isVideo) preview.append(h('video', { src: api.fileUrl(item.currentPath), controls: true, preload: 'metadata', poster: d.thumb ? api.fileUrl(d.thumb) : null }));
    else if (d.thumb || (d.fileExists && /\.(jpe?g|png|webp)$/i.test(item.currentPath))) {
      const img = h('img', { src: api.fileUrl(d.fileExists && /\.(jpe?g|png|webp)$/i.test(item.currentPath) ? item.currentPath : d.thumb), alt: '' });
      img.addEventListener('error', () => { if (d.thumb && !img.dataset.fell) { img.dataset.fell = '1'; img.src = api.fileUrl(d.thumb); } });
      preview.append(img);
    } else preview.append(h('div', { style: 'color:#9aa3b2;padding:40px' }, icon(isVideo ? 'film' : 'image', 48)));

    const name = h('input', { type: 'text', value: item.friendlyName || (d.analysis ? d.analysis.suggestedName : '') || '', maxlength: 60, placeholder: 'A friendly name', spellcheck: false });
    const picker = categoryPicker(cats, item.categoryId, !failed);
    let unlockName = false;
    let unlockCategory = false;
    const lock = (isLocked, what, onUnlock) => {
      if (!isLocked) return h('div.lockrow', h('span.pillnote', icon('unlock', 12), `${what} set by the model; Re-sort may change it`));
      const row = h('div.lockrow', h('span.pillnote', icon('lock', 12), `${what} set by you; Re-sort keeps it`),
        h('button.link', { onclick: () => { onUnlock(); row.replaceChildren(h('span.pillnote', icon('unlock', 12), 'Unlocks when you save')); } }, 'Unlock'));
      return row;
    };
    const facts = h('dl.kv',
      h('dt', 'File'), h('dd.selectable', item.originalName),
      h('dt', 'Where'), h('dd.selectable', d.displayPath),
      isVideo && item.durationMs ? [h('dt', 'Length'), h('dd', duration(item.durationMs))] : null,
      item.width && item.height ? [h('dt', 'Size'), h('dd', `${item.width} × ${item.height}`)] : null,
      item.takenAt ? [h('dt', 'Taken'), h('dd', new Date(item.takenAt).toLocaleString())] : null,
      d.analysis ? [h('dt', 'Shot'), h('dd', d.analysis.shotType)] : null,
      d.analysis ? [h('dt', 'Model'), h('dd', `${d.analysis.modelId} · ${d.analysis.generatedTokens} tokens`)] : null);

    const right = h('div');
    if (d.simulated) right.append(h('div.note', icon('info', 18), h('span.grow', h('b', 'Simulated analysis. '), 'This text was not written by a vision model.')));
    if (item.status === 'Missing') right.append(h('div.note', icon('warn', 18), h('span.grow', h('b', 'File missing. '), 'It is no longer at this path. Put it back, or remove the item from the project.')));
    if (item.status === 'Failed') {
      right.append(h('div.note', icon('warn', 18), h('span.grow', h('b', 'Analysis failed. '), d.failure, failed ? ' Retry, or give it a name and category by hand.' : '')));
    }
    right.append(
      h('label.field', h('span.label', 'Name'), name),
      failed ? null : lock(item.nameSetByUser, 'Name', () => { unlockName = true; }),
      h('div.field', h('span.label', 'Category'), ...picker.nodes),
      failed || !item.categoryId ? null : lock(item.categorySetByUser, 'Category', () => { unlockCategory = true; }));
    if (d.analysis) {
      right.append(h('div.sect', h('h3', 'Description'), h('p.selectable', { style: 'margin:0;font-size:14.5px;line-height:1.5' }, d.analysis.description)));
      if (d.analysis.actions.length) right.append(h('div.sect', h('h3', 'Actions'), h('ul.actions.selectable', d.analysis.actions.map((a) => h('li', h('time', a.t), h('span', a.text))))));
      if (d.tags.length) right.append(h('div.sect', h('h3', 'Tags'), h('div.tags', d.tags.map((t) => h('button.tag', { title: `Search for ${t}`, onclick: () => { S.query = t; close(); show('media'); } }, t)))));
    }

    const save = async () => {
      try {
        if (failed) {
          applied(await api.call('item.setByHand', { id, name: name.value, category: picker.required() }), close);
        } else {
          applied(await api.call('item.save', { id, name: name.value, category: picker.value(), unlockName, unlockCategory }), close);
        }
      } catch (e) { fail(e); }
    };
    name.addEventListener('keydown', (e) => { if (e.key === 'Enter') save(); });
    return {
      title: item.friendlyName || (d.analysis && d.analysis.suggestedName) || item.originalName,
      wide: true,
      body: h('div.sbody', h('div.detail', h('div', preview, facts), right)),
      foot: h('div.sfoot',
        h('button.secondary.danger', { title: 'The file stays where it is', onclick: () => { close(); removeItems([id]); } }, icon('trash'), 'Remove'),
        d.fileExists ? h('button.secondary', { onclick: () => api.reveal(item.currentPath).catch(fail) }, icon('open'), 'Show in Explorer') : null,
        h('div.spacer'),
        item.status === 'Failed' ? h('button.secondary', { onclick: async () => { try { applied(await api.call('item.retry', { id }), close); } catch (e) { fail(e); } } }, icon('refresh'), 'Retry analysis') : null,
        h('button.primary', { style: 'flex:none', onclick: save }, failed ? 'Set by hand' : 'Save')),
    };
  });
}

export function moveTo(ids) {
  const cats = S.all ? S.all.categories : [];
  openSheet((close) => {
    const picker = categoryPicker(cats, null, false);
    return {
      title: `Move ${plural(ids.length, 'item')} to a category`,
      body: h('div.sbody',
        h('div.field', h('span.label', 'Category'), ...picker.nodes),
        h('p.hint', 'Their category becomes yours: a Re-sort keeps it. Items not analyzed yet are left as they are. Files already binned move to the new folder right away.')),
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'),
        h('button.primary', { style: 'flex:none', onclick: async () => {
          try {
            applied(await api.call('items.move', { ids, category: picker.required() }), () => { S.selecting = false; S.selected.clear(); close(); });
          } catch (e) { fail(e); }
        } }, 'Move')),
    };
  });
}

export async function removeItems(ids) {
  const ok = await confirmSheet(`Remove ${plural(ids.length, 'item')} from the project?`,
    'The project forgets them: their names, descriptions and categories. The files themselves are not deleted and stay exactly where they are.', 'Remove from project', true);
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

export function categoryEdit() {
  const cats = S.all ? S.all.categories : [];
  const counts = new Map((S.all ? S.all.chips : []).map((c) => [c.chip, c.count]));
  openSheet((close) => {
    const rows = cats.map((c) => {
      const input = h('input.input', { type: 'text', value: c.name, maxlength: 40, 'aria-label': `Name of ${c.name}`, spellcheck: false });
      const merge = h('select', { 'aria-label': `Merge ${c.name} into` }, h('option', { value: '' }, 'Keep'), cats.filter((o) => o.id !== c.id).map((o) => h('option', { value: String(o.id) }, `Merge into ${o.name}`)));
      merge.addEventListener('change', () => { input.disabled = !!merge.value; });
      return { c, input, merge };
    });
    const save = async () => {
      const merges = rows.filter((r) => r.merge.value).map((r) => [Number(r.merge.value), r.c.id]);
      const dropped = new Set(merges.map((m) => m[1]));
      if (merges.some((m) => dropped.has(m[0]))) return toast('A category can\'t be merged into one that is itself being merged');
      const renames = rows.filter((r) => !dropped.has(r.c.id) && r.input.value.trim() !== r.c.name).map((r) => [r.c.id, r.input.value]);
      if (!merges.length && !renames.length) return close();
      try { applied(await api.call('categories.apply', { renames, merges }), close); } catch (e) { fail(e); }
    };
    return {
      title: 'Edit categories',
      tall: true,
      body: h('div.sbody',
        h('p.hint', 'Rename a category or merge it into another. A category you change becomes yours, so a Re-sort keeps it. Files already binned move to match when you save.'),
        h('ul.list.catedit', rows.map((r) => h('li', r.input, h('span', { style: 'margin:0;min-width:34px;text-align:right' }, (counts.get(String(r.c.id)) || 0).toLocaleString()), r.merge)))),
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'), h('button.primary', { style: 'flex:none', onclick: save }, 'Save')),
    };
  });
}

export function resort() {
  openSheet((close) => {
    const s = S.boot.settings;
    const model = h('select', { 'aria-label': 'Sorting model' },
      h('option', { value: 'Lfm3_1_7B', selected: s.sortingModel === 'Lfm3_1_7B' }, S.boot.simulated ? 'Simulated sorter (on this computer)' : `${S.boot.models.text.displayName} (on this computer)`),
      h('option', { value: 'Claude', selected: s.sortingModel === 'Claude' }, `Claude (cloud${S.boot.apiKey ? '' : ', needs an API key'})`));
    const keep = h('input', { type: 'checkbox', checked: true });
    return {
      title: 'Re-sort',
      body: h('div.sbody',
        h('p.hint', 'Sorts every item into categories again from its description and tags. The photos and videos are not analyzed again.'),
        h('label.field', h('span.label', 'Sort with'), model),
        h('label.check', keep, h('span', 'Keep my edits', h('small', 'Names and categories you set stay as they are. Untick to let the model redo everything.'))),
        h('p.hint', 'Files already in category folders stay there until the next Bin Files moves them. With Claude, only the text the local model wrote is sent, never your photos or videos.')),
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'),
        h('button.primary', { style: 'flex:none', onclick: async () => {
          try {
            const r = await api.call('analysis.resort', { model: model.value, keepEdits: keep.checked });
            if (r.ok) close(); else toast(r.error);
          } catch (e) { fail(e); }
        } }, 'Re-sort')),
    };
  });
}

export function filters() {
  openSheet((close) => {
    const f = S.filter;
    const shot = h('select', h('option', { value: '' }, 'Any'), ['wide', 'medium', 'close'].map((t) => h('option', { value: t, selected: f.shotType === t }, t[0].toUpperCase() + t.slice(1))));
    const day = (ms) => (ms ? new Date(ms - new Date(ms).getTimezoneOffset() * 60000).toISOString().slice(0, 10) : '');
    const from = h('input', { type: 'date', value: day(f.takenFrom) });
    const to = h('input', { type: 'date', value: day(f.takenTo) });
    const apply = () => {
      f.shotType = shot.value || null;
      f.takenFrom = from.value ? new Date(from.value + 'T00:00:00').getTime() : null;
      f.takenTo = to.value ? new Date(to.value + 'T23:59:59.999').getTime() : null;
      close();
      refresh();
    };
    return {
      title: 'Filters',
      body: h('div.sbody',
        h('label.field', h('span.label', 'Shot type'), shot),
        h('div.two-up', h('label.field', h('span.label', 'Taken from'), from), h('label.field', h('span.label', 'Taken to'), to))),
      foot: h('div.sfoot', h('button.secondary', { onclick: () => { shot.value = ''; from.value = ''; to.value = ''; apply(); } }, 'Clear'), h('div.spacer'), h('button.primary', { style: 'flex:none', onclick: apply }, 'Show results')),
    };
  });
}

// ---------------------------------------------------------------------------------------------
// Bin Files
// ---------------------------------------------------------------------------------------------

export async function binFiles() {
  let data;
  try { data = await api.call('bin.preview'); } catch (e) { return fail(e); }
  if (data.unexpected.length) return unknownFiles(data.unexpected);
  const p = data.preview;
  openSheet((close) => {
    const body = h('div.sbody');
    if (p.files === 0) {
      body.append(h('p.big', 'Nothing to move'), h('p.hint', p.inPlace ? `${plural(p.inPlace, 'file')} ${p.inPlace === 1 ? 'is' : 'are'} already in place. Bin Files will check the category folders.` : 'Bin Files will check the category folders.'));
    } else {
      body.append(
        h('p.big', `Move ${plural(p.files, 'file')} into ${plural(p.folders, 'category folder')}`),
        h('p.hint', `Inside ${data.projectPath}. Each file is renamed to its friendly name and moved into a folder named for its category. Nothing is ever overwritten: a taken name gets a number.`));
      if (p.sources.length) body.append(h('div.sect', h('h3', 'Moved from'), h('ul.list', p.sources.slice(0, 8).map((s) => h('li', h('b', s.folder), h('small', { style: 'color:var(--sub);min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap' }, s.path), h('span', s.count.toLocaleString()))),
        p.sources.length > 8 ? h('li', `and ${p.sources.length - 8} more folders`) : null)));
      if (p.otherStorage) body.append(h('div.note.info', icon('info', 18), h('span.grow', h('b', `${plural(p.otherStorage, 'file')} on another drive. `), 'Each is copied, the copy is checked against the original, and only then is the original deleted.')));
      if (p.shared) body.append(h('div.note.info', icon('info', 18), h('span.grow', h('b', `${plural(p.shared, 'file')} also in another project. `), 'These are copied and checked; the originals stay for the other project.')));
      if (p.otherStorage || p.shared) body.append(h('p.hint', `The copies need ${data.copyBytesText}; ${data.freeBytesText} is free.`));
      if (!data.enoughSpace) body.append(h('div.note', icon('warn', 18), h('span.grow', h('b', 'Not enough free space. '), `Free up some room on the project's drive first.`)));
      if (p.inPlace) body.append(h('p.hint', `${plural(p.inPlace, 'file')} already in place.`));
    }
    if (p.leftOut.length) {
      body.append(h('div.sect', h('h3', `Left where they are · ${p.leftOut.length}`),
        h('ul.list', p.leftOut.slice(0, 6).map((l) => h('li', h('b', l.name), h('span', l.reason === 'Failed' ? 'Analysis failed' : 'File missing'))), p.leftOut.length > 6 ? h('li', `and ${p.leftOut.length - 6} more`) : null)));
    }
    body.append(h('div.note', icon('warn', 18), h('span.grow', h('b', 'There is no undo. '), 'You can pause or stop between files, and a stopped run picks up where it left off.')));
    return {
      title: 'Bin Files',
      body,
      foot: h('div.sfoot', h('div.spacer'), h('button.secondary', { onclick: close }, 'Cancel'),
        h('button.primary', { style: 'flex:none', disabled: p.files > 0 && !data.enoughSpace, onclick: async () => {
          try {
            const r = await api.call('bin.start');
            if (r.ok) { close(); show('media'); } else toast(r.error);
          } catch (e) { fail(e); }
        } }, icon('bin'), p.files ? `Move ${plural(p.files, 'file')}` : 'Check the folders')),
    };
  });
}

/** Files in the category folders that no item points to: they block Bin Files until answered. */
function unknownFiles(files) {
  openSheet((close) => {
    const media = files.filter((f) => f.isMedia);
    const answer = async (cmd, paths, done) => {
      try {
        await api.call(cmd, { paths });
        close();
        toast(done);
        await refresh();
        if (cmd === 'bin.leave') binFiles();
      } catch (e) { fail(e); }
    };
    return {
      title: 'Files the project doesn\'t know',
      body: h('div.sbody',
        h('p.hint', `${plural(files.length, 'file')} in the category folders ${files.length === 1 ? 'isn\'t' : 'aren\'t'} in the project. Bin Files won't move anything until you decide what they are.`),
        h('ul.list', files.slice(0, 12).map((f) => h('li', h('b', f.name), h('span', f.folder))), files.length > 12 ? h('li', `and ${files.length - 12} more`) : null)),
      foot: h('div.sfoot',
        h('button.secondary', { onclick: () => answer('bin.leave', files.map((f) => f.path), 'Left where they are. Bin Files won\'t ask about them again.') }, 'Leave them'),
        h('div.spacer'),
        media.length ? h('button.primary', { style: 'flex:none', onclick: () => answer('bin.add', media.map((f) => f.path), `Adding ${plural(media.length, 'file')} to the project`) }, `Add ${media.length === files.length ? 'them' : plural(media.length, 'photo or video', 'photos and videos')} to the project`) : null),
    };
  });
}

let reportShown = null;
/** Shows the last run's report until it is closed, also after a restart. */
export async function reportIfUnread(state) {
  if (!project() || running()) return;
  if (state && state.moveOutcome === 'Blocked') {
    try { const d = await api.call('bin.preview'); if (d.unexpected.length) unknownFiles(d.unexpected); } catch (e) { fail(e); }
    return;
  }
  let r;
  try { r = (await api.call('bin.report')).report; } catch { return; }
  if (!r || reportShown === r.runId) return;
  reportShown = r.runId;
  const reasons = { 'copy-mismatch': 'The copy didn\'t match the original twice', interrupted: 'The app closed twice while moving it', 'rename-check': 'It wasn\'t where it should be after moving', io: 'Windows refused the move (is it open in another program?)' };
  openSheet((close, entry) => {
    entry.onClose = () => { api.call('bin.closeReport', { runId: r.runId }).catch(() => {}); refresh(); };
    const body = h('div.sbody', h('p.big', r.headline));
    if (r.message) body.append(h('div.note', icon('warn', 18), h('span.grow', r.message)));
    if (r.state !== 'Done' && r.stillToMove) body.append(h('p.hint', `${plural(r.stillToMove, 'file')} still to move${r.lastMoved ? `. The last one moved was ${r.lastMoved}` : ''}. Press Bin Files to carry on.`));
    const facts = h('dl.kv');
    const fact = (k, v) => { if (v) facts.append(h('dt', k), h('dd', v)); };
    fact('Moved and checked', r.moved ? plural(r.moved, 'file') : '');
    fact('Copied (originals kept)', r.copiedShared ? plural(r.copiedShared, 'file') : '');
    fact('Already in place', r.inPlace ? plural(r.inPlace, 'file') : '');
    fact('Gone when its turn came', r.becameMissing ? plural(r.becameMissing, 'file') : '');
    fact('Empty folders removed', r.removedFolders.join(', '));
    fact('Folder check', r.checkOk === true ? 'Every binned file is where it should be' : r.checkOk === false ? 'Something is missing or extra; press Bin Files to see' : '');
    if (facts.children.length) body.append(facts);
    if (r.numbered.length) body.append(h('div.sect', h('h3', `Got a number because the name was taken · ${r.numbered.length}`), h('ul.list', r.numbered.slice(0, 8).map((n) => h('li', n)))));
    if (r.skipped.length) body.append(h('div.sect', h('h3', `Skipped · ${r.skipped.length}`), h('ul.list', r.skipped.slice(0, 10).map((s) => h('li', h('b', s.name), h('span', { style: 'white-space:normal;text-align:right' }, reasons[s.reason] || s.reason)))), h('p.hint', 'Skipped files are untouched. Bin Files tries them again.')));
    if (r.leftOut.length) body.append(h('div.sect', h('h3', `Left where they are · ${r.leftOut.length}`), h('ul.list', r.leftOut.slice(0, 8).map((l) => h('li', h('b', l.name), h('span', l.reason === 'Failed' ? 'Analysis failed' : 'File missing'))))));
    return { title: r.kind === 'Save' ? 'Saved' : 'Bin Files report', body, foot: h('div.sfoot', h('div.spacer'), h('button.primary', { style: 'flex:none', onclick: close }, 'Done')) };
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
          h('label.field', h('span.label', 'Most frames read from one clip'), seg(b.frameCaps.map((n) => [String(n), n]), s.maxFramesPerClip, (v) => set({ maxFramesPerClip: v }))),
          h('p.hint', `${b.frameCapHint}. More frames see more of a long clip and take longer.`),
          h('div.statusline', h(`i.dot${b.ffmpeg.path ? '.ok' : '.warn'}`), h('span.grow.selectable', b.ffmpeg.path ? `FFmpeg · ${b.ffmpeg.path}` : 'FFmpeg not found: the app\'s own window reads video frames instead, for formats Windows can play')),
          h('label.field', h('span.label', 'Path to ffmpeg.exe (only if it isn\'t found)'), pathField('ffmpegPath', 'C:\\…\\ffmpeg.exe')),
          h('label.check', h('input', { type: 'checkbox', checked: s.keepAwake, onchange: (e) => set({ keepAwake: e.target.checked }) }), h('span', 'Keep the computer awake while a run is in progress'))),

        h('div.sect', h('h3', 'Sorting into categories'),
          seg([[S.boot.simulated ? 'Simulated sorter' : `${b.models.text.displayName} · on this computer`, 'Lfm3_1_7B'], ['Claude · cloud', 'Claude']], s.sortingModel, (v) => set({ sortingModel: v })),
          h('p.hint', 'Claude works only from the text the local model wrote (names, descriptions, tags). Your photos and videos are never sent. It needs your own Anthropic API key, and each sort is a paid call.'),
          b.apiKey
            ? h('div.statusline', h('i.dot.ok'), h('span.grow', `API key saved · ${b.apiKey}`), h('button.mini', { onclick: async () => { try { await reboot(await api.call('apiKey.clear')); draw(); } catch (e) { fail(e); } } }, 'Remove'))
            : h('div', { style: 'display:flex;gap:8px' }, keyInput, h('button.mini', { style: 'height:46px', onclick: async () => { try { await reboot(await api.call('apiKey.set', { key: keyInput.value })); draw(); toast('Key saved in Windows Credential Manager'); } catch (e) { fail(e); } } }, icon('key', 16), 'Save key'))),

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
