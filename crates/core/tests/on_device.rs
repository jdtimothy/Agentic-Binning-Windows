//! Tests that need the real thing: GenieX with its models on a Snapdragon X PC, or a second
//! drive. All are `#[ignore]`, so `cargo test` and the Build workflow skip them. Run them by hand:
//!
//! ```powershell
//! cargo test -p binning-core --test on_device -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` matters: two GenieX commands at once would fight over the NPU.
//! `--nocapture` prints each reply, its numbers and the exact command, which is what to keep
//! when something fails. `docs/windows-agent-brief.md` says what each test is for.
//!
//! Settings, all optional environment variables:
//! - `AB_GENIEX`: the path to `geniex.exe` when it is not on the PATH.
//! - `AB_COMPUTE`: `npu` (the default), `gpu` or `cpu`.
//! - `AB_OTHER_DRIVE`: an empty folder on another drive (for example `D:\ab-test`), for the two
//!   tests that move files between drives. They are skipped with a note when it is not set.
//!
//! Nothing here touches a file it did not create, and nothing calls Claude.

use binning_core::analysis::{self, AnalysisResult, ItemKind};
use binning_core::app::App;
use binning_core::db::{ItemStatus, NewItem, Profile, ProjectDb};
use binning_core::files;
use binning_core::geniex::{self, GenieCli, GenieTextModel, GenieVisionModel, LFM3_1_7B, LFM3_VL_2B};
use binning_core::geniex_serve::GenieServer;
use binning_core::media::{Ffmpeg, MediaPreparer, PreparedMedia};
use binning_core::models::{ReplyFormat, TextModel, VisionModel, VisionReply};
use binning_core::moves::{BinFilesJob, MoveControl, MoveOutcome};
use binning_core::paths::norm;
use binning_core::sorting::{self, prompt as sort_prompt, schema, validator, Assignment, NewCategoryEntity, SortItem, Validation};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn temp() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = norm(dir.path());
    (dir, root)
}

fn program() -> String {
    let configured = std::env::var("AB_GENIEX").ok();
    geniex::locate(configured.as_deref()).expect("geniex was not found: put it on the PATH, or set AB_GENIEX to the full path of geniex.exe")
}

fn compute() -> String {
    std::env::var("AB_COMPUTE").unwrap_or_else(|_| "npu".into())
}

/// Whether the tests go through `geniex serve`, as the app does. `AB_NO_SERVE=1` runs one
/// `geniex infer` per call instead, which is what the app falls back to.
fn serving() -> bool {
    std::env::var("AB_NO_SERVE").is_err()
}

/// One server for every test in this run, as the app has one for all its runs. Windows ends it
/// with the test process.
fn cli(scratch: &str) -> Arc<GenieCli> {
    static SERVER: std::sync::OnceLock<Arc<GenieServer>> = std::sync::OnceLock::new();
    let cli = GenieCli::new(program(), compute(), scratch.to_string());
    if !serving() {
        return Arc::new(cli);
    }
    let server = SERVER.get_or_init(|| Arc::new(GenieServer::new(norm(std::env::temp_dir().join("ab-on-device-serve")))));
    Arc::new(cli.with_server(server.clone()))
}

fn vision(scratch: &str) -> GenieVisionModel {
    GenieVisionModel::new(cli(scratch), LFM3_VL_2B)
}

fn text(scratch: &str) -> GenieTextModel {
    GenieTextModel { cli: cli(scratch), spec: LFM3_1_7B }
}

/// A picture that is hard to describe wrongly: a big square of one colour on white.
fn square_picture(path: &str, rgb: [u8; 3]) {
    std::fs::create_dir_all(binning_core::paths::parent(path)).unwrap();
    let mut img = image::RgbImage::from_pixel(640, 480, image::Rgb([255, 255, 255]));
    for y in 120..360 {
        for x in 200..440 {
            img.put_pixel(x, y, image::Rgb(rgb));
        }
    }
    img.save(path).unwrap();
}

