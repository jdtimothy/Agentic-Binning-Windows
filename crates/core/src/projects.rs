//! Projects: a project is a folder with a hidden `.binning` folder holding its database and
//! thumbnails. This module keeps the list of projects loaded into the app, decides whether a
//! picked folder may be opened, and registers media files as items.

use crate::analysis::ItemKind;
use crate::db::{self, DatabaseFacts, NewItem, ProjectDb, PROJECT_DB_VERSION};
use crate::error::{Error, Result};
use crate::files;
use crate::media::{self, Ffmpeg};
use crate::moves::{self, COPIED_FROM, COPY_MARGIN_BYTES, COPY_TRIES};
use crate::names::project_folder_name;
use crate::paths::{self, canonical, file_name, is_under, key, parent, same_path};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The hidden folder that holds the project's database and thumbnails.
pub const BINNING_DIR: &str = ".binning";

pub fn binning_dir(project_dir: &str) -> String {
    format!("{}/{BINNING_DIR}", project_dir.trim_end_matches('/'))
}

pub fn database_file(project_dir: &str) -> PathBuf {
    PathBuf::from(format!("{}/project.db", binning_dir(project_dir)))
}

pub fn thumbs_dir(project_dir: &str) -> String {
    format!("{}/thumbs", binning_dir(project_dir))
}

/// Marks a folder hidden in Explorer; on other systems the leading dot already hides it.
fn hide(path: &str) {
    #[cfg(windows)]
    {
        let _ = media::quiet_command("attrib").args(["+h", &paths::display(path)]).status();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RegistryData {
    #[serde(default)]
    projects: Vec<ProjectRef>,
    #[serde(default)]
    current: Option<String>,
}

/// A project taken off the list: enough to put it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedProject {
    pub project: ProjectRef,
    pub index: usize,
    pub was_current: bool,
}

/// The projects loaded into the app ("Current projects") plus the open one, persisted as JSON
/// (`{"projects":[...],"current":"<path>"}`). New projects are created under `projects_root`.
///
/// Reads are lenient: an unreadable file lists no projects. Writes are strict: they fail instead
/// of overwriting a file that exists but can't be parsed. Nothing here ever touches a project's
/// folder except [`ProjectRegistry::create`], which makes a new one.
pub struct ProjectRegistry {
    file: PathBuf,
    pub projects_root: String,
    lock: parking_lot::Mutex<()>,
}

impl ProjectRegistry {
    pub fn new(file: PathBuf, projects_root: String) -> Self {
        Self { file, projects_root, lock: parking_lot::Mutex::new(()) }
    }

    fn read_for_write(&self) -> Result<RegistryData> {
        if !self.file.exists() {
            return Ok(RegistryData::default());
        }
        let text = std::fs::read_to_string(&self.file).map_err(|_| Error::msg("Couldn't update the project list"))?;
        serde_json::from_str(&text).map_err(|_| Error::msg("Couldn't update the project list"))
    }

    fn read(&self) -> RegistryData {
        self.read_for_write().unwrap_or_default()
    }

    /// Writes to a temp file, then replaces.
    fn write(&self, data: &RegistryData) -> Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(data)?)?;
        std::fs::rename(&tmp, &self.file)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<ProjectRef> {
        let _g = self.lock.lock();
        self.read().projects
    }

    pub fn current(&self) -> Option<ProjectRef> {
        let _g = self.lock.lock();
        let data = self.read();
        data.projects.into_iter().find(|p| Some(&p.path) == data.current.as_ref())
    }

    /// The listed project at `path` (any spelling of it).
    pub fn find(&self, path: &str) -> Option<ProjectRef> {
        self.list().into_iter().find(|p| same_path(&p.path, path))
    }

    /// Makes `project` the open one, adding it to the list if no entry has the same folder. An
    /// existing entry keeps its own name and path. Returns the entry as stored.
    pub fn set_current(&self, project: &ProjectRef) -> Result<ProjectRef> {
        let _g = self.lock.lock();
        let mut data = self.read_for_write()?;
        let stored = data.projects.iter().find(|p| same_path(&p.path, &project.path)).cloned();
        let entry = match stored {
            Some(s) => s,
            None => {
                data.projects.push(project.clone());
                project.clone()
            }
        };
        data.current = Some(entry.path.clone());
        self.write(&data)?;
        Ok(entry)
    }

    /// Forgets the current project without removing it from the list.
    pub fn clear_current(&self) -> Result<()> {
        let _g = self.lock.lock();
        let mut data = self.read_for_write()?;
        if data.current.take().is_some() {
            self.write(&data)?;
        }
        Ok(())
    }

    /// Takes the project at `path` off the list. Only the list changes; the project's folder,
    /// database and media are never touched.
    pub fn remove(&self, path: &str) -> Result<Option<RemovedProject>> {
        let _g = self.lock.lock();
        let mut data = self.read_for_write()?;
        let Some(index) = data.projects.iter().position(|p| same_path(&p.path, path)) else { return Ok(None) };
        let project = data.projects.remove(index);
        let was_current = data.current.as_deref().is_some_and(|c| same_path(c, &project.path));
        if was_current {
            data.current = None;
        }
        self.write(&data)?;
        Ok(Some(RemovedProject { project, index, was_current }))
    }

    /// Undo for [`remove`](Self::remove): puts the entry back at its old position. Does nothing
    /// if the folder is listed again meanwhile. It does not make it current.
    pub fn restore(&self, removed: &RemovedProject) -> Result<()> {
        let _g = self.lock.lock();
        let mut data = self.read_for_write()?;
        if data.projects.iter().any(|p| same_path(&p.path, &removed.project.path)) {
            return Ok(());
        }
        let at = removed.index.min(data.projects.len());
        data.projects.insert(at, removed.project.clone());
        self.write(&data)
    }

    /// Creates a new project folder under the projects root and lists it.
    pub fn create(&self, name: &str) -> Result<ProjectRef> {
        let _g = self.lock.lock();
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(Error::msg("Give the project a name"));
        }
        let folder = project_folder_name(trimmed);
        if folder.is_empty() {
            return Err(Error::msg("That name has no characters a folder can use"));
        }
        let mut data = self.read_for_write()?;
        let dir = format!("{}/{folder}", self.projects_root.trim_end_matches('/'));
        if Path::new(&dir).exists() {
            return Err(Error::msg("A project folder with that name already exists"));
        }
        std::fs::create_dir_all(thumbs_dir(&dir))?;
        hide(&binning_dir(&dir));
        let project = ProjectRef { name: trimmed.to_string(), path: dir };
        data.projects.push(project.clone());
        self.write(&data)?;
        Ok(project)
    }

    /// The listed project whose folder is an ancestor of `path`.
    pub fn project_containing(&self, path: &str) -> Option<ProjectRef> {
        let target = canonical(path);
        self.list().into_iter().find(|p| is_under(&target, &canonical(&p.path)))
    }
}

