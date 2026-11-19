//! The app behind the window: one object that owns the settings, the project list, the open
//! project and the pipeline, and answers every command the window sends ([`App::dispatch`]).
//! The window is told about changes through named events ([`Emit`]).

use crate::analysis::ItemKind;
use crate::db::{self, ItemStatus, MediaRow, ProjectDb};
use crate::error::{Error, Result};
use crate::geniex::{self, GenieCli, GenieStatus, GenieTextModel, GenieVisionModel, ModelSpec, LFM3_1_7B, LFM3_VL_2B};
use crate::geniex_serve::GenieServer;
use crate::media::{self, Ffmpeg, FrameGrabber, MediaPreparer};
use crate::models::{MockTextModel, MockVisionModel, TextModel, VisionModel, MOCK_VISION_ID};
use crate::moves::{self, BinFilesJob};
use crate::names::clean_name;
use crate::paths::{self, norm};
use crate::pipeline::{self, failure, MoveJob, Phase, Pipeline, PipelineState, RunEnv, SortChoice};
use crate::projects::{self, HeldFile, Placement, ProjectRef, ProjectRegistry, RemovedProject};
use crate::settings::{ApiKeyStore, AppSettings, SettingsStore};
use crate::sorting::validator;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Tells the window something changed: an event name and its payload.
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct OpenProject {
    pub project: ProjectRef,
    pub db: Arc<ProjectDb>,
}

pub struct App {
    pub data_dir: PathBuf,
    settings_store: SettingsStore,
    settings: Mutex<AppSettings>,
    keys: Arc<ApiKeyStore>,
    pub registry: Arc<ProjectRegistry>,
    current: Mutex<Option<Arc<OpenProject>>>,
    pub pipeline: Pipeline,
    emit: Emit,
    adds_running: AtomicUsize,
    /// A thread is writing thumbnails for items that wait to be analyzed.
    thumbs_running: AtomicBool,
    closing: AtomicBool,
    pull_cancel: Arc<AtomicBool>,
    pulling: Mutex<Option<String>>,
    genie: Mutex<GenieStatus>,
    /// The `geniex serve` process that keeps the model loaded between items; started by a run.
    genie_server: Arc<GenieServer>,
    grabber: Mutex<Option<Arc<dyn FrameGrabber>>>,
    log: Arc<Mutex<VecDeque<String>>>,
    /// Pause between the simulated model's pieces of text (zero in tests).
    pub mock_delay: Mutex<Duration>,
    /// The simulated models stand in for GenieX. Only tests and the browser devserver turn this
    /// on; the app itself always uses GenieX, and says so when it isn't installed.
    simulated_models: AtomicBool,
    /// `winget install` is running for GenieX or FFmpeg (the package id).
    installing: Mutex<Option<String>>,
}

const LOG_LINES: usize = 400;
const PLAN_FAILED: &str = "Couldn't work out the file moves, so nothing was saved";
const MOVE_WAITS_FOR_ADD: &str = "Bin Files waits until the files are added";
const SAVE_WAITS_FOR_ADD: &str = "Saving moves files, so it waits until the files are added";

fn arg<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T> {
    serde_json::from_value(args.clone()).map_err(|e| Error::msg(format!("Bad request: {e}")))
}

/// A category picked in a sheet: an existing one, or a new name.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CategoryChoice {
    Existing(i64),
    New(String),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaFilter {
    /// `all`, `waiting`, `failed`, or a category id as a string.
    pub chip: String,
    pub kind: Option<ItemKind>,
    pub shot_type: Option<String>,
    pub taken_from: Option<i64>,
    pub taken_to: Option<i64>,
}

fn is_waiting(r: &MediaRow) -> bool {
    r.status == ItemStatus::Analyzed && r.category_id.is_none() && !r.category_set_by_user
}

impl MediaFilter {
    fn matches_without_chip(&self, r: &MediaRow) -> bool {
        self.kind.is_none_or(|k| r.kind == k)
            && self.shot_type.as_ref().is_none_or(|s| r.shot_type.as_deref() == Some(s.as_str()))
            && self.taken_from.is_none_or(|f| r.taken_at.is_some_and(|t| t >= f))
            && self.taken_to.is_none_or(|to| r.taken_at.is_some_and(|t| t <= to))
    }

    fn matches(&self, r: &MediaRow) -> bool {
        let chip = match self.chip.as_str() {
            "" | "all" => true,
            "waiting" => is_waiting(r),
            "failed" => r.status == ItemStatus::Failed,
            id => id.parse::<i64>().ok().is_some_and(|id| r.category_id == Some(id)),
        };
        chip && self.matches_without_chip(r)
    }
}

/// Item counts for one project. Every item is in exactly one bucket.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCounts {
    pub total: usize,
    pub pending: usize,
    pub analyzing: usize,
    /// Analyzed, but sorting hasn't given it a category yet.
    pub waiting: usize,
    pub failed: usize,
    pub missing: usize,
    /// Has a category, and its file is not yet at its target path.
    pub ready_to_bin: usize,
    pub binned: usize,
}

pub fn counts_of(rows: &[MediaRow]) -> MediaCounts {
    let n = |s: ItemStatus| rows.iter().filter(|r| r.status == s).count();
    MediaCounts {
        total: rows.len(),
        pending: n(ItemStatus::Pending),
        analyzing: n(ItemStatus::Analyzing),
        waiting: rows.iter().filter(|r| is_waiting(r)).count(),
        failed: n(ItemStatus::Failed),
        missing: n(ItemStatus::Missing),
        ready_to_bin: n(ItemStatus::Sorted),
        binned: n(ItemStatus::Binned),
    }
}

/// The one job allowed to run on the project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ActiveJob {
    Analysis,
    Sorting,
    Saving { done: usize, total: usize, paused: bool },
    Binning { done: usize, total: usize, paused: bool },
}

pub fn active_job(state: &PipelineState) -> Option<ActiveJob> {
    if !state.phase.is_running() {
        return None;
    }
    if state.phase == Phase::Categorizing {
        return Some(ActiveJob::Sorting);
    }
    if state.phase.is_moving() {
        let paused = state.phase == Phase::MovingPaused;
        let (done, total, save) = state.move_progress.as_ref().map(|p| (p.done, p.total, p.kind == "Save")).unwrap_or((0, 0, false));
        return Some(if save { ActiveJob::Saving { done, total, paused } } else { ActiveJob::Binning { done, total, paused } });
    }
    Some(ActiveJob::Analysis)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatus {
    pub text: String,
    /// `Running`, `Attention`, `Ready` or `Quiet`.
    pub tone: &'static str,
    /// Shows a "Categorize now" link after the text.
    pub categorize_now: bool,
    pub bin_files_enabled: bool,
    pub ready_to_bin: usize,
    /// Why Bin Files is off, for the note a click on it shows.
    pub blocked_reason: Option<String>,
}

fn n_files(n: usize) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{n} files")
    }
}