/// Prepares `source` exactly as the app does before a model call (a square JPEG and a thumbnail).
fn prepare(root: &str, id: i64, source: &str, edge: u32) -> PreparedMedia {
    let preparer = MediaPreparer { work_dir: format!("{root}/work"), thumbs_dir: format!("{root}/thumbs"), ffmpeg: Ffmpeg::default(), grabber: None };
    std::fs::create_dir_all(&preparer.thumbs_dir).unwrap();
    preparer.prepare(id, source, ItemKind::Photo, None, edge, 16, &|_, _| {}).expect("the test picture could not be prepared")
}

/// Runs one vision call and prints everything worth keeping.
fn describe(model: &GenieVisionModel, media: &PreparedMedia) -> (VisionReply, usize, f64) {
    let started = Instant::now();
    let mut pieces = 0;
    let reply = model.generate(media, &mut |_| pieces += 1);
    let seconds = started.elapsed().as_secs_f64();
    println!("\ncommand: {}", model.last_command().unwrap_or_default());
    let reply = reply.unwrap_or_else(|e| panic!("geniex infer failed after {seconds:.1} s: {}\nRun the command above in a terminal to see what GenieX says.", e.0));
    println!("reply:   {}", reply.text);
    println!("numbers: {seconds:.1} s in all, stop reason {:?}, {} tokens, {:.1} tokens a second, first token after {:.0} ms, streamed in {pieces} pieces", reply.profile.stop_reason, reply.profile.generated_tokens, reply.profile.decoding_speed, reply.profile.ttft_ms);
    (reply, pieces, seconds)
}

fn words_of(result: &AnalysisResult) -> String {
    format!("{} {} {}", result.description, result.tags.join(" "), result.suggested_name).to_lowercase()
}

// ---------------------------------------------------------------------------------------------
// 1. GenieX is there
// ---------------------------------------------------------------------------------------------

/// `geniex --version` and `geniex list --format json` answer in the shape the app reads, and
/// both models are downloaded. Everything else here needs this one to pass first.
#[test]
#[ignore = "needs GenieX"]
fn g01_geniex_reports_its_version_and_both_models() {
    let status = geniex::status(std::env::var("AB_GENIEX").ok().as_deref());
    println!("{status:#?}");
    assert!(status.path.is_some(), "geniex was not found");
    assert!(status.version.as_deref().is_some_and(|v| !v.is_empty()), "`geniex --version` printed nothing the app could read");
    assert_eq!(status.error, None, "`geniex list --format json` failed");
    assert!(!status.models.is_empty(), "`geniex list --format json` gave no models the app could read: its JSON shape may have changed (see parse_list in geniex.rs)");
    assert!(status.vision_ready, "the vision model is missing: geniex pull {} --model-type vlm", LFM3_VL_2B.key());
    assert!(status.text_ready, "the sorting model is missing: geniex pull {} --model-type llm", LFM3_1_7B.key());
}

// ---------------------------------------------------------------------------------------------
// 2. The vision model
// ---------------------------------------------------------------------------------------------

/// One photo: the command ends by itself, the picture is really attached (the reply says "red"),
/// the reply is the JSON the grammar asks for, and it ends with `eos` inside the token cap.
#[test]
#[ignore = "needs GenieX"]
fn g02_a_photo_returns_json_that_describes_it() {
    let (_d, root) = temp();
    square_picture(&format!("{root}/camera/red.png"), [220, 20, 20]);
    let model = vision(&format!("{root}/scratch"));
    model.load(16).unwrap();
    let media = prepare(&root, 1, &format!("{root}/camera/red.png"), model.image_edge());
    let (reply, pieces, _) = describe(&model, &media);
    assert_eq!(reply.profile.stop_reason, "eos", "the reply did not end by itself");
    assert!((1..=400).contains(&reply.profile.generated_tokens), "photos are capped at 400 tokens");
    let result = analysis::parse(&reply.text, ItemKind::Photo).expect("the reply is not the JSON object the app reads: is --grammar-path honoured?");
    assert!(words_of(&result).contains("red"), "the reply never mentions red, so the picture was probably not attached: check how the CLI finds the image path in the prompt file");
    assert!(result.actions.is_empty() && (3..=10).contains(&result.tags.len()));
    if pieces < 2 {
        println!("NOTE: the reply arrived in one piece, so the card won't show it being typed. The CLI may buffer its output when it is not a terminal.");
    }
}

