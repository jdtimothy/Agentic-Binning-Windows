//! Runs one job at a time for the open project on a worker thread: analysis batches followed by
//! categorizing, a Re-sort, or a file move. Ported from the Android app's `pipeline` package.
//!
//! reset interrupted items → analyze each Pending item → (more Pending items? another pass) →
//! categorize → Idle.
//!
//! A failure that is not the item's fault (the model, the app's storage) ends the run in Stopped
//! with an error and the item back in Pending, as do three Failed items in a row. Pause lets the
//! current item finish and holds. Stop cancels the current generation, returns that item to
//! Pending and ends in Stopped; Resume after a stop starts a new run.

use crate::analysis::{self, prompt, PartialAnalysis};
use crate::db::{Item, ItemStatus, Profile, ProjectDb};
use crate::media::{MediaPreparer, PrepareError};
use crate::models::{ReplyFormat, SortUnavailable, TextError, TextModel, VisionModel};
use crate::moves::{BinFilesJob, MoveControl, MoveOutcome, MoveProgress};
use crate::sorting::{self, planner, schema, validator, CategoryInfo, NewCategoryEntity, SortItem, Validation, SORT_PROMPT_VERSION};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// The codes stored in `item.failureReason`, and how the app words them for people.
pub mod failure {
    pub const DECODE: &str = "decode";
    pub const LENGTH: &str = "length";
    pub const INVALID_JSON: &str = "invalid-json";
    pub const PROMPT: &str = "prompt";
    pub const MODEL: &str = "model";
    pub const INTERRUPTED: &str = "interrupted";
    /// A video, with neither FFmpeg nor the app's window able to read it.
    pub const NO_VIDEO_DECODER: &str = "no-video-decoder";

    pub const OUT_OF_SPACE: &str = "The computer ran out of space for analysis files";
    pub const MODEL_FAILED: &str = "The vision model failed";
    pub const MODEL_NOT_STARTED: &str = "The vision model couldn't start";
    pub const MODEL_NOT_DOWNLOADED: &str = "The vision model couldn't be downloaded";

    /// A short sentence for `code`; unknown or missing codes get a generic one.
    pub fn describe(code: Option<&str>) -> &'static str {
        match code {
            Some(DECODE) => "Couldn't read this file.",
            Some(LENGTH) => "The model's reply was cut off.",
            Some(INVALID_JSON) => "The model's reply wasn't readable.",
            Some(PROMPT) => "Couldn't build the model's prompt for this file.",
            Some(MODEL) => "The vision model failed on this file.",
            Some(INTERRUPTED) => "Stopped here last time the app closed unexpectedly.",
            Some(NO_VIDEO_DECODER) => "Videos need FFmpeg installed, or a format Windows can play.",
            _ => "Couldn't analyze this file.",
        }
    }
}

pub const FAIL_INTERRUPTED_AFTER: i64 = 2;
pub const MAX_FAILURES_IN_A_ROW: usize = 3;
const MAX_ATTEMPTS: usize = 2;