/// Why Open project refused a folder. Every case leaves the disk untouched.
pub mod refusal {
    pub const NO_PROJECT: &str = "No project in this folder";
    pub const CANT_OPEN: &str = "This project can't be opened";
    pub const NEWER: &str = "Made with a newer app version. Update to open it.";
}

/// Where a picked folder stands relative to the listed projects.
#[derive(Debug, Clone, PartialEq)]
pub enum Placement {
    /// The folder is already listed: open that entry, add nothing.
    Listed(ProjectRef),
    /// The folder lies inside a listed project.
    Inside(ProjectRef),
    /// The folder contains a listed project.
    Contains(ProjectRef),
    Unlisted,
}

pub fn placement(dir: &str, listed: &[ProjectRef]) -> Placement {
    let target = canonical(dir);
    if let Some(p) = listed.iter().find(|p| key(&canonical(&p.path)) == key(&target)) {
        return Placement::Listed(p.clone());
    }
    if let Some(p) = listed.iter().find(|p| is_under(&target, &canonical(&p.path))) {
        return Placement::Inside(p.clone());
    }
    if let Some(p) = listed.iter().find(|p| is_under(&canonical(&p.path), &target)) {
        return Placement::Contains(p.clone());
    }
    Placement::Unlisted
}

/// Someone who picks the hidden `.binning` folder itself means its project.
pub fn project_dir_of(picked: &str) -> String {
    if file_name(picked) == BINNING_DIR {
        parent(picked).to_string()
    } else {
        picked.to_string()
    }
}