/// The same with spaces in every path, as under `C:\Users\First Last\...`.
#[test]
#[ignore = "needs GenieX"]
fn g03_paths_with_spaces_work() {
    let (_d, root) = temp();
    let spaced = format!("{root}/My Pictures and Clips");
    square_picture(&format!("{spaced}/camera roll/blue one.png"), [20, 40, 220]);
    let model = vision(&format!("{spaced}/scratch files"));
    model.load(16).unwrap();
    let media = prepare(&spaced, 1, &format!("{spaced}/camera roll/blue one.png"), model.image_edge());
    assert!(media.image_paths[0].contains(' '));
    let (reply, _, _) = describe(&model, &media);
    let result = analysis::parse(&reply.text, ItemKind::Photo).expect("the reply is not readable JSON");
    assert!(words_of(&result).contains("blue"), "the picture at a path with spaces was not attached: the CLI's path pattern may stop at a space");
}

/// A clip is sent as several images with their times. The reply must have 2-5 actions with
/// `m:ss` times, and should notice that the square changes colour half way.
#[test]
#[ignore = "needs GenieX"]
fn g04_video_frames_get_actions_with_times() {
    let (_d, root) = temp();
    let model = vision(&format!("{root}/scratch"));
    model.load(16).unwrap();
    let mut media = PreparedMedia { kind: ItemKind::Video, image_paths: Vec::new(), frame_times_ms: Vec::new(), thumbnail_path: String::new() };
    for i in 0..8i64 {
        let source = format!("{root}/frames/f{i}.png");
        square_picture(&source, if i < 4 { [220, 20, 20] } else { [20, 40, 220] });
        let frame = prepare(&root, 100 + i, &source, model.image_edge());
        media.image_paths.push(frame.image_paths[0].clone());
        media.frame_times_ms.push(i * 500);
        media.thumbnail_path = frame.thumbnail_path;
    }
    println!("prompt file:\n{}", geniex::vision_prompt(&media).unwrap());
    let (reply, _, _) = describe(&model, &media);
    assert_eq!(reply.profile.stop_reason, "eos");
    assert!(reply.profile.generated_tokens <= 520, "videos are capped at 520 tokens");
    let result = analysis::parse(&reply.text, ItemKind::Video).expect("the reply is not readable JSON");
    assert!(result.actions.is_empty(), "the main reply leaves a video's actions to the moment calls: {:?}", result.actions);
    // The app picks the moments: the opening, and the frame where the square changes colour.
    let changes = binning_core::media::frame_changes(&media.image_paths);
    let picked = analysis::moments::pick(&changes);
    println!("change per frame: {changes:.0?}  picked: {picked:?}");
    assert_eq!(picked, vec![0, 4], "the only change is between frames 3 and 4");
    let mut phrases = Vec::new();
    for frame in picked {
        let moment = model.describe_moment(&media.image_paths[frame]).unwrap();
        println!("{}  {:?}  ({} tokens, {})", analysis::moments::clock(media.frame_times_ms[frame]), moment.text, moment.profile.generated_tokens, moment.profile.stop_reason);
        phrases.push(analysis::moments::clean(&moment.text).expect("a moment call gave no usable phrase").to_lowercase());
    }
    if !(phrases[0].contains("red") && phrases[1].contains("blue")) {
        println!("NOTE: the phrases do not name red at the opening and blue at the change: {phrases:?}");
    }
}

/// Without `--grammar-path`. If g02 fails and this passes, the grammar flag is the problem.
#[test]
#[ignore = "needs GenieX"]
fn g05_a_photo_without_the_grammar_flag() {
    let (_d, root) = temp();
    square_picture(&format!("{root}/camera/red.png"), [220, 20, 20]);
    let mut model = vision(&format!("{root}/scratch"));
    model.use_grammar = false;
    model.load(16).unwrap();
    let media = prepare(&root, 1, &format!("{root}/camera/red.png"), model.image_edge());
    let (reply, _, _) = describe(&model, &media);
    assert!(!reply.text.trim().is_empty());
    println!("parses as the app's JSON without a grammar: {}", analysis::parse(&reply.text, ItemKind::Photo).is_some());
}

