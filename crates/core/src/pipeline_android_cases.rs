//! The Android app's test cases for the `pipeline` package, run against this port:
//! `PipelineControllerTest`, `AnalysisEngineTest`, `CategorizerTest` and `ResortJobTest`.
//!
//! Android tests each class against fakes of the others. Here the real job runner works on a real
//! project database and real picture files; only the two models are scripted stand-ins. Cases
//! about Android's service, coroutine cancellation and releasing a loaded model have no
//! counterpart: GenieX runs as one command per call, so nothing stays loaded.

use super::*;
use crate::analysis::ItemKind;
use crate::db::tests::{add, analyze};
use crate::db::{NewItem, SortRun};
use crate::media::{Ffmpeg, PreparedMedia};
use crate::models::{TextReply, VisionError, VisionReply};
use crate::paths::norm;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

const VALID_JSON: &str = r#"{"description":"A red square on white.","actions":[],"tags":["red","square","graphic"],"suggested_name":"Red square","shot_type":"close"}"#;

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Holds a model call until the test lets it go, or until the run is stopped.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    hold_at: Option<i64>,
    waiting: bool,
    open: bool,
    cancelled: bool,
}

impl Gate {
    fn hold_at(&self, key: i64) {
        self.state.lock().hold_at = Some(key);
    }

    /// Called by the model; true if the call was cancelled while held.
    fn pass(&self, key: i64) -> bool {
        let mut s = self.state.lock();
        if s.hold_at != Some(key) {
            return false;
        }
        s.waiting = true;
        self.changed.notify_all();
        while !s.open && !s.cancelled {
            assert!(!self.changed.wait_for(&mut s, Duration::from_secs(20)).timed_out(), "a held model call was never released");
        }
        let cancelled = s.cancelled;
        *s = GateState::default();
        cancelled
    }

    fn wait_entered(&self) {
        let mut s = self.state.lock();
        while !s.waiting {
            assert!(!self.changed.wait_for(&mut s, Duration::from_secs(20)).timed_out(), "the model call to hold never came");
        }
    }

    fn release(&self) {
        self.state.lock().open = true;
        self.changed.notify_all();
    }

    fn cancel(&self) {
        let mut s = self.state.lock();
        if s.waiting {
            s.cancelled = true;
        }
        self.changed.notify_all();
    }
}

type Events = Arc<Mutex<Vec<String>>>;

struct FakeVision {
    events: Events,
    /// Replies per item id, in order: Ok((text, stop reason)) or Err(model error). Empty: a valid reply.
    scripts: Mutex<HashMap<i64, VecDeque<Result<(String, String), String>>>>,
    load_error: Mutex<Option<String>>,
    loads: Mutex<Vec<u32>>,
    calls: Mutex<Vec<i64>>,
    gate: Gate,
}

impl FakeVision {
    fn script(&self, item: i64, replies: &[Result<(&str, &str), &str>]) {
        let replies = replies.iter().map(|r| r.map(|(t, s)| (t.to_string(), s.to_string())).map_err(str::to_string)).collect();
        self.scripts.lock().insert(item, replies);
    }

    fn calls_for(&self, item: i64) -> usize {
        self.calls.lock().iter().filter(|i| **i == item).count()
    }
}

fn item_of(media: &PreparedMedia) -> i64 {
    crate::paths::file_name(&media.thumbnail_path).trim_end_matches(".jpg").parse().unwrap()
}

impl VisionModel for FakeVision {
    fn model_id(&self) -> String {
        "fake/vision".into()
    }
    fn image_edge(&self) -> u32 {
        64
    }
    fn load(&self, max_frames: u32) -> Result<(), VisionError> {
        self.events.lock().push("vision.load".into());
        self.loads.lock().push(max_frames);
        match self.load_error.lock().clone() {
            Some(e) => Err(VisionError(e)),
            None => Ok(()),
        }
    }
    fn generate(&self, media: &PreparedMedia, on_token: &mut dyn FnMut(&str)) -> Result<VisionReply, VisionError> {
        let item = item_of(media);
        self.events.lock().push(format!("vision.generate {item}"));
        self.calls.lock().push(item);
        let stopped = |text: &str, stop: &str| VisionReply { text: text.to_string(), profile: Profile { stop_reason: stop.to_string(), generated_tokens: 40, ..Default::default() } };
        if self.gate.pass(item) {
            return Ok(stopped("", "user"));
        }
        let next = self.scripts.lock().get_mut(&item).and_then(VecDeque::pop_front);
        match next {
            Some(Err(e)) => Err(VisionError(e)),
            Some(Ok((text, stop))) => {
                on_token(&text);
                Ok(stopped(&text, &stop))
            }
            None => {
                on_token(VALID_JSON);
                Ok(stopped(VALID_JSON, "eos"))
            }
        }
    }
    fn cancel(&self) {
        self.gate.cancel();
    }
}

type Answer = Box<dyn Fn(&str) -> (String, String) + Send + Sync>;

struct FakeText {
    id: String,
    events: Events,
    /// Replies used first, in order: (text, stop reason).
    replies: Mutex<VecDeque<(String, String)>>,
    /// The reply once `replies` is used up; by default every listed item goes to "Beach".
    answer: Mutex<Answer>,
    /// The error returned once `replies` is used up.
    failure: Mutex<Option<TextError>>,
    load_failure: Mutex<Option<TextError>>,
    /// Run just before a reply is returned.
    before_reply: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    prompts: Mutex<Vec<String>>,
    formats: Mutex<Vec<ReplyFormat>>,
    max_tokens: Mutex<Vec<u32>>,
    /// Holds the call with this index (from 0).
    gate: Gate,
}

impl FakeText {
    fn new(id: &str, events: Events) -> Self {
        FakeText {
            id: id.into(),
            events,
            replies: Mutex::new(VecDeque::new()),
            answer: Mutex::new(Box::new(|user| (assign_all(&ids_in(user), "Beach"), "eos".into()))),
            failure: Mutex::new(None),
            load_failure: Mutex::new(None),
            before_reply: Mutex::new(None),
            prompts: Mutex::new(Vec::new()),
            formats: Mutex::new(Vec::new()),
            max_tokens: Mutex::new(Vec::new()),
            gate: Gate::default(),
        }
    }

    fn reply(&self, text: &str, stop: &str) {
        self.replies.lock().push_back((text.to_string(), stop.to_string()));
    }

    fn answer(&self, f: impl Fn(&str) -> (String, String) + Send + Sync + 'static) {
        *self.answer.lock() = Box::new(f);
    }

    fn calls(&self) -> usize {
        self.prompts.lock().len()
    }

    fn prompt(&self, call: usize) -> String {
        self.prompts.lock()[call].clone()
    }

    /// The item ids each call was asked about.
    fn asked(&self) -> Vec<Vec<i64>> {
        self.prompts.lock().iter().map(|p| ids_in(p)).collect()
    }
}

