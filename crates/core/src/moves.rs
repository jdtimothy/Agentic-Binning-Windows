//! Bin Files and Save: planning every destination before anything moves, the folder check, the
//! engine that moves one file at a time without ever losing or overwriting one, and the report.
//! Ported from the Android app's `move` package.

use crate::db::{self, BinItem, ItemStatus, MoveRun, MoveStep, NewStep, ProjectDb};
use crate::error::Result;
use crate::files;
use crate::names::project_folder_name;
use crate::paths::{file_name, key, parent};
use parking_lot::{Condvar, Mutex};
use regex::Regex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Every run keeps this much free, so the database journal always has room.
pub const MIN_FREE_BYTES: i64 = 50 * 1024 * 1024;
/// A copy needs its own size plus this much free.
pub const COPY_MARGIN_BYTES: i64 = 200 * 1024 * 1024;
/// A step may start twice (once, and once more after a crash); the third start skips it.
pub const MAX_STARTS: i64 = 2;
/// Copies of one file before it is skipped.
pub const COPY_TRIES: usize = 2;
const RENAME_TRIES: usize = 5;
const RECENT: usize = 2;
/// The start of a folder that copies of another project's files go into.
pub const COPIED_FROM: &str = "Copied from ";

pub mod step_errors {
    /// A copy didn't match its original twice; the source is untouched.
    pub const COPY_MISMATCH: &str = "copy-mismatch";
    /// The step was started twice without finishing: a file that crashes the app can't loop.
    pub const INTERRUPTED: &str = "interrupted";
    /// A rename's check failed: the file isn't at its target with the same size. Stops the run.
    pub const RENAME_CHECK: &str = "rename-check";
    /// The rename or copy was refused by the system.
    pub const IO: &str = "io";
}

fn is_final(state: &str) -> bool {
    !matches!(state, "Planned" | "Copied" | "Verified")
}

// ---------------------------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------------------------

/// One item whose file should end up at `<project>/<folder>/<baseName><extension>`.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveRequest {
    pub item_id: i64,
    pub source: String,
    /// The category's name; made safe here.
    pub folder: String,
    /// The friendly name; made safe here. Falls back to the source file's own name if empty.
    pub base_name: String,
    /// The original extension, with or without the dot; kept as it is.
    pub extension: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedMove {
    pub item_id: i64,
    pub source: String,
    pub dest: String,
}

/// The folder name for a category, safe on Windows.
pub fn folder_for(category_name: &str) -> String {
    let f = project_folder_name(category_name);
    if f.is_empty() {
        "Category".into()
    } else {
        f
    }
}

fn ext_of(r: &MoveRequest) -> String {
    let e = r.extension.trim();
    if e.is_empty() || e.starts_with('.') {
        e.to_string()
    } else {
        format!(".{e}")
    }
}

fn source_stem(path: &str) -> String {
    let name = file_name(path);
    let stem = match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    };
    if stem.is_empty() {
        "Item".into()
    } else {
        stem.to_string()
    }
}

fn target(root: &str, r: &MoveRequest, n: usize) -> String {
    let mut base = project_folder_name(&r.base_name);
    if base.is_empty() {
        base = source_stem(&r.source);
    }
    let name = if n == 1 { base } else { format!("{base} {n}") };
    format!("{root}/{}/{name}{}", folder_for(&r.folder), ext_of(r))
}

/// The target with no number. A destination other than this was numbered.
pub fn plain_target(project_dir: &str, r: &MoveRequest) -> String {
    target(project_dir.trim_end_matches('/'), r, 1)
}

/// Whether the request's source is already at its target: the plain target, or the same name with
/// a " 2"-style number it got earlier, in the right folder (names compare ignoring case).
pub fn is_in_place(project_dir: &str, r: &MoveRequest) -> bool {
    let root = project_dir.trim_end_matches('/');
    let plain = target(root, r, 1);
    if key(&plain) == key(&r.source) {
        return true;
    }
    if key(parent(&r.source)) != key(parent(&plain)) {
        return false;
    }
    let ext = ext_of(r);
    let plain_name = file_name(&plain);
    let base = &plain_name[..plain_name.len() - ext.len()];
    let pattern = format!("(?i)^{} ([2-9]|[1-9][0-9]+){}$", regex::escape(base), regex::escape(&ext));
    Regex::new(&pattern).map(|re| re.is_match(file_name(&r.source))).unwrap_or(false)
}

/// Works out every destination path before anything moves. It never overwrites: a name already
/// taken, on disk or earlier in the same plan, gets " 2", " 3" and so on. Every existing path
/// counts as taken, including files that will move away later in the plan, so the result doesn't
/// depend on the order the moves run in.
pub fn plan_moves(project_dir: &str, requests: &[MoveRequest], existing_paths: &HashSet<String>) -> Vec<PlannedMove> {
    let root = project_dir.trim_end_matches('/');
    let on_disk: HashSet<String> = existing_paths.iter().map(|p| key(p)).collect();
    let mut reserved: HashSet<String> = HashSet::new();
    let mut sorted: Vec<&MoveRequest> = requests.iter().collect();
    sorted.sort_by_key(|r| r.item_id);
    // Files already at their target keep their name; plan them first so nothing else takes it.
    let mut in_place = HashSet::new();
    for r in &sorted {
        if is_in_place(root, r) {
            in_place.insert(r.item_id);
            reserved.insert(key(&r.source));
        }
    }
    let mut moves = Vec::new();
    for r in sorted {
        if in_place.contains(&r.item_id) {
            continue;
        }
        let mut n = 1;
        let mut dest = target(root, r, n);
        while reserved.contains(&key(&dest)) || (on_disk.contains(&key(&dest)) && key(&dest) != key(&r.source)) {
            n += 1;
            dest = target(root, r, n);
        }
        reserved.insert(key(&dest));
        if key(&dest) != key(&r.source) {
            moves.push(PlannedMove { item_id: r.item_id, source: r.source.clone(), dest });
        }
    }
    moves
}