/// Stop must end the call quickly, and the next call must work. With one `geniex infer` per
/// call (`AB_NO_SERVE=1`) no GenieX process may be left running; `geniex serve` stays, by design.
#[test]
#[ignore = "needs GenieX"]
fn g06_stop_ends_the_command_and_the_next_call_works() {
    let (_d, root) = temp();
    square_picture(&format!("{root}/camera/red.png"), [220, 20, 20]);
    let model = Arc::new(vision(&format!("{root}/scratch")));
    model.load(16).unwrap();
    let media = prepare(&root, 1, &format!("{root}/camera/red.png"), model.image_edge());
    let (worker_model, worker_media) = (model.clone(), media.clone());
    let worker = std::thread::spawn(move || worker_model.generate(&worker_media, &mut |_| {}));
    // Through the server the model is already loaded and a photo takes about 3 s, so Stop comes sooner.
    std::thread::sleep(Duration::from_millis(if serving() { 300 } else { 1500 }));
    let asked = Instant::now();
    model.cancel();
    let reply = worker.join().unwrap().expect("a stopped call is not an error");
    println!("stopped {:.1} s after asking; stop reason {:?}", asked.elapsed().as_secs_f64(), reply.profile.stop_reason);
    assert_eq!(reply.profile.stop_reason, "user", "the call was not stopped (if it simply finished in under 1.5 s, that is good news: say so in the notes)");
    assert!(asked.elapsed() < Duration::from_secs(10), "Stop took too long");
    std::thread::sleep(Duration::from_secs(2));
    if cfg!(windows) && !serving() {
        let tasks = std::process::Command::new("tasklist").output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
        let left: Vec<&str> = tasks.lines().filter(|l| l.contains("geniex")).collect();
        assert!(left.is_empty(), "a GenieX process is still running after Stop (the CLI may start a helper that outlives it): {left:?}");
    }
    model.load(16).unwrap();
    let (again, _, _) = describe(&model, &media);
    assert_eq!(again.profile.stop_reason, "eos", "the call after a Stop must run normally");
}

/// Not a pass or fail: how long a call takes, twice in a row. Through `geniex serve` the model
/// is loaded once, before the first photo. With `AB_NO_SERVE=1` each photo starts `geniex` again
/// and loads the model again, which is the difference these numbers show.
#[test]
#[ignore = "needs GenieX"]
fn g07_timings_for_two_photos_in_a_row() {
    let (_d, root) = temp();
    let model = vision(&format!("{root}/scratch"));
    let loading = Instant::now();
    model.load(16).unwrap();
    println!("loading the model took {:.1} s (nothing is loaded here with AB_NO_SERVE=1)", loading.elapsed().as_secs_f64());
    let mut seconds = Vec::new();
    for (i, rgb) in [[220, 20, 20], [20, 160, 40]].into_iter().enumerate() {
        let source = format!("{root}/camera/{i}.png");
        square_picture(&source, rgb);
        let media = prepare(&root, i as i64 + 1, &source, model.image_edge());
        let (reply, _, took) = describe(&model, &media);
        let writing = if reply.profile.decoding_speed > 0.0 { reply.profile.generated_tokens as f64 / reply.profile.decoding_speed } else { 0.0 };
        println!("photo {}: {took:.1} s in all, about {writing:.1} s of it writing the reply, so about {:.1} s on everything else (reading the picture; with AB_NO_SERVE=1 also starting GenieX and loading the model)", i + 1, (took - writing).max(0.0));
        seconds.push(took);
    }
    println!("\nWRITE DOWN: seconds per photo = {seconds:.1?}. A 1,000 photo project takes about {:.0} minutes at this rate.", seconds.iter().sum::<f64>() / seconds.len() as f64 * 1000.0 / 60.0);
}

// ---------------------------------------------------------------------------------------------
// 3. The sorting model
// ---------------------------------------------------------------------------------------------

