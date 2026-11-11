//! The whole app through its command interface, with the simulated models standing in for GenieX:
//! create a project, add media, analyze, categorize, search, edit, and bin the files.

use binning_core::app::App;
use binning_core::paths::norm;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Harness {
    _dir: tempfile::TempDir,
    root: String,
    app: Arc<App>,
    events: Arc<Mutex<Vec<(String, Value)>>>,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let root = norm(dir.path());
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let app = App::new(dir.path().join("data"), format!("{root}/Projects"), Arc::new(move |name, payload| sink.lock().push((name.to_string(), payload))));
    *app.mock_delay.lock() = Duration::ZERO;
    app.dispatch("settings.set", &json!({"backend": "Simulated"})).unwrap();
    Harness { _dir: dir, root, app, events }
}

impl Harness {
    fn call(&self, cmd: &str, args: Value) -> Value {
        self.app.dispatch(cmd, &args).unwrap_or_else(|e| panic!("{cmd} failed: {e}"))
    }

    fn photo(&self, rel: &str, rgb: [u8; 3]) -> String {
        let path = format!("{}/{rel}", self.root);
        std::fs::create_dir_all(binning_core::paths::parent(&path)).unwrap();
        image::RgbImage::from_pixel(96, 64, image::Rgb(rgb)).save(&path).unwrap();
        path
    }