/// `dest` if `taken` says it is free, otherwise the same name with the next free number:
/// "Sunset.jpg" → "Sunset 2.jpg", and "Sunset 2.jpg" → "Sunset 3.jpg".
pub fn next_free(dest: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(dest) {
        return dest.to_string();
    }
    let dir = parent(dest);
    let file = file_name(dest);
    let (stem, ext) = match file.rfind('.') {
        Some(i) if i > 0 => (&file[..i], &file[i..]),
        _ => (file, ""),
    };
    let numbered = Regex::new(r"^(.*) ([2-9]|[1-9][0-9]+)$").unwrap();
    let (base, mut n) = match numbered.captures(stem) {
        Some(c) => (c[1].to_string(), c[2].parse::<u64>().unwrap_or(1) + 1),
        None => (stem.to_string(), 2),
    };
    loop {
        let candidate = format!("{dir}/{base} {n}{ext}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LeftOutReason {
    Failed,
    Missing,
    NoCategory,
}

/// An item Bin Files leaves where it is; the report lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeftOut {
    pub item_id: i64,
    pub reason: LeftOutReason,
    pub name: String,
    pub path: String,
    /// A Failed item's failure code.
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedStep {
    pub item_id: i64,
    pub source: String,
    pub dest: String,
    /// Another project also holds the source: copy it and leave the original.
    pub keep_source: bool,
    /// The destination got a " 2"-style number because its plain name was taken.
    pub numbered: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BinPlan {
    pub steps: Vec<PlannedStep>,
    /// Items found at their target but not yet marked binned: marked, never moved.
    pub in_place: Vec<i64>,
    /// Items whose file is gone: marked Missing, left out, never blocking.
    pub became_missing: Vec<i64>,
}

fn extension_of(item: &BinItem) -> String {
    let from_original = crate::paths::extension(&item.original_name);
    let ext = if from_original.is_empty() { crate::paths::extension(&item.current_path) } else { from_original };
    if ext.is_empty() {
        String::new()
    } else {
        format!(".{ext}")
    }
}

/// Decides, by path rather than status, which items move, which are already in place, and which
/// stay where they are. `only` limits the plan to these items (a Save).
pub fn plan_bin(
    project_dir: &str,
    items: &[BinItem],
    category_names: &HashMap<i64, String>,
    existing_paths: &HashSet<String>,
    exists: impl Fn(&str) -> bool,
    shared_paths: &[String],
    only: Option<&HashSet<i64>>,
) -> BinPlan {
    let mut plan = BinPlan::default();
    let mut requests = Vec::new();
    let mut sorted: Vec<&BinItem> = items.iter().collect();
    sorted.sort_by_key(|i| i.id);
    for item in sorted {
        if only.is_some_and(|o| !o.contains(&item.id)) {
            continue;
        }
        let category = item.category_id.and_then(|id| category_names.get(&id));
        let movable = matches!(item.status, ItemStatus::Sorted | ItemStatus::Binned);
        let Some(category) = category.filter(|_| movable) else { continue };
        if !exists(&item.current_path) {
            plan.became_missing.push(item.id);
            continue;
        }
        let request = MoveRequest {
            item_id: item.id,
            source: item.current_path.clone(),
            folder: category.clone(),
            base_name: item.friendly_name.clone().unwrap_or_default(),
            extension: extension_of(item),
        };
        if is_in_place(project_dir, &request) {
            if item.status != ItemStatus::Binned {
                plan.in_place.push(item.id);
            }
        } else {
            requests.push(request);
        }
    }
    let shared: HashSet<String> = shared_paths.iter().map(|p| key(p)).collect();
    let by_id: HashMap<i64, &MoveRequest> = requests.iter().map(|r| (r.item_id, r)).collect();
    plan.steps = plan_moves(project_dir, &requests, existing_paths)
        .into_iter()
        .map(|m| PlannedStep {
            numbered: key(&m.dest) != key(&plain_target(project_dir, by_id[&m.item_id])),
            keep_source: shared.contains(&key(&m.source)),
            item_id: m.item_id,
            source: m.source,
            dest: m.dest,
        })
        .collect();
    plan
}

// ---------------------------------------------------------------------------------------------
// The folder check
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct LayoutProblems {
    /// Binned items whose file isn't where the database says.
    pub missing: Vec<String>,
    /// Files in a category folder that no item points to.
    pub unexpected: Vec<String>,
}

impl LayoutProblems {
    pub fn ok(&self) -> bool {
        self.missing.is_empty() && self.unexpected.is_empty()
    }
}

/// Every file under each category folder.
pub fn category_files(project_dir: &str, category_names: &[String]) -> Vec<String> {
    let root = project_dir.trim_end_matches('/');
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for name in category_names {
        let folder = folder_for(name);
        if seen.insert(key(&folder)) {
            out.extend(files::list_files(&format!("{root}/{folder}")));
        }
    }
    out
}

/// The category folders must hold the binned items' files and nothing unknown. `items` are
/// (path, binned); `removed` are files the user removed from the project or chose to leave.
/// Hidden files (a name starting with ".") are never media and are ignored.
pub fn folder_check(project_dir: &str, category_names: &[String], items: &[(String, bool)], removed: &[String], files_on_disk: &[String]) -> LayoutProblems {
    let root = format!("{}/", project_dir.trim_end_matches('/'));
    let root_key = key(&root);
    let folders: HashSet<String> = category_names.iter().map(|n| key(&folder_for(n))).collect();
    let in_category_folder = |path: &str| -> bool {
        let k = key(path);
        let Some(rel) = k.strip_prefix(&root_key) else { return false };
        if rel.split('/').any(|seg| seg.starts_with('.')) {
            return false;
        }
        match rel.split_once('/') {
            Some((first, _)) => !first.is_empty() && folders.contains(first),
            None => false,
        }
    };
    let disk: Vec<&String> = files_on_disk.iter().filter(|p| in_category_folder(p)).collect();
    let disk_keys: HashSet<String> = disk.iter().map(|p| key(p)).collect();
    let binned: Vec<&String> = items.iter().filter(|(_, b)| *b).map(|(p, _)| p).collect();
    let known: HashSet<String> = items.iter().map(|(p, _)| key(p)).chain(removed.iter().map(|p| key(p))).collect();
    let mut missing: Vec<String> = binned.iter().filter(|p| !disk_keys.contains(&key(p))).map(|p| p.to_string()).collect();
    let mut unexpected: Vec<String> = disk.iter().filter(|p| !known.contains(&key(p))).map(|p| p.to_string()).collect();
    missing.sort();
    unexpected.sort();
    LayoutProblems { missing, unexpected }
}

// ---------------------------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Go,
    Pause,
    Stop,
}

/// Pause and Stop for a running move. The engine reads it only between files, so the file in
/// progress always finishes first.
pub struct MoveControl {
    state: Mutex<Request>,
    changed: Condvar,
}

impl MoveControl {
    pub fn new(start_paused: bool) -> Self {
        Self { state: Mutex::new(if start_paused { Request::Pause } else { Request::Go }), changed: Condvar::new() }
    }
    pub fn request(&self) -> Request {
        *self.state.lock()
    }
    pub fn pause(&self) {
        let mut s = self.state.lock();
        if *s == Request::Go {
            *s = Request::Pause;
        }
        self.changed.notify_all();
    }
    pub fn resume(&self) {
        let mut s = self.state.lock();
        if *s == Request::Pause {
            *s = Request::Go;
        }
        self.changed.notify_all();
    }
    pub fn stop(&self) {
        *self.state.lock() = Request::Stop;
        self.changed.notify_all();
    }
    /// Blocks while paused; returns the request that ended the pause.
    fn wait_while_paused(&self) -> Request {
        let mut s = self.state.lock();
        while *s == Request::Pause {
            self.changed.wait(&mut s);
        }
        *s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MovePhase {
    Moving,
    Copying,
    Checking,
    Placing,
    DeletingOriginal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MovedFile {
    pub name: String,
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveProgress {
    /// `Bin` or `Save`.
    pub kind: String,
    /// Steps finished (in any final state).
    pub done: usize,
    pub total: usize,
    /// The file in progress, by its destination's file name; None between files.
    pub current: Option<String>,
    pub phase: Option<MovePhase>,
    pub paused: bool,
    pub from: Option<String>,
    pub to: Option<String>,
    /// The file a paused run moves next.
    pub next: Option<String>,
    /// The last files that moved, oldest first (at most two).
    pub recent: Vec<MovedFile>,
}

/// How a run ended, or why it didn't start.
#[derive(Debug, Clone, PartialEq)]
pub enum MoveOutcome {
    Done,
    /// The user pressed Stop; the file in progress finished first.
    Stopped,
    /// Not enough free space for the next copy, or under the floor every run keeps.
    StorageFull(i64),
    /// Something the run can't carry on from (a rename's check failed).
    Failed(String),
    /// The check before found files no item points to; nothing was written or moved.
    Blocked(LayoutProblems),
    /// The plan had nothing to move; this many items were only marked binned.
    NothingToMove(usize),
}

enum StepResult {
    Next,
    Moved(String),
    Stop(MoveOutcome),
}

fn folder_of(path: &str) -> String {
    file_name(parent(path)).to_string()
}

fn moved_file(dest: &str) -> MovedFile {
    MovedFile { name: file_name(dest).to_string(), folder: folder_of(dest) }
}

/// Runs a written plan (`move_step` rows) one file at a time, never losing or overwriting a file.
///
/// - Same volume: rename, then check the file is at its target with the same size and gone from
///   the source. A failed check stops the run; an I/O error that leaves the source untouched
///   skips the file.
/// - Another volume, or a source another project holds: copy to `.binning/moving/<step>.tmp`
///   hashing the source, read the copy back (size and SHA-256), rename it into place, record it,
///   then delete the original (never for a shared source). A bad copy is retried once, then skipped.
/// - A step found part-way after a crash is finished from its recorded state; a step started a
///   third time without finishing is skipped.
/// - Pause and Stop act only between files.
struct Engine<'a> {
    db: &'a ProjectDb,
    root: String,
}

impl Engine<'_> {
    fn run(&self, run_id: i64, kind: &str, control: &MoveControl, on_progress: &dyn Fn(MoveProgress)) -> Result<MoveOutcome> {
        let steps = self.db.steps(run_id)?;
        let total = steps.len();
        let mut done = steps.iter().filter(|s| is_final(&s.state)).count();
        let mut recent: Vec<MovedFile> = steps.iter().filter(|s| s.state == "Done").map(|s| moved_file(&s.dest)).collect();
        if recent.len() > RECENT {
            recent.drain(..recent.len() - RECENT);
        }
        let progress = |done: usize, recent: &[MovedFile], step: Option<(&MoveStep, MovePhase)>, paused: bool, next: Option<String>| MoveProgress {
            kind: kind.to_string(),
            done,
            total,
            current: step.map(|(s, _)| file_name(&s.dest).to_string()),
            phase: step.map(|(_, p)| p),
            paused,
            from: step.map(|(s, _)| folder_of(&s.source)),
            to: step.map(|(s, _)| folder_of(&s.dest)),
            next,
            recent: recent.to_vec(),
        };
        for step in &steps {
            if is_final(&step.state) {
                continue;
            }
            // Between files: Stop, Pause and the space floor.
            if control.request() == Request::Pause {
                self.db.set_run_state(run_id, "Paused", None)?;
                on_progress(progress(done, &recent, None, true, Some(file_name(&step.dest).to_string())));
                if control.wait_while_paused() == Request::Stop {
                    return Ok(MoveOutcome::Stopped);
                }
                self.db.set_run_state(run_id, "Running", None)?;
            }
            if control.request() == Request::Stop {
                return Ok(MoveOutcome::Stopped);
            }
            let free = files::usable_space(&self.root);
            if free < MIN_FREE_BYTES {
                return Ok(MoveOutcome::StorageFull(MIN_FREE_BYTES - free));
            }
            let report = |s: &MoveStep, phase: MovePhase| on_progress(progress(done, &recent, Some((s, phase)), false, None));
            match self.run_step(step.clone(), &report)? {
                StepResult::Next => done += 1,
                StepResult::Moved(dest) => {
                    done += 1;
                    recent.push(moved_file(&dest));
                    if recent.len() > RECENT {
                        recent.remove(0);
                    }
                }
                StepResult::Stop(outcome) => return Ok(outcome),
            }
            on_progress(progress(done, &recent, None, false, None));
        }
        Ok(MoveOutcome::Done)
    }

    fn temp_for(&self, step: &MoveStep) -> String {
        format!("{}/.binning/moving/{}.tmp", self.root, step.id)
    }

    fn with_dest(&self, mut step: MoveStep, dest: String) -> Result<MoveStep> {
        self.db.set_step_dest(step.id, &dest)?;
        step.dest = dest;
        Ok(step)
    }

    fn run_step(&self, mut step: MoveStep, report: &dyn Fn(&MoveStep, MovePhase)) -> Result<StepResult> {
        let starts = self.db.start_step(step.id)?;
        let resumed = starts > 1;
        let temp = self.temp_for(&step);
        if step.state == "Verified" {
            return self.finish_verified(&step, report);
        }
        // Planned or Copied: an unchecked temp copy is never trusted.
        if files::exists(&temp) {
            files::delete(&temp);
        }
        if step.state == "Copied" {
            self.db.set_step_state(step.id, "Planned", None)?;
        }
        if starts > MAX_STARTS {
            self.db.set_step_state(step.id, "Skipped", Some(step_errors::INTERRUPTED))?;
            return Ok(StepResult::Next);
        }
        if !files::exists(&step.source) {
            // A rename that happened just before the app died, not yet recorded.
            if resumed && files::exists(&step.dest) && files::size(&step.dest) == step.bytes {
                let hash = files::sha256(&step.dest).ok();
                self.db.record_moved(step.id, step.item_id, &step.dest, hash.as_deref(), "Done")?;
                return Ok(StepResult::Moved(step.dest));
            }
            self.db.record_missing(step.id, step.item_id)?;
            return Ok(StepResult::Next);
        }
        if files::exists(&step.dest) && key(&step.dest) != key(&step.source) {
            let dest = next_free(&step.dest, files::exists);
            step = self.with_dest(step, dest)?;
        }
        if !step.keep_source && files::same_volume(&step.source, parent(&step.dest)) {
            self.rename(step, &temp, report)
        } else {
            self.copy(step, &temp, report)
        }
    }

    fn rename(&self, mut step: MoveStep, temp: &str, report: &dyn Fn(&MoveStep, MovePhase)) -> Result<StepResult> {
        let size = files::size(&step.source);
        files::mkdirs(parent(&step.dest));
        report(&step, MovePhase::Moving);
        let mut tries = 0;
        loop {
            match files::rename_no_replace(&step.source, &step.dest) {
                Ok(true) => break,
                Ok(false) => {
                    // Taken since the check a moment ago: take the next number, never overwrite.
                    tries += 1;
                    if tries < RENAME_TRIES {
                        let dest = next_free(&step.dest, files::exists);
                        step = self.with_dest(step, dest)?;
                        continue;
                    }
                    self.db.set_step_state(step.id, "Skipped", Some(step_errors::IO))?;
                    return Ok(StepResult::Next);
                }
                // The folder sits on another volume after all (a mounted folder or a junction).
                Err(e) if files::is_cross_device(&e) => return self.copy(step, temp, report),
                Err(e) => {
                    if files::exists(&step.source) && !files::exists(&step.dest) {
                        self.db.set_step_state(step.id, "Skipped", Some(step_errors::IO))?;
                        return Ok(StepResult::Next);
                    }
                    self.db.set_step_state(step.id, "Failed", Some(step_errors::RENAME_CHECK))?;
                    return Ok(StepResult::Stop(MoveOutcome::Failed(format!("Moving {} went wrong: {e}", file_name(&step.source)))));
                }
            }
        }
        report(&step, MovePhase::Checking);
        let same_file = key(&step.dest) == key(&step.source);
        let ok = files::exists(&step.dest) && files::size(&step.dest) == size && (same_file || !files::exists(&step.source));
        if !ok {
            self.db.set_step_state(step.id, "Failed", Some(step_errors::RENAME_CHECK))?;
            return Ok(StepResult::Stop(MoveOutcome::Failed(format!("{} wasn't where it should be after moving it", file_name(&step.source)))));
        }
        let hash = files::sha256(&step.dest).ok();
        self.db.record_moved(step.id, step.item_id, &step.dest, hash.as_deref(), "Done")?;
        Ok(StepResult::Moved(step.dest))
    }

    fn copy(&self, mut step: MoveStep, temp: &str, report: &dyn Fn(&MoveStep, MovePhase)) -> Result<StepResult> {
        let size = files::size(&step.source);
        let free = files::usable_space(&self.root);
        if free < size + COPY_MARGIN_BYTES {
            return Ok(StepResult::Stop(MoveOutcome::StorageFull(size + COPY_MARGIN_BYTES - free)));
        }
        files::mkdirs(parent(temp));
        let mut hash: Option<String> = None;
        for _ in 0..COPY_TRIES {
            report(&step, MovePhase::Copying);
            let source_hash = match files::copy_hashing(&step.source, temp) {
                Ok(h) => h,
                Err(e) if files::is_no_space(&e) => {
                    files::delete(temp);
                    self.db.set_step_state(step.id, "Planned", None)?;
                    return Ok(StepResult::Stop(MoveOutcome::StorageFull(size)));
                }
                Err(_) => {
                    files::delete(temp);
                    self.db.set_step_state(step.id, "Skipped", Some(step_errors::IO))?;
                    return Ok(StepResult::Next);
                }
            };
            files::set_last_modified(temp, files::last_modified(&step.source));
            self.db.set_step_state(step.id, "Copied", None)?;
            report(&step, MovePhase::Checking);
            if files::size(temp) == size && files::sha256(temp).ok().as_deref() == Some(source_hash.as_str()) {
                hash = Some(source_hash);
                break;
            }
            files::delete(temp);
            self.db.set_step_state(step.id, "Planned", None)?;
        }
        let Some(hash) = hash else {
            self.db.set_step_state(step.id, "Skipped", Some(step_errors::COPY_MISMATCH))?;
            return Ok(StepResult::Next);
        };
        report(&step, MovePhase::Placing);
        files::mkdirs(parent(&step.dest));
        let mut tries = 0;
        loop {
            match files::rename_no_replace(temp, &step.dest) {
                Ok(true) => break,
                Ok(false) if tries + 1 < RENAME_TRIES => {
                    tries += 1;
                    let dest = next_free(&step.dest, files::exists);
                    step = self.with_dest(step, dest)?;
                }
                _ => {
                    files::delete(temp);
                    self.db.set_step_state(step.id, "Skipped", Some(step_errors::IO))?;
                    return Ok(StepResult::Next);
                }
            }
        }
        self.db.record_moved(step.id, step.item_id, &step.dest, Some(&hash), "Verified")?;
        step.state = "Verified".into();
        self.finish_verified(&step, report)
    }

    /// The file is at its destination and recorded: delete the original (unless shared) and finish.
    fn finish_verified(&self, step: &MoveStep, report: &dyn Fn(&MoveStep, MovePhase)) -> Result<StepResult> {
        if !files::exists(&step.dest) {
            // Gone since it was recorded; never delete the only copy left.
            if files::exists(&step.source) {
                self.db.record_reverted(step.id, step.item_id, &step.source, step_errors::IO)?;
            } else {
                self.db.record_missing(step.id, step.item_id)?;
            }
            return Ok(StepResult::Next);
        }
        if !step.keep_source && files::exists(&step.source) {
            report(step, MovePhase::DeletingOriginal);
            files::delete(&step.source);
        }
        self.db.set_step_state(step.id, "Done", None)?;
        Ok(StepResult::Moved(step.dest.clone()))
    }
}

// ---------------------------------------------------------------------------------------------
// The job
// ---------------------------------------------------------------------------------------------

/// A folder files move out of, for the confirm sheet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BinSource {
    pub folder: String,
    pub path: String,
    pub count: usize,
}

/// What a Bin Files would do now, for the confirm sheet.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinPreview {
    /// Files the run would move or copy.
    pub files: usize,
    /// Category folders they would go into.
    pub folders: usize,
    /// Folders on this drive, largest first; these files are renamed into place.
    pub sources: Vec<BinSource>,
    /// Files on another drive: copied, checked, then deleted.
    pub other_storage: usize,
    /// Files another project also holds: copied, and the original stays.
    pub shared: usize,
    pub copy_bytes: i64,
    pub free_bytes: i64,
    /// Sorted items already at their target.
    pub in_place: usize,
    /// Failed and Missing items, which stay where they are and don't block.
    pub left_out: Vec<LeftOut>,
}

/// Which of these paths another listed project also holds: copied, never moved.
pub type SharedLookup<'a> = &'a dyn Fn(&[String]) -> Vec<String>;

/// Bin Files from start to end: the folder check, the plan (written in one transaction before
/// anything moves), the engine, removing empty folders the app made, the check again, and the
/// run's end.
pub struct BinFilesJob<'a> {
    pub db: &'a ProjectDb,
    root: String,
    shared: SharedLookup<'a>,
}

fn names_map(names: &[(i64, String)]) -> HashMap<i64, String> {
    names.iter().cloned().collect()
}

fn names_list(names: &[(i64, String)]) -> Vec<String> {
    names.iter().map(|(_, n)| n.clone()).collect()
}

fn movable(items: &[BinItem]) -> Vec<&BinItem> {
    items.iter().filter(|i| matches!(i.status, ItemStatus::Sorted | ItemStatus::Binned)).collect()
}

fn left_out_of(items: &[BinItem], gone_now: &[i64]) -> Vec<LeftOut> {
    items
        .iter()
        .filter_map(|i| {
            let reason = if i.status == ItemStatus::Failed {
                LeftOutReason::Failed
            } else if i.status == ItemStatus::Missing || gone_now.contains(&i.id) {
                LeftOutReason::Missing
            } else {
                return None;
            };
            Some(LeftOut { item_id: i.id, reason, name: file_name(&i.current_path).to_string(), path: i.current_path.clone(), failure: i.failure_reason.clone() })
        })
        .collect()
}

fn check_items(items: &[BinItem]) -> Vec<(String, bool)> {
    items.iter().map(|i| (i.current_path.clone(), i.status == ItemStatus::Binned)).collect()
}

fn binned_at(items: &[BinItem], missing_paths: &[String]) -> Vec<i64> {
    let keys: HashSet<String> = missing_paths.iter().map(|p| key(p)).collect();
    items.iter().filter(|i| i.status == ItemStatus::Binned && keys.contains(&key(&i.current_path))).map(|i| i.id).collect()
}

fn new_steps(plan: &BinPlan) -> Vec<NewStep> {
    plan.steps
        .iter()
        .map(|s| NewStep { item_id: s.item_id, source: s.source.clone(), dest: s.dest.clone(), bytes: files::size(&s.source), keep_source: s.keep_source, numbered: s.numbered })
        .collect()
}

impl<'a> BinFilesJob<'a> {
    pub fn new(db: &'a ProjectDb, project_dir: &str, shared: SharedLookup<'a>) -> Self {
        Self { db, root: project_dir.trim_end_matches('/').to_string(), shared }
    }

    /// What a Bin Files would do now. Writes and moves nothing.
    pub fn preview(&self) -> Result<BinPreview> {
        let names = self.db.category_names()?;
        let categories = names_map(&names);
        let items = self.db.bin_items()?;
        let on_disk: HashSet<String> = category_files(&self.root, &names_list(&names)).into_iter().collect();
        let movable_paths: Vec<String> = movable(&items).iter().map(|i| i.current_path.clone()).collect();
        let shared = (self.shared)(&movable_paths);
        let plan = plan_bin(&self.root, &items, &categories, &on_disk, files::exists, &shared, None);
        let (kept, moved): (Vec<&PlannedStep>, Vec<&PlannedStep>) = plan.steps.iter().partition(|s| s.keep_source);
        let (renames, other): (Vec<&PlannedStep>, Vec<&PlannedStep>) = moved.into_iter().partition(|s| files::same_volume(&s.source, &self.root));
        let mut by_folder: Vec<(String, usize)> = Vec::new();
        for s in &renames {
            let folder = parent(&s.source).to_string();
            match by_folder.iter_mut().find(|(f, _)| *f == folder) {
                Some(e) => e.1 += 1,
                None => by_folder.push((folder, 1)),
            }
        }
        by_folder.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let eligible = movable(&items).iter().filter(|i| i.category_id.is_some_and(|c| categories.contains_key(&c))).count();
        let mut folders = HashSet::new();
        for s in &plan.steps {
            folders.insert(key(parent(&s.dest)));
        }
        Ok(BinPreview {
            files: plan.steps.len(),
            folders: folders.len(),
            sources: by_folder.into_iter().map(|(f, count)| BinSource { folder: file_name(&f).to_string(), path: crate::paths::display(&f), count }).collect(),
            other_storage: other.len(),
            shared: kept.len(),
            copy_bytes: other.iter().chain(kept.iter()).map(|s| files::size(&s.source)).sum(),
            free_bytes: files::usable_space(&self.root),
            in_place: eligible.saturating_sub(plan.steps.len() + plan.became_missing.len()),
            left_out: left_out_of(&items, &plan.became_missing),
        })
    }

    fn check(&self, names: &[(i64, String)], items: &[BinItem], removed: &[String]) -> LayoutProblems {
        let list = names_list(names);
        folder_check(&self.root, &list, &check_items(items), removed, &category_files(&self.root, &list))
    }

    /// The folder check as it stands now, for the sheet that offers to add or leave unknown files.
    pub fn check_now(&self) -> Result<LayoutProblems> {
        Ok(self.check(&self.db.category_names()?, &self.db.bin_items()?, &self.db.removed_paths()?))
    }

    pub fn bin_files(&self, control: &MoveControl, on_progress: &dyn Fn(MoveProgress)) -> Result<MoveOutcome> {
        if let Some(open) = self.db.open_move_run()? {
            return self.resume(open.id, control, on_progress);
        }
        let names = self.db.category_names()?;
        let mut removed = self.db.removed_paths()?;
        let gone: Vec<String> = removed.iter().filter(|p| !files::exists(p)).cloned().collect();
        if !gone.is_empty() {
            self.db.drop_removed(&gone)?;
            removed.retain(|p| !gone.contains(p));
        }
        let mut items = self.db.bin_items()?;
        let before = self.check(&names, &items, &removed);
        let missing_binned = binned_at(&items, &before.missing);
        if !missing_binned.is_empty() {
            self.db.mark_items(&[], &missing_binned)?;
            items = self.db.bin_items()?;
        }
        if !before.unexpected.is_empty() {
            return Ok(MoveOutcome::Blocked(before));
        }
        let on_disk: HashSet<String> = category_files(&self.root, &names_list(&names)).into_iter().collect();
        let movable_paths: Vec<String> = movable(&items).iter().map(|i| i.current_path.clone()).collect();
        let shared = if movable_paths.is_empty() { Vec::new() } else { (self.shared)(&movable_paths) };
        let plan = plan_bin(&self.root, &items, &names_map(&names), &on_disk, files::exists, &shared, None);
        let steps = new_steps(&plan);
        let run_id = self.db.tx(|t| db::write_run(t, "Bin", &steps, &plan.in_place, &plan.became_missing, true))?;
        if plan.steps.is_empty() {
            self.db.finish_move_run(run_id, "Done", None, &[], Some(true))?;
            return Ok(MoveOutcome::NothingToMove(plan.in_place.len()));
        }
        self.execute(run_id, "Bin", control, on_progress)
    }

    /// A Save's plan: for those of `ids` that are binned and whose target changed, writes a run to
    /// start after the edit commits, and returns it. None when no file has to move. Call in the
    /// edit's transaction (`c`), so the edit and its plan are one.
    pub fn plan_save(&self, c: &rusqlite::Connection, ids: &[i64]) -> Result<Option<i64>> {
        let items = db::bin_items(c)?;
        let binned: HashSet<i64> = items.iter().filter(|i| ids.contains(&i.id) && i.status == ItemStatus::Binned).map(|i| i.id).collect();
        if binned.is_empty() {
            return Ok(None);
        }
        let names = db::category_names(c)?;
        let on_disk: HashSet<String> = category_files(&self.root, &names_list(&names)).into_iter().collect();
        let paths: Vec<String> = items.iter().filter(|i| binned.contains(&i.id)).map(|i| i.current_path.clone()).collect();
        let shared = (self.shared)(&paths);
        let plan = plan_bin(&self.root, &items, &names_map(&names), &on_disk, files::exists, &shared, Some(&binned));
        if plan.steps.is_empty() {
            return Ok(None);
        }
        let run_id = db::write_run(c, "Save", &new_steps(&plan), &[], &plan.became_missing, false)?;
        // A Save stopped half way must not hide behind "All files binned".
        let ids: Vec<i64> = plan.steps.iter().map(|s| s.item_id).collect();
        db::mark_sorted(c, &ids)?;
        Ok(Some(run_id))
    }

    /// Finishes the open run; a paused one waits for `control` to resume.
    pub fn resume(&self, run_id: i64, control: &MoveControl, on_progress: &dyn Fn(MoveProgress)) -> Result<MoveOutcome> {
        let Some(run) = self.db.move_run(run_id)? else { return Ok(MoveOutcome::Failed("The run to resume is gone".into())) };
        let kind = if run.kind.eq_ignore_ascii_case("Save") { "Save" } else { "Bin" };
        self.execute(run_id, kind, control, on_progress)
    }

    fn execute(&self, run_id: i64, kind: &str, control: &MoveControl, on_progress: &dyn Fn(MoveProgress)) -> Result<MoveOutcome> {
        let engine = Engine { db: self.db, root: self.root.clone() };
        let outcome = engine.run(run_id, kind, control, on_progress)?;
        let removed_folders = self.remove_empty_folders(run_id)?;
        let mut check_ok = None;
        if !matches!(outcome, MoveOutcome::Failed(_)) {
            let names = self.db.category_names()?;
            let items = self.db.bin_items()?;
            let after = self.check(&names, &items, &self.db.removed_paths()?);
            let missing = binned_at(&items, &after.missing);
            if !missing.is_empty() {
                self.db.mark_items(&[], &missing)?;
            }
            check_ok = Some(after.ok());
        }
        let (state, message) = match &outcome {
            MoveOutcome::Done => ("Done", None),
            MoveOutcome::Failed(m) => ("Failed", Some(m.clone())),
            MoveOutcome::StorageFull(n) => ("Stopped", Some(format!("Storage is full. Free up {} and press Bin Files again.", format_bytes(*n)))),
            _ => ("Stopped", None),
        };
        self.db.finish_move_run(run_id, state, message.as_deref(), &removed_folders, check_ok)?;
        Ok(outcome)
    }

    /// A folder directly inside the project that this run emptied is removed only if the app made
    /// it (a finished step once put a file there, or it is a `Copied from` folder) and nothing at
    /// all is left inside.
    fn remove_empty_folders(&self, run_id: i64) -> Result<Vec<String>> {
        let mut emptied: Vec<String> = Vec::new();
        for s in self.db.steps(run_id)? {
            if s.state != "Done" || s.keep_source {
                continue;
            }
            let folder = parent(&s.source).to_string();
            if key(parent(&folder)) == key(&self.root) && !folder.ends_with("/.binning") && !emptied.contains(&folder) {
                emptied.push(folder);
            }
        }
        if emptied.is_empty() {
            return Ok(emptied);
        }
        let made: HashSet<String> = self.db.binned_folders()?.iter().map(|f| key(f)).collect();
        emptied.sort();
        Ok(emptied.into_iter().filter(|f| made.contains(&key(f)) || file_name(f).starts_with(COPIED_FROM)).filter(|f| files::remove_empty_dir(f)).collect())
    }

    pub fn report(&self, run_id: i64) -> Result<Option<MoveReport>> {
        let Some(run) = self.db.move_run(run_id)? else { return Ok(None) };
        Ok(Some(MoveReport::of(&run, &self.db.steps(run_id)?, left_out_of(&self.db.bin_items()?, &[]))))
    }

    /// The newest run's report while the user hasn't closed it.
    pub fn unread_report(&self) -> Result<Option<MoveReport>> {
        match self.db.unread_move_run()? {
            Some(run) => self.report(run.id),
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFile {
    pub item_id: i64,
    pub name: String,
    pub reason: String,
}

/// What a finished or stopped run did. Built from the run and its steps, so it can be shown again
/// after a restart.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveReport {
    pub run_id: i64,
    pub kind: String,
    pub state: String,
    pub headline: String,
    pub message: Option<String>,
    /// Files moved (renamed or copied, original deleted).
    pub moved: usize,
    /// Files copied from a source another project holds, which stays.
    pub copied_shared: usize,
    /// Category folders that received a file.
    pub folders: usize,
    pub skipped: Vec<SkippedFile>,
    /// Destination names that got a " 2"-style number.
    pub numbered: Vec<String>,
    pub became_missing: usize,
    pub in_place: i64,
    pub last_moved: Option<String>,
    pub removed_folders: Vec<String>,
    pub still_to_move: usize,
    pub left_out: Vec<LeftOut>,
    pub check_ok: Option<bool>,
}

fn n_files(n: usize) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{n} files")
    }
}

impl MoveReport {
    pub fn of(run: &MoveRun, steps: &[MoveStep], left_out: Vec<LeftOut>) -> Self {
        let save = run.kind.eq_ignore_ascii_case("Save");
        let finished = run.state == "Done";
        let done: Vec<&MoveStep> = steps.iter().filter(|s| s.state == "Done").collect();
        let moved = done.iter().filter(|s| !s.keep_source).count();
        let shared = done.len() - moved;
        let folders = done.iter().map(|s| key(parent(&s.dest))).collect::<HashSet<_>>().len();
        let in_place = run.in_place.max(0) as usize;
        let headline = if finished && save {
            if shared == 0 {
                format!("Saved. {} moved and checked.", n_files(moved))
            } else if moved == 0 {
                format!("Saved. {} copied and checked.", n_files(shared))
            } else {
                format!("Saved. {} moved and {shared} copied, all checked.", n_files(moved))
            }
        } else if finished && done.is_empty() && in_place > 0 {
            format!("Nothing to move. {} {} already in place.", n_files(in_place), if in_place == 1 { "was" } else { "were" })
        } else if finished && done.is_empty() {
            "Nothing to move.".into()
        } else if finished {
            format!("{} binned into {folders} category {}", n_files(done.len()), if folders == 1 { "folder" } else { "folders" })
        } else if save {
            "Saving stopped".into()
        } else {
            "Bin Files stopped".into()
        };
        MoveReport {
            run_id: run.id,
            kind: if save { "Save".into() } else { "Bin".into() },
            state: run.state.clone(),
            headline,
            message: run.message.clone(),
            moved,
            copied_shared: shared,
            folders,
            skipped: steps
                .iter()
                .filter(|s| s.state == "Skipped")
                .map(|s| SkippedFile { item_id: s.item_id, name: file_name(&s.dest).to_string(), reason: s.error.clone().unwrap_or_else(|| step_errors::IO.into()) })
                .collect(),
            numbered: done.iter().filter(|s| s.numbered).map(|s| file_name(&s.dest).to_string()).collect(),
            became_missing: steps.iter().filter(|s| s.state == "Missing").count(),
            in_place: run.in_place,
            last_moved: done.last().map(|s| file_name(&s.dest).to_string()),
            removed_folders: run.removed_folders.as_deref().unwrap_or("").lines().filter(|l| !l.trim().is_empty()).map(|l| file_name(l).to_string()).collect(),
            still_to_move: steps.iter().filter(|s| !is_final(&s.state)).count(),
            left_out,
            check_ok: run.check_ok,
        }
    }
}

/// "3.1 GB", "200 MB", "12 KB".
pub fn format_bytes(bytes: i64) -> String {
    let units = [("KB", 1024.0), ("MB", 1024.0 * 1024.0), ("GB", 1024.0 * 1024.0 * 1024.0)];
    let (unit, size) = units.iter().rev().find(|(_, s)| bytes as f64 >= *s).copied().unwrap_or(units[0]);
    let value = bytes as f64 / size;
    if value >= 100.0 || value == value.floor() {
        format!("{:.0} {unit}", value.round().max(1.0))
    } else {
        format!("{value:.1} {unit}")
    }
}

#[cfg(test)]
#[path = "moves_android_cases.rs"]
mod android_cases;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::ItemKind;
    use crate::db::{NewItem, Profile};
    use crate::paths::norm;
    use crate::sorting::{Assignment, NewCategoryEntity};
    use std::fs;

    fn req(id: i64, source: &str, folder: &str, base: &str) -> MoveRequest {
        MoveRequest { item_id: id, source: source.into(), folder: folder.into(), base_name: base.into(), extension: "jpg".into() }
    }

    #[test]
    fn plans_without_overwriting() {
        let existing: HashSet<String> = ["P:/proj/Sunsets/Beach.jpg".to_string()].into_iter().collect();
        let reqs = vec![req(1, "P:/cam/a.jpg", "Sunsets", "Beach"), req(2, "P:/cam/b.jpg", "Sunsets", "beach"), req(3, "P:/proj/sunsets/Dusk 2.jpg", "Sunsets", "Dusk")];
        let moves = plan_moves("P:/proj", &reqs, &existing);
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].dest, "P:/proj/Sunsets/Beach 2.jpg");
        assert_eq!(moves[1].dest, "P:/proj/Sunsets/beach 3.jpg");
        assert!(is_in_place("P:/proj", &reqs[2]));
        assert_eq!(next_free("a/Sunset 2.jpg", |p| p != "a/Sunset 4.jpg"), "a/Sunset 4.jpg");
        assert_eq!(next_free("a/Sunset.jpg", |p| p == "a/Sunset.jpg"), "a/Sunset 2.jpg");
        assert_eq!(folder_for("  A/B: c. "), "A_B_ c");
        assert_eq!(folder_for("..."), "Category");
    }

    #[test]
    fn folder_check_finds_unknown_files() {
        let cats = vec!["Sunsets".to_string()];
        let items = vec![("P:/proj/Sunsets/a.jpg".to_string(), true), ("P:/proj/Sunsets/gone.jpg".to_string(), true), ("P:/proj/sunsets/waiting.jpg".to_string(), false)];
        let disk: Vec<String> =
            ["P:/proj/Sunsets/a.jpg", "P:/proj/Sunsets/waiting.jpg", "P:/proj/Sunsets/stray.jpg", "P:/proj/Sunsets/left.jpg", "P:/proj/Sunsets/.thumbs/x.jpg", "P:/proj/Other/y.jpg"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let p = folder_check("P:/proj", &cats, &items, &["P:/proj/Sunsets/LEFT.jpg".to_string()], &disk);
        assert_eq!(p.missing, vec!["P:/proj/Sunsets/gone.jpg"]);
        assert_eq!(p.unexpected, vec!["P:/proj/Sunsets/stray.jpg"]);
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(200 * 1024 * 1024), "200 MB");
        assert_eq!(format_bytes(3_328_599_654), "3.1 GB");
        assert_eq!(format_bytes(10), "0.0 KB");
    }

    /// A project folder with media in a camera folder, analyzed and sorted into categories.
    fn sorted_project(names: &[(&str, &str, &str)]) -> (tempfile::TempDir, String, ProjectDb, Vec<i64>) {
        let dir = tempfile::tempdir().unwrap();
        let root = norm(dir.path().join("proj"));
        let cam = norm(dir.path().join("camera"));
        fs::create_dir_all(&cam).unwrap();
        let db = ProjectDb::open(std::path::Path::new(&format!("{root}/.binning/project.db")), "Test").unwrap();
        let mut ids = Vec::new();
        for (file, friendly, category) in names {
            let path = format!("{cam}/{file}");
            fs::write(&path, format!("bytes of {file}")).unwrap();
            let id = db
                .insert_item(&NewItem { content_hash: files::sha256(&path).unwrap(), path, name: file.to_string(), kind: ItemKind::Photo, taken_at: None, duration_ms: None, width: None, height: None })
                .unwrap();
            let r = crate::analysis::AnalysisResult { description: "d".into(), actions: vec![], tags: vec![], suggested_name: friendly.to_string(), shot_type: "wide".into() };
            db.save_analysis(id, &r, "{}", "m", "v", &Profile::default()).unwrap();
            db.apply_chunk(
                &[NewCategoryEntity { name: category.to_string(), description: String::new() }],
                &[Assignment { item_id: id, category_name: category.to_string(), friendly_name: friendly.to_string() }],
            )
            .unwrap();
            ids.push(id);
        }
        (dir, root, db, ids)
    }

    fn none(_: &[String]) -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn bins_files_into_category_folders() {
        let (_d, root, db, ids) = sorted_project(&[("IMG_1.jpg", "Beach Sunset", "Sunsets"), ("IMG_2.JPG", "Beach Sunset", "Sunsets"), ("IMG_3.jpg", "Sea Turtle", "Animals")]);
        let job = BinFilesJob::new(&db, &root, &none);
        let preview = job.preview().unwrap();
        assert_eq!((preview.files, preview.folders, preview.sources.len()), (3, 2, 1));
        assert_eq!(preview.sources[0].count, 3);
        let seen = std::cell::RefCell::new(Vec::new());
        let outcome = job.bin_files(&MoveControl::new(false), &|p| seen.borrow_mut().push(p)).unwrap();
        assert_eq!(outcome, MoveOutcome::Done);
        assert!(files::exists(&format!("{root}/Sunsets/Beach Sunset.jpg")));
        assert!(files::exists(&format!("{root}/Sunsets/Beach Sunset 2.JPG")));
        assert!(files::exists(&format!("{root}/Animals/Sea Turtle.jpg")));
        let item = db.item(ids[1]).unwrap().unwrap();
        assert_eq!(item.status, ItemStatus::Binned);
        assert_eq!(item.current_path, format!("{root}/Sunsets/Beach Sunset 2.JPG"));
        assert_eq!(seen.borrow().last().unwrap().done, 3);
        let report = job.unread_report().unwrap().unwrap();
        assert_eq!(report.headline, "3 files binned into 2 category folders");
        assert_eq!(report.numbered, vec!["Beach Sunset 2.JPG"]);
        assert_eq!(report.check_ok, Some(true));
        // A second run has nothing to do.
        assert_eq!(job.bin_files(&MoveControl::new(false), &|_| {}).unwrap(), MoveOutcome::NothingToMove(0));
    }

    #[test]
    fn unknown_file_blocks_and_leave_unblocks() {
        let (_d, root, db, _ids) = sorted_project(&[("IMG_1.jpg", "Beach Sunset", "Sunsets")]);
        fs::create_dir_all(format!("{root}/Sunsets")).unwrap();
        fs::write(format!("{root}/Sunsets/stray.jpg"), b"x").unwrap();
        let job = BinFilesJob::new(&db, &root, &none);
        match job.bin_files(&MoveControl::new(false), &|_| {}).unwrap() {
            MoveOutcome::Blocked(p) => {
                assert_eq!(p.unexpected.len(), 1);
                db.add_removed(&p.unexpected, "left").unwrap();
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(job.bin_files(&MoveControl::new(false), &|_| {}).unwrap(), MoveOutcome::Done);
    }

    #[test]
    fn stop_then_resume_finishes_the_run() {
        let (_d, root, db, _ids) = sorted_project(&[("a.jpg", "One", "Things"), ("b.jpg", "Two", "Things"), ("c.jpg", "Three", "Things")]);
        let job = BinFilesJob::new(&db, &root, &none);
        let control = MoveControl::new(false);
        let outcome = job
            .bin_files(&control, &|p| {
                if p.done == 1 {
                    control.stop();
                }
            })
            .unwrap();
        assert_eq!(outcome, MoveOutcome::Stopped);
        let report = job.unread_report().unwrap().unwrap();
        assert_eq!((report.headline.as_str(), report.moved, report.still_to_move), ("Bin Files stopped", 1, 2));
        // The stopped run is finished, so Bin Files plans the rest again.
        assert_eq!(job.bin_files(&MoveControl::new(false), &|_| {}).unwrap(), MoveOutcome::Done);
        assert_eq!(files::list_files(&format!("{root}/Things")).len(), 3);
    }

    #[test]
    fn shared_source_is_copied_and_kept() {
        let (d, root, db, ids) = sorted_project(&[("a.jpg", "One", "Things")]);
        let source = db.item(ids[0]).unwrap().unwrap().current_path;
        let shared = move |paths: &[String]| paths.to_vec();
        let job = BinFilesJob::new(&db, &root, &shared);
        assert_eq!(job.preview().unwrap().shared, 1);
        assert_eq!(job.bin_files(&MoveControl::new(false), &|_| {}).unwrap(), MoveOutcome::Done);
        assert!(files::exists(&source));
        assert!(files::exists(&format!("{root}/Things/One.jpg")));
        assert_eq!(job.unread_report().unwrap().unwrap().copied_shared, 1);
        assert!(files::list_files(&format!("{root}/.binning/moving")).is_empty());
        drop(d);
    }

    #[test]
    fn save_moves_a_binned_file_after_a_rename() {
        let (_d, root, db, ids) = sorted_project(&[("a.jpg", "One", "Things")]);
        let job = BinFilesJob::new(&db, &root, &none);
        assert_eq!(job.bin_files(&MoveControl::new(false), &|_| {}).unwrap(), MoveOutcome::Done);
        let run = db
            .tx(|t| {
                t.execute("UPDATE item SET friendlyName = 'Uno' WHERE id = ?1", [ids[0]])?;
                job.plan_save(t, &ids)
            })
            .unwrap()
            .unwrap();
        assert_eq!(db.item(ids[0]).unwrap().unwrap().status, ItemStatus::Sorted);
        assert_eq!(job.resume(run, &MoveControl::new(false), &|_| {}).unwrap(), MoveOutcome::Done);
        assert!(files::exists(&format!("{root}/Things/Uno.jpg")));
        assert!(!files::exists(&format!("{root}/Things/One.jpg")));
        assert_eq!(job.report(run).unwrap().unwrap().headline, "Saved. 1 file moved and checked.");
    }
}