impl TextModel for FakeText {
    fn model_id(&self) -> String {
        self.id.clone()
    }
    fn load(&self) -> Result<(), TextError> {
        self.events.lock().push("text.load".into());
        match self.load_failure.lock().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn complete(&self, _system: &str, user: &str, format: &ReplyFormat, max_tokens: u32) -> Result<TextReply, TextError> {
        self.events.lock().push("text.complete".into());
        let call = {
            let mut prompts = self.prompts.lock();
            prompts.push(user.to_string());
            prompts.len() as i64 - 1
        };
        self.formats.lock().push(format.clone());
        self.max_tokens.lock().push(max_tokens);
        let reply = |text: String, stop: String| TextReply { text, stop_reason: stop, input_tokens: 0, output_tokens: 0, millis: 1 };
        if self.gate.pass(call) {
            return Ok(reply(String::new(), "user".into()));
        }
        let scripted = self.replies.lock().pop_front();
        let (text, stop) = match scripted {
            Some(r) => r,
            None => {
                if let Some(e) = self.failure.lock().clone() {
                    return Err(e);
                }
                (self.answer.lock())(user)
            }
        };
        if let Some(hook) = self.before_reply.lock().as_ref() {
            hook();
        }
        Ok(reply(text, stop))
    }
    fn cancel(&self) {
        self.gate.cancel();
    }
}

/// The item ids of an assignment prompt, in order.
fn ids_in(user: &str) -> Vec<i64> {
    user.lines().skip_while(|l| !l.starts_with("Items to sort")).skip(1).take_while(|l| !l.is_empty()).filter_map(|l| l.split('\t').next()?.parse().ok()).collect()
}

/// A reply that puts every one of `ids` in `category`, named "Item <id>".
fn assign_all(ids: &[i64], category: &str) -> String {
    assign_each(ids, |_| category.to_string())
}

fn assign_each(ids: &[i64], category: impl Fn(i64) -> String) -> String {
    serde_json::json!({
        "new_categories": [],
        "assignments": ids.iter().map(|id| serde_json::json!({"id": id, "category": category(*id), "name": format!("Item {id}")})).collect::<Vec<_>>(),
    })
    .to_string()
}

/// A project on disk with the real job runner and scripted models.
struct World {
    _dir: tempfile::TempDir,
    root: String,
    db: Arc<ProjectDb>,
    events: Events,
    vision: Arc<FakeVision>,
    /// Stands in for LFM3-1.7B.
    text: Arc<FakeText>,
    claude: Arc<FakeText>,
    choice: Mutex<SortChoice>,
    /// Why sorting can't run now, as the app's check would say.
    sorting_unavailable: Arc<Mutex<Option<String>>>,
    log: Arc<Mutex<Vec<String>>>,
    states: Arc<Mutex<Vec<PipelineState>>>,
    pipeline: Pipeline,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = norm(dir.path());
        let db = Arc::new(ProjectDb::open(std::path::Path::new(&format!("{root}/proj/.binning/project.db")), "La Jolla Trip").unwrap());
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let states = Arc::new(Mutex::new(Vec::new()));
        let sink = states.clone();
        let vision = Arc::new(FakeVision {
            events: events.clone(),
            scripts: Mutex::new(HashMap::new()),
            load_error: Mutex::new(None),
            loads: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            gate: Gate::default(),
        });
        World {
            _dir: dir,
            db,
            vision,
            text: Arc::new(FakeText::new("fake/lfm", events.clone())),
            claude: Arc::new(FakeText::new("fake/claude", events.clone())),
            events,
            choice: Mutex::new(SortChoice::Lfm3),
            sorting_unavailable: Arc::new(Mutex::new(None)),
            log: Arc::new(Mutex::new(Vec::new())),
            states,
            pipeline: Pipeline::new(move |s| sink.lock().push(s.clone()), || {}),
            root,
        }
    }

    fn env(&self) -> RunEnv {
        self.env_with_frames(16)
    }

    fn env_with_frames(&self, max_frames: u32) -> RunEnv {
        let (text, claude) = (self.text.clone(), self.claude.clone());
        let unavailable = self.sorting_unavailable.clone();
        let log = self.log.clone();
        RunEnv {
            project_dir: format!("{}/proj", self.root),
            db: self.db.clone(),
            vision: self.vision.clone(),
            text_for: Box::new(move |choice| -> Arc<dyn TextModel> {
                match choice {
                    SortChoice::Claude => claude.clone(),
                    SortChoice::Lfm3 => text.clone(),
                }
            }),
            sort_choice: *self.choice.lock(),
            max_frames,
            preparer: MediaPreparer { work_dir: self.work_dir(), thumbs_dir: format!("{}/proj/.binning/thumbs", self.root), ffmpeg: Ffmpeg::default(), grabber: None },
            ensure_vision: Box::new(|_| Ok(())),
            sorting_available: Box::new(move |_| unavailable.lock().clone()),
            shared: Box::new(|_| Vec::new()),
            log: Arc::new(move |line| log.lock().push(line.to_string())),
        }
    }

    fn work_dir(&self) -> String {
        format!("{}/work", self.root)
    }

    /// A Pending photo whose file is a real picture. Ids count up from 1.
    fn photo(&self) -> i64 {
        self.file(|path| image::RgbImage::from_pixel(8, 8, image::Rgb([200, 30, 30])).save(path).unwrap())
    }

    /// A Pending photo whose file can't be decoded.
    fn broken(&self) -> i64 {
        self.file(|path| std::fs::write(path, b"not a picture").unwrap())
    }

    /// A Pending photo whose file is gone.
    fn gone(&self) -> i64 {
        self.file(|_| {})
    }

    fn file(&self, write: impl Fn(&str)) -> i64 {
        let n = self.db.item_count().unwrap() + 1;
        let path = format!("{}/camera/IMG_{n}.png", self.root);
        std::fs::create_dir_all(format!("{}/camera", self.root)).unwrap();
        write(&path);
        let name = format!("IMG_{n}.png");
        let id = self.db.insert_item(&NewItem { content_hash: format!("hash-{n}"), path, name, kind: ItemKind::Photo, taken_at: None, duration_ms: None, width: None, height: None }).unwrap();
        std::fs::create_dir_all(format!("{}/proj/.binning/thumbs", self.root)).unwrap();
        id
    }

    /// `n` Analyzed items waiting for a category. Ids count up from 1.
    fn analyzed(&self, n: i64) {
        for i in 1..=n {
            let id = add(&self.db, &format!("{i}.jpg"));
            analyze(&self.db, id, &format!("Name {i}"), &["sea"]);
        }
    }

    fn category(&self, name: &str, created_by: &str, description: &str) -> i64 {
        self.db
            .read(|c| {
                c.execute("INSERT INTO category (name, description, createdBy) VALUES (?1, ?2, ?3)", rusqlite::params![name, description, created_by])?;
                Ok(c.last_insert_rowid())
            })
            .unwrap()
    }

    fn sql(&self, statement: &str) {
        self.db.read(|c| Ok(c.execute_batch(statement)?)).unwrap();
    }

    fn item(&self, id: i64) -> Item {
        self.db.item(id).unwrap().unwrap()
    }

    fn status(&self, id: i64) -> ItemStatus {
        self.item(id).status
    }

    fn statuses(&self) -> Vec<ItemStatus> {
        self.db.items().unwrap().iter().map(|i| i.status).collect()
    }

    fn category_of(&self, id: i64) -> Option<String> {
        let category = self.item(id).category_id?;
        self.db.category_names().unwrap().into_iter().find(|(c, _)| *c == category).map(|(_, n)| n)
    }

    fn category_names(&self) -> Vec<String> {
        self.db.category_names().unwrap().into_iter().map(|(_, n)| n).collect()
    }

    fn state(&self) -> PipelineState {
        self.pipeline.state()
    }

    fn count(&self, event: &str) -> usize {
        self.events.lock().iter().filter(|e| e.starts_with(event)).count()
    }

    fn logged(&self, part: &str) -> bool {
        self.log.lock().iter().any(|l| l.contains(part))
    }

    /// Runs analysis and categorizing to the end.
    fn run(&self) -> PipelineState {
        self.pipeline.start(self.env());
        self.pipeline.wait();
        self.state()
    }

    /// Categorizes what is waiting, to the end; returns the notice (None when everything was sorted).
    fn categorize(&self) -> Option<String> {
        self.pipeline.categorize_now(self.env());
        self.pipeline.wait();
        let state = self.state();
        assert_eq!(state.phase, Phase::Idle);
        assert_eq!(state.error, None, "categorizing never sets the run's error");
        state.sort_notice
    }