pub const SAVE_BUSY: &str = "Saving moves files, so it waits until analysis is done";
pub const MOVE_BUSY: &str = "Bin Files is available when analysis is stopped or done";
pub const MOVE_NOT_OPEN: &str = "Open the project to bin its files";
pub const RESORT_BUSY: &str = "Re-sort is available when analysis is stopped or done";
pub const RESORT_NOT_OPEN: &str = "Open the project to re-sort it";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortChoice {
    Claude,
    #[serde(rename = "Lfm3_1_7B")]
    Lfm3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Phase {
    Idle,
    LoadingModel,
    Preparing,
    Describing,
    Paused,
    Stopped,
    Releasing,
    Categorizing,
    Moving,
    MovingPaused,
}

impl Phase {
    /// True while a run is in progress (including Paused).
    pub fn is_running(self) -> bool {
        !matches!(self, Phase::Idle | Phase::Stopped)
    }
    pub fn is_moving(self) -> bool {
        matches!(self, Phase::Moving | Phase::MovingPaused)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineState {
    pub phase: Phase,
    /// The open project's folder.
    pub project: Option<String>,
    pub current_item_id: Option<i64>,
    pub partial: Option<PartialAnalysis>,
    pub last_profile: Option<Profile>,
    /// Items still to analyze, including the current one.
    pub pending: i64,
    pub analyzed: i64,
    pub total: i64,
    /// Failed and Missing items: done for progress purposes.
    pub failed: i64,
    /// Set while the vision model is being downloaded (0..1).
    pub download_fraction: Option<f64>,
    /// Why the last run ended in Stopped without being asked to.
    pub error: Option<String>,
    pub frames_ready: usize,
    pub frames_total: usize,
    pub sort_done: usize,
    pub sort_total: usize,
    /// Why the last categorization left items uncategorized.
    pub sort_notice: Option<String>,
    pub move_progress: Option<MoveProgress>,
    /// How the last move ended: `Done`, `Stopped`, `StorageFull`, `Failed`, `Blocked` or `NothingToMove`.
    pub move_outcome: Option<String>,
    pub move_message: Option<String>,
    /// Counts finished moves: a change means one just ended.
    pub move_ended: u64,
}

impl PipelineState {
    fn idle(project: Option<String>) -> Self {
        PipelineState {
            phase: Phase::Idle,
            project,
            current_item_id: None,
            partial: None,
            last_profile: None,
            pending: 0,
            analyzed: 0,
            total: 0,
            failed: 0,
            download_fraction: None,
            error: None,
            frames_ready: 0,
            frames_total: 0,
            sort_done: 0,
            sort_total: 0,
            sort_notice: None,
            move_progress: None,
            move_outcome: None,
            move_message: None,
            move_ended: 0,
        }
    }
}

/// Everything a run needs, captured when it starts so a settings change applies to the next run.
pub struct RunEnv {
    pub project_dir: String,
    pub db: Arc<ProjectDb>,
    pub vision: Arc<dyn VisionModel>,
    pub text_for: Box<dyn Fn(SortChoice) -> Arc<dyn TextModel> + Send + Sync>,
    pub sort_choice: SortChoice,
    pub max_frames: u32,
    pub preparer: MediaPreparer,
    /// Makes sure the vision model's weights are on the computer, reporting download progress (0..1).
    pub ensure_vision: Box<dyn Fn(&dyn Fn(f64)) -> Result<(), String> + Send + Sync>,
    /// None if this sorting model can run now; otherwise why not.
    pub sorting_available: Box<dyn Fn(SortChoice) -> Option<String> + Send + Sync>,
    /// Which of these paths another listed project also holds.
    pub shared: Box<dyn Fn(&[String]) -> Vec<String> + Send + Sync>,
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MoveJob {
    BinFiles,
    Save(i64),
    Resume(i64),
}

enum Job {
    Run { analyze: bool, resort: Option<(SortChoice, bool)> },
    Move(MoveJob, Arc<MoveControl>),
}

#[derive(Clone, Copy, PartialEq)]
enum Control {
    Run,
    Pause,
    Stop,
}

struct Stopped;

struct Inner {
    state: Mutex<PipelineState>,
    control: Mutex<Control>,
    changed: Condvar,
    worker: Mutex<Option<JoinHandle<()>>>,
    busy: AtomicBool,
    /// The run's first model command has been written to the log.
    command_logged: AtomicBool,
    accepting_reruns: AtomicBool,
    rerun: AtomicBool,
    move_control: Mutex<Option<Arc<MoveControl>>>,
    vision: Mutex<Option<Arc<dyn VisionModel>>>,
    text: Mutex<Option<Arc<dyn TextModel>>>,
    on_state: Box<dyn Fn(&PipelineState) + Send + Sync>,
    /// Called after anything changed in the project's database.
    on_data: Box<dyn Fn() + Send + Sync>,
}

impl Inner {
    fn update(&self, change: impl FnOnce(&mut PipelineState)) {
        let snapshot = {
            let mut s = self.state.lock();
            change(&mut s);
            s.clone()
        };
        (self.on_state)(&snapshot);
    }

    fn stopping(&self) -> bool {
        *self.control.lock() == Control::Stop
    }

    fn check_stop(&self) -> Result<(), Stopped> {
        if self.stopping() {
            Err(Stopped)
        } else {
            Ok(())
        }
    }

    fn refresh_counts(&self, db: &ProjectDb) {
        if let Ok(items) = db.grid() {
            let count = |f: &dyn Fn(ItemStatus) -> bool| items.iter().filter(|i| f(i.status)).count() as i64;
            let pending = count(&|s| matches!(s, ItemStatus::Pending | ItemStatus::Analyzing));
            let analyzed = count(&|s| matches!(s, ItemStatus::Analyzed | ItemStatus::Sorted | ItemStatus::Binned));
            let failed = count(&|s| matches!(s, ItemStatus::Failed | ItemStatus::Missing));
            self.update(|s| {
                s.pending = pending;
                s.analyzed = analyzed;
                s.failed = failed;
                s.total = items.len() as i64;
            });
        }
        (self.on_data)();
    }
}

/// The one job runner. Cheap to clone.
#[derive(Clone)]
pub struct Pipeline {
    inner: Arc<Inner>,
}

enum ItemOutcome {
    Analyzed,
    Failed(&'static str),
    Missing,
}

impl Pipeline {
    pub fn new(on_state: impl Fn(&PipelineState) + Send + Sync + 'static, on_data: impl Fn() + Send + Sync + 'static) -> Self {
        Pipeline {
            inner: Arc::new(Inner {
                state: Mutex::new(PipelineState::idle(None)),
                control: Mutex::new(Control::Run),
                changed: Condvar::new(),
                worker: Mutex::new(None),
                busy: AtomicBool::new(false),
                command_logged: AtomicBool::new(false),
                accepting_reruns: AtomicBool::new(false),
                rerun: AtomicBool::new(false),
                move_control: Mutex::new(None),
                vision: Mutex::new(None),
                text: Mutex::new(None),
                on_state: Box::new(on_state),
                on_data: Box::new(on_data),
            }),
        }
    }

    pub fn state(&self) -> PipelineState {
        self.inner.state.lock().clone()
    }

    pub fn is_running(&self) -> bool {
        self.inner.busy.load(Ordering::SeqCst)
    }

    /// Shows `project`'s counts with nothing running (after opening it).
    pub fn show_project(&self, project_dir: Option<&str>, db: Option<&ProjectDb>, sort_notice: Option<String>) {
        self.inner.update(|s| {
            let ended = s.move_ended;
            *s = PipelineState::idle(project_dir.map(str::to_string));
            s.move_ended = ended;
            s.sort_notice = sort_notice;
        });
        if let Some(db) = db {
            self.inner.refresh_counts(db);
        }
    }

    pub fn refresh_counts(&self, db: &ProjectDb) {
        self.inner.refresh_counts(db);
    }

    fn join_finished(&self) {
        if let Some(h) = self.inner.worker.lock().take() {
            let _ = h.join();
        }
    }

    fn spawn(&self, env: RunEnv, job: Job) {
        self.join_finished();
        let inner = self.inner.clone();
        *inner.control.lock() = Control::Run;
        inner.busy.store(true, Ordering::SeqCst);
        inner.command_logged.store(false, Ordering::SeqCst);
        inner.rerun.store(false, Ordering::SeqCst);
        inner.accepting_reruns.store(matches!(job, Job::Run { analyze: true, .. }), Ordering::SeqCst);
        let first_phase = match &job {
            Job::Run { analyze: true, .. } => Phase::LoadingModel,
            Job::Run { .. } => Phase::Categorizing,
            Job::Move(..) => Phase::Moving,
        };
        let dir = env.project_dir.clone();
        inner.update(|s| {
            let same = s.project.as_deref() == Some(dir.as_str());
            let mut next = PipelineState::idle(Some(dir.clone()));
            next.phase = first_phase;
            next.move_ended = s.move_ended;
            if same {
                next.last_profile = s.last_profile.clone();
                next.pending = s.pending;
                next.analyzed = s.analyzed;
                next.total = s.total;
                next.failed = s.failed;
                next.sort_notice = s.sort_notice.clone();
            }
            *s = next;
        });
        let thread_inner = inner.clone();
        let handle = std::thread::Builder::new()
            .name("binning-pipeline".into())
            .spawn(move || {
                match job {
                    Job::Run { analyze, resort } => run(&thread_inner, &env, analyze, resort),
                    Job::Move(job, control) => run_move(&thread_inner, &env, job, &control),
                }
                *thread_inner.vision.lock() = None;
                *thread_inner.text.lock() = None;
                *thread_inner.move_control.lock() = None;
                thread_inner.accepting_reruns.store(false, Ordering::SeqCst);
                thread_inner.busy.store(false, Ordering::SeqCst);
                (thread_inner.on_data)();
            })
            .expect("spawn pipeline thread");
        *inner.worker.lock() = Some(handle);
    }

    /// Starts a run for the project in `env`. While a run for it is in progress this asks that run
    /// to look for Pending items once more before it ends; it does not lift a pause.
    pub fn start(&self, env: RunEnv) {
        if self.is_running() {
            if self.inner.accepting_reruns.load(Ordering::SeqCst) {
                self.inner.rerun.store(true, Ordering::SeqCst);
                return;
            }
            if self.inner.state.lock().phase.is_moving() {
                return; // Nothing starts while files move, and nothing queues behind the move.
            }
            // A run that is categorizing or winding down: wait for it, then start afresh.
            self.join_finished();
        }
        self.spawn(env, Job::Run { analyze: true, resort: None });
    }

    /// Starts a run that skips analysis and only categorizes the uncategorized items. A run still
    /// active categorizes anyway, so this adds nothing to it.
    pub fn categorize_now(&self, env: RunEnv) {
        if self.is_running() {
            return;
        }
        self.spawn(env, Job::Run { analyze: false, resort: None });
    }

    /// Re-sorts with `model`: refused while a run is in progress, never queued.
    pub fn resort(&self, env: RunEnv, model: SortChoice, keep_edits: bool) -> Result<(), &'static str> {
        if self.is_running() {
            return Err(RESORT_BUSY);
        }
        self.spawn(env, Job::Run { analyze: false, resort: Some((model, keep_edits)) });
        Ok(())
    }

    /// Bin Files, a Save's written plan, or finishing a run the app didn't: refused while any run
    /// is in progress, never queued. Pause, Resume and Stop then act between files.
    pub fn start_move(&self, env: RunEnv, job: MoveJob, paused: bool) -> Result<(), &'static str> {
        if self.is_running() {
            return Err(MOVE_BUSY);
        }
        let control = Arc::new(MoveControl::new(paused));
        *self.inner.move_control.lock() = Some(control.clone());
        self.spawn(env, Job::Move(job, control));
        Ok(())
    }

    /// Holds after the current item (or file); ignored while categorizing.
    pub fn pause(&self) {
        if let Some(m) = self.inner.move_control.lock().as_ref() {
            m.pause();
            return;
        }
        let phase = self.inner.state.lock().phase;
        if self.is_running() && !matches!(phase, Phase::Categorizing | Phase::Paused) {
            let mut c = self.inner.control.lock();
            if *c == Control::Run {
                *c = Control::Pause;
            }
        }
    }

    /// Continues a paused run. Returns false when nothing was paused (the caller then starts a new run).
    pub fn resume(&self) -> bool {
        if let Some(m) = self.inner.move_control.lock().as_ref() {
            m.resume();
            return true;
        }
        if !self.is_running() {
            return false;
        }
        let mut c = self.inner.control.lock();
        if *c == Control::Pause {
            *c = Control::Run;
            self.inner.changed.notify_all();
        }
        true
    }

    /// Cancels the run: the current generation stops, its item returns to Pending, and the run
    /// ends in Stopped. A move finishes the file in progress first.
    pub fn stop(&self) {
        if !self.is_running() {
            return;
        }
        *self.inner.control.lock() = Control::Stop;
        self.inner.changed.notify_all();
        if let Some(m) = self.inner.move_control.lock().as_ref() {
            m.stop();
        }
        if let Some(v) = self.inner.vision.lock().as_ref() {
            v.cancel();
        }
        if let Some(t) = self.inner.text.lock().as_ref() {
            t.cancel();
        }
    }

    /// Stops and waits until the worker has ended (before switching projects or closing).
    pub fn stop_and_wait(&self) {
        self.stop();
        self.join_finished();
    }

    /// Waits for the current job to end by itself.
    pub fn wait(&self) {
        self.join_finished();
    }
}

fn run(inner: &Inner, env: &RunEnv, analyze: bool, resort: Option<(SortChoice, bool)>) {
    let mut current: Option<i64> = None;
    let result: Result<Option<String>, Result<Stopped, String>> = (|| {
        if analyze {
            let _ = env.db.recover_interrupted(FAIL_INTERRUPTED_AFTER, failure::INTERRUPTED);
            inner.refresh_counts(&env.db);
            loop {
                if env.db.next_pending().map_err(|e| Err(e.to_string()))?.is_some() {
                    run_batch(inner, env, &mut current)?;
                    current = None;
                }
                if !inner.rerun.swap(false, Ordering::SeqCst) {
                    inner.accepting_reruns.store(false, Ordering::SeqCst);
                    // A start that slipped in between the two lines above still gets its pass.
                    if !inner.rerun.swap(false, Ordering::SeqCst) {
                        break;
                    }
                    inner.accepting_reruns.store(true, Ordering::SeqCst);
                }
            }
        } else {
            inner.refresh_counts(&env.db);
        }
        inner.check_stop().map_err(Ok)?;
        categorize(inner, env, resort).map_err(Ok)
    })();
    inner.accepting_reruns.store(false, Ordering::SeqCst);
    match result {
        Ok(notice) => {
            inner.refresh_counts(&env.db);
            inner.update(|s| {
                s.phase = Phase::Idle;
                s.current_item_id = None;
                s.sort_notice = notice;
                s.sort_done = 0;
                s.sort_total = 0;
            });
        }
        Err(end) => {
            if let Some(id) = current {
                let _ = env.db.return_to_pending(id);
            }
            let error = end.err();
            if let Some(e) = &error {
                (env.log)(&format!("Run stopped: {e}"));
            }
            inner.refresh_counts(&env.db);
            inner.update(|s| {
                s.phase = Phase::Stopped;
                s.current_item_id = None;
                s.partial = None;
                s.download_fraction = None;
                s.error = error;
                s.sort_done = 0;
                s.sort_total = 0;
            });
        }
    }
}

type RunEnd = Result<Stopped, String>;

/// Checks the model, then analyzes every Pending item.
fn run_batch(inner: &Inner, env: &RunEnv, current: &mut Option<i64>) -> Result<(), RunEnd> {
    inner.update(|s| s.phase = Phase::LoadingModel);
    *inner.vision.lock() = Some(env.vision.clone());
    (env.ensure_vision)(&|fraction| inner.update(|s| s.download_fraction = Some(fraction))).map_err(|e| {
        (env.log)(&format!("Could not fetch the vision model: {e}"));
        Err(failure::MODEL_NOT_DOWNLOADED.to_string())
    })?;
    inner.update(|s| s.download_fraction = None);
    inner.check_stop().map_err(Ok)?;
    env.vision.load(env.max_frames).map_err(|e| {
        (env.log)(&format!("Could not load the vision model: {}", e.0));
        Err(failure::MODEL_NOT_STARTED.to_string())
    })?;
    let mut failures_in_a_row = 0;
    loop {
        {
            let mut c = inner.control.lock();
            if *c == Control::Pause {
                drop(c);
                inner.update(|s| {
                    s.phase = Phase::Paused;
                    s.current_item_id = None;
                });
                c = inner.control.lock();
                while *c == Control::Pause {
                    inner.changed.wait(&mut c);
                }
            }
            if *c == Control::Stop {
                return Err(Ok(Stopped));
            }
        }
        let Some(item) = env.db.next_pending().map_err(|e| Err(e.to_string()))? else { break };
        *current = Some(item.id);
        env.db.mark_analyzing(item.id).map_err(|e| Err(e.to_string()))?;
        inner.update(|s| {
            s.phase = Phase::Preparing;
            s.current_item_id = Some(item.id);
            s.partial = None;
            s.frames_ready = 0;
            s.frames_total = 0;
        });
        (inner.on_data)();
        let outcome = analyze_item(inner, env, &item)?;
        *current = None;
        inner.refresh_counts(&env.db);
        match outcome {
            ItemOutcome::Analyzed => failures_in_a_row = 0,
            ItemOutcome::Failed(reason) => {
                failures_in_a_row += 1;
                if failures_in_a_row >= MAX_FAILURES_IN_A_ROW {
                    return Err(Err(format!("Analysis stopped after repeated failures: {}", failure::describe(Some(reason)))));
                }
            }
            ItemOutcome::Missing => {} // A missing file says nothing about the rest.
        }
    }
    inner.update(|s| {
        s.phase = Phase::Releasing;
        s.current_item_id = None;
    });
    Ok(())
}

/// Analyzes one item: prepare, generate (retrying once on a `length` stop or unparseable JSON),
/// parse and save. The preparer's work files are always cleaned up.
fn analyze_item(inner: &Inner, env: &RunEnv, item: &Item) -> Result<ItemOutcome, RunEnd> {
    let fail = |reason: &'static str| -> Result<ItemOutcome, RunEnd> {
        env.db.set_status(item.id, ItemStatus::Failed, Some(reason)).map_err(|e| Err(e.to_string()))?;
        Ok(ItemOutcome::Failed(reason))
    };
    let prepared = env.preparer.prepare(item.id, &item.current_path, item.kind, item.duration_ms, env.vision.image_edge(), env.max_frames as usize, &|ready, total| {
        inner.update(|s| {
            s.frames_ready = ready;
            s.frames_total = total;
        })
    });
    let media = match prepared {
        Ok(m) => m,
        Err(PrepareError::Missing) => {
            env.preparer.cleanup(item.id);
            env.db.set_status(item.id, ItemStatus::Missing, None).map_err(|e| Err(e.to_string()))?;
            return Ok(ItemOutcome::Missing);
        }
        Err(PrepareError::WorkFile(e)) => {
            env.preparer.cleanup(item.id);
            (env.log)(&format!("Cannot write the work files of item {}: {e}", item.id));
            return Err(Err(failure::OUT_OF_SPACE.to_string()));
        }
        Err(PrepareError::NoVideoDecoder) => {
            env.preparer.cleanup(item.id);
            return fail(failure::NO_VIDEO_DECODER);
        }
        Err(PrepareError::Decode(e)) => {
            env.preparer.cleanup(item.id);
            (env.log)(&format!("Cannot prepare item {} ({}): {e}", item.id, item.original_name));
            return fail(failure::DECODE);
        }
    };
    (inner.on_data)(); // The thumbnail exists now.
    if inner.stopping() {
        env.preparer.cleanup(item.id);
        return Err(Ok(Stopped));
    }
    inner.update(|s| s.phase = Phase::Describing);
    let mut reason = failure::INVALID_JSON;
    let mut outcome = None;
    for _ in 0..MAX_ATTEMPTS {
        let mut text = String::new();
        let mut last_emit = std::time::Instant::now();
        let reply = env.vision.generate(&media, &mut |token| {
            text.push_str(token);
            // The window redraws at most about 25 times a second.
            if last_emit.elapsed().as_millis() >= 40 {
                last_emit = std::time::Instant::now();
                let partial = analysis::partial(&text);
                inner.update(|s| s.partial = Some(partial));
            }
        });
        // The first command of a run goes to the log, so it can be tried by hand in a terminal.
        let failed = reply.is_err();
        if failed || !inner.command_logged.swap(true, Ordering::SeqCst) {
            if let Some(command) = env.vision.last_command() {
                (env.log)(&format!("Ran: {command}"));
            }
        }
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                env.preparer.cleanup(item.id);
                if inner.stopping() {
                    return Err(Ok(Stopped));
                }
                (env.log)(&format!("The vision model failed on item {}: {}", item.id, e.0));
                return Err(Err(format!("{}: {}", failure::MODEL_FAILED, e.0)));
            }
        };
        let partial = analysis::partial(&reply.text);
        inner.update(|s| {
            s.partial = Some(partial);
            s.last_profile = Some(reply.profile.clone());
        });
        if reply.profile.stop_reason == "user" || inner.stopping() {
            env.preparer.cleanup(item.id);
            return Err(Ok(Stopped));
        }
        if reply.profile.stop_reason == "length" {
            reason = failure::LENGTH;
            continue;
        }
        match analysis::parse(&reply.text, item.kind) {
            Some(mut result) => {
                if item.kind == analysis::ItemKind::Video {
                    match moment_actions(inner, env, item, &media) {
                        Ok(actions) => result.actions = actions,
                        Err(end) => {
                            env.preparer.cleanup(item.id);
                            return Err(end);
                        }
                    }
                }
                env.db.save_analysis(item.id, &result, &reply.text, &env.vision.model_id(), prompt::PROMPT_VERSION, &reply.profile).map_err(|e| Err(e.to_string()))?;
                outcome = Some(ItemOutcome::Analyzed);
                break;
            }
            None => reason = failure::INVALID_JSON,
        }
    }
    env.preparer.cleanup(item.id);
    match outcome {
        Some(o) => Ok(o),
        None => fail(reason),
    }
}