/// The status line and Bin Files state for Project Media. Bin Files is enabled only when files
/// are waiting to move, nothing is pending, analyzing or waiting to be categorized, and no job
/// runs. Failed and missing items never block it. `check_folders`: the last run's folder check
/// found something, so Bin Files stays on to show it.
pub fn media_status(c: &MediaCounts, job: Option<ActiveJob>, check_folders: bool) -> MediaStatus {
    let check_only = check_folders && c.ready_to_bin == 0;
    let enabled = job.is_none() && (c.ready_to_bin > 0 || check_only) && c.pending == 0 && c.analyzing == 0 && c.waiting == 0;
    let blocked_reason = match job {
        Some(ActiveJob::Binning { .. }) => Some("Bin Files is running".to_string()),
        Some(ActiveJob::Saving { .. }) => Some("Wait for the files to finish moving".to_string()),
        Some(_) => Some(pipeline::MOVE_BUSY.to_string()),
        None if c.pending > 0 || c.analyzing > 0 => Some("Bin Files waits for analysis to finish".to_string()),
        None if c.waiting > 0 => Some(format!("Bin Files waits for {} to be categorized", if c.waiting == 1 { "1 item".into() } else { format!("{} items", c.waiting) })),
        None if c.ready_to_bin == 0 && check_folders => None,
        None if c.ready_to_bin == 0 => Some(if c.binned > 0 { "All files are binned" } else { "Nothing to bin yet" }.to_string()),
        None => None,
    };
    let make = |text: String, tone: &'static str, categorize_now: bool, enabled: bool| MediaStatus {
        text,
        tone,
        categorize_now,
        bin_files_enabled: enabled,
        ready_to_bin: c.ready_to_bin,
        blocked_reason: blocked_reason.clone(),
    };
    let analyzing = || make(format!("Analyzing {} left", c.pending + c.analyzing), "Running", false, false);
    match job {
        Some(ActiveJob::Saving { done, total, paused }) => {
            return if paused {
                make(format!("Saving paused · {done} of {}", n_files(total)), "Attention", false, false)
            } else if total == 0 {
                make("Saving…".into(), "Running", false, false)
            } else {
                make(format!("Saving · moving {} of {}", (done + 1).min(total), n_files(total)), "Running", false, false)
            };
        }
        Some(ActiveJob::Binning { done, total, paused }) => {
            return if paused {
                make(format!("Bin Files paused · {done} of {}", n_files(total)), "Attention", false, false)
            } else if total == 0 {
                make("Binning · checking the folders".into(), "Running", false, false)
            } else {
                make(format!("Binning · {} of {}", (done + 1).min(total), n_files(total)), "Running", false, false)
            };
        }
        Some(ActiveJob::Sorting) => return make("Categorizing…".into(), "Running", false, false),
        Some(ActiveJob::Analysis) => return analyzing(),
        None => {}
    }
    if c.analyzing > 0 {
        return analyzing();
    }
    let first = if c.ready_to_bin > 0 {
        Some(format!("{} ready to bin", n_files(c.ready_to_bin)))
    } else if c.binned > 0 {
        Some("All files binned".to_string())
    } else {
        None
    };
    let (second, action) = if c.pending > 0 {
        (Some(format!("{} waiting to analyze", c.pending)), false)
    } else if c.waiting > 0 {
        (Some(format!("{} waiting to be categorized", c.waiting)), true)
    } else if check_only {
        (Some("check the folders".to_string()), false)
    } else if c.failed > 0 && c.missing > 0 {
        (Some(format!("{} failed, {} missing", c.failed, c.missing)), false)
    } else if c.failed > 0 {
        (Some(format!("{} failed", c.failed)), false)
    } else if c.missing > 0 {
        (Some(format!("{} missing", c.missing)), false)
    } else {
        (None, false)
    };
    let parts: Vec<String> = [first, second].into_iter().flatten().collect();
    let text = if parts.is_empty() { "Nothing to bin yet".to_string() } else { parts.join(" · ") };
    let tone = if c.pending > 0 || c.waiting > 0 {
        "Attention"
    } else if enabled {
        "Ready"
    } else {
        "Quiet"
    };
    make(text, tone, action, enabled)
}

/// What an edit came to.
enum Edit {
    Ok,
    Created(i64),
    Clash(String),
    InvalidName,
    NotFound,
    Busy(String),
}

impl Edit {
    fn json(&self) -> Value {
        match self {
            Edit::Ok => json!({"ok": true}),
            Edit::Created(id) => json!({"ok": true, "id": id}),
            Edit::Clash(existing) => json!({"ok": false, "error": format!("A category named “{existing}” already exists.")}),
            Edit::InvalidName => json!({"ok": false, "error": "That name has no characters a folder can use"}),
            Edit::NotFound => json!({"ok": false, "error": "It isn't in the project any more"}),
            Edit::Busy(reason) => json!({"ok": false, "error": reason}),
        }
    }
}

/// The existing name that `name` clashes with, or None if it is free. `except` is the category
/// being renamed, so renaming "Beach" to "Beaches" is not a clash with itself.
fn clash(name: &str, existing: &[String], except: Option<&str>) -> Option<String> {
    let k = validator::normalize(name);
    if k.is_empty() {
        return None;
    }
    let except_key = except.map(validator::normalize);
    existing.iter().find(|e| validator::normalize(e) == k && Some(validator::normalize(e)) != except_key).cloned()
}

fn ensure_category(c: &rusqlite::Connection, name: &str) -> Result<Edit> {
    let clean = clean_name(name, validator::MAX_CATEGORY_NAME);
    if clean.is_empty() {
        return Ok(Edit::InvalidName);
    }
    let existing: Vec<String> = db::categories(c)?.into_iter().map(|c| c.name).collect();
    if let Some(e) = clash(&clean, &existing, None) {
        return Ok(Edit::Clash(e));
    }
    c.execute("INSERT INTO category (name, description, createdBy, folderName) VALUES (?1, '', 'user', NULL)", [&clean])?;
    Ok(Edit::Created(c.last_insert_rowid()))
}

/// Resolves a choice to a category id, creating a new one; `Err(edit)` carries why it couldn't.
fn resolve_category(c: &rusqlite::Connection, choice: &CategoryChoice) -> Result<std::result::Result<i64, Edit>> {
    Ok(match choice {
        CategoryChoice::Existing(id) => match db::category(c, *id)? {
            Some(_) => Ok(*id),
            None => Err(Edit::NotFound),
        },
        CategoryChoice::New(name) => match ensure_category(c, name)? {
            Edit::Created(id) => Ok(id),
            other => Err(other),
        },
    })
}

fn update_item_edit(c: &rusqlite::Connection, id: i64, name: Option<&str>, name_set: bool, category_id: Option<i64>, category_set: bool, status: ItemStatus) -> Result<()> {
    c.execute(
        "UPDATE item SET friendlyName = ?2, nameSetByUser = ?3, categoryId = ?4, categorySetByUser = ?5, status = ?6 WHERE id = ?1",
        rusqlite::params![id, name, name_set, category_id, category_set, status.as_str()],
    )?;
    db::refresh_fts(c, id)
}