    fn last_sort_run(&self) -> SortRun {
        self.db.recent_sort_runs().unwrap().remove(0)
    }

    /// The (done, total) pairs the Categorizing phase reported, without repeats.
    fn sort_progress(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for s in self.states.lock().iter().filter(|s| s.phase == Phase::Categorizing && s.sort_total > 0) {
            if out.last() != Some(&(s.sort_done, s.sort_total)) {
                out.push((s.sort_done, s.sort_total));
            }
        }
        out
    }
}

use ItemStatus::{Analyzed, Failed, Missing, Pending, Sorted};

// ---------------------------------------------------------------------------------------------
// PipelineControllerTest
// ---------------------------------------------------------------------------------------------

#[test]
fn processes_all_pending_then_categorizes_once() {
    let w = World::new();
    for _ in 0..3 {
        w.photo();
    }
    let state = w.run();
    assert_eq!(state.phase, Phase::Idle);
    assert_eq!((state.pending, state.analyzed, state.failed, state.total), (0, 3, 0, 3));
    assert_eq!(w.statuses(), vec![Sorted; 3]);
    // The vision model is done before the sorting model is asked for anything.
    assert_eq!(*w.events.lock(), vec!["vision.load", "vision.generate 1", "vision.generate 2", "vision.generate 3", "text.load", "text.complete"]);
    assert_eq!(state.current_item_id, None);
    assert_eq!(state.last_profile.unwrap().stop_reason, "eos");
}

#[test]
fn start_with_nothing_pending_does_not_load_the_model() {
    let w = World::new();
    assert_eq!(w.run().phase, Phase::Idle);
    assert!(w.events.lock().is_empty());
}

#[test]
fn the_frame_cap_reaches_the_model_and_changes_with_the_next_run() {
    let w = World::new();
    w.photo();
    w.run();
    w.photo();
    w.pipeline.start(w.env_with_frames(32));
    w.pipeline.wait();
    assert_eq!(*w.vision.loads.lock(), vec![16, 32]);
}

/// An item left Analyzing by a process that died goes back to Pending; one that was being
/// analyzed when the process died twice is failed, not retried forever.
#[test]
fn start_fails_items_interrupted_twice_and_retries_the_rest() {
    let w = World::new();
    let (once, twice, fresh) = (w.photo(), w.photo(), w.photo());
    w.sql("UPDATE item SET status = 'Analyzing', attempts = 1 WHERE id = 1; UPDATE item SET status = 'Analyzing', attempts = 2 WHERE id = 2;");
    w.run();
    assert_eq!((w.status(once), w.status(twice), w.status(fresh)), (Sorted, Failed, Sorted));
    assert_eq!(w.item(twice).failure_reason.as_deref(), Some(failure::INTERRUPTED));
    assert_eq!(w.vision.calls_for(twice), 0);
}

#[test]
fn pause_finishes_the_current_item_then_holds() {
    let w = World::new();
    let (one, two) = (w.photo(), w.photo());
    w.vision.gate.hold_at(one);
    w.pipeline.start(w.env());
    w.vision.gate.wait_entered();
    w.pipeline.pause();
    w.vision.gate.release();
    wait_for("the pause", || w.state().phase == Phase::Paused);
    assert_eq!((w.status(one), w.status(two)), (Analyzed, Pending));
    assert_eq!(w.state().current_item_id, None);
    assert!(w.pipeline.is_running());
    assert!(w.pipeline.resume());
    w.pipeline.wait();
    assert_eq!(w.statuses(), vec![Sorted; 2]);
}

#[test]
fn stop_cancels_the_current_item_returns_it_to_pending_and_a_new_start_carries_on() {
    let w = World::new();
    let (one, two) = (w.photo(), w.photo());
    w.vision.gate.hold_at(two);
    w.pipeline.start(w.env());
    w.vision.gate.wait_entered();
    // Each start of an item's analysis is counted, so a crash here would be noticed next time.
    assert_eq!((w.status(two), w.item(two).attempts), (ItemStatus::Analyzing, 1));
    w.pipeline.stop_and_wait();
    let state = w.state();
    assert_eq!((state.phase, state.error.clone(), state.current_item_id), (Phase::Stopped, None, None));
    assert_eq!((w.status(one), w.status(two)), (Analyzed, Pending));
    assert_eq!(w.item(two).attempts, 0, "a Stop is not a failed attempt");
    assert_eq!(w.count("text."), 0, "a stopped run doesn't categorize");
    assert!(!w.pipeline.resume(), "nothing is paused, so the caller starts a new run");
    assert_eq!(w.run().phase, Phase::Idle);
    assert_eq!(w.statuses(), vec![Sorted; 2]);
    assert_eq!(w.vision.calls_for(one), 1);
}

#[test]
fn a_failed_item_does_not_stop_the_batch() {
    let w = World::new();
    let (one, bad, three) = (w.photo(), w.broken(), w.photo());
    let state = w.run();
    assert_eq!((w.status(one), w.status(bad), w.status(three)), (Sorted, Failed, Sorted));
    assert_eq!(w.item(bad).failure_reason.as_deref(), Some(failure::DECODE));
    assert_eq!((state.phase, state.failed, state.analyzed), (Phase::Idle, 1, 2));
}

#[test]
fn load_failure_ends_stopped_with_an_error_and_items_stay_pending() {
    let w = World::new();
    w.photo();
    w.photo();
    *w.vision.load_error.lock() = Some("QNN error 1002 at C:/secret/path".into());
    let state = w.run();
    assert_eq!(state.phase, Phase::Stopped);
    // The user sees a fixed sentence; the raw error stays in the log.
    assert_eq!(state.error.as_deref(), Some(failure::MODEL_NOT_STARTED));
    assert!(w.logged("QNN error 1002"));
    assert_eq!(w.statuses(), vec![Pending; 2]);
    assert_eq!(w.count("vision.generate"), 0);
}

/// An NPU or SDK error is not the item's fault: the run stops instead of failing every item in turn.
#[test]
fn model_error_during_generation_ends_stopped_and_leaves_every_item_pending() {
    let w = World::new();
    let (one, _two) = (w.photo(), w.photo());
    w.vision.script(one, &[Err("the NPU went away")]);
    let state = w.run();
    assert_eq!(state.phase, Phase::Stopped);
    assert!(state.error.as_deref().unwrap().starts_with(failure::MODEL_FAILED), "{:?}", state.error);
    assert_eq!(w.statuses(), vec![Pending; 2]);
    assert_eq!(w.item(one).attempts, 0);
    assert_eq!(w.vision.calls.lock().len(), 1);
    assert!(!std::path::Path::new(&format!("{}/{one}", w.work_dir())).exists(), "work files are cleaned up");
}

/// Three Failed items in a row stop the run with a readable reason; the rest stay Pending.
#[test]
fn three_failures_in_a_row_stop_the_run() {
    let w = World::new();
    for _ in 0..4 {
        w.broken();
    }
    let good = w.photo();
    let state = w.run();
    assert_eq!(state.phase, Phase::Stopped);
    assert_eq!(state.error.as_deref(), Some("Analysis stopped after repeated failures: Couldn't read this file."));
    assert_eq!(w.statuses(), vec![Failed, Failed, Failed, Pending, Pending]);
    assert_eq!(w.vision.calls_for(good), 0);
}

/// A success in between resets the count, and Missing files do not count.
#[test]
fn failures_broken_by_success_do_not_stop_the_run() {
    let w = World::new();
    w.broken();
    w.broken();
    w.photo();
    w.broken();
    w.gone();
    w.broken();
    let state = w.run();
    assert_eq!(state.phase, Phase::Idle);
    assert_eq!(w.statuses(), vec![Failed, Failed, Sorted, Failed, Missing, Failed]);
    assert_eq!(state.failed, 5, "Failed and Missing items are done for progress purposes");
}