/// Whether a project database may be opened, from what the read-only probe found. Only a current
/// or an older schema with the known identity hash for that version opens; anything newer,
/// foreign or damaged is refused, untouched. `Ok(Some(v))` means back it up first.
pub fn decide(facts: &DatabaseFacts) -> std::result::Result<Option<i32>, &'static str> {
    if !facts.exists {
        return Err(refusal::NO_PROJECT);
    }
    if !facts.readable || !facts.quick_check_ok {
        return Err(refusal::CANT_OPEN);
    }
    let Some(version) = facts.user_version else { return Err(refusal::CANT_OPEN) };
    if version > PROJECT_DB_VERSION {
        return Err(refusal::NEWER);
    }
    if facts.identity_hash.is_none() || facts.identity_hash.as_deref() != db::known_hash(version) || !facts.has_meta {
        return Err(refusal::CANT_OPEN);
    }
    Ok((version < PROJECT_DB_VERSION).then_some(version))
}

/// Copies the database aside as `project.db.v<N>.bak` (or `.v<N>.<k>.bak`) before an upgrade,
/// which is one-way. Never an existing file.
pub fn backup_before_upgrade(db_file: &Path, version: i32) -> Result<PathBuf> {
    let name = db_file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "project.db".into());
    let dir = db_file.parent().unwrap_or(Path::new("."));
    let mut candidate = dir.join(format!("{name}.v{version}.bak"));
    let mut k = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{name}.v{version}.{k}.bak"));
        k += 1;
    }
    std::fs::copy(db_file, &candidate)?;
    Ok(candidate)
}

/// Opens the project in `dir`, backing up an older database first. With `existing_only`, a folder
/// with no project is refused instead of becoming one.
pub fn open_database(dir: &str, name: &str, existing_only: bool) -> Result<ProjectDb> {
    if !Path::new(dir).is_dir() {
        return Err(Error::msg(format!("Project folder is missing: {}", paths::display(dir))));
    }
    let file = database_file(dir);
    let facts = db::probe(&file);
    if facts.exists {
        if let Some(version) = decide(&facts).map_err(Error::msg)? {
            backup_before_upgrade(&file, version)?;
        }
    } else if existing_only {
        return Err(Error::msg(refusal::NO_PROJECT));
    }
    let opened = ProjectDb::open(&file, name)?;
    if !facts.exists {
        let _ = std::fs::create_dir_all(thumbs_dir(dir));
        hide(&binning_dir(dir));
    }
    Ok(opened)
}

// ---------------------------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------------------------

/// A file another project holds, left out of a batch until the user chooses to copy it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldFile {
    pub path: String,
    pub project: String,
    /// False for a project found by its folder's database, off Current projects.
    pub listed: bool,
}

/// Running totals of a registration run, rendered as the one-line summary in the project sheet.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationSummary {
    pub added: usize,
    pub duplicates: usize,
    pub unreadable: usize,
    /// Each file another project holds, for the notice that offers to copy them.
    pub held: Vec<HeldFile>,
    pub text: String,
}