impl App {
    pub fn new(data_dir: PathBuf, projects_root: String, emit: Emit) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&data_dir);
        let settings_store = SettingsStore::new(data_dir.join("settings.json"));
        let settings = settings_store.load();
        let registry = Arc::new(ProjectRegistry::new(data_dir.join("projects.json"), projects_root));
        let state_emit = emit.clone();
        let data_emit = emit.clone();
        let pipeline = Pipeline::new(
            move |state| state_emit("pipeline", serde_json::to_value(state).unwrap_or(Value::Null)),
            move || data_emit("data", Value::Null),
        );
        let app = Arc::new(App {
            keys: Arc::new(ApiKeyStore::new(&data_dir)),
            genie_server: Arc::new(GenieServer::new(norm(data_dir.join("scratch")))),
            data_dir,
            settings_store,
            settings: Mutex::new(settings),
            registry,
            current: Mutex::new(None),
            pipeline,
            emit,
            adds_running: AtomicUsize::new(0),
            thumbs_running: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            pull_cancel: Arc::new(AtomicBool::new(false)),
            pulling: Mutex::new(None),
            genie: Mutex::new(GenieStatus::default()),
            grabber: Mutex::new(None),
            log: Arc::new(Mutex::new(VecDeque::new())),
            mock_delay: Mutex::new(Duration::from_millis(18)),
            simulated_models: AtomicBool::new(false),
            installing: Mutex::new(None),
        });
        app.refresh_genie();
        app
    }

    /// The app's data folder and the folder new projects are made in, for this computer.
    pub fn default_dirs() -> (PathBuf, String) {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("Agentic Binning");
        let pictures = home.join("Pictures");
        let root = if pictures.is_dir() { pictures } else { home }.join("Agentic Binning");
        (data, norm(root))
    }

    pub fn set_frame_grabber(&self, grabber: Arc<dyn FrameGrabber>) {
        *self.grabber.lock() = Some(grabber);
    }

    fn note(&self, line: &str) {
        let mut log = self.log.lock();
        if log.len() >= LOG_LINES {
            log.pop_front();
        }
        log.push_back(format!("{} {line}", chrono::Local::now().format("%H:%M:%S")));
    }

    pub fn settings(&self) -> AppSettings {
        self.settings.lock().clone()
    }

    fn refresh_genie(&self) -> GenieStatus {
        let s = self.settings();
        let status = geniex::status(Some(&s.geniex_path));
        *self.genie.lock() = status.clone();
        status
    }

    /// Whether the simulated models stand in for GenieX.
    pub fn simulated(&self) -> bool {
        self.simulated_models.load(Ordering::SeqCst)
    }

    /// For tests and the browser devserver only: analyze and sort with the simulated models,
    /// whose output always says it is simulated, instead of GenieX.
    pub fn use_simulated_models(&self, on: bool) {
        self.simulated_models.store(on, Ordering::SeqCst);
    }

    fn ffmpeg(&self) -> Ffmpeg {
        Ffmpeg::locate(Some(&self.settings().ffmpeg_path))
    }

    pub fn current(&self) -> Option<Arc<OpenProject>> {
        self.current.lock().clone()
    }

    fn open(&self) -> Result<Arc<OpenProject>> {
        self.current().ok_or_else(|| Error::msg("No project is open"))
    }

    // ---- projects ----

    /// Opens `project` (stopping whatever runs for the previous one), lists it and makes it current.
    fn open_project(self: &Arc<Self>, project: &ProjectRef, existing_only: bool) -> Result<Arc<OpenProject>> {
        if let Some(cur) = self.current() {
            if paths::same_path(&cur.project.path, &project.path) {
                return Ok(cur);
            }
        }
        let database = projects::open_database(&project.path, &project.name, existing_only)?;
        self.pipeline.stop_and_wait();
        let stored = self.registry.set_current(project)?;
        let opened = Arc::new(OpenProject { project: stored, db: Arc::new(database) });
        *self.current.lock() = Some(opened.clone());
        let notice = pipeline::stored_sort_notice(&opened.db.recent_sort_runs().unwrap_or_default());
        self.pipeline.show_project(Some(&opened.project.path), Some(&opened.db), notice);
        self.note(&format!("Opened {}", opened.project.name));
        self.resume_work(&opened);
        self.start_thumbnails(&opened);
        (self.emit)("projects", Value::Null);
        Ok(opened)
    }

    /// After opening: finish a move the app didn't, else carry on analyzing unless the user stopped it.
    fn resume_work(&self, open: &Arc<OpenProject>) {
        if let Ok(Some(run)) = open.db.open_move_run() {
            let _ = self.pipeline.start_move(self.env(open), MoveJob::Resume(run.id), run.state == "Paused");
            return;
        }
        let stopped = open.db.stopped_by_user().unwrap_or(false);
        if !stopped && open.db.next_pending().ok().flatten().is_some() {
            self.pipeline.start(self.env(open));
        }
    }

    /// Opens the project that was open last time, if its folder is still there.
    pub fn restore_current(self: &Arc<Self>) {
        if let Some(p) = self.registry.current() {
            if let Err(e) = self.open_project(&p, true) {
                self.note(&format!("Couldn't reopen {}: {e}", p.name));
                let _ = self.registry.clear_current();
            }
        }
    }

    fn close_current(&self) {
        self.pipeline.stop_and_wait();
        *self.current.lock() = None;
        self.pipeline.show_project(None, None, None);
    }

    fn project_infos(&self) -> Value {
        let current = self.current().map(|c| c.project.path.clone());
        let list: Vec<Value> = self
            .registry
            .list()
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "path": p.path,
                    "displayPath": paths::display(&p.path),
                    "open": current.as_deref().is_some_and(|c| paths::same_path(c, &p.path)),
                    "missing": !std::path::Path::new(&p.path).is_dir(),
                })
            })
            .collect();
        json!({"projects": list, "current": self.current().map(|c| json!({"name": c.project.name, "path": c.project.path, "displayPath": paths::display(&c.project.path)}))})
    }

    // ---- runs ----

    /// Everything a run needs, from the settings as they are now.
    fn env(&self, open: &Arc<OpenProject>) -> RunEnv {
        let settings = self.settings();
        let simulated = self.simulated();
        let delay = *self.mock_delay.lock();
        let genie = self.genie.lock().clone();
        let program = genie.path.clone().unwrap_or_else(|| "geniex".to_string());
        let scratch = norm(self.data_dir.join("scratch"));
        let server = self.genie_server.clone();
        let vision_cli = Arc::new(GenieCli::new(program.clone(), settings.compute.clone(), scratch.clone()).with_server(server.clone()));
        let vision: Arc<dyn VisionModel> = if simulated { Arc::new(MockVisionModel::new(delay)) } else { Arc::new(GenieVisionModel::new(vision_cli, LFM3_VL_2B)) };
        let keys = self.keys.clone();
        let (text_program, text_compute, text_scratch) = (program.clone(), settings.compute.clone(), scratch);
        let text_for = move |choice: SortChoice| -> Arc<dyn TextModel> {
            match choice {
                SortChoice::Claude => {
                    let keys = keys.clone();
                    Arc::new(crate::claude::ClaudeTextModel::new(move || keys.get()))
                }
                SortChoice::Lfm3 if simulated => Arc::new(MockTextModel::new(delay * 12)),
                SortChoice::Lfm3 => Arc::new(GenieTextModel { cli: Arc::new(GenieCli::new(text_program.clone(), text_compute.clone(), text_scratch.clone()).with_server(server.clone())), spec: LFM3_1_7B }),
            }
        };
        let keys = self.keys.clone();
        let installed = genie.path.is_some();
        let text_ready = genie.text_ready;
        let sorting_available = move |choice: SortChoice| -> Option<String> {
            match choice {
                SortChoice::Claude if keys.get().is_none() => Some("Add a Claude API key in Settings to categorize".into()),
                SortChoice::Claude => None,
                SortChoice::Lfm3 if simulated => None,
                SortChoice::Lfm3 if !installed => Some("GenieX isn't installed, so the sorting model can't run".into()),
                SortChoice::Lfm3 if !text_ready => Some("Download the sorting model in Settings".into()),
                SortChoice::Lfm3 => None,
            }
        };
        let geniex_path = settings.geniex_path.clone();
        let cancel = self.pull_cancel.clone();
        let ensure_vision = move |on_progress: &dyn Fn(f64)| -> std::result::Result<(), String> {
            if simulated {
                return Ok(());
            }
            let status = geniex::status(Some(&geniex_path));
            let Some(program) = status.path else { return Err(failure::GENIEX_MISSING.into()) };
            if status.vision_ready || status.error.is_some() {
                // With an unreadable list the run goes ahead: `geniex infer` says what is wrong.
                return Ok(());
            }
            cancel.store(false, Ordering::SeqCst);
            geniex::pull(&program, &LFM3_VL_2B, &cancel, &|percent| on_progress(percent / 100.0))
        };
        let registry = self.registry.clone();
        let dir = open.project.path.clone();
        let log = self.log.clone();
        RunEnv {
            project_dir: open.project.path.clone(),
            db: open.db.clone(),
            vision,
            text_for: Box::new(text_for),
            sort_choice: settings.sorting_model,
            max_frames: settings.max_frames_per_clip,
            preparer: MediaPreparer {
                work_dir: norm(self.data_dir.join("work")),
                thumbs_dir: projects::thumbs_dir(&open.project.path),
                ffmpeg: self.ffmpeg(),
                grabber: self.grabber.lock().clone(),
            },
            ensure_vision: Box::new(ensure_vision),
            sorting_available: Box::new(sorting_available),
            shared: Box::new(move |paths| projects::shared_with_other_projects(&registry, &dir, paths)),
            log: Arc::new(move |line| {
                let mut log = log.lock();
                if log.len() >= LOG_LINES {
                    log.pop_front();
                }
                log.push_back(format!("{} {line}", chrono::Local::now().format("%H:%M:%S")));
            }),
        }
    }

    fn start_analysis(&self, open: &Arc<OpenProject>) {
        let _ = open.db.set_stopped_by_user(false);
        self.pipeline.start(self.env(open));
    }

    /// Writes a thumbnail for every item of `open` that waits to be analyzed, in the order they
    /// will be analyzed, on a background thread, so the window shows what is coming instead of
    /// blank cards. Analysis writes the same thumbnails when it reaches an item; this only gets
    /// there first. It ends when none is left to write, the project is closed, or the app closes.
    fn start_thumbnails(self: &Arc<Self>, open: &Arc<OpenProject>) {
        if self.thumbs_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let (app, open) = (self.clone(), open.clone());
        std::thread::spawn(move || {
            let preparer = MediaPreparer { work_dir: norm(app.data_dir.join("work")), thumbs_dir: projects::thumbs_dir(&open.project.path), ffmpeg: app.ffmpeg(), grabber: None };
            let max_frames = app.settings().max_frames_per_clip as usize;
            let wanted = || !app.closing.load(Ordering::SeqCst) && app.current().is_some_and(|c| Arc::ptr_eq(&c, &open));
            // Items that gave no thumbnail (unreadable, or a video without FFmpeg) are not tried again.
            let mut skip: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let mut last_emit = std::time::Instant::now();
            let mut unseen = false;
            while wanted() {
                let waiting: Vec<_> = open.db.items().unwrap_or_default().into_iter().filter(|i| i.status == ItemStatus::Pending && !skip.contains(&i.id) && !std::path::Path::new(&preparer.thumb_path(i.id)).is_file()).collect();
                if waiting.is_empty() {
                    // Files still being added will need theirs too.
                    if app.adds_running.load(Ordering::SeqCst) == 0 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                for item in waiting {
                    if !wanted() {
                        break;
                    }
                    match preparer.thumbnail(item.id, &item.current_path, item.kind, item.duration_ms, max_frames) {
                        Ok(true) => unseen = true,
                        _ => {
                            skip.insert(item.id);
                        }
                    }
                    // The window reloads its rows at most twice a second.
                    if unseen && last_emit.elapsed() >= Duration::from_millis(500) {
                        (app.emit)("data", Value::Null);
                        last_emit = std::time::Instant::now();
                        unseen = false;
                    }
                }
            }
            app.thumbs_running.store(false, Ordering::SeqCst);
            if unseen {
                (app.emit)("data", Value::Null);
            }
            // Another project was opened while this one's thumbnails were written: it is next.
            if let Some(now_open) = app.current().filter(|c| !Arc::ptr_eq(c, &open) && !app.closing.load(Ordering::SeqCst)) {
                app.start_thumbnails(&now_open);
            }
        });
    }

    /// Registers `candidates` on a background thread, then starts analysis. Progress and the
    /// summary reach the window as `registration` events.
    fn add_media(self: &Arc<Self>, open: Arc<OpenProject>, candidates: Vec<String>, skipped: usize) {
        let app = self.clone();
        self.adds_running.fetch_add(1, Ordering::SeqCst);
        self.start_thumbnails(&open);
        std::thread::spawn(move || {
            let ffmpeg = app.ffmpeg();
            let emit = app.emit.clone();
            let mut summary = projects::register(&open.project, &open.db, &app.registry, &ffmpeg, &candidates, &|done, total| {
                emit("registration", json!({"done": done, "total": total}));
                true
            });
            summary.unreadable += skipped;
            summary.text = summary.describe();
            app.adds_running.fetch_sub(1, Ordering::SeqCst);
            app.note(&format!("Added media to {}: {}", open.project.name, summary.text));
            let still_open = app.current().is_some_and(|c| Arc::ptr_eq(&c, &open));
            if still_open {
                app.pipeline.refresh_counts(&open.db);
                if summary.added > 0 {
                    app.start_analysis(&open);
                    // In case the thumbnail thread ended just before the last files were added.
                    app.start_thumbnails(&open);
                }
            }
            (app.emit)("registration", json!({"finished": true, "summary": summary}));
        });
    }

    // ---- edits ----

    /// Runs `edit` in one transaction with the move plan for the items it reports. An edit that
    /// would move files while a run is in progress rolls back with its plan, so nothing is half
    /// done and nothing is queued. The plan runs after the commit.
    fn with_moves(&self, open: &Arc<OpenProject>, edit: impl FnOnce(&rusqlite::Transaction, &mut Vec<i64>) -> Result<Edit>) -> Result<Value> {
        let registry = self.registry.clone();
        let dir = open.project.path.clone();
        let shared = move |paths: &[String]| projects::shared_with_other_projects(&registry, &dir, paths);
        let job = BinFilesJob::new(&open.db, &open.project.path, &shared);
        let busy = if self.adds_running.load(Ordering::SeqCst) > 0 {
            Some(SAVE_WAITS_FOR_ADD)
        } else if self.pipeline.is_running() {
            Some(pipeline::SAVE_BUSY)
        } else {
            None
        };
        let mut run_id = None;
        let mut refused: Option<String> = None;
        let outcome = open.db.tx(|t| {
            let mut affected = Vec::new();
            let result = edit(t, &mut affected)?;
            if matches!(result, Edit::Ok) && !affected.is_empty() {
                match job.plan_save(t, &affected) {
                    Ok(Some(id)) => match busy {
                        Some(reason) => {
                            refused = Some(reason.to_string());
                            return Err(Error::msg(reason));
                        }
                        None => run_id = Some(id),
                    },
                    Ok(None) => {}
                    Err(e) => {
                        self.note(&format!("A Save's move plan failed: {e}"));
                        refused = Some(PLAN_FAILED.to_string());
                        return Err(Error::msg(PLAN_FAILED));
                    }
                }
            }
            Ok(result)
        });
        let outcome = match (outcome, refused) {
            (Err(_), Some(reason)) => Edit::Busy(reason),
            (Err(e), None) => return Err(e),
            (Ok(o), _) => o,
        };
        if let Some(id) = run_id {
            if let Err(reason) = self.pipeline.start_move(self.env(open), MoveJob::Save(id), false) {
                self.note(&format!("A Save's moves could not start: {reason}"));
            }
        }
        (self.emit)("data", Value::Null);
        Ok(outcome.json())
    }

    fn save_item(&self, open: &Arc<OpenProject>, id: i64, name: &str, category: Option<CategoryChoice>, unlock_name: bool, unlock_category: bool) -> Result<Value> {
        let name = clean_name(name, validator::MAX_FRIENDLY_NAME);
        self.with_moves(open, |t, affected| {
            affected.push(id);
            let Some(item) = db::item(t, id)? else { return Ok(Edit::NotFound) };
            let (category_id, mut category_set) = match &category {
                None => (item.category_id, item.category_set_by_user),
                Some(choice) => match resolve_category(t, choice)? {
                    Ok(cid) => (Some(cid), true),
                    Err(e) => return Ok(e),
                },
            };
            if unlock_category {
                category_set = false; // The category itself stays.
            }
            let new_name = if name.is_empty() { item.friendly_name.clone() } else { Some(name.clone()) };
            let name_set = if unlock_name { false } else if new_name != item.friendly_name { true } else { item.name_set_by_user };
            let status = if category_id.is_some() && item.status == ItemStatus::Analyzed { ItemStatus::Sorted } else { item.status };
            update_item_edit(t, id, new_name.as_deref(), name_set, category_id, category_set, status)?;
            Ok(Edit::Ok)
        })
    }

    /// Bulk Move to category: each analyzed item gets the category, locked as the user's. Items
    /// not analyzed yet are left as they are.
    fn move_items(&self, open: &Arc<OpenProject>, ids: &[i64], category: CategoryChoice) -> Result<Value> {
        self.with_moves(open, |t, affected| {
            affected.extend_from_slice(ids);
            let category_id = match resolve_category(t, &category)? {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            for id in ids {
                let Some(item) = db::item(t, *id)? else { continue };
                if !matches!(item.status, ItemStatus::Analyzed | ItemStatus::Sorted | ItemStatus::Binned) {
                    continue;
                }
                let status = if item.status == ItemStatus::Analyzed { ItemStatus::Sorted } else { item.status };
                update_item_edit(t, *id, item.friendly_name.as_deref(), item.name_set_by_user, Some(category_id), true, status)?;
            }
            Ok(Edit::Ok)
        })
    }

    /// Remove from project: the rows go, the files stay. A file left in the project's folders is
    /// remembered, so the folder check never stops on it.
    fn remove_items(&self, open: &Arc<OpenProject>, ids: &[i64]) -> Result<Value> {
        let root = open.project.path.trim_end_matches('/').to_string();
        open.db.tx(|t| {
            let mut kept = Vec::new();
            for id in ids {
                let Some(item) = db::item(t, *id)? else { continue };
                let inside = paths::is_under(&item.current_path, &root) && !paths::is_under(&item.current_path, &projects::binning_dir(&root));
                if item.status == ItemStatus::Binned || inside {
                    kept.push(item.current_path);
                }
                t.execute("DELETE FROM item_fts WHERE rowid = ?1", [id])?;
                t.execute("DELETE FROM item WHERE id = ?1", [id])?;
            }
            db::add_removed(t, &kept, "removed")
        })?;
        for id in ids {
            let _ = std::fs::remove_file(format!("{}/{id}.jpg", projects::thumbs_dir(&root)));
        }
        self.pipeline.refresh_counts(&open.db);
        Ok(json!({"ok": true}))
    }

    /// The Category Edit sheet's Save: every merge (keep, drop) and rename in one transaction.
    /// The names after Save are checked first, so swapping two names works and a clash changes
    /// nothing. Every category touched becomes the user's.
    fn apply_category_edits(&self, open: &Arc<OpenProject>, renames: &[(i64, String)], merges: &[(i64, i64)]) -> Result<Value> {
        self.with_moves(open, |t, affected| {
            let cats = db::categories(t)?;
            let has = |id: i64| cats.iter().any(|c| c.id == id);
            if merges.iter().any(|(keep, drop)| keep == drop || !has(*keep) || !has(*drop)) {
                return Ok(Edit::NotFound);
            }
            let dropped: Vec<i64> = merges.iter().map(|m| m.1).collect();
            let cleaned: Vec<(i64, String)> = renames.iter().filter(|(id, _)| !dropped.contains(id)).map(|(id, n)| (*id, clean_name(n, validator::MAX_CATEGORY_NAME))).collect();
            if cleaned.iter().any(|(id, _)| !has(*id)) {
                return Ok(Edit::NotFound);
            }
            if cleaned.iter().any(|(_, n)| n.is_empty()) {
                return Ok(Edit::InvalidName);
            }
            let final_names: Vec<String> =
                cats.iter().filter(|c| !dropped.contains(&c.id)).map(|c| cleaned.iter().find(|(id, _)| *id == c.id).map(|(_, n)| n.clone()).unwrap_or_else(|| c.name.clone())).collect();
            for (i, name) in final_names.iter().enumerate() {
                let k = validator::normalize(name);
                if let Some(other) = final_names.iter().enumerate().find(|(j, n)| *j != i && validator::normalize(n) == k).map(|(_, n)| n) {
                    // Name the one that was already there, as a single rename's clash does.
                    let renamed = |n: &String| cleaned.iter().any(|(_, c)| c == n);
                    return Ok(Edit::Clash(if renamed(name) && !renamed(other) { other.clone() } else { name.clone() }));
                }
            }
            let mut touched: Vec<i64> = Vec::new();
            for (keep, drop) in merges {
                t.execute("UPDATE item SET categoryId = ?1 WHERE categoryId = ?2", [keep, drop])?;
                t.execute("UPDATE category SET createdBy = 'user' WHERE id = ?1", [keep])?;
                t.execute("DELETE FROM category WHERE id = ?1", [drop])?;
                touched.retain(|id| id != drop);
                if !touched.contains(keep) {
                    touched.push(*keep);
                }
            }
            for (id, name) in &cleaned {
                // The user's rename: the category becomes theirs and loses the description the model
                // wrote for the old name.
                t.execute("UPDATE category SET name = ?2, description = '', createdBy = 'user' WHERE id = ?1", rusqlite::params![id, name])?;
                if !touched.contains(id) {
                    touched.push(*id);
                }
            }
            for id in touched {
                for item in db::item_ids_in_category(t, id)? {
                    db::refresh_fts(t, item)?;
                    if !affected.contains(&item) {
                        affected.push(item);
                    }
                }
            }
            Ok(Edit::Ok)
        })
    }

    /// Retry analysis: the item back to Pending with its attempts reset and a remembered Stop
    /// lifted; analysis starts after it commits.
    fn retry_failed(&self, open: &Arc<OpenProject>, id: i64) -> Result<Value> {
        let ok = open.db.tx(|t| {
            let failed = db::item(t, id)?.is_some_and(|i| i.status == ItemStatus::Failed);
            if failed {
                t.execute("UPDATE item SET status = 'Pending', attempts = 0, failureReason = NULL WHERE id = ?1", [id])?;
                t.execute("UPDATE project_meta SET stoppedByUser = 0 WHERE id = 1", [])?;
            }
            Ok(failed)
        })?;
        if !ok {
            return Ok(Edit::NotFound.json());
        }
        self.pipeline.refresh_counts(&open.db);
        self.start_analysis(open);
        Ok(json!({"ok": true}))
    }

    /// A failed item's name and category set by hand, both locked.
    fn set_by_hand(&self, open: &Arc<OpenProject>, id: i64, name: &str, category: CategoryChoice) -> Result<Value> {
        let name = clean_name(name, validator::MAX_FRIENDLY_NAME);
        if name.is_empty() {
            return Ok(Edit::InvalidName.json());
        }
        let result = open.db.tx(|t| {
            if db::item(t, id)?.is_none() {
                return Ok(Edit::NotFound);
            }
            let category_id = match resolve_category(t, &category)? {
                Ok(id) => id,
                Err(e) => return Ok(e),
            };
            update_item_edit(t, id, Some(&name), true, Some(category_id), true, ItemStatus::Sorted)?;
            Ok(Edit::Ok)
        })?;
        self.pipeline.refresh_counts(&open.db);
        Ok(result.json())
    }

    // ---- queries ----

    fn grid(&self, open: &Arc<OpenProject>, filter: &MediaFilter, query: &str) -> Result<Value> {
        let all = open.db.grid()?;
        let categories = open.db.categories()?;
        let counts = counts_of(&all);
        let state = self.pipeline.state();
        let check_failed = open.db.last_check_ok()? == Some(false);
        let status = media_status(&counts, active_job(&state), check_failed);
        // Chips count what the search and the other filters leave, so a chip never promises items a
        // click won't show. While searching, categories with no matches drop out of the row.
        let fts = crate::search::fts_query(query);
        let hits = fts.as_ref().map(|q| open.db.search(q).unwrap_or_default());
        let found = |r: &MediaRow| hits.as_ref().is_none_or(|h| h.contains(&r.id));
        let base: Vec<&MediaRow> = all.iter().filter(|r| filter.matches_without_chip(r) && found(r)).collect();
        let mut chips = vec![json!({"chip": "all", "label": "All", "count": base.len()})];
        let waiting = base.iter().filter(|r| is_waiting(r)).count();
        if waiting > 0 || filter.chip == "waiting" {
            chips.push(json!({"chip": "waiting", "label": "Waiting", "count": waiting}));
        }
        let failed = base.iter().filter(|r| r.status == ItemStatus::Failed).count();
        if failed > 0 || filter.chip == "failed" {
            chips.push(json!({"chip": "failed", "label": "Failed", "count": failed}));
        }
        for c in &categories {
            let n = base.iter().filter(|r| r.category_id == Some(c.id)).count();
            if hits.is_some() && n == 0 && filter.chip != c.id.to_string() {
                continue;
            }
            chips.push(json!({"chip": c.id.to_string(), "label": c.name, "count": n, "locked": c.created_by != "model"}));
        }
        let mut rows: Vec<MediaRow> = all.iter().filter(|r| filter.matches(r) && found(r)).cloned().collect();
        if let Some(q) = &fts {
            let matched = open.db.matched_in(q).unwrap_or_default();
            for r in rows.iter_mut() {
                r.matched_in = matched.iter().find(|(id, _)| *id == r.id).map(|(_, m)| *m);
            }
        }
        let thumbs = projects::thumbs_dir(&open.project.path);
        let rows: Vec<Value> = rows
            .iter()
            .map(|r| {
                let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
                let thumb = format!("{thumbs}/{}.jpg", r.id);
                // When the thumbnail was written, so the window reloads a picture only when it changed.
                let written = std::fs::metadata(&thumb).ok().filter(|m| m.is_file()).map(|m| m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0));
                v["thumbAt"] = json!(written);
                v["thumb"] = if written.is_some() { json!(thumb) } else { Value::Null };
                v["categoryName"] = json!(r.category_id.and_then(|id| categories.iter().find(|c| c.id == id)).map(|c| c.name.clone()));
                v["failure"] = json!((r.status == ItemStatus::Failed).then(|| failure::describe(r.failure_reason.as_deref())));
                v
            })
            .collect();
        Ok(json!({
            "rows": rows,
            "chips": chips,
            "counts": counts,
            "status": status,
            "categories": categories,
            "searching": fts.is_some(),
            "total": all.len(),
            "addsRunning": self.adds_running.load(Ordering::SeqCst) > 0,
        }))
    }

    fn detail(&self, open: &Arc<OpenProject>, id: i64) -> Result<Value> {
        let Some(item) = open.db.item(id)? else { return Ok(Value::Null) };
        let analysis = open.db.analysis(id)?;
        let category = match item.category_id {
            Some(cid) => open.db.read(|c| db::category(c, cid))?,
            None => None,
        };
        let thumb = format!("{}/{id}.jpg", projects::thumbs_dir(&open.project.path));
        Ok(json!({
            "item": item,
            "displayPath": paths::display(&item.current_path),
            "fileExists": crate::files::is_file(&item.current_path),
            "thumb": std::path::Path::new(&thumb).is_file().then_some(thumb),
            "tags": if analysis.is_some() { open.db.tags_for(id)? } else { Vec::new() },
            "analysis": analysis,
            "category": category,
            "failure": (item.status == ItemStatus::Failed).then(|| failure::describe(item.failure_reason.as_deref())),
            "simulated": analysis_is_simulated(&open.db, id),
        }))
    }

    fn boot(&self) -> Value {
        let genie = self.genie.lock().clone();
        let ff = self.ffmpeg();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "settings": self.settings(),
            "projects": self.project_infos(),
            "pipeline": self.pipeline.state(),
            "genie": genie,
            "simulated": self.simulated(),
            "ffmpeg": {"path": ff.ffmpeg, "ffprobe": ff.ffprobe, "machine": ff.machine, "emulated": ff.emulated, "fromSetting": ff.from_setting},
            "apiKey": self.keys.masked(),
            "projectsRoot": paths::display(&self.registry.projects_root),
            "models": {"vision": LFM3_VL_2B, "text": LFM3_1_7B},
            "pulling": self.pulling.lock().clone(),
            "installing": self.installing.lock().clone(),
            "canInstall": crate::install::winget().is_some(),
            "frameCaps": crate::settings::FRAME_CAP_OPTIONS,
            "frameCapHint": crate::analysis::budget::hint(self.settings().max_frames_per_clip),
            "windows": cfg!(windows),
        })
    }

    fn pull_model(self: &Arc<Self>, spec: ModelSpec) -> Result<Value> {
        let Some(program) = self.genie.lock().path.clone() else { return Err(Error::msg("GenieX isn't installed")) };
        {
            let mut pulling = self.pulling.lock();
            if pulling.is_some() {
                return Err(Error::msg("A download is already running"));
            }
            *pulling = Some(spec.id.to_string());
        }
        self.pull_cancel.store(false, Ordering::SeqCst);
        let app = self.clone();
        std::thread::spawn(move || {
            let emit = app.emit.clone();
            let result = geniex::pull(&program, &spec, &app.pull_cancel, &|percent| emit("pull", json!({"model": spec.id, "percent": percent})));
            *app.pulling.lock() = None;
            app.refresh_genie();
            if let Err(e) = &result {
                app.note(&format!("Downloading {} failed: {e}", spec.display_name));
            }
            (app.emit)("pull", json!({"model": spec.id, "finished": true, "error": result.err()}));
        });
        Ok(json!({"ok": true}))
    }

    /// Installs GenieX or FFmpeg with winget on a background thread, after the person pressed
    /// Install in Settings. Its lines go to the activity log; `install` events tell the window.
    fn install(self: &Arc<Self>, package: crate::install::Package) -> Result<Value> {
        {
            let mut installing = self.installing.lock();
            if installing.is_some() {
                return Err(Error::msg("An install is already running"));
            }
            *installing = Some(package.name.to_string());
        }
        let app = self.clone();
        std::thread::spawn(move || {
            let emit = app.emit.clone();
            app.note(&format!("Installing {}", package.name));
            let result = crate::install::run(&package, &|line| {
                app.note(line);
                emit("install", json!({"what": package.name, "line": line}));
            });
            *app.installing.lock() = None;
            app.refresh_genie();
            let found = if package == crate::install::GENIEX { app.genie.lock().path.is_some() } else { app.ffmpeg().ffmpeg.is_some() };
            let error = match result {
                Ok(()) if found => None,
                Ok(()) => Some(format!("winget finished, but the app can't find {} yet. Restart the app, or set its path below", package.name)),
                Err(e) => Some(e),
            };
            app.note(&match &error {
                None => format!("{} is installed", package.name),
                Some(e) => format!("Installing {} failed: {e}", package.name),
            });
            (app.emit)("install", json!({"what": package.name, "finished": true, "error": error}));
        });
        Ok(self.boot())
    }

    /// Answers one command from the window. Every command and its arguments are listed here.
    pub fn dispatch(self: &Arc<Self>, cmd: &str, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct PathArg {
            path: String,
        }
        #[derive(Deserialize)]
        struct IdArg {
            id: i64,
        }
        #[derive(Deserialize)]
        struct IdsArg {
            ids: Vec<i64>,
        }
        #[derive(Deserialize)]
        struct PathsArg {
            paths: Vec<String>,
        }
        match cmd {
            "boot" => Ok(self.boot()),
            "projects.list" => Ok(self.project_infos()),
            "project.create" => {
                #[derive(Deserialize)]
                struct A {
                    name: String,
                }
                let a: A = arg(args)?;
                let project = self.registry.create(&a.name)?;
                self.open_project(&project, false)?;
                Ok(self.project_infos())
            }
            // Open project: a folder that already holds a project (its `.binning` database).
            "project.open" => {
                let a: PathArg = arg(args)?;
                let dir = projects::project_dir_of(&norm(&a.path));
                let listed = self.registry.list();
                let project = match projects::placement(&dir, &listed) {
                    Placement::Listed(p) => p,
                    Placement::Inside(p) => return Err(Error::msg(format!("This folder is inside {}", p.name))),
                    Placement::Contains(p) => return Err(Error::msg(format!("This folder contains {}", p.name))),
                    Placement::Unlisted => {
                        let facts = db::probe(&projects::database_file(&dir));
                        projects::decide(&facts).map_err(Error::msg)?;
                        let name = facts.meta_name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| paths::file_name(&dir).to_string());
                        ProjectRef { name, path: dir }
                    }
                };
                self.open_project(&project, true)?;
                Ok(self.project_infos())
            }
            "project.switch" => {
                let a: PathArg = arg(args)?;
                let project = self.registry.find(&a.path).ok_or_else(|| Error::msg("That project isn't on the list any more"))?;
                self.open_project(&project, true)?;
                Ok(self.project_infos())
            }
            // × on Current projects: only takes it off the list; its files and database stay.
            "project.remove" => {
                let a: PathArg = arg(args)?;
                let was_open = self.current().is_some_and(|c| paths::same_path(&c.project.path, &a.path));
                if was_open {
                    self.close_current();
                }
                let removed = self.registry.remove(&a.path)?;
                if was_open {
                    // The next project on the list that still opens takes its place.
                    for p in self.registry.list() {
                        if self.open_project(&p, true).is_ok() {
                            break;
                        }
                    }
                }
                (self.emit)("projects", Value::Null);
                Ok(json!({"removed": removed, "projects": self.project_infos()}))
            }
            "project.restore" => {
                let removed: RemovedProject = arg(args)?;
                self.registry.restore(&removed)?;
                if removed.was_current && self.current().is_none() {
                    let _ = self.open_project(&removed.project, true);
                }
                Ok(self.project_infos())
            }
            // Add a folder: every photo and video under it, other projects' folders left out.
            "media.addFolder" => {
                let a: PathArg = arg(args)?;
                let open = self.open()?;
                let dir = norm(&a.path);
                if !std::path::Path::new(&dir).is_dir() {
                    return Err(Error::msg("That folder isn't there"));
                }
                let found = media::scan(&dir, &|_| false);
                let total = found.len();
                self.add_media(open, found, 0);
                Ok(json!({"started": true, "total": total}))
            }
            "media.addFiles" => {
                let a: PathsArg = arg(args)?;
                let open = self.open()?;
                let all: Vec<String> = a.paths.iter().map(norm).collect();
                let (media_files, other): (Vec<String>, Vec<String>) = all.into_iter().partition(|p| media::kind_of(p).is_some() && crate::files::is_file(p));
                let total = media_files.len();
                self.add_media(open, media_files, other.len());
                Ok(json!({"started": true, "total": total}))
            }
            // Copy files another project holds into this one, checked, leaving the originals.
            "media.copyHeld" => {
                #[derive(Deserialize)]
                struct A {
                    held: Vec<HeldFile>,
                }
                let a: A = arg(args)?;
                let open = self.open()?;
                if self.pipeline.state().phase.is_moving() {
                    return Err(Error::msg("Files are moving, so nothing was copied. Pick them again when the files have moved."));
                }
                let app = self.clone();
                self.adds_running.fetch_add(1, Ordering::SeqCst);
                // Copied files wait with their thumbnails as added ones do. The thread Add started
                // has ended by now: everything it was given is held by another project.
                self.start_thumbnails(&open);
                std::thread::spawn(move || {
                    let emit = app.emit.clone();
                    let result = projects::copy_shared(&open.project.path, &open.db, &app.ffmpeg(), &a.held, &|done, total| emit("copy", json!({"done": done, "total": total})));
                    app.adds_running.fetch_sub(1, Ordering::SeqCst);
                    app.note(&format!("Copied held files: {}", result.text));
                    if app.current().is_some_and(|c| Arc::ptr_eq(&c, &open)) {
                        app.pipeline.refresh_counts(&open.db);
                        if result.copied > 0 {
                            app.start_analysis(&open);
                            // In case the thumbnail thread ended just before the last files were copied.
                            app.start_thumbnails(&open);
                        }
                    }
                    (app.emit)("copy", json!({"finished": true, "result": result}));
                });
                Ok(json!({"started": true}))
            }
            "grid" => {
                #[derive(Deserialize, Default)]
                #[serde(default)]
                struct A {
                    filter: MediaFilter,
                    query: String,
                }
                let a: A = arg(args)?;
                self.grid(&self.open()?, &a.filter, &a.query)
            }
            "item.detail" => {
                let a: IdArg = arg(args)?;
                self.detail(&self.open()?, a.id)
            }
            "item.save" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct A {
                    id: i64,
                    name: String,
                    #[serde(default)]
                    category: Option<CategoryChoice>,
                    #[serde(default)]
                    unlock_name: bool,
                    #[serde(default)]
                    unlock_category: bool,
                }
                let a: A = arg(args)?;
                self.save_item(&self.open()?, a.id, &a.name, a.category, a.unlock_name, a.unlock_category)
            }
            "items.move" => {
                #[derive(Deserialize)]
                struct A {
                    ids: Vec<i64>,
                    category: CategoryChoice,
                }
                let a: A = arg(args)?;
                self.move_items(&self.open()?, &a.ids, a.category)
            }
            "items.remove" => {
                let a: IdsArg = arg(args)?;
                self.remove_items(&self.open()?, &a.ids)
            }
            "item.retry" => {
                let a: IdArg = arg(args)?;
                self.retry_failed(&self.open()?, a.id)
            }
            "item.setByHand" => {
                #[derive(Deserialize)]
                struct A {
                    id: i64,
                    name: String,
                    category: CategoryChoice,
                }
                let a: A = arg(args)?;
                self.set_by_hand(&self.open()?, a.id, &a.name, a.category)
            }
            "categories.apply" => {
                #[derive(Deserialize)]
                struct A {
                    #[serde(default)]
                    renames: Vec<(i64, String)>,
                    #[serde(default)]
                    merges: Vec<(i64, i64)>,
                }
                let a: A = arg(args)?;
                self.apply_category_edits(&self.open()?, &a.renames, &a.merges)
            }
            "analysis.start" | "analysis.resume" => {
                let open = self.open()?;
                if !self.pipeline.resume() {
                    self.start_analysis(&open);
                }
                Ok(json!({"ok": true}))
            }
            "analysis.pause" => {
                self.pipeline.pause();
                Ok(json!({"ok": true}))
            }
            // Stop is remembered: analysis does not start on its own until Resume or Add media.
            "analysis.stop" => {
                if let Some(open) = self.current() {
                    if !self.pipeline.state().phase.is_moving() {
                        let _ = open.db.set_stopped_by_user(true);
                    }
                }
                self.pull_cancel.store(true, Ordering::SeqCst);
                self.pipeline.stop();
                Ok(json!({"ok": true}))
            }
            "analysis.categorizeNow" => {
                let open = self.open()?;
                self.pipeline.categorize_now(self.env(&open));
                Ok(json!({"ok": true}))
            }
            "analysis.resort" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct A {
                    model: SortChoice,
                    #[serde(default = "yes")]
                    keep_edits: bool,
                }
                fn yes() -> bool {
                    true
                }
                let a: A = arg(args)?;
                let open = self.open()?;
                match self.pipeline.resort(self.env(&open), a.model, a.keep_edits) {
                    Ok(()) => Ok(json!({"ok": true})),
                    Err(reason) => Ok(json!({"ok": false, "error": reason})),
                }
            }
            "bin.preview" => {
                let open = self.open()?;
                let registry = self.registry.clone();
                let dir = open.project.path.clone();
                let shared = move |paths: &[String]| projects::shared_with_other_projects(&registry, &dir, paths);
                let job = BinFilesJob::new(&open.db, &open.project.path, &shared);
                let preview = job.preview()?;
                let check = job.check_now()?;
                Ok(json!({
                    "preview": preview,
                    "unexpected": check.unexpected.iter().map(|p| json!({
                        "path": p,
                        "name": paths::file_name(p),
                        "folder": paths::file_name(paths::parent(p)),
                        "isMedia": media::kind_of(p).is_some(),
                        "modified": crate::files::last_modified(p),
                        "sizeText": moves::format_bytes(crate::files::size(p)),
                    })).collect::<Vec<_>>(),
                    "missing": check.missing.iter().map(|p| json!({"name": paths::file_name(p), "folder": paths::file_name(paths::parent(p))})).collect::<Vec<_>>(),
                    // Nothing to move, but the last run's folder check found something: a tap runs only the check.
                    "checkOnly": preview.files == 0 && open.db.last_check_ok()? == Some(false),
                    "projectName": open.project.name,
                    "copyBytesText": moves::format_bytes(preview.copy_bytes),
                    "freeBytesText": moves::format_bytes(preview.free_bytes),
                    "enoughSpace": preview.free_bytes >= preview.copy_bytes + moves::MIN_FREE_BYTES,
                    "projectPath": paths::display(&open.project.path),
                }))
            }
            "bin.start" => {
                let open = self.open()?;
                if self.adds_running.load(Ordering::SeqCst) > 0 {
                    return Ok(json!({"ok": false, "error": MOVE_WAITS_FOR_ADD}));
                }
                match self.pipeline.start_move(self.env(&open), MoveJob::BinFiles, false) {
                    Ok(()) => Ok(json!({"ok": true})),
                    Err(reason) => Ok(json!({"ok": false, "error": reason})),
                }
            }
            "bin.report" => {
                let open = self.open()?;
                let none = |_: &[String]| Vec::new();
                let job = BinFilesJob::new(&open.db, &open.project.path, &none);
                let report = job.unread_report()?;
                Ok(json!({"report": report.map(|r| {
                    let mut v = serde_json::to_value(&r).unwrap_or(Value::Null);
                    v["leftOut"] = json!(r.left_out.iter().map(|l| json!({"itemId": l.item_id, "reason": l.reason, "name": l.name, "why": failure::describe(l.failure.as_deref())})).collect::<Vec<_>>());
                    v
                })}))
            }
            "bin.closeReport" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct A {
                    run_id: i64,
                }
                let a: A = arg(args)?;
                self.open()?.db.close_report(a.run_id)?;
                Ok(json!({"ok": true}))
            }
            // The folder check's two answers for files no item points to: leave them, or add them.
            "bin.leave" => {
                let a: PathsArg = arg(args)?;
                self.open()?.db.add_removed(&a.paths, "left")?;
                Ok(json!({"ok": true}))
            }
            "bin.add" => {
                let a: PathsArg = arg(args)?;
                let open = self.open()?;
                let added = projects::register_in_place(&open.db, &self.ffmpeg(), &a.paths);
                self.pipeline.refresh_counts(&open.db);
                if !added.is_empty() {
                    self.start_analysis(&open);
                }
                let left: Vec<&String> = a.paths.iter().filter(|p| !added.contains(p)).collect();
                Ok(json!({"ok": true, "added": added.len(), "notAdded": left}))
            }
            "settings.set" => {
                let mut next: AppSettings = {
                    let mut merged = serde_json::to_value(self.settings())?;
                    if let (Some(m), Some(a)) = (merged.as_object_mut(), args.as_object()) {
                        for (k, v) in a {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    serde_json::from_value(merged).map_err(|e| Error::msg(format!("Bad setting: {e}")))?
                };
                if args.get("sortingModel").is_some() {
                    next.sorting_model_chosen_by_user = true;
                }
                self.settings_store.save(&next)?;
                *self.settings.lock() = self.settings_store.load();
                self.refresh_genie();
                // A run started before the change keeps its server; otherwise the next run starts
                // one for the settings as they are now, and none is left running for nothing.
                if !self.pipeline.is_running() {
                    self.genie_server.stop();
                }
                Ok(self.boot())
            }
            "apiKey.set" => {
                #[derive(Deserialize)]
                struct A {
                    key: String,
                }
                let a: A = arg(args)?;
                if a.key.trim().is_empty() {
                    return Err(Error::msg("Paste the key first"));
                }
                self.keys.set(&a.key).map_err(Error::msg)?;
                // Saving a key switches sorting to Claude unless the user already chose a model.
                let before = self.settings();
                let s = before.clone().after_key_saved();
                if s != before {
                    self.settings_store.save(&s)?;
                    *self.settings.lock() = s;
                }
                Ok(self.boot())
            }
            "apiKey.clear" => {
                self.keys.clear().map_err(Error::msg)?;
                Ok(self.boot())
            }
            "genie.status" => {
                self.refresh_genie();
                Ok(self.boot())
            }
            "genie.pull" => {
                #[derive(Deserialize)]
                struct A {
                    model: String,
                }
                let a: A = arg(args)?;
                let spec = if a.model == LFM3_1_7B.id { LFM3_1_7B } else { LFM3_VL_2B };
                self.pull_model(spec)
            }
            "install" => {
                #[derive(Deserialize)]
                struct A {
                    what: String,
                }
                let a: A = arg(args)?;
                let package = crate::install::package(&a.what).ok_or_else(|| Error::msg(format!("Nothing called {} can be installed", a.what)))?;
                self.install(package)
            }
            "genie.cancelPull" => {
                self.pull_cancel.store(true, Ordering::SeqCst);
                Ok(json!({"ok": true}))
            }
            "log" => Ok(json!({"lines": self.log.lock().iter().cloned().collect::<Vec<_>>()})),
            other => Err(Error::msg(format!("Unknown command: {other}"))),
        }
    }

    /// Stops any run and waits for it, before the app closes.
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.pull_cancel.store(true, Ordering::SeqCst);
        self.pipeline.stop_and_wait();
        self.genie_server.stop();
    }
}