/// Media added while a batch runs is analyzed by that run, and categorizing still runs once.
#[test]
fn media_added_during_a_run_is_analyzed_and_categorizing_runs_once() {
    let w = World::new();
    let one = w.photo();
    w.vision.gate.hold_at(one);
    w.pipeline.start(w.env());
    w.vision.gate.wait_entered();
    w.photo();
    w.photo();
    w.pipeline.start(w.env());
    w.vision.gate.release();
    w.pipeline.wait();
    assert_eq!(w.statuses(), vec![Sorted; 3]);
    assert_eq!(w.count("text.complete"), 1);
    assert_eq!(w.text.asked(), vec![vec![1, 2, 3]]);
}

/// Adding media while paused must not resume the run; only Resume does.
#[test]
fn start_while_paused_stays_paused() {
    let w = World::new();
    let (one, two) = (w.photo(), w.photo());
    w.vision.gate.hold_at(one);
    w.pipeline.start(w.env());
    w.vision.gate.wait_entered();
    w.pipeline.pause();
    w.vision.gate.release();
    wait_for("the pause", || w.state().phase == Phase::Paused);
    let three = w.photo();
    w.pipeline.start(w.env());
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(w.state().phase, Phase::Paused);
    assert_eq!((w.status(two), w.status(three)), (Pending, Pending));
    w.pipeline.resume();
    w.pipeline.wait();
    assert_eq!(w.statuses(), vec![Sorted; 3]);
    assert_eq!(w.count("text.complete"), 1);
}

/// Media added while categorizing is analyzed too, by a fresh run that waits for this one.
#[test]
fn start_during_categorizing_runs_another_run() {
    let w = World::new();
    w.photo();
    w.text.gate.hold_at(0);
    w.pipeline.start(w.env());
    w.text.gate.wait_entered();
    assert_eq!(w.state().phase, Phase::Categorizing);
    let two = w.photo();
    std::thread::scope(|s| {
        let second = s.spawn(|| w.pipeline.start(w.env()));
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(w.status(two), Pending, "the new run waits for the categorizing to end");
        w.text.gate.release();
        second.join().unwrap();
    });
    w.pipeline.wait();
    assert_eq!(w.statuses(), vec![Sorted; 2]);
    assert_eq!(w.count("vision.load"), 2);
    assert_eq!(w.text.asked(), vec![vec![1], vec![2]]);
}

/// One job at a time: a Re-sort and a file move are refused while a run is in progress, never queued.
#[test]
fn resort_and_moves_are_refused_while_a_run_is_in_progress() {
    let w = World::new();
    let one = w.photo();
    w.vision.gate.hold_at(one);
    w.pipeline.start(w.env());
    w.vision.gate.wait_entered();
    assert_eq!(w.pipeline.resort(w.env(), SortChoice::Lfm3, true), Err(RESORT_BUSY));
    assert_eq!(w.pipeline.start_move(w.env(), MoveJob::BinFiles, false), Err(MOVE_BUSY));
    w.vision.gate.release();
    w.pipeline.wait();
    assert_eq!(w.status(one), Sorted);
    assert_eq!(w.count("text.complete"), 1, "neither refused job ran afterwards");
    assert!(w.db.open_move_run().unwrap().is_none() && w.db.unread_move_run().unwrap().is_none());
}

// ---- categorizing as part of a run

#[test]
fn sorting_unavailable_leaves_items_uncategorized_with_the_reason_and_no_error() {
    let w = World::new();
    w.photo();
    w.photo();
    *w.choice.lock() = SortChoice::Claude;
    *w.claude.failure.lock() = Some(TextError::Unavailable(SortUnavailable::NoApiKey, "No Claude API key saved".into()));
    let state = w.run();
    assert_eq!((state.phase, state.error.clone()), (Phase::Idle, None));
    assert_eq!(state.sort_notice.as_deref(), Some(SortUnavailable::NoApiKey.describe()));
    assert_eq!(w.statuses(), vec![Analyzed; 2]);
    assert_eq!(w.text.calls(), 0, "the other model is never asked instead");
}

#[test]
fn sort_notice_survives_the_next_start_and_clears_when_sorting_succeeds() {
    let w = World::new();
    w.photo();
    *w.text.failure.lock() = Some(TextError::Unavailable(SortUnavailable::ModelNotDownloaded, "LFM3-1.7B is not downloaded".into()));
    let notice = w.run().sort_notice;
    assert_eq!(notice.as_deref(), Some(SortUnavailable::ModelNotDownloaded.describe()));
    // The next run shows the notice until its own categorizing has an answer.
    *w.text.failure.lock() = None;
    let two = w.photo();
    w.vision.gate.hold_at(two);
    w.pipeline.start(w.env());
    w.vision.gate.wait_entered();
    assert_eq!(w.state().sort_notice, notice);
    w.vision.gate.release();
    w.pipeline.wait();
    assert_eq!(w.state().sort_notice, None);
    assert_eq!(w.statuses(), vec![Sorted; 2]);
}

#[test]
fn a_sorting_failure_never_sets_the_error_or_fails_analysis() {
    let w = World::new();
    w.photo();
    *w.text.failure.lock() = Some(TextError::Other("Generation failed: QNN error 1002".into()));
    let state = w.run();
    assert_eq!((state.phase, state.error.clone(), state.failed), (Phase::Idle, None, 0));
    assert_eq!(state.sort_notice.as_deref(), Some("Categorizing ran into a problem; 1 will be tried again after the next run"));
    assert_eq!(w.statuses(), vec![Analyzed]);
    assert!(w.logged("QNN error 1002"));
}

#[test]
fn stop_during_categorizing_keeps_the_chunks_already_written() {
    let w = World::new();
    w.analyzed(25);
    w.category("Beach", "model", "");
    w.text.gate.hold_at(1);
    w.pipeline.categorize_now(w.env());
    w.text.gate.wait_entered();
    w.pipeline.stop_and_wait();
    let state = w.state();
    assert_eq!((state.phase, state.error.clone()), (Phase::Stopped, None));
    assert_eq!(w.statuses(), [vec![Sorted; 10], vec![Analyzed; 15]].concat());
    let run = w.last_sort_run();
    assert_eq!((run.outcome.as_str(), run.sorted, run.message.as_deref()), ("partial", 10, Some(SORT_STOPPED)));
    // Resume after the stop goes straight back to categorizing the rest.
    assert_eq!(w.categorize(), None);
    assert_eq!(w.statuses(), vec![Sorted; 25]);
    assert_eq!(w.text.asked()[2..], [(11..=20).collect::<Vec<i64>>(), (21..=25).collect()]);
}

#[test]
fn a_run_with_nothing_to_analyze_still_categorizes_leftovers() {
    let w = World::new();
    w.analyzed(2);
    assert_eq!(w.run().phase, Phase::Idle);
    assert_eq!(w.statuses(), vec![Sorted; 2]);
    assert_eq!(w.count("vision."), 0);
}

#[test]
fn categorize_now_skips_analysis() {
    let w = World::new();
    w.analyzed(2);
    let waiting = w.photo();
    assert_eq!(w.categorize(), None);
    assert_eq!(w.statuses(), vec![Sorted, Sorted, Pending]);
    assert_eq!(w.count("vision."), 0);
    assert_eq!(w.vision.calls_for(waiting), 0);
}

/// A second categorize-only start while one is categorizing is absorbed, so nothing is sent twice.
#[test]
fn categorize_now_while_categorizing_is_absorbed() {
    let w = World::new();
    w.analyzed(3);
    w.text.gate.hold_at(0);
    w.pipeline.categorize_now(w.env());
    w.text.gate.wait_entered();
    w.pipeline.categorize_now(w.env());
    // Pause has nothing to hold during Categorizing; it is ignored rather than latched.
    w.pipeline.pause();
    assert_eq!(w.state().phase, Phase::Categorizing);
    w.text.gate.release();
    w.pipeline.wait();
    assert_eq!(w.state().phase, Phase::Idle);
    assert_eq!(w.text.calls(), 1);
    assert_eq!(w.statuses(), vec![Sorted; 3]);
}