fn grouped(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl RegistrationSummary {
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.added > 0 {
            parts.push(format!("{} added", grouped(self.added)));
        }
        if self.duplicates > 0 {
            parts.push(format!("{} already in this project", grouped(self.duplicates)));
        }
        if !self.held.is_empty() {
            let mut names: Vec<&str> = Vec::new();
            for h in &self.held {
                if !names.contains(&h.project.as_str()) {
                    names.push(&h.project);
                }
            }
            let which = if names.len() == 1 { "another project" } else { "other projects" };
            parts.push(format!("{} in {which} ({})", grouped(self.held.len()), names.join(", ")));
        }
        if self.unreadable > 0 {
            parts.push(format!("{} couldn't be read", grouped(self.unreadable)));
        }
        if parts.is_empty() {
            "No photos or videos found".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// Finds the project a file sits inside by looking for its database on disk, so a project taken
/// off Current projects still owns its folder. It only reads. One finder serves one scan and asks
/// each folder once.
#[derive(Default)]
struct ProjectFinder {
    known: HashMap<String, Option<(String, String)>>,
}

impl ProjectFinder {
    /// (folder, name) of the project containing `file`.
    fn project_containing(&mut self, file: &str) -> Option<(String, String)> {
        let mut walked = Vec::new();
        let mut dir = parent(file).to_string();
        let mut found = None;
        loop {
            if let Some(k) = self.known.get(&dir) {
                found = k.clone();
                break;
            }
            walked.push(dir.clone());
            let db_file = database_file(&dir);
            if db_file.is_file() {
                let name = db::probe(&db_file).meta_name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| file_name(&dir).to_string());
                found = Some((dir.clone(), name));
                break;
            }
            let up = parent(&dir).to_string();
            if up == dir || up.is_empty() || !up.contains('/') {
                break;
            }
            dir = up;
        }
        for w in walked {
            self.known.insert(w, found.clone());
        }
        found
    }
}

/// Another listed project's database, opened read-only to ask whether it holds a file.
pub struct OtherProject {
    pub name: String,
    conn: rusqlite::Connection,
}

impl OtherProject {
    pub fn open(project: &ProjectRef) -> Option<Self> {
        let file = database_file(&project.path);
        if !file.is_file() {
            return None;
        }
        let conn = rusqlite::Connection::open_with_flags(&file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        Some(Self { name: project.name.clone(), conn })
    }

    pub fn holds(&self, hash: Option<&str>, path: &str) -> bool {
        self.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM item WHERE contentHash = ?1 OR currentPath = ?2 COLLATE NOCASE)", rusqlite::params![hash.unwrap_or(""), path], |r| r.get::<_, bool>(0))
            .unwrap_or(false)
    }
}

/// Which of `paths` another listed project also holds: Bin Files copies those and leaves the original.
pub fn shared_with_other_projects(registry: &ProjectRegistry, current_dir: &str, paths: &[String]) -> Vec<String> {
    let others: Vec<OtherProject> = registry.list().iter().filter(|p| !same_path(&p.path, current_dir)).filter_map(OtherProject::open).collect();
    if others.is_empty() {
        return Vec::new();
    }
    paths.iter().filter(|p| others.iter().any(|o| o.holds(None, p))).cloned().collect()
}

/// Registers media files as Pending items of the open project. Files are only read, never moved,
/// renamed or deleted. `on_progress` gets (done, total) after each file; return false to stop.
pub fn register(
    project: &ProjectRef,
    db: &ProjectDb,
    registry: &ProjectRegistry,
    ffmpeg: &Ffmpeg,
    candidates: &[String],
    on_progress: &dyn Fn(usize, usize) -> bool,
) -> RegistrationSummary {
    let this_folder = canonical(&project.path);
    let others: Vec<OtherProject> = registry.list().iter().filter(|p| !same_path(&p.path, &project.path)).filter_map(OtherProject::open).collect();
    let mut finder = ProjectFinder::default();
    let mut summary = RegistrationSummary::default();
    for (index, path) in candidates.iter().enumerate() {
        let Some(kind) = media::kind_of(path) else {
            summary.unreadable += 1;
            continue;
        };
        match register_one(project, db, registry, ffmpeg, &others, &mut finder, &this_folder, path, kind) {
            Ok(Outcome::Added) => summary.added += 1,
            Ok(Outcome::Duplicate) => summary.duplicates += 1,
            Ok(Outcome::Held(name, listed)) => summary.held.push(HeldFile { path: path.clone(), project: name, listed }),
            Err(_) => summary.unreadable += 1,
        }
        if !on_progress(index + 1, candidates.len()) {
            break;
        }
    }
    summary.text = summary.describe();
    summary
}

enum Outcome {
    Added,
    Duplicate,
    Held(String, bool),
}

#[allow(clippy::too_many_arguments)]
fn register_one(
    _project: &ProjectRef,
    db: &ProjectDb,
    registry: &ProjectRegistry,
    ffmpeg: &Ffmpeg,
    others: &[OtherProject],
    finder: &mut ProjectFinder,
    this_folder: &str,
    path: &str,
    kind: ItemKind,
) -> Result<Outcome> {
    // 1. Inside another project's folder.
    if let Some(owner) = registry.project_containing(path) {
        if key(&canonical(&owner.path)) != key(this_folder) {
            return Ok(Outcome::Held(owner.name, true));
        }
    }
    if let Some((dir, name)) = finder.project_containing(path) {
        if key(&canonical(&dir)) != key(this_folder) {
            return Ok(Outcome::Held(name, false));
        }
    }
    // 2. Already in this project; the path is checked first so a known file is never re-hashed.
    if db.has_path(path)? {
        return Ok(Outcome::Duplicate);
    }
    let hash = files::sha256(path)?;
    if db.has_hash(&hash)? {
        return Ok(Outcome::Duplicate);
    }
    // 3. Held by another project.
    if let Some(o) = others.iter().find(|o| o.holds(Some(&hash), path)) {
        return Ok(Outcome::Held(o.name.clone(), true));
    }
    // 4. New.
    insert(db, ffmpeg, path, kind, hash)?;
    Ok(Outcome::Added)
}

fn insert(db: &ProjectDb, ffmpeg: &Ffmpeg, path: &str, kind: ItemKind, hash: String) -> Result<i64> {
    let meta = media::read_metadata(path, kind, ffmpeg);
    db.insert_item(&NewItem {
        content_hash: hash,
        path: path.to_string(),
        name: file_name(path).to_string(),
        kind,
        taken_at: meta.taken_at,
        duration_ms: meta.duration_ms,
        width: meta.width,
        height: meta.height,
    })
}

/// Adds files found in a category folder that no item points to (the folder check's "Add them").
/// Returns the paths added.
pub fn register_in_place(db: &ProjectDb, ffmpeg: &Ffmpeg, paths: &[String]) -> Vec<String> {
    let mut added = Vec::new();
    for path in paths {
        let Some(kind) = media::kind_of(path) else { continue };
        if !files::is_file(path) || db.has_path(path).unwrap_or(true) {
            continue;
        }
        let Ok(hash) = files::sha256(path) else { continue };
        if db.has_hash(&hash).unwrap_or(true) {
            continue;
        }
        if insert(db, ffmpeg, path, kind, hash).is_ok() {
            added.push(path.clone());
        }
    }
    added
}

/// What copying files another project holds came to.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedCopyResult {
    pub copied: usize,
    /// Copies that never matched their original, or couldn't be read or added: left out, nothing lost.
    pub failed: usize,
    /// Files this project already holds, not copied again.
    pub already_here: usize,
    /// Files not tried because storage ran out; `needed_bytes` says how much to free.
    pub not_tried: usize,
    pub needed_bytes: i64,
    /// The `Copied from …` folders files went into, in first-used order.
    pub folders: Vec<String>,
    pub text: String,
}

const TEMP_PREFIX: &str = "copy-";

/// Copies files another project holds into `<project>/Copied from <other>/`. It reads the
/// originals and never moves, renames or deletes them. Each copy is written to a temp file, read
/// back and compared by size and SHA-256, and only then put in place: nothing is overwritten.
pub fn copy_shared(project_dir: &str, db: &ProjectDb, ffmpeg: &Ffmpeg, held: &[HeldFile], on_progress: &dyn Fn(usize, usize)) -> SharedCopyResult {
    let root = project_dir.trim_end_matches('/');
    let moving = format!("{root}/{BINNING_DIR}/moving");
    // Unchecked temp files from a copy the app died in; never trusted.
    for f in files::list_files(&moving) {
        if file_name(&f).starts_with(TEMP_PREFIX) {
            files::delete(&f);
        }
    }
    let mut result = SharedCopyResult::default();
    for (index, file) in held.iter().enumerate() {
        match copy_one(root, &moving, db, ffmpeg, file, index) {
            CopyOutcome::Copied(folder) => {
                result.copied += 1;
                if !result.folders.contains(&folder) {
                    result.folders.push(folder);
                }
            }
            CopyOutcome::Failed => result.failed += 1,
            CopyOutcome::AlreadyHere => result.already_here += 1,
            CopyOutcome::NoSpace(needed) => {
                result.not_tried = held.len() - index;
                result.needed_bytes = needed;
                break;
            }
        }
        on_progress(index + 1, held.len());
    }
    let mut parts = Vec::new();
    if result.copied > 0 {
        parts.push(format!("{} copied into {}", grouped(result.copied), result.folders.join(", ")));
    }
    if result.already_here > 0 {
        parts.push(format!("{} already in this project", grouped(result.already_here)));
    }
    if result.failed > 0 {
        parts.push(format!("{} couldn't be copied", grouped(result.failed)));
    }
    if result.not_tried > 0 {
        parts.push(format!("{} not copied: free up {}", grouped(result.not_tried), moves::format_bytes(result.needed_bytes)));
    }
    result.text = if parts.is_empty() { "Nothing to copy".into() } else { parts.join(" · ") };
    result
}

enum CopyOutcome {
    Copied(String),
    Failed,
    AlreadyHere,
    NoSpace(i64),
}

fn copy_one(root: &str, moving: &str, db: &ProjectDb, ffmpeg: &Ffmpeg, file: &HeldFile, index: usize) -> CopyOutcome {
    let Some(kind) = media::kind_of(&file.path) else { return CopyOutcome::Failed };
    if !files::exists(&file.path) {
        return CopyOutcome::Failed;
    }
    let size = files::size(&file.path);
    let free = files::usable_space(root);
    if free < size + COPY_MARGIN_BYTES {
        return CopyOutcome::NoSpace(size + COPY_MARGIN_BYTES - free);
    }
    files::mkdirs(moving);
    let temp = format!("{moving}/{TEMP_PREFIX}{index}.tmp");
    files::delete(&temp);
    let mut checked: Option<String> = None;
    for _ in 0..COPY_TRIES {
        let hash = match files::copy_hashing(&file.path, &temp) {
            Ok(h) => h,
            Err(e) => {
                files::delete(&temp);
                return if files::is_no_space(&e) { CopyOutcome::NoSpace(size) } else { CopyOutcome::Failed };
            }
        };
        if db.has_hash(&hash).unwrap_or(false) {
            files::delete(&temp);
            return CopyOutcome::AlreadyHere;
        }
        if files::size(&temp) == size && files::sha256(&temp).ok().as_deref() == Some(hash.as_str()) {
            checked = Some(hash);
            break;
        }
        files::delete(&temp);
    }
    let Some(hash) = checked else { return CopyOutcome::Failed };
    files::set_last_modified(&temp, files::last_modified(&file.path));
    let folder = format!("{COPIED_FROM}{}", moves::folder_for(&file.project));
    files::mkdirs(&format!("{root}/{folder}"));
    let mut dest = moves::next_free(&format!("{root}/{folder}/{}", file_name(&file.path)), files::exists);
    let mut tries = 0;
    loop {
        match files::rename_no_replace(&temp, &dest) {
            Ok(true) => break,
            Ok(false) if tries < 4 => {
                tries += 1;
                dest = moves::next_free(&dest, files::exists);
            }
            _ => {
                files::delete(&temp);
                return CopyOutcome::Failed;
            }
        }
    }
    if insert(db, ffmpeg, &dest, kind, hash).is_err() {
        // Only this app's own checked copy: an unregistered file would only clutter the folder.
        files::delete(&dest);
        return CopyOutcome::Failed;
    }
    CopyOutcome::Copied(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::norm;

    fn setup() -> (tempfile::TempDir, ProjectRegistry, String) {
        let d = tempfile::tempdir().unwrap();
        let root = norm(d.path());
        let reg = ProjectRegistry::new(d.path().join("config/projects.json"), format!("{root}/Projects"));
        (d, reg, root)
    }

    fn photo(path: &str, shade: u8) {
        std::fs::create_dir_all(parent(path)).unwrap();
        image::RgbImage::from_pixel(8, 8, image::Rgb([shade, 10, 10])).save(path).unwrap();
    }

    #[test]
    fn registry_lists_creates_and_removes() {
        let (_d, reg, root) = setup();
        let a = reg.create(" Maui: 2026 ").unwrap();
        assert_eq!(a.name, "Maui: 2026");
        assert_eq!(a.path, format!("{root}/Projects/Maui_ 2026"));
        assert!(Path::new(&thumbs_dir(&a.path)).is_dir());
        assert!(reg.create("Maui: 2026").is_err());
        assert!(reg.create("  ").is_err());
        let b = reg.create("Second").unwrap();
        reg.set_current(&b).unwrap();
        assert_eq!(reg.current().unwrap().name, "Second");
        assert_eq!(placement(&format!("{}/sub", a.path), &reg.list()), Placement::Inside(a.clone()));
        assert_eq!(placement(&format!("{root}/Projects"), &reg.list()), Placement::Contains(a.clone()));
        let removed = reg.remove(&b.path).unwrap().unwrap();
        assert!(removed.was_current && reg.current().is_none());
        assert!(Path::new(&b.path).is_dir(), "removing only changes the list");
        reg.restore(&removed).unwrap();
        assert_eq!(reg.list().len(), 2);
        std::fs::write(&reg.file, "not json").unwrap();
        assert!(reg.list().is_empty());
        assert!(reg.set_current(&a).is_err(), "an unreadable list is never overwritten");
        assert_eq!(std::fs::read_to_string(&reg.file).unwrap(), "not json");
    }

    #[test]
    fn open_rules() {
        let (_d, reg, root) = setup();
        let a = reg.create("One").unwrap();
        assert!(open_database(&format!("{root}/nowhere"), "x", true).is_err());
        assert_eq!(open_database(&format!("{root}/Projects"), "x", true).err().unwrap().to_string(), refusal::NO_PROJECT);
        drop(open_database(&a.path, "One", false).unwrap());
        assert_eq!(decide(&db::probe(&database_file(&a.path))), Ok(None));
        // An older Android-made schema is backed up, then upgraded.
        let c = rusqlite::Connection::open(database_file(&a.path)).unwrap();
        c.execute_batch(&format!("PRAGMA user_version = 4; UPDATE room_master_table SET identity_hash = '{}';", db::known_hash(4).unwrap())).unwrap();
        drop(c);
        assert_eq!(decide(&db::probe(&database_file(&a.path))), Ok(Some(4)));
        drop(open_database(&a.path, "One", true).unwrap());
        assert!(Path::new(&format!("{}/project.db.v4.bak", binning_dir(&a.path))).is_file());
        // A newer or foreign one is refused.
        let c = rusqlite::Connection::open(database_file(&a.path)).unwrap();
        c.execute_batch("PRAGMA user_version = 9;").unwrap();
        drop(c);
        assert_eq!(open_database(&a.path, "One", true).err().unwrap().to_string(), refusal::NEWER);
        assert_eq!(project_dir_of("C:/a/.binning"), "C:/a");
    }

    #[test]
    fn registers_and_reports_duplicates_and_held_files() {
        let (_d, reg, root) = setup();
        let one = reg.create("One").unwrap();
        let two = reg.create("Two").unwrap();
        let db_one = open_database(&one.path, "One", false).unwrap();
        let db_two = open_database(&two.path, "Two", false).unwrap();
        let cam = format!("{root}/camera");
        photo(&format!("{cam}/a.png"), 10);
        photo(&format!("{cam}/b.png"), 20);
        photo(&format!("{cam}/copy-of-a.png"), 10);
        std::fs::write(format!("{cam}/broken.jpg"), b"").unwrap();
        photo(&format!("{}/inside.png", two.path), 30);
        let ff = Ffmpeg::default();
        let files = media::scan(&cam, &|_| false);
        let s = register(&one, &db_one, &reg, &ff, &files, &|_, _| true);
        assert_eq!((s.added, s.duplicates, s.unreadable), (3, 1, 0));
        assert_eq!(s.text, "3 added · 1 already in this project");
        let item = &db_one.items().unwrap()[0];
        assert_eq!((item.width, item.height), (Some(8), Some(8)));
        // The second project is offered the same files: held by One. A file inside Two's folder is Two's.
        let mut all = files.clone();
        all.push(format!("{}/inside.png", two.path));
        let s2 = register(&two, &db_two, &reg, &ff, &all, &|_, _| true);
        assert_eq!(s2.added, 1);
        assert_eq!(s2.held.len(), 4);
        assert!(s2.text.contains("4 in another project (One)"));
        let s3 = register(&one, &db_one, &reg, &ff, &[format!("{}/inside.png", two.path)], &|_, _| true);
        assert_eq!(s3.held[0].project, "Two");
        // Copy the held files into Two.
        let copied = copy_shared(&two.path, &db_two, &ff, &s2.held[..2], &|_, _| {});
        assert_eq!((copied.copied, copied.failed), (2, 0));
        assert_eq!(copied.folders, vec!["Copied from One"]);
        assert!(files::exists(&format!("{}/Copied from One/a.png", two.path)));
        assert!(files::exists(&format!("{cam}/a.png")), "the original stays");
        assert_eq!(shared_with_other_projects(&reg, &one.path, &[format!("{cam}/a.png")]).len(), 0);
        assert_eq!(grouped(1234567), "1,234,567");
    }
}