/// Puts the actions found so far into the reply the window is showing.
fn show(inner: &Inner, actions: &[analysis::ActionEntry]) {
    let so_far = actions.to_vec();
    inner.update(|s| {
        if let Some(p) = s.partial.as_mut() {
            p.actions = so_far;
        }
    });
}

/// A clip's actions: the app picks the moments from how much the frames change, the model says
/// in one short call each what happens there, and the time is the frame's own. Each action shows
/// in the window as it arrives. A clip in which nothing changes gets none.
fn moment_actions(inner: &Inner, env: &RunEnv, item: &Item, media: &crate::media::PreparedMedia) -> Result<Vec<analysis::ActionEntry>, RunEnd> {
    let changes = crate::media::frame_changes(&media.image_paths);
    let picked = analysis::moments::pick(&changes);
    (env.log)(&format!(
        "Moments of item {}: frames {:?} of {} (change per frame: {})",
        item.id,
        picked,
        changes.len(),
        changes.iter().map(|c| format!("{c:.0}")).collect::<Vec<_>>().join(" ")
    ));
    let mut actions: Vec<analysis::ActionEntry> = Vec::new();
    for frame in picked {
        if inner.stopping() {
            return Err(Ok(Stopped));
        }
        let reply = match env.vision.describe_moment(&media.image_paths[frame]) {
            Ok(r) => r,
            Err(e) => {
                if inner.stopping() {
                    return Err(Ok(Stopped));
                }
                (env.log)(&format!("The vision model failed on a moment of item {}: {}", item.id, e.0));
                return Err(Err(format!("{}: {}", failure::MODEL_FAILED, e.0)));
            }
        };
        if reply.profile.stop_reason == "user" || inner.stopping() {
            return Err(Ok(Stopped));
        }
        let Some(text) = analysis::moments::clean(&reply.text) else { continue };
        // The same thing in other words says nothing new.
        if actions.iter().any(|a| analysis::moments::same_thing(&a.text, &text)) {
            continue;
        }
        let time = media.frame_times_ms.get(frame).copied().unwrap_or(0);
        actions.push(analysis::ActionEntry { t: analysis::moments::clock(time), text });
        show(inner, &actions);
    }
    // The opening alone only repeats the description: every later moment said the same thing.
    if actions.len() == 1 {
        actions.clear();
        show(inner, &actions);
    }
    Ok(actions)
}