// ---------------------------------------------------------------------------------------------
// AnalysisEngineTest
// ---------------------------------------------------------------------------------------------

#[test]
fn valid_reply_is_saved() {
    let w = World::new();
    let id = w.photo();
    w.run();
    let saved = w.db.analysis(id).unwrap().unwrap();
    assert_eq!(saved.description, "A red square on white.");
    let raw: String = w.db.read(|c| Ok(c.query_row("SELECT rawJson FROM analysis WHERE itemId = ?1", [id], |r| r.get(0))?)).unwrap();
    assert_eq!(raw, VALID_JSON);
    assert_eq!(saved.model_id, "fake/vision");
    assert_eq!(saved.prompt_version, prompt::PROMPT_VERSION);
    assert_eq!(saved.stop_reason, "eos");
    assert_eq!(w.db.tags_for(id).unwrap(), vec!["graphic", "red", "square"]);
    assert_eq!(w.vision.calls_for(id), 1);
    // The card shows the reply as it arrives.
    let partial = w.states.lock().iter().rev().find_map(|s| s.partial.clone()).unwrap();
    assert_eq!(partial.description, "A red square on white.");
    assert_eq!(partial.tags, vec!["red", "square", "graphic"]);
    // The thumbnail stays; the work files go.
    assert!(std::path::Path::new(&format!("{}/proj/.binning/thumbs/{id}.jpg", w.root)).exists());
    assert!(!std::path::Path::new(&format!("{}/{id}", w.work_dir())).exists());
}