fn library() -> Vec<SortItem> {
    let item = |id: i64, name: &str, tags: &[&str], description: &str| SortItem {
        id,
        kind: ItemKind::Photo,
        suggested_name: name.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        description: description.into(),
        actions: vec![],
        name_set_by_user: false,
    };
    vec![
        item(1, "Sea turtle on sand", &["turtle", "beach", "sand"], "A sea turtle rests on wet sand near the waterline."),
        item(2, "Monk seal sleeping", &["seal", "beach", "rocks"], "A monk seal sleeps on dark rocks beside the sea."),
        item(3, "Gecko on a wall", &["gecko", "lizard", "wall"], "A green gecko clings to a wooden wall."),
        item(4, "Macadamia pancakes", &["pancakes", "breakfast", "plate"], "A stack of pancakes with macadamia sauce on a white plate."),
        item(5, "Shrimp plate lunch", &["shrimp", "rice", "lunch"], "Garlic shrimp with two scoops of rice in a paper tray."),
        item(6, "Shave ice", &["dessert", "ice", "syrup"], "A cup of shave ice with three colours of syrup."),
        item(7, "Sunset over the sea", &["sunset", "ocean", "clouds"], "The sun sets behind low clouds over a calm ocean."),
        item(8, "Orange sky at dusk", &["sunset", "sky", "palms"], "Palm trees stand dark against an orange sky at dusk."),
        item(9, "Sunrise from the crater", &["sunrise", "mountain", "clouds"], "The sun rises above a sea of clouds seen from a summit."),
        item(10, "Red hibiscus", &["flower", "hibiscus", "garden"], "A red hibiscus flower in close-up with green leaves behind."),
    ]
}

/// Ten items in one call, as the app sends them. The grammar must make the reply list exactly
/// the ten ids once each, and the app's own checks should accept it.
#[test]
#[ignore = "needs GenieX"]
fn g08_the_sorting_model_sorts_ten_items() {
    let (_d, root) = temp();
    let model = text(&format!("{root}/scratch"));
    model.load().unwrap_or_else(|e| panic!("the sorting model can't run: {e:?}"));
    let items = library();
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let format = ReplyFormat { json_schema: schema::assign_json_schema(), grammar: schema::assign_grammar(&ids, &[]) };
    let started = Instant::now();
    let reply = model.complete(sort_prompt::SYSTEM, &sort_prompt::assign_user(&items, &[], &[], sorting::MAX_NEW_CATEGORIES), &format, 30 * 10 + 600);
    println!("\ncommand: {}", model.last_command().unwrap_or_default());
    let reply = reply.unwrap_or_else(|e| panic!("geniex infer failed: {e:?}"));
    println!("reply:   {}\nnumbers: {:.1} s, stop reason {:?}, {} tokens in, {} tokens out", reply.text, started.elapsed().as_secs_f64(), reply.stop_reason, reply.input_tokens, reply.output_tokens);
    assert_eq!(reply.stop_reason, "eos", "the reply was cut off or stopped");
    let parsed = sorting::parse_reply(&reply.text).expect("the reply is not the JSON object the app reads");
    assert_eq!(parsed.assignments.iter().map(|a| a.id).collect::<Vec<_>>(), ids, "the grammar lists every id once, in order: is --grammar-path honoured for the text model?");
    assert!(parsed.assignments.iter().all(|a| !a.category.trim().is_empty() && !a.name.trim().is_empty()));
    match validator::validate(&parsed, &items, &[], 10, false, sorting::MAX_NEW_CATEGORIES) {
        Validation::Ok { new_categories, .. } => println!("accepted; categories: {:?}", new_categories.iter().map(|c| &c.name).collect::<Vec<_>>()),
        // A small model's odd choices are retried by the app; they are not a fault of the plumbing.
        Validation::Rejected(problems) => println!("NOTE: the app would ask again, with these problems: {problems:?}"),
    }
}