// ---------------------------------------------------------------------------------------------
// Categorizing
// ---------------------------------------------------------------------------------------------

pub const SORT_STOPPED: &str = "Stopped";
pub const SORT_INTERRUPTED: &str = "Interrupted";
pub const DETAILS_SEPARATOR: &str = " · ";
const REPLY_TOKENS_PER_ITEM: u32 = 30;
const REPLY_TOKENS_EXTRA: u32 = 600;
const PROPOSE_MAX_TOKENS: u32 = 1000;
/// The most model calls one planned chunk may make across its splits and retries.
const MAX_CALLS_PER_CHUNK: usize = 6;
const UNREADABLE: &str = "The reply was not a JSON object with new_categories and assignments. Reply with the JSON object only.";

/// The sort notice to show when no live one exists, from the project's finished runs, newest
/// first: the newest run that wasn't stopped or interrupted decides, unless it finished `done`.
pub fn stored_sort_notice(runs: &[crate::db::SortRun]) -> Option<String> {
    let last = runs.iter().find(|r| {
        let m = r.message.as_deref().unwrap_or("");
        ![SORT_STOPPED, SORT_INTERRUPTED].iter().any(|w| m == *w || m.starts_with(&format!("{w}{DETAILS_SEPARATOR}")))
    })?;
    if last.outcome == "done" {
        return None;
    }
    last.message.as_deref().and_then(|m| m.split(DETAILS_SEPARATOR).next()).map(str::trim).filter(|m| !m.is_empty()).map(str::to_string)
}