    /// Waits until the pipeline is neither running nor about to start.
    fn settle(&self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut quiet = 0;
        while Instant::now() < deadline {
            let adding = self.call("grid", json!({}))["addsRunning"].as_bool().unwrap_or(false);
            if !adding && !self.app.pipeline.is_running() {
                quiet += 1;
                if quiet >= 3 {
                    return;
                }
            } else {
                quiet = 0;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        panic!("the pipeline never settled: {:?}", self.app.pipeline.state());
    }

    fn wait_for_event(&self, name: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some((_, v)) = self.events.lock().iter().find(|(n, v)| n == name && done(v)) {
                return v.clone();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no {name} event");
    }
}

#[test]
fn analyze_categorize_search_edit_and_bin() {
    let h = harness();
    let boot = h.call("boot", json!({}));
    assert_eq!(boot["simulated"], true);
    assert!(boot["projects"]["current"].is_null());

    let ocean = h.photo("camera/IMG_0001.png", [20, 70, 210]);
    h.photo("camera/IMG_0002.png", [25, 80, 200]);
    h.photo("camera/trip/IMG_0003.png", [30, 160, 40]);
    h.photo("camera/IMG_0004.png", [230, 110, 40]);
    std::fs::write(format!("{}/camera/broken.jpg", h.root), b"not a picture").unwrap();
    std::fs::write(format!("{}/camera/notes.txt", h.root), b"ignore me").unwrap();

    let projects = h.call("project.create", json!({"name": "Maui"}));
    assert_eq!(projects["current"]["name"], "Maui");
    let project_dir = projects["current"]["path"].as_str().unwrap().to_string();

    h.call("media.addFolder", json!({"path": format!("{}/camera", h.root)}));
    let done = h.wait_for_event("registration", |v| v["finished"] == true);
    assert_eq!(done["summary"]["text"], "5 added");
    h.settle();

    let state = h.app.pipeline.state();
    assert_eq!(format!("{:?}", state.phase), "Idle");
    assert_eq!((state.total, state.analyzed, state.failed, state.pending), (5, 4, 1, 0));
    assert!(state.sort_notice.is_none(), "{:?}", state.sort_notice);

    let grid = h.call("grid", json!({}));
    assert_eq!(grid["rows"].as_array().unwrap().len(), 5);
    assert_eq!(grid["counts"]["readyToBin"], 4);
    assert_eq!(grid["counts"]["failed"], 1);
    assert_eq!(grid["status"]["text"], "4 files ready to bin · 1 failed");
    assert_eq!(grid["status"]["binFilesEnabled"], true);
    let labels: Vec<&str> = grid["chips"].as_array().unwrap().iter().map(|c| c["label"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["All", "Failed", "Ocean", "Sunsets", "Nature"]);
    assert!(grid["rows"][0]["thumb"].is_string());

    // Search finds by tag, with where it matched; a category chip narrows the grid.
    let found = h.call("grid", json!({"query": "fores"}));
    assert_eq!(found["rows"].as_array().unwrap().len(), 1);
    assert_eq!(found["rows"][0]["matchedIn"], "Name");
    let ocean_chip = grid["chips"].as_array().unwrap().iter().find(|c| c["label"] == "Ocean").unwrap()["chip"].as_str().unwrap().to_string();
    let only_ocean = h.call("grid", json!({"filter": {"chip": ocean_chip}}));
    assert_eq!(only_ocean["rows"].as_array().unwrap().len(), 2);
    let ocean_id = only_ocean["rows"][0]["id"].as_i64().unwrap();

    let detail = h.call("item.detail", json!({"id": ocean_id}));
    assert_eq!(detail["simulated"], true);
    assert_eq!(detail["category"]["name"], "Ocean");
    assert!(detail["analysis"]["description"].as_str().unwrap().starts_with("Simulated analysis"));

    // The failed item is set by hand; a clash with an existing category is refused.
    let failed_id = h.call("grid", json!({"filter": {"chip": "failed"}}))["rows"][0]["id"].as_i64().unwrap();
    let clash = h.call("items.move", json!({"ids": [ocean_id], "category": {"new": "oceans"}}));
    assert_eq!(clash["ok"], false);
    let by_hand = h.call("item.setByHand", json!({"id": failed_id, "name": "Broken One", "category": {"new": "Odd Ones"}}));
    assert_eq!(by_hand["ok"], true);
    // Rename a file's friendly name: it becomes the user's.
    let saved = h.call("item.save", json!({"id": ocean_id, "name": "Blue Lagoon"}));
    assert_eq!(saved["ok"], true);
    assert_eq!(h.call("item.detail", json!({"id": ocean_id}))["item"]["nameSetByUser"], true);

    // Bin Files: preview, run, report.
    let preview = h.call("bin.preview", json!({}));
    assert_eq!(preview["preview"]["files"], 5);
    assert_eq!(preview["preview"]["folders"], 4);
    assert_eq!(h.call("bin.start", json!({}))["ok"], true);
    h.settle();
    assert!(std::path::Path::new(&format!("{project_dir}/Ocean/Blue Lagoon.png")).is_file());
    assert!(std::path::Path::new(&format!("{project_dir}/Odd Ones/Broken One.jpg")).is_file());
    assert!(!std::path::Path::new(&ocean).exists(), "the original moved");
    let report = h.call("bin.report", json!({}));
    assert_eq!(report["report"]["headline"], "5 files binned into 4 category folders");
    h.call("bin.closeReport", json!({"runId": report["report"]["runId"]}));
    assert!(h.call("bin.report", json!({}))["report"].is_null());
    assert_eq!(h.call("grid", json!({}))["status"]["text"], "All files binned");

    // A Save on a binned file moves it at once.
    let renamed = h.call("item.save", json!({"id": ocean_id, "name": "Lagoon At Noon"}));
    assert_eq!(renamed["ok"], true);
    h.settle();
    assert!(std::path::Path::new(&format!("{project_dir}/Ocean/Lagoon At Noon.png")).is_file());
    assert!(!std::path::Path::new(&format!("{project_dir}/Ocean/Blue Lagoon.png")).exists());

    // Merge two categories and rename the survivor: binned files follow.
    let cats = h.call("grid", json!({}))["categories"].clone();
    let id_of = |name: &str| cats.as_array().unwrap().iter().find(|c| c["name"] == name).unwrap()["id"].as_i64().unwrap();
    let edit = h.call("categories.apply", json!({"merges": [[id_of("Ocean"), id_of("Nature")]], "renames": [[id_of("Ocean"), "Outdoors"]]}));
    assert_eq!(edit["ok"], true, "{edit}");
    h.settle();
    assert_eq!(std::fs::read_dir(format!("{project_dir}/Outdoors")).unwrap().count(), 3);
    assert!(!std::path::Path::new(&format!("{project_dir}/Nature")).exists(), "the emptied folder is removed");
    h.call("bin.closeReport", json!({"runId": h.call("bin.report", json!({}))["report"]["runId"]}));

    // Re-sort keeps the user's edits and files everything again.
    assert_eq!(h.call("analysis.resort", json!({"model": "Lfm3_1_7B", "keepEdits": true}))["ok"], true);
    h.settle();
    let after = h.call("grid", json!({}));
    assert_eq!(after["counts"]["waiting"], 0);
    assert_eq!(h.call("item.detail", json!({"id": ocean_id}))["item"]["friendlyName"], "Lagoon At Noon");

    // Remove from project leaves the file where it is and the folder check quiet.
    h.call("items.remove", json!({"ids": [failed_id]}));
    assert!(std::path::Path::new(&format!("{project_dir}/Odd Ones/Broken One.jpg")).is_file());
    assert_eq!(h.call("bin.preview", json!({}))["unexpected"].as_array().unwrap().len(), 0);

    // The project list: remove takes it off the list only; restore puts it back and reopens it.
    let removed = h.call("project.remove", json!({"path": project_dir}));
    assert!(removed["projects"]["current"].is_null());
    assert!(std::path::Path::new(&project_dir).is_dir());
    let back = h.call("project.restore", removed["removed"].clone());
    assert_eq!(back["current"]["name"], "Maui");
    h.app.shutdown();
}

#[test]
fn stop_is_remembered_and_resume_finishes() {
    let h = harness();
    *h.app.mock_delay.lock() = Duration::from_millis(3);
    for i in 0..6 {
        h.photo(&format!("cam/p{i}.png"), [10 + i * 30, 90, 200 - i * 20]);
    }
    h.call("project.create", json!({"name": "Stops"}));
    h.call("media.addFolder", json!({"path": format!("{}/cam", h.root)}));
    h.wait_for_event("pipeline", |v| v["phase"] == "Describing");
    h.call("analysis.stop", json!({}));
    h.settle();
    let state = h.app.pipeline.state();
    assert_eq!(format!("{:?}", state.phase), "Stopped");
    assert!(state.error.is_none());
    assert!(state.pending > 0, "the stopped item went back to pending");
    assert_eq!(h.call("grid", json!({}))["counts"]["analyzing"], 0);
    h.call("analysis.resume", json!({}));
    h.settle();
    let state = h.app.pipeline.state();
    assert_eq!((format!("{:?}", state.phase).as_str(), state.pending, state.analyzed), ("Idle", 0, 6));
    h.app.shutdown();
}

#[test]
fn held_files_are_reported_and_can_be_copied() {
    let h = harness();
    let a = h.photo("cam/a.png", [200, 40, 40]);
    h.call("project.create", json!({"name": "First"}));
    h.call("media.addFiles", json!({"paths": [a]}));
    h.wait_for_event("registration", |v| v["finished"] == true);
    h.settle();
    h.events.lock().clear();
    h.call("project.create", json!({"name": "Second"}));
    h.call("media.addFiles", json!({"paths": [a, format!("{}/cam/missing.png", h.root)]}));
    let done = h.wait_for_event("registration", |v| v["finished"] == true);
    assert_eq!(done["summary"]["text"], "1 in another project (First) · 1 couldn't be read");
    h.call("media.copyHeld", json!({"held": done["summary"]["held"]}));
    let copied = h.wait_for_event("copy", |v| v["finished"] == true);
    assert_eq!(copied["result"]["text"], "1 copied into Copied from First");
    h.settle();
    assert_eq!(h.call("grid", json!({}))["counts"]["readyToBin"], 1);
    assert!(std::path::Path::new(&a).is_file());
    // Opening a folder that holds no project is refused; one inside a project too.
    assert!(h.app.dispatch("project.open", &json!({"path": format!("{}/cam", h.root)})).is_err());
    h.app.shutdown();
}