/// Existing category names go into the grammar as literals, quotes and apostrophes included.
/// GenieX must accept that grammar, and the reply must use those exact names or new one-word ones.
#[test]
#[ignore = "needs GenieX"]
fn g09_a_grammar_with_existing_category_names_is_accepted() {
    let (_d, root) = temp();
    let model = text(&format!("{root}/scratch"));
    model.load().unwrap_or_else(|e| panic!("the sorting model can't run: {e:?}"));
    let items = library();
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let existing: Vec<sorting::CategoryInfo> = ["Sea Animals", "Mama's \"Best\" Food", "Sunsets"]
        .iter()
        .enumerate()
        .map(|(i, n)| sorting::CategoryInfo { id: i as i64 + 1, name: n.to_string(), description: String::new(), created_by: "user".into(), items: 3 })
        .collect();
    let names: Vec<String> = existing.iter().map(|c| c.name.clone()).collect();
    let format = ReplyFormat { json_schema: schema::assign_json_schema(), grammar: schema::assign_grammar(&ids, &names) };
    let reply = model.complete(sort_prompt::SYSTEM, &sort_prompt::assign_user(&items, &existing, &[], 3), &format, 30 * 10 + 600);
    println!("\ncommand: {}", model.last_command().unwrap_or_default());
    let reply = reply.unwrap_or_else(|e| panic!("geniex infer failed (the grammar file is beside the prompt file named in the command): {e:?}"));
    println!("reply:   {}", reply.text);
    let parsed = sorting::parse_reply(&reply.text).expect("the reply is not readable JSON");
    assert_eq!(parsed.assignments.len(), 10);
    for a in &parsed.assignments {
        assert!(names.contains(&a.category) || !a.category.contains(' '), "{:?} is neither an existing name nor one word", a.category);
    }
}

// ---------------------------------------------------------------------------------------------
// 4. The whole app with GenieX
// ---------------------------------------------------------------------------------------------