#[derive(Default)]
struct Tally {
    items: usize,
    sorted: usize,
    already: usize,
    left_out: usize,
    skipped_chunks: usize,
}

impl Tally {
    fn left(&self) -> usize {
        self.items.saturating_sub(self.sorted + self.already)
    }
    fn details(&self) -> Option<String> {
        let parts: Vec<String> = [("skipped chunks", self.skipped_chunks), ("left out", self.left_out), ("already categorized", self.already)]
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(label, n)| format!("{label}: {n}"))
            .collect();
        (!parts.is_empty()).then(|| parts.join(DETAILS_SEPARATOR))
    }
}

fn joined(first: &str, second: Option<String>) -> String {
    match second {
        Some(s) => format!("{first}{DETAILS_SEPARATOR}{s}"),
        None => first.to_string(),
    }
}

enum SortEnd {
    Stopped,
    Unavailable(SortUnavailable, String),
    Failed(String),
}

impl From<TextError> for SortEnd {
    fn from(e: TextError) -> Self {
        match e {
            TextError::Unavailable(reason, message) => SortEnd::Unavailable(reason, message),
            TextError::Other(m) => SortEnd::Failed(m),
        }
    }
}

impl From<crate::error::Error> for SortEnd {
    fn from(e: crate::error::Error) -> Self {
        SortEnd::Failed(e.to_string())
    }
}