#[test]
fn length_stop_retries_once_then_succeeds() {
    let w = World::new();
    let id = w.photo();
    w.vision.script(id, &[Ok((r#"{"description":"A red square on white, with a very long"#, "length")), Ok((VALID_JSON, "eos"))]);
    w.run();
    assert_eq!(w.status(id), Sorted);
    assert_eq!(w.vision.calls_for(id), 2);
}

#[test]
fn length_twice_marks_failed_length() {
    let w = World::new();
    let id = w.photo();
    let cut = Ok((r#"{"description":"A red"#, "length"));
    w.vision.script(id, &[cut, cut]);
    w.run();
    assert_eq!((w.status(id), w.item(id).failure_reason.as_deref()), (Failed, Some(failure::LENGTH)));
    assert_eq!(w.vision.calls_for(id), 2);
    assert!(w.db.analysis(id).unwrap().is_none());
}

#[test]
fn invalid_json_twice_marks_failed_invalid_json() {
    let w = World::new();
    let id = w.photo();
    w.vision.script(id, &[Ok(("I see a red square.", "eos")), Ok((r#"{"description":"","suggested_name":"x"}"#, "eos"))]);
    w.run();
    assert_eq!((w.status(id), w.item(id).failure_reason.as_deref()), (Failed, Some(failure::INVALID_JSON)));
    assert_eq!(w.vision.calls_for(id), 2);
    assert!(!std::path::Path::new(&format!("{}/{id}", w.work_dir())).exists());
}

#[test]
fn missing_file_marks_missing_without_asking_the_model() {
    let w = World::new();
    let id = w.gone();
    w.run();
    assert_eq!((w.status(id), w.item(id).failure_reason), (Missing, None));
    assert_eq!(w.vision.calls_for(id), 0);
}

#[test]
fn user_stop_is_not_retried_and_changes_no_status() {
    let w = World::new();
    let id = w.photo();
    w.vision.script(id, &[Ok(("", "user"))]);
    let state = w.run();
    assert_eq!((state.phase, state.error), (Phase::Stopped, None));
    assert_eq!((w.status(id), w.item(id).failure_reason), (Pending, None));
    assert_eq!(w.vision.calls_for(id), 1);
}

#[test]
fn failure_codes_are_worded_for_people() {
    for code in [failure::DECODE, failure::LENGTH, failure::INVALID_JSON, failure::PROMPT, failure::MODEL, failure::INTERRUPTED, failure::NO_VIDEO_DECODER] {
        let sentence = failure::describe(Some(code));
        assert!(sentence.ends_with('.'), "{code}: {sentence}");
        assert_ne!(sentence, failure::describe(None), "{code}");
    }
    assert_eq!(failure::describe(Some("something new")), "Couldn't analyze this file.");
}

// ---------------------------------------------------------------------------------------------
// CategorizerTest
// ---------------------------------------------------------------------------------------------

#[test]
fn categorize_applies_valid_chunks() {
    let w = World::new();
    w.analyzed(25);
    w.category("Beach", "model", "");
    assert_eq!(w.categorize(), None);
    for id in 1..=25 {
        assert_eq!((w.status(id), w.category_of(id).as_deref()), (Sorted, Some("Beach")), "{id}");
    }
    // LFM3-1.7B takes 10 items per chunk; the reply cap is 30 per item plus 600.
    assert_eq!(w.text.calls(), 3);
    assert_eq!(*w.text.max_tokens.lock(), vec![900, 900, 750]);
    assert_eq!(w.text.asked(), vec![(1..=10).collect::<Vec<i64>>(), (11..=20).collect(), (21..=25).collect()]);
    // The local grammar fixes the chunk's ids and offers the existing names.
    assert!(w.text.formats.lock()[0].grammar.contains(r#""\"Beach\"""#));
    assert_eq!(*w.events.lock(), vec!["text.load", "text.complete", "text.complete", "text.complete"]);
    assert_eq!(w.sort_progress(), vec![(0, 25), (10, 25), (20, 25), (25, 25)]);
    let runs = w.db.recent_sort_runs().unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!((run.outcome.as_str(), run.items, run.sorted, run.new_categories, run.model_id.as_str()), ("done", 25, 25, 0, "fake/lfm"));
    assert!(run.finished_at.is_some());
}

#[test]
fn nothing_uncategorized_is_nothing_to_do_without_loading_or_a_run_row() {
    let w = World::new();
    w.photo();
    assert_eq!(w.categorize(), None);
    assert!(w.events.lock().is_empty());
    assert!(w.db.recent_sort_runs().unwrap().is_empty());
}

#[test]
fn budget_follows_the_sorting_model() {
    let w = World::new();
    w.analyzed(15);
    w.category("Beach", "model", "");
    *w.choice.lock() = SortChoice::Claude;
    assert_eq!(w.categorize(), None);
    assert_eq!(w.claude.calls(), 1);
    assert_eq!(*w.claude.max_tokens.lock(), vec![30 * 15 + 600]);
    // Claude reads the JSON schema.
    assert!(w.claude.formats.lock()[0].json_schema.get("properties").is_some());
    assert_eq!(w.last_sort_run().model_id, "fake/claude");
    assert_eq!(w.text.calls(), 0);
}

#[test]
fn missing_id_retried_once_with_problems_then_only_that_item_is_left_out() {
    let w = World::new();
    w.analyzed(15);
    w.category("Beach", "model", "");
    // Chunk 1 (items 1-10) leaves out item 10 twice; chunk 2 is answered normally.
    let missing_ten = assign_all(&(1..=9).collect::<Vec<_>>(), "Beach");
    w.text.reply(&missing_ten, "eos");
    w.text.reply(&missing_ten, "eos");
    let notice = w.categorize().unwrap();
    assert_eq!(w.text.calls(), 3);
    assert!(!w.text.prompt(0).contains("rejected"));
    assert!(w.text.prompt(1).contains("Your previous reply was rejected"));
    assert!(w.text.prompt(1).contains("Missing ids (assign every listed item): 10"));
    // The retry's valid part is applied; only item 10 stays uncategorized.
    assert_eq!((w.status(10), w.category_of(10)), (Analyzed, None));
    assert!((1..=15).filter(|id| *id != 10).all(|id| w.status(id) == Sorted));
    assert_eq!(notice, "Categorized 14 of 15; 1 will be tried again after the next run");
    let run = w.last_sort_run();
    assert_eq!((run.outcome.as_str(), run.sorted), ("partial", 14));
    assert_eq!(run.message, Some(format!("{notice} · left out: 1")));
    assert_eq!(w.sort_progress(), vec![(0, 15), (10, 15), (15, 15)]);
}

#[test]
fn unreadable_twice_skips_the_chunk() {
    let w = World::new();
    w.analyzed(15);
    w.category("Beach", "model", "");
    w.text.reply("no idea", "eos");
    w.text.reply("still no idea", "eos");
    let notice = w.categorize().unwrap();
    assert_eq!(w.text.calls(), 3);
    assert_eq!(w.statuses(), [vec![Analyzed; 10], vec![Sorted; 5]].concat());
    assert_eq!(notice, "Categorized 5 of 15; 10 will be tried again after the next run");
    assert_eq!(w.last_sort_run().message, Some(format!("{notice} · skipped chunks: 1")));
}

#[test]
fn unreadable_reply_is_retried_and_then_applied() {
    let w = World::new();
    w.analyzed(3);
    w.category("Beach", "model", "");
    w.text.reply("I would put these at the beach.", "eos");
    assert_eq!(w.categorize(), None);
    assert_eq!(w.text.calls(), 2);
    assert!(w.text.prompt(1).contains("Your previous reply was rejected"));
    assert_eq!(w.statuses(), vec![Sorted; 3]);
}

#[test]
fn retry_applies_the_valid_assignments_and_leaves_out_the_rest() {
    let w = World::new();
    w.analyzed(4);
    w.category("Beach", "model", "");
    // Item 4 has no usable category, both times.
    let bad = assign_each(&[1, 2, 3, 4], |id| if id == 4 { String::new() } else { "Beach".into() });
    w.text.reply(&bad, "eos");
    w.text.reply(&bad, "eos");
    assert_eq!(w.categorize().as_deref(), Some("Categorized 3 of 4; 1 will be tried again after the next run"));
    assert_eq!(w.text.calls(), 2);
    assert!((1..=3).all(|id| w.category_of(id).as_deref() == Some("Beach")));
    assert_eq!(w.status(4), Analyzed);
    let run = w.last_sort_run();
    assert_eq!((run.outcome.as_str(), run.sorted), ("partial", 3));
}

#[test]
fn proposal_pass_offers_its_categories_but_writes_only_the_used_ones() {
    // No categories yet and more items than one LFM chunk: a proposal pass runs first.
    let w = World::new();
    w.analyzed(12);
    w.text.reply(r#"{"new_categories":[{"name":"Beach","description":"Sand and waves"},{"name":"Food","description":"Meals"},{"name":"Mountains","description":"Peaks and trails"}]}"#, "eos");
    w.text.answer(|user| (assign_each(&ids_in(user), |id| if id % 2 == 0 { "Beach".into() } else { "Food".into() }), "eos".into()));
    assert_eq!(w.categorize(), None);
    assert_eq!(w.text.calls(), 3);
    assert!(w.text.prompt(0).starts_with("Propose categories"));
    assert_eq!(*w.text.max_tokens.lock(), vec![1000, 900, 660]);
    // The proposed categories are listed to every assignment chunk...
    assert!(w.text.prompt(1).contains("Mountains\t0\topen\tPeaks and trails"));
    assert!(w.text.formats.lock()[1].grammar.contains("Mountains"));
    assert!(w.text.prompt(2).contains("Mountains\t0\topen\tPeaks and trails"));
    // ...but only the ones an assignment used are written.
    let mut names = w.category_names();
    names.sort();
    assert_eq!(names, vec!["Beach", "Food"]);
    assert_eq!(w.db.categories().unwrap().iter().find(|c| c.name == "Beach").unwrap().description, "Sand and waves");
    let run = w.last_sort_run();
    assert_eq!((run.sorted, run.new_categories), (12, 2));
}

#[test]
fn unreadable_proposal_is_skipped_and_chunks_still_run() {
    let w = World::new();
    w.analyzed(12);
    w.text.reply("no idea", "eos");
    assert_eq!(w.categorize(), None);
    assert_eq!(w.statuses(), vec![Sorted; 12]);
    assert_eq!(w.last_sort_run().new_categories, 1);
    assert!(w.logged("The proposal pass gave no usable categories"));
}

#[test]
fn claude_unavailable_leaves_items_uncategorized_with_reason() {
    let w = World::new();
    w.analyzed(2);
    *w.choice.lock() = SortChoice::Claude;
    *w.claude.failure.lock() = Some(TextError::Unavailable(SortUnavailable::NoApiKey, "No Claude API key saved".into()));
    assert_eq!(w.categorize().as_deref(), Some("Add a Claude API key in Settings to categorize"));
    assert_eq!(w.statuses(), vec![Analyzed; 2]);
    let run = w.last_sort_run();
    assert_eq!((run.outcome.as_str(), run.sorted), ("unavailable", 0));
    // The reason and message are logged.
    assert!(w.log.lock().iter().any(|l| l.contains("NoApiKey") && l.contains("No Claude API key saved")));
}

#[test]
fn model_not_downloaded_is_unavailable() {
    let w = World::new();
    w.analyzed(1);
    *w.text.load_failure.lock() = Some(TextError::Unavailable(SortUnavailable::ModelNotDownloaded, "LFM3-1.7B is not downloaded".into()));
    assert_eq!(w.categorize().as_deref(), Some("Download the sorting model in Settings"));
    assert_eq!(*w.events.lock(), vec!["text.load"]);
    assert_eq!(w.last_sort_run().outcome, "unavailable");
}

/// The run row keeps what happened before the failure.
#[test]
fn unexpected_model_error_ends_partial_and_the_row_keeps_the_details() {
    let w = World::new();
    w.analyzed(25);
    w.category("Beach", "model", "");
    // Chunk 1 is unreadable twice and is skipped; chunk 2 then fails outright.
    w.text.reply("no idea", "eos");
    w.text.reply("no idea", "eos");
    *w.text.failure.lock() = Some(TextError::Other("Generation failed: QNN error 1002".into()));
    let notice = w.categorize().unwrap();
    assert_eq!(notice, "Categorizing ran into a problem; 25 will be tried again after the next run");
    assert_eq!(w.statuses(), vec![Analyzed; 25]);
    let run = w.last_sort_run();
    assert_eq!(run.outcome, "failed");
    assert_eq!(run.message, Some(format!("{notice} · skipped chunks: 1")));
}

#[test]
fn item_categorized_meanwhile_is_skipped_not_overwritten() {
    let w = World::new();
    w.analyzed(3);
    let trips = w.category("Trips", "user", "");
    w.category("Beach", "model", "");
    // The user files item 2 while the model is thinking.
    let db = w.db.clone();
    *w.text.before_reply.lock() = Some(Box::new(move || {
        db.read(|c| Ok(c.execute("UPDATE item SET categoryId = ?1, categorySetByUser = 1, status = 'Sorted' WHERE id = 2", [trips]).map(|_| ())?)).unwrap();
    }));
    assert_eq!(w.categorize(), None);
    assert_eq!(w.category_of(2).as_deref(), Some("Trips"));
    assert_eq!(w.category_of(1).as_deref(), Some("Beach"));
    let run = w.last_sort_run();
    assert_eq!((run.outcome.as_str(), run.sorted), ("done", 2));
    assert!(run.message.as_deref().unwrap_or("").contains("already categorized: 1"), "{:?}", run.message);
}

/// Once categories exist, an LFM chunk may add at most 3; the retry keeps the 3 largest.
#[test]
fn lfm_chunk_with_categories_adds_at_most_three_new_ones() {
    let w = World::new();
    w.analyzed(10);
    w.category("Beach", "model", "");
    // Four new categories: Alpha holds 4 items, Bravo 3, Charlie 2 and Delta 1, so Delta's item is the one left out.
    let names: Vec<&str> = [("Alpha", 4), ("Bravo", 3), ("Charlie", 2), ("Delta", 1)].iter().flat_map(|(n, k)| vec![*n; *k]).collect();
    let four = assign_each(&(1..=10).collect::<Vec<_>>(), |id| names[id as usize - 1].to_string());
    w.text.reply(&four, "eos");
    w.text.reply(&four, "eos");
    let notice = w.categorize();
    assert!(w.text.prompt(0).contains("Add at most 3 new categories in this reply."));
    assert!(w.text.prompt(1).contains("Too many new categories (4; at most 3 allowed)"));
    assert_eq!(w.category_names(), vec!["Beach", "Alpha", "Bravo", "Charlie"]);
    assert_eq!(w.status(10), Analyzed);
    assert_eq!(notice.as_deref(), Some("Categorized 9 of 10; 1 will be tried again after the next run"));
}

#[test]
fn new_category_cap_is_twelve_for_claude_and_for_a_first_lfm_chunk() {
    let w = World::new();
    w.analyzed(2);
    w.category("Beach", "model", "");
    *w.choice.lock() = SortChoice::Claude;
    w.categorize();
    assert!(!w.claude.prompt(0).contains("new categories in this reply"));
    let first = World::new();
    first.analyzed(2);
    first.categorize();
    assert!(!first.text.prompt(0).contains("new categories in this reply"));
    assert!(first.text.prompt(0).contains("(none yet)"));
}

/// Chunks are sized with the real category list, so many categories make smaller chunks.
#[test]
fn many_categories_shrink_the_chunks() {
    let w = World::new();
    w.analyzed(10);
    w.category("Beach", "model", "");
    let waiting: Vec<SortItem> = w
        .db
        .uncategorized()
        .unwrap()
        .into_iter()
        .map(|i| SortItem { id: i.id, kind: i.kind, suggested_name: i.suggested_name, tags: i.tags, description: i.description, actions: vec![], name_set_by_user: false })
        .collect();
    let limit = (planner::FIT_FRACTION * sorting::BUDGET_LFM3_1_7B.context_tokens as f64) as usize;
    assert!(planner::chunk_tokens(&waiting, planner::category_tokens(&w.db.category_infos().unwrap())) <= limit, "ten items fit beside one category");
    // Add categories until the ten no longer fit beside the list.
    let mut n = 0;
    while planner::chunk_tokens(&waiting, planner::category_tokens(&w.db.category_infos().unwrap())) <= limit {
        n += 1;
        w.category(&format!("Topic {n}"), "user", &format!("A topic with a long description that fills the prompt, number {n} of many. ").repeat(4));
    }
    assert_eq!(w.categorize(), None);
    let asked = w.text.asked();
    assert!((1..10).contains(&asked[0].len()), "{:?}", asked[0]);
    assert_eq!(asked.concat(), (1..=10).collect::<Vec<i64>>());
    assert_eq!(w.statuses(), vec![Sorted; 10]);
}

/// A reply cut off at its length cap splits the chunk in half instead of retrying the same size.
#[test]
fn length_stop_splits_the_chunk_in_half() {
    let w = World::new();
    w.analyzed(4);
    w.category("Beach", "model", "");
    w.text.reply(r#"{"new_categories":[],"assignments":[{"id":1,"category":"Be"#, "length");
    assert_eq!(w.categorize(), None);
    assert_eq!(w.text.asked(), vec![vec![1, 2, 3, 4], vec![1, 2], vec![3, 4]]);
    assert!(w.text.prompts.lock().iter().all(|p| !p.contains("rejected")));
    assert_eq!(*w.text.max_tokens.lock(), vec![720, 660, 660]);
    assert_eq!(w.sort_progress(), vec![(0, 4), (4, 4)]);
}

#[test]
fn max_tokens_stop_splits_down_to_one_item_then_retries_as_usual() {
    let w = World::new();
    w.analyzed(4);
    w.category("Beach", "model", "");
    *w.choice.lock() = SortChoice::Claude;
    for _ in 0..3 {
        w.claude.reply("{", "max_tokens");
    }
    assert_eq!(w.categorize(), None);
    assert_eq!(w.claude.asked(), vec![vec![1, 2, 3, 4], vec![1, 2], vec![1], vec![1], vec![2], vec![3, 4]]);
    // A one-item chunk can't be split: its truncated reply is retried with the problem.
    assert!(w.claude.prompt(3).contains("Your previous reply was rejected"));
}

/// A run row a process death left open is closed as interrupted when the next run starts.
#[test]
fn open_run_row_is_closed_as_interrupted_at_the_next_runs_start() {
    let w = World::new();
    w.analyzed(2);
    w.category("Beach", "model", "");
    let stale = w.db.start_sort_run("m", "sort-v2", 2).unwrap();
    assert_eq!(w.categorize(), None);
    let runs = w.db.recent_sort_runs().unwrap();
    assert_eq!(runs.len(), 2);
    let (current, old) = (&runs[0], &runs[1]);
    assert_eq!((old.id, old.outcome.as_str(), old.message.as_deref()), (stale, "partial", Some(SORT_INTERRUPTED)));
    assert!(old.finished_at.is_some());
    assert_eq!(current.outcome, "done");
}

/// The notice a cold start shows comes from the newest finished run that wasn't stopped.
#[test]
fn stored_sort_notice_reads_the_last_runs_sentence() {
    let run = |id: i64, outcome: &str, message: Option<&str>| SortRun {
        id,
        started_at: id,
        finished_at: Some(id + 1),
        model_id: "m".into(),
        items: 10,
        sorted: 0,
        new_categories: 0,
        outcome: outcome.into(),
        message: message.map(str::to_string),
    };
    let unavailable = run(3, "unavailable", Some("No connection; categorizing will try again after the next run"));
    let partial = run(2, "partial", Some("Categorized 3 of 10; 7 will be tried again after the next run · left out: 7"));
    assert_eq!(stored_sort_notice(&[]), None);
    assert_eq!(stored_sort_notice(&[unavailable.clone(), partial.clone()]), unavailable.message);
    // The details after " · " are for the record only.
    assert_eq!(stored_sort_notice(&[partial]).as_deref(), Some("Categorized 3 of 10; 7 will be tried again after the next run"));
    // A done run clears it; a Stop or an interrupted run keeps the one before (as the live notice does).
    assert_eq!(stored_sort_notice(&[run(4, "done", None), unavailable.clone()]), None);
    assert_eq!(stored_sort_notice(&[run(5, "partial", Some("Stopped · left out: 1")), run(4, "partial", Some("Interrupted")), unavailable.clone()]), unavailable.message);
    assert_eq!(stored_sort_notice(&[run(6, "failed", None)]), None);
}

/// Replies that are cut off (stop `max_tokens`) for prompts of more than `limit` items, else valid.
fn cut_off_above(model: &FakeText, limit: usize) {
    model.answer(move |user| {
        let ids = ids_in(user);
        if ids.len() > limit {
            ("{".into(), "max_tokens".into())
        } else {
            (assign_all(&ids, "Beach"), "end_turn".into())
        }
    });
}

fn sizes(model: &FakeText) -> Vec<usize> {
    model.asked().iter().map(Vec::len).collect()
}

/// Splitting is bounded: a chunk that always stops at its cap costs 6 calls, then waits.
#[test]
fn always_cut_off_200_item_chunk_makes_six_calls_and_leaves_every_item_out() {
    let w = World::new();
    w.analyzed(200);
    w.category("Beach", "model", "");
    *w.choice.lock() = SortChoice::Claude;
    cut_off_above(&w.claude, 0);
    let notice = w.categorize().unwrap();
    // A length stop splits at once: no call repeats a chunk at the same size.
    assert_eq!(sizes(&w.claude), vec![200, 100, 50, 25, 13, 7]);
    assert_eq!(notice, "Categorized 0 of 200; 200 will be tried again after the next run");
    assert_eq!(w.statuses(), vec![Analyzed; 200]);
    let message = w.last_sort_run().message.unwrap();
    assert!(message.contains("left out: 200") && !message.contains("skipped chunks"), "{message}");
}

#[test]
fn cut_off_only_above_50_sorts_everything_within_the_cap() {
    let w = World::new();
    w.analyzed(100);
    w.category("Beach", "model", "");
    *w.choice.lock() = SortChoice::Claude;
    cut_off_above(&w.claude, 50);
    assert_eq!(w.categorize(), None);
    assert_eq!(sizes(&w.claude), vec![100, 50, 50]);
}

/// 200 items cut off above 50 need 7 calls (200, 100, 50, 50, 100, 50, 50): the last 50 wait.
#[test]
fn cut_off_above_50_on_200_items_stops_at_the_cap_and_leaves_the_rest_out() {
    let w = World::new();
    w.analyzed(200);
    w.category("Beach", "model", "");
    *w.choice.lock() = SortChoice::Claude;
    cut_off_above(&w.claude, 50);
    assert_eq!(w.categorize().as_deref(), Some("Categorized 150 of 200; 50 will be tried again after the next run"));
    assert_eq!(sizes(&w.claude), vec![200, 100, 50, 50, 100, 50]);
    assert_eq!(w.statuses(), [vec![Sorted; 150], vec![Analyzed; 50]].concat());
    assert!(w.last_sort_run().message.unwrap().contains("left out: 50"));
}

// ---------------------------------------------------------------------------------------------
// ResortJobTest
// ---------------------------------------------------------------------------------------------

/// Trips (the user's, id 1) holds item 3, placed by the user. Beach (the model's, id 2) holds
/// item 1, item 2 (named by the user) and item 4 (binned). The sorting model answers "Fresh".
fn library() -> World {
    let w = World::new();
    w.analyzed(4);
    let trips = w.category("Trips", "user", "");
    let beach = w.category("Beach", "model", "");
    w.sql(&format!(
        "UPDATE item SET status = 'Sorted', categoryId = {beach}, friendlyName = 'Name ' || id WHERE id IN (1, 2, 4);
         UPDATE item SET friendlyName = 'My name', nameSetByUser = 1 WHERE id = 2;
         UPDATE item SET status = 'Sorted', categoryId = {trips}, categorySetByUser = 1, friendlyName = 'Name 3' WHERE id = 3;
         UPDATE item SET status = 'Binned' WHERE id = 4;"
    ));
    for model in [&w.text, &w.claude] {
        model.answer(|user| (assign_all(&ids_in(user), "Fresh"), "eos".into()));
    }
    w
}

fn resort(w: &World, model: SortChoice, keep_edits: bool) -> PipelineState {
    w.pipeline.resort(w.env(), model, keep_edits).unwrap();
    w.pipeline.wait();
    w.state()
}

#[test]
fn resort_keeps_locked_names_and_categories() {
    let w = library();
    let state = resort(&w, SortChoice::Lfm3, true);
    assert_eq!((state.phase, state.sort_notice), (Phase::Idle, None));
    assert_eq!(w.text.asked(), vec![vec![1, 2, 4]], "the item the user placed is not sent");
    assert_eq!(w.category_of(3).as_deref(), Some("Trips"));
    for id in [1, 2, 4] {
        assert_eq!((w.status(id), w.category_of(id).as_deref()), (Sorted, Some("Fresh")), "{id}");
    }
    // The model's rename is applied, except where the user chose the name.
    assert_eq!(w.item(1).friendly_name.as_deref(), Some("Item 1"));
    assert_eq!(w.item(2).friendly_name.as_deref(), Some("My name"));
    // The model's emptied category goes; the user's stays.
    assert_eq!(w.category_names(), vec!["Trips", "Fresh"]);
    assert!(w.text.prompt(0).contains("Trips\t1\tlocked"));
}

#[test]
fn resort_without_keep_edits_clears_locks() {
    let w = library();
    resort(&w, SortChoice::Lfm3, false);
    assert_eq!(w.text.asked(), vec![vec![1, 2, 3, 4]]);
    assert!((1..=4).all(|id| w.category_of(id).as_deref() == Some("Fresh")));
    assert_eq!(w.item(2).friendly_name.as_deref(), Some("Item 2"));
    assert_eq!(w.category_names(), vec!["Fresh"]);
    assert!(w.db.items().unwrap().iter().all(|i| !i.name_set_by_user && !i.category_set_by_user));
}

/// Re-sort uses the model chosen in its sheet, not the one in Settings.
#[test]
fn resort_uses_the_chosen_model_not_settings() {
    let w = library();
    assert_eq!(*w.choice.lock(), SortChoice::Lfm3);
    resort(&w, SortChoice::Claude, true);
    assert_eq!((w.claude.calls(), w.text.calls()), (1, 0));
    assert_eq!(w.last_sort_run().model_id, "fake/claude");
}

#[test]
fn resort_with_an_unavailable_backend_clears_nothing() {
    let w = library();
    *w.sorting_unavailable.lock() = Some("Add a Claude API key in Settings to categorize".into());
    let state = resort(&w, SortChoice::Claude, false);
    assert_eq!(state.sort_notice.as_deref(), Some("Add a Claude API key in Settings to categorize"));
    assert_eq!(w.category_names(), vec!["Trips", "Beach"]);
    assert_eq!(w.category_of(1).as_deref(), Some("Beach"));
    assert_eq!(w.status(4), ItemStatus::Binned);
    assert!(w.item(2).name_set_by_user && w.item(3).category_set_by_user);
    assert_eq!(w.claude.calls(), 0);
}

#[test]
fn categorize_now_sorts_only_waiting_items() {
    let w = library();
    let id = add(&w.db, "5.jpg");
    analyze(&w.db, id, "Name 5", &["sea"]);
    assert_eq!(w.categorize(), None);
    assert_eq!(w.text.asked(), vec![vec![5]]);
    assert_eq!(w.category_of(1).as_deref(), Some("Beach"));
    assert_eq!(w.category_of(5).as_deref(), Some("Fresh"));
}

#[test]
fn a_stopped_resort_keeps_what_it_sorted_and_the_rest_wait() {
    let w = World::new();
    w.analyzed(15);
    let beach = w.category("Beach", "model", "");
    w.sql(&format!("UPDATE item SET status = 'Sorted', categoryId = {beach}"));
    w.text.answer(|user| (assign_all(&ids_in(user), "Fresh"), "eos".into()));
    // The re-sort empties Beach, so call 0 proposes categories, call 1 sorts ten items, and call 2 is held.
    w.text.gate.hold_at(2);
    w.pipeline.resort(w.env(), SortChoice::Lfm3, true).unwrap();
    w.text.gate.wait_entered();
    w.pipeline.stop_and_wait();
    assert_eq!(w.state().phase, Phase::Stopped);
    assert_eq!(w.statuses(), [vec![Sorted; 10], vec![Analyzed; 5]].concat());
    assert_eq!(w.categorize(), None);
    assert!((1..=15).all(|id| w.category_of(id).as_deref() == Some("Fresh")));
}