/// Create a project, add a folder of six pictures and one unreadable file, and let the app
/// analyze, sort and bin them with the real models. Uses its own data and project folders.
#[test]
#[ignore = "needs GenieX"]
fn g10_the_app_analyzes_sorts_and_bins_a_small_project() {
    let (_d, root) = temp();
    let app = App::new(std::path::PathBuf::from(format!("{root}/data")), format!("{root}/Projects"), Arc::new(|_, _| {}));
    let call = |cmd: &str, args: Value| app.dispatch(cmd, &args).unwrap_or_else(|e| panic!("{cmd} failed: {e}"));
    // The sorting model is named outright, so this test can never make a paid Claude call.
    let boot = call("settings.set", json!({"geniexPath": program(), "compute": compute(), "sortingModel": "Lfm3_1_7B"}));
    assert_eq!(boot["simulated"], false, "the app fell back to the simulated model: {}", boot["genie"]);
    assert_eq!(boot["settings"]["sortingModel"], "Lfm3_1_7B");
    for (i, rgb) in [[220, 20, 20], [230, 40, 30], [20, 40, 220], [30, 60, 210], [20, 160, 40], [240, 200, 30]].into_iter().enumerate() {
        square_picture(&format!("{root}/camera/IMG_{i}.png"), rgb);
    }
    std::fs::write(format!("{root}/camera/broken.jpg"), b"not a picture").unwrap();
    let before: Vec<(String, String)> = files::list_files(&format!("{root}/camera")).into_iter().map(|p| (files::sha256(&p).unwrap(), p)).collect();
    let project = call("project.create", json!({"name": "On Device"}))["current"]["path"].as_str().unwrap().to_string();
    call("media.addFolder", json!({"path": format!("{root}/camera")}));
    let started = Instant::now();
    let settle = || {
        let mut quiet = 0;
        while quiet < 5 {
            assert!(started.elapsed() < Duration::from_secs(1800), "still running after 30 minutes: {:?}", app.pipeline.state());
            let adding = call("grid", json!({}))["addsRunning"].as_bool().unwrap_or(false);
            quiet = if adding || app.pipeline.is_running() { 0 } else { quiet + 1 };
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    settle();
    let state = app.pipeline.state();
    let log = call("log", json!({}));
    println!("analysis and sorting took {:.0} s\nstate: {state:?}\nactivity log: {log:#}", started.elapsed().as_secs_f64());
    let grid = call("grid", json!({}));
    for row in grid["rows"].as_array().unwrap() {
        println!("{:<12} {:<10} {:<16} {}", row["originalName"].as_str().unwrap_or(""), row["status"].as_str().unwrap_or(""), row["categoryName"].as_str().unwrap_or("-"), row["friendlyName"].as_str().unwrap_or("-"));
    }
    assert_eq!(state.error, None, "the run stopped with an error");
    assert_eq!((state.total, state.analyzed, state.failed, state.pending), (7, 6, 1, 0), "six pictures analyzed, the unreadable file failed");
    assert_eq!(state.sort_notice, None, "sorting left items uncategorized");
    assert_eq!(grid["status"]["binFilesEnabled"], true, "{}", grid["status"]);
    assert_eq!(call("bin.start", json!({}))["ok"], true);
    settle();
    let report = call("bin.report", json!({}))["report"].clone();
    println!("report: {report:#}");
    assert_eq!((report["moved"].as_i64(), report["checkOk"].as_bool()), (Some(6), Some(true)));
    // Every picture is in the project, byte for byte, and nothing is left behind or lost.
    let after: Vec<String> = files::list_files(&project).into_iter().filter(|p| !p.contains("/.binning/")).map(|p| files::sha256(&p).unwrap()).collect();
    for (hash, path) in &before {
        if path.ends_with("broken.jpg") {
            assert!(files::exists(path), "a failed file stays where it was");
        } else {
            assert!(after.contains(hash), "{path} is not in the project after Bin Files");
            assert!(!files::exists(path), "{path} was moved, so the original is gone");
        }
    }
    assert_eq!(after.len(), 6);
    app.shutdown();
}

// ---------------------------------------------------------------------------------------------
// 5. Moving files between drives (no GenieX needed)
// ---------------------------------------------------------------------------------------------

/// The folder `AB_OTHER_DRIVE` names, or None with a note. A fresh folder is made inside it and
/// removed afterwards, so what is already there is never touched.
fn other_drive(test: &str) -> Option<String> {
    let Ok(base) = std::env::var("AB_OTHER_DRIVE") else {
        println!("SKIPPED {test}: set AB_OTHER_DRIVE to an empty folder on another drive, for example D:\\ab-test");
        return None;
    };
    let dir = format!("{}/ab-on-device-{}", norm(&base).trim_end_matches('/'), std::process::id());
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot create {dir}: {e}"));
    Some(dir)
}

/// A project at `project` whose three items, already sorted, have their files in `camera`.
fn sorted_project(project: &str, camera: &str) -> (ProjectDb, Vec<(String, String)>) {
    std::fs::create_dir_all(camera).unwrap();
    let db = ProjectDb::open(std::path::Path::new(&format!("{project}/.binning/project.db")), "Drives").unwrap();
    let mut originals = Vec::new();
    for (n, (file, friendly, category, megabytes)) in [("clip.mp4", "Surf Clip", "Surf", 40usize), ("a.jpg", "Morning", "Beach", 3), ("b.jpg", "Evening", "Beach", 3)].into_iter().enumerate() {
        let path = format!("{camera}/{file}");
        // Bytes that differ along the file and between files, so a copy that repeats or drops a block is caught.
        let bytes: Vec<u8> = (0..megabytes * 1024 * 1024).map(|i| ((i / 4096) ^ (i % 251) ^ n) as u8).collect();
        std::fs::write(&path, bytes).unwrap();
        files::set_last_modified(&path, 1_600_000_000_000);
        let hash = files::sha256(&path).unwrap();
        let id = db.insert_item(&NewItem { content_hash: hash.clone(), path: path.clone(), name: file.into(), kind: ItemKind::Photo, taken_at: None, duration_ms: None, width: None, height: None }).unwrap();
        let analysis = AnalysisResult { description: "d".into(), actions: vec![], tags: vec![], suggested_name: friendly.into(), shot_type: "wide".into() };
        db.save_analysis(id, &analysis, "{}", "test", "v", &Profile::default()).unwrap();
        db.apply_chunk(&[NewCategoryEntity { name: category.into(), description: String::new() }], &[Assignment { item_id: id, category_name: category.into(), friendly_name: friendly.into() }]).unwrap();
        originals.push((hash, path));
    }
    (db, originals)
}

fn bin_between_drives(project: &str, camera: &str) {
    assert!(!files::same_volume(camera, project), "{camera} and {project} are on the same drive: point AB_OTHER_DRIVE at another one");
    let (db, originals) = sorted_project(project, camera);
    let nobody = |_: &[String]| Vec::new();
    let job = BinFilesJob::new(&db, project, &nobody);
    let preview = job.preview().unwrap();
    println!("preview: {preview:?}");
    assert_eq!((preview.files, preview.other_storage, preview.sources.len()), (3, 3, 0), "all three files are on the other drive, so all are copied");
    assert_eq!(preview.copy_bytes, 46 * 1024 * 1024);
    let phases = std::cell::RefCell::new(Vec::new());
    let started = Instant::now();
    let outcome = job.bin_files(&MoveControl::new(false), &|p| phases.borrow_mut().extend(p.phase)).unwrap();
    println!("{outcome:?} in {:.1} s; phases: {:?}", started.elapsed().as_secs_f64(), phases.borrow());
    assert_eq!(outcome, MoveOutcome::Done);
    let report = job.unread_report().unwrap().unwrap();
    println!("report: {report:?}");
    assert_eq!((report.moved, report.skipped.len(), report.check_ok), (3, 0, Some(true)));
    for (name, (hash, original)) in ["Surf/Surf Clip.mp4", "Beach/Morning.jpg", "Beach/Evening.jpg"].iter().zip(&originals) {
        let dest = format!("{project}/{name}");
        assert_eq!(&files::sha256(&dest).unwrap(), hash, "{dest} is not the file that was at {original}");
        assert_eq!(files::last_modified(&dest), 1_600_000_000_000, "the copy keeps the file's time");
        assert!(!files::exists(original), "{original} was copied and checked, so the original is deleted");
    }
    assert!(files::list_files(&format!("{project}/.binning/moving")).is_empty(), "no half copy is left behind");
    assert!(db.items().unwrap().iter().all(|i| i.status == ItemStatus::Binned));
    drop(db);
}

/// Sources on the other drive, the project on this one: each file is copied, read back, put in
/// place, and only then is its original deleted.
#[test]
#[ignore = "needs a second drive"]
fn m01_bin_files_copies_from_another_drive_and_checks_every_file() {
    let Some(other) = other_drive("m01") else { return };
    let (_d, root) = temp();
    bin_between_drives(&format!("{root}/project"), &format!("{other}/camera"));
    let _ = std::fs::remove_dir_all(&other);
}

/// The other way round: the project on the other drive (try an exFAT stick here too).
#[test]
#[ignore = "needs a second drive"]
fn m02_a_project_on_another_drive_takes_files_from_this_one() {
    let Some(other) = other_drive("m02") else { return };
    let (_d, root) = temp();
    bin_between_drives(&format!("{other}/project"), &format!("{root}/camera"));
    let _ = std::fs::remove_dir_all(&other);
}

/// A rename inside the other drive: on exFAT or FAT32 this is the path with no hard links, and
/// it must still refuse to replace a file.
#[test]
#[ignore = "needs a second drive"]
fn m03_moves_inside_the_other_drive_never_replace() {
    let Some(other) = other_drive("m03") else { return };
    let (a, b, c) = (format!("{other}/a.jpg"), format!("{other}/Beach/b.jpg"), format!("{other}/Beach/c.jpg"));
    std::fs::create_dir_all(format!("{other}/Beach")).unwrap();
    std::fs::write(&a, b"new").unwrap();
    std::fs::write(&b, b"kept").unwrap();
    assert!(!files::rename_no_replace(&a, &b).unwrap());
    assert_eq!((std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()), (b"new".to_vec(), b"kept".to_vec()));
    assert!(files::rename_no_replace(&a, &c).unwrap());
    assert!(!files::exists(&a));
    assert_eq!(std::fs::read(&c).unwrap(), b"new");
    println!("free space the app sees there: {}", binning_core::moves::format_bytes(files::usable_space(&other)));
    let _ = std::fs::remove_dir_all(&other);
}