/// Sorts the uncategorized items and returns the new sort notice (None when all were sorted).
fn categorize(inner: &Inner, env: &RunEnv, resort: Option<(SortChoice, bool)>) -> Result<Option<String>, Stopped> {
    let choice = resort.map(|r| r.0).unwrap_or(env.sort_choice);
    if let Some((model, keep_edits)) = resort {
        inner.update(|s| s.phase = Phase::Categorizing);
        if let Some(reason) = (env.sorting_available)(model) {
            return Ok(Some(reason));
        }
        if let Err(e) = env.db.prepare_resort(keep_edits) {
            (env.log)(&format!("Could not prepare the re-sort: {e}"));
            return Ok(Some("Categorizing ran into a problem; it will try again after the next run".into()));
        }
        (inner.on_data)();
    }
    let items = match env.db.uncategorized() {
        Ok(i) => i,
        Err(e) => {
            (env.log)(&format!("Categorizing failed: {e}"));
            return Ok(Some("Categorizing ran into a problem; it will try again after the next run".into()));
        }
    };
    if items.is_empty() {
        return Ok(None);
    }
    let total = items.len();
    inner.update(|s| {
        s.phase = Phase::Categorizing;
        s.current_item_id = None;
        s.sort_done = 0;
        s.sort_total = total;
    });
    let model = (env.text_for)(choice);
    *inner.text.lock() = Some(model.clone());
    let before = env.db.category_infos().unwrap_or_default();
    if let Ok(n) = env.db.close_open_sort_runs(SORT_INTERRUPTED) {
        if n > 0 {
            (env.log)(&format!("Closed {n} sort run record(s) left open by an earlier run"));
        }
    }
    let run_id = env.db.start_sort_run(&model.model_id(), SORT_PROMPT_VERSION, total as i64).ok();
    let mut tally = Tally { items: total, ..Default::default() };
    let budget = match choice {
        SortChoice::Claude => sorting::BUDGET_CLAUDE,
        SortChoice::Lfm3 => sorting::BUDGET_LFM3_1_7B,
    };
    let sort_items: Vec<SortItem> = items
        .iter()
        .map(|i| SortItem {
            id: i.id,
            kind: i.kind,
            suggested_name: i.suggested_name.clone(),
            tags: i.tags.clone(),
            description: i.description.clone(),
            actions: SortItem::actions_from_json(&i.actions_json),
            name_set_by_user: i.name_set_by_user,
        })
        .collect();
    let result: Result<(), SortEnd> = (|| {
        model.load()?;
        inner.check_stop().map_err(|_| SortEnd::Stopped)?;
        let proposals = match planner::proposal_sample(&sort_items, !before.is_empty(), &budget) {
            Some(sample) => {
                let reply = model.complete(
                    sorting::prompt::SYSTEM,
                    &sorting::prompt::propose_user(&sample),
                    &ReplyFormat { json_schema: schema::propose_json_schema(), grammar: schema::propose_grammar() },
                    PROPOSE_MAX_TOKENS,
                )?;
                inner.check_stop().map_err(|_| SortEnd::Stopped)?;
                let proposed = sorting::parse_reply(&reply.text).map(|r| r.new_categories).unwrap_or_default();
                if proposed.is_empty() {
                    (env.log)(&format!("The proposal pass gave no usable categories (stop {}); sorting without it", reply.stop_reason));
                }
                validator::merge_proposals(&proposed, &before)
            }
            None => Vec::new(),
        };
        let project_items = env.db.item_count()?;
        let mut done = 0;
        while done < total {
            inner.check_stop().map_err(|_| SortEnd::Stopped)?;
            // Each chunk is sized for the category list it will be shown, which grows as chunks add to it.
            let (listed, _) = offered(&env.db, &proposals)?;
            let n = planner::next_chunk_len(&sort_items[done..], &budget, planner::category_tokens(&listed));
            let mut calls = MAX_CALLS_PER_CHUNK;
            sort_chunk(inner, env, model.as_ref(), &sort_items[done..done + n], &proposals, project_items, &budget, &mut tally, &mut calls)?;
            done += n;
            inner.update(|s| s.sort_done = done);
            (inner.on_data)();
        }
        Ok(())
    })();
    *inner.text.lock() = None;
    let new_categories = env.db.category_infos().map(|c| c.len().saturating_sub(before.len())).unwrap_or(0) as i64;
    let finish = |outcome: &str, message: Option<String>| {
        if let Some(id) = run_id {
            let _ = env.db.finish_sort_run(id, tally.sorted as i64, new_categories, outcome, message.as_deref());
        }
    };
    let partial_or_failed = if tally.sorted == 0 { "failed" } else { "partial" };
    match result {
        Ok(()) if tally.left() == 0 => {
            finish("done", tally.details());
            Ok(None)
        }
        Ok(()) => {
            let message = format!("Categorized {} of {total}; {} will be tried again after the next run", tally.sorted, tally.left());
            finish(partial_or_failed, Some(joined(&message, tally.details())));
            Ok(Some(message))
        }
        Err(SortEnd::Stopped) => {
            finish("partial", Some(joined(SORT_STOPPED, tally.details())));
            Err(Stopped)
        }
        Err(SortEnd::Unavailable(reason, message)) => {
            if inner.stopping() {
                finish("partial", Some(joined(SORT_STOPPED, tally.details())));
                return Err(Stopped);
            }
            (env.log)(&format!("Categorizing unavailable ({reason:?}): {message}"));
            finish("unavailable", Some(reason.describe().to_string()));
            Ok(Some(reason.describe().to_string()))
        }
        Err(SortEnd::Failed(e)) => {
            (env.log)(&format!("Categorizing failed: {e}"));
            let message = format!("Categorizing ran into a problem; {} will be tried again after the next run", tally.left());
            finish(partial_or_failed, Some(joined(&message, tally.details())));
            Ok(Some(message))
        }
    }
}