fn analysis_is_simulated(db: &ProjectDb, id: i64) -> bool {
    db.analysis(id).ok().flatten().is_some_and(|a| a.model_id == MOCK_VISION_ID)
}

#[cfg(test)]
#[path = "app_android_cases.rs"]
mod android_cases;

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(ready: usize, binned: usize, pending: usize, waiting: usize, failed: usize) -> MediaCounts {
        MediaCounts { total: ready + binned + pending + waiting + failed, pending, analyzing: 0, waiting, failed, missing: 0, ready_to_bin: ready, binned }
    }

    #[test]
    fn status_lines() {
        let s = media_status(&counts(35, 0, 0, 0, 0), None, false);
        assert_eq!((s.text.as_str(), s.tone, s.bin_files_enabled), ("35 files ready to bin", "Ready", true));
        let s = media_status(&counts(3, 0, 0, 2, 1), None, false);
        assert_eq!((s.text.as_str(), s.categorize_now, s.bin_files_enabled), ("3 files ready to bin · 2 waiting to be categorized", true, false));
        assert_eq!(s.blocked_reason.as_deref(), Some("Bin Files waits for 2 items to be categorized"));
        let s = media_status(&counts(0, 4, 0, 0, 1), None, false);
        assert_eq!((s.text.as_str(), s.tone), ("All files binned · 1 failed", "Quiet"));
        assert_eq!(media_status(&counts(0, 0, 0, 0, 0), None, false).text, "Nothing to bin yet");
        assert!(media_status(&counts(0, 4, 0, 0, 0), None, true).bin_files_enabled);
        let s = media_status(&counts(5, 0, 0, 0, 0), Some(ActiveJob::Binning { done: 2, total: 5, paused: false }), false);
        assert_eq!(s.text, "Binning · 3 of 5 files");
        assert_eq!(media_status(&counts(1, 0, 7, 0, 0), Some(ActiveJob::Analysis), false).text, "Analyzing 7 left");
    }

    #[test]
    fn category_name_clashes() {
        let existing = vec!["Sea Lions".to_string(), "Beach".to_string()];
        assert_eq!(clash("the sea-lion!", &existing, None).as_deref(), Some("Sea Lions"));
        assert_eq!(clash("Beaches", &existing, Some("Beach")), None);
        assert_eq!(clash("Sunsets", &existing, None), None);
    }
}