/// The categories a chunk is offered: the written ones, then the proposed ones not written yet
/// (offered to every chunk but written only once one is used), with those unwritten proposals.
fn offered(db: &ProjectDb, proposals: &[NewCategoryEntity]) -> Result<(Vec<CategoryInfo>, Vec<NewCategoryEntity>), SortEnd> {
    let mut listed = db.category_infos()?;
    let unwritten: Vec<NewCategoryEntity> = proposals.iter().filter(|p| !listed.iter().any(|c| c.name == p.name)).cloned().collect();
    listed.extend(unwritten.iter().map(|p| CategoryInfo { id: 0, name: p.name.clone(), description: p.description.clone(), created_by: "model".into(), items: 0 }));
    Ok((listed, unwritten))
}

/// Sorts `chunk` with one retry. An unusable reply that stopped at its length cap splits the chunk
/// in half at once instead. Every call is taken from `calls`; once it is used up, the items still
/// waiting are left out and stay uncategorized for the next run.
#[allow(clippy::too_many_arguments)]
fn sort_chunk(
    inner: &Inner,
    env: &RunEnv,
    model: &dyn TextModel,
    chunk: &[SortItem],
    proposals: &[NewCategoryEntity],
    project_items: i64,
    budget: &sorting::ChunkBudget,
    tally: &mut Tally,
    calls: &mut usize,
) -> Result<(), SortEnd> {
    let (listed, unwritten) = offered(&env.db, proposals)?;
    // Once categories exist (written or proposed), a chunk may add only the backend's few.
    let max_new = if listed.is_empty() { sorting::MAX_NEW_CATEGORIES } else { budget.max_new_with_categories };
    let ids: Vec<i64> = chunk.iter().map(|i| i.id).collect();
    let names: Vec<String> = listed.iter().map(|c| c.name.clone()).collect();
    let format = ReplyFormat { json_schema: schema::assign_json_schema(), grammar: schema::assign_grammar(&ids, &names) };
    let max_tokens = REPLY_TOKENS_PER_ITEM * chunk.len() as u32 + REPLY_TOKENS_EXTRA;
    let mut feedback: Vec<String> = Vec::new();
    for attempt in 1..=2 {
        if *calls == 0 {
            tally.left_out += chunk.len();
            (env.log)(&format!("Reached {MAX_CALLS_PER_CHUNK} calls for this chunk; {} items stay uncategorized for the next run", chunk.len()));
            return Ok(());
        }
        *calls -= 1;
        let reply = model.complete(sorting::prompt::SYSTEM, &sorting::prompt::assign_user(chunk, &listed, &feedback, max_new), &format, max_tokens);
        if reply.is_err() || !inner.command_logged.swap(true, Ordering::SeqCst) {
            if let Some(command) = model.last_command() {
                (env.log)(&format!("Ran: {command}"));
            }
        }
        let reply = reply?;
        if reply.stop_reason == "user" || inner.stopping() {
            return Err(SortEnd::Stopped);
        }
        let validation = match sorting::parse_reply(&reply.text) {
            None => Validation::Rejected(vec![UNREADABLE.to_string()]),
            Some(parsed) => validator::validate(&parsed, chunk, &listed, project_items, attempt == 2, max_new),
        };
        match validation {
            Validation::Ok { new_categories, assignments, warnings, left_out } => {
                for w in &warnings {
                    (env.log)(&format!("Sorting: {w}"));
                }
                for (_, problem) in &left_out {
                    (env.log)(&format!("Sorting left an item out: {problem}"));
                }
                let mut offer = new_categories;
                offer.extend(unwritten.iter().cloned());
                match env.db.apply_chunk(&offer, &assignments) {
                    Ok(applied) => {
                        tally.sorted += applied;
                        tally.already += assignments.len() - applied;
                        tally.left_out += left_out.len();
                    }
                    Err(e) => {
                        tally.skipped_chunks += 1;
                        (env.log)(&format!("Could not write a chunk of {} items: {e}", chunk.len()));
                    }
                }
                return Ok(());
            }
            Validation::Rejected(_) if matches!(reply.stop_reason.as_str(), "length" | "max_tokens") && chunk.len() > 1 => {
                let half = chunk.len().div_ceil(2);
                (env.log)(&format!("Sorting reply for {} items stopped at its length cap; splitting into {half} and {}", chunk.len(), chunk.len() - half));
                sort_chunk(inner, env, model, &chunk[..half], proposals, project_items, budget, tally, calls)?;
                return sort_chunk(inner, env, model, &chunk[half..], proposals, project_items, budget, tally, calls);
            }
            Validation::Rejected(problems) => {
                (env.log)(&format!("Sorting reply for {} items rejected (attempt {attempt}, stop {}): {}", chunk.len(), reply.stop_reason, problems.join("; ")));
                feedback = problems;
            }
        }
    }
    tally.skipped_chunks += 1;
    (env.log)(&format!("Skipped a chunk of {} items after the retry; they stay uncategorized", chunk.len()));
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Moving
// ---------------------------------------------------------------------------------------------

fn run_move(inner: &Inner, env: &RunEnv, job: MoveJob, control: &MoveControl) {
    let shared = |paths: &[String]| (env.shared)(paths);
    let bin = BinFilesJob::new(&env.db, &env.project_dir, &shared);
    let on_progress = |p: MoveProgress| {
        inner.update(|s| {
            s.phase = if p.paused { Phase::MovingPaused } else { Phase::Moving };
            s.move_progress = Some(p);
        });
    };
    let outcome = match job {
        MoveJob::BinFiles => bin.bin_files(control, &on_progress),
        MoveJob::Save(run_id) | MoveJob::Resume(run_id) => bin.resume(run_id, control, &on_progress),
    };
    let outcome = outcome.unwrap_or_else(|e| {
        (env.log)(&format!("Moving files failed: {e}"));
        MoveOutcome::Failed(e.to_string())
    });
    let (name, message) = match &outcome {
        MoveOutcome::Done => ("Done", None),
        MoveOutcome::Stopped => ("Stopped", None),
        MoveOutcome::StorageFull(n) => ("StorageFull", Some(format!("Storage is full. Free up {} and press Bin Files again.", crate::moves::format_bytes(*n)))),
        MoveOutcome::Failed(m) => ("Failed", Some(m.clone())),
        MoveOutcome::Blocked(p) => ("Blocked", Some(format!("{} files in the category folders aren't in the project", p.unexpected.len()))),
        MoveOutcome::NothingToMove(_) => ("NothingToMove", None),
    };
    inner.refresh_counts(&env.db);
    inner.update(|s| {
        s.phase = Phase::Idle;
        s.move_progress = None;
        s.move_outcome = Some(name.to_string());
        s.move_message = message;
        s.move_ended += 1;
    });
}

#[cfg(test)]
#[path = "pipeline_android_cases.rs"]
mod android_cases;
