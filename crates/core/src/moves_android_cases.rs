//! The Android app's test cases for the `move` package, run against this port: `MovePlannerTest`,
//! `BinPlannerTest`, `FolderCheckTest`, `LayoutCheckTest`, `MoveModelTest`, `MoveReportTest`,
//! `MoveEngineTest` and `BinFilesJobTest`. Each test names its Android case in a comment where
//! the name differs.
//!
//! Android runs the engine against a fake file system. Here the engine runs on real files in a
//! temporary folder, and `files::faults` switches on what a healthy disk never does (another
//! volume, a full disk, a bad copy, a refused rename) for the test's own thread.

use super::*;
use crate::analysis::ItemKind;
use crate::db::NewItem;
use crate::files::faults;
use crate::paths::norm;
use std::cell::RefCell;
use std::fs;
use std::sync::mpsc;

const P: &str = "C:/Users/me/Pictures/Agentic Binning/Hawaii Photos";

fn set(paths: &[&str]) -> HashSet<String> {
    paths.iter().map(|s| s.to_string()).collect()
}

fn list(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------------------------
// MovePlannerTest
// ---------------------------------------------------------------------------------------------

fn req(id: i64, source: &str, folder: &str, name: &str) -> MoveRequest {
    req_ext(id, source, folder, name, ".jpg")
}

fn req_ext(id: i64, source: &str, folder: &str, name: &str, ext: &str) -> MoveRequest {
    MoveRequest { item_id: id, source: source.into(), folder: folder.into(), base_name: name.into(), extension: ext.into() }
}

fn dests(moves: &[PlannedMove]) -> Vec<String> {
    moves.iter().map(|m| m.dest.clone()).collect()
}

#[test]
fn moves_into_the_category_folder_under_the_friendly_name() {
    let plan = plan_moves(P, &[req(1, "D:/DCIM/Camera/20260715_102000.jpg", "Turtle Beach", "Sea Turtles Resting Sand")], &set(&[]));
    assert_eq!(plan, vec![PlannedMove { item_id: 1, source: "D:/DCIM/Camera/20260715_102000.jpg".into(), dest: format!("{P}/Turtle Beach/Sea Turtles Resting Sand.jpg") }]);
}

#[test]
fn never_overwrites_a_file_on_disk() {
    let existing = set(&[&format!("{P}/Beach Scenes/Empty Beach.jpg")]);
    let plan = plan_moves(P, &[req(1, "D:/a.jpg", "Beach Scenes", "Empty Beach")], &existing);
    assert_eq!(dests(&plan), vec![format!("{P}/Beach Scenes/Empty Beach 2.jpg")]);
}

#[test]
fn same_name_twice_in_one_plan_gets_numbered() {
    let plan = plan_moves(P, &[req(3, "D:/c.jpg", "Sunset", "Sunset"), req(1, "D:/a.jpg", "Sunset", "Sunset"), req(2, "D:/b.jpg", "Sunset", "Sunset")], &set(&[]));
    assert_eq!(plan.iter().map(|m| file_name(&m.dest)).collect::<Vec<_>>(), vec!["Sunset.jpg", "Sunset 2.jpg", "Sunset 3.jpg"]);
    assert_eq!(plan.iter().map(|m| m.item_id).collect::<Vec<_>>(), vec![1, 2, 3]);
}

#[test]
fn names_compare_ignoring_case() {
    let existing = set(&[&format!("{P}/Sunset/SUNSET.JPG")]);
    let plan = plan_moves(P, &[req(1, "D:/a.jpg", "sunset", "Sunset")], &existing);
    assert_eq!(dests(&plan), vec![format!("{P}/sunset/Sunset 2.jpg")]);
}

#[test]
fn file_already_in_place_is_not_moved_and_keeps_its_name() {
    let in_place = format!("{P}/Sunset/Sunset.jpg");
    let plan = plan_moves(P, &[req(9, &in_place, "Sunset", "Sunset"), req(1, "D:/new.jpg", "Sunset", "Sunset")], &set(&[&in_place]));
    assert_eq!(plan, vec![PlannedMove { item_id: 1, source: "D:/new.jpg".into(), dest: format!("{P}/Sunset/Sunset 2.jpg") }]);
}

#[test]
fn earlier_numbered_name_is_kept_when_the_plain_name_is_free() {
    let numbered = format!("{P}/Sunset/Sunset 2.jpg");
    assert!(plan_moves(P, &[req(1, &numbered, "Sunset", "Sunset")], &set(&[&numbered])).is_empty());
}

#[test]
fn case_only_difference_counts_as_in_place() {
    let src = format!("{P}/Sunset/sunset.jpg");
    assert!(plan_moves(P, &[req(1, &src, "Sunset", "Sunset")], &set(&[&src])).is_empty());
}

#[test]
fn renamed_item_moves_within_its_folder() {
    let src = format!("{P}/Green Lizard/Gecko on Wooden Wall.jpg");
    let plan = plan_moves(P, &[req(1, &src, "Green Lizard", "Gecko on the Barn Wall")], &set(&[&src]));
    assert_eq!(dests(&plan), vec![format!("{P}/Green Lizard/Gecko on the Barn Wall.jpg")]);
}

#[test]
fn swapped_names_never_overwrite() {
    let a = format!("{P}/X/A.jpg");
    let b = format!("{P}/X/B.jpg");
    let plan = plan_moves(P, &[req(1, &a, "X", "B"), req(2, &b, "X", "A")], &set(&[&a, &b]));
    assert_eq!(dests(&plan), vec![format!("{P}/X/B 2.jpg"), format!("{P}/X/A 2.jpg")]);
}

#[test]
fn unsafe_names_are_made_safe_and_empty_names_fall_back() {
    let plan = plan_moves(
        P,
        &[req_ext(1, "D:/x/20260718_175109.mp4", "Food/Drink: Lunch?", "Shrimp \"Truck\" Plates", "mp4"), req_ext(2, "D:/x/20260718_175110.MP4", "...", "  ", ".MP4")],
        &set(&[]),
    );
    assert_eq!(plan[0].dest, format!("{P}/Food_Drink_ Lunch_/Shrimp _Truck_ Plates.mp4"));
    assert_eq!(plan[1].dest, format!("{P}/Category/20260718_175110.MP4"));
}

#[test]
fn project_dir_trailing_slash_is_ignored() {
    let plan = plan_moves(&format!("{P}/"), &[req(1, "D:/a.jpg", "A", "B")], &set(&[]));
    assert_eq!(dests(&plan), vec![format!("{P}/A/B.jpg")]);
}

// ---------------------------------------------------------------------------------------------
// BinPlannerTest
// ---------------------------------------------------------------------------------------------

fn item(id: i64, path: &str) -> BinItem {
    BinItem {
        id,
        status: ItemStatus::Sorted,
        category_id: Some(1),
        friendly_name: Some(format!("Item {id}")),
        current_path: path.into(),
        original_name: file_name(path).to_string(),
        failure_reason: None,
    }
}

fn named(id: i64, path: &str, name: &str) -> BinItem {
    BinItem { friendly_name: Some(name.into()), ..item(id, path) }
}

fn with_status(id: i64, path: &str, status: ItemStatus) -> BinItem {
    BinItem { status, ..item(id, path) }
}

fn cats() -> HashMap<i64, String> {
    [(1, "Beach Scenes".to_string()), (2, "Turtle Beach".to_string())].into_iter().collect()
}

/// A plan where every item's file exists unless `on_disk` says otherwise.
fn bin_plan(items: &[BinItem], on_disk: Option<&HashSet<String>>, shared: &[&str], only: Option<&HashSet<i64>>) -> BinPlan {
    let all: HashSet<String> = items.iter().map(|i| i.current_path.clone()).collect();
    let disk = on_disk.unwrap_or(&all);
    plan_bin(P, items, &cats(), disk, |p| disk.contains(p), &list(shared), only)
}

#[test]
fn sorted_items_get_steps_in_id_order() {
    let r = bin_plan(&[named(2, "D:/DCIM/Camera/b.jpg", "Umbrellas"), named(1, "D:/Download/a.JPG", "Morning")], None, &[], None);
    assert_eq!(
        r.steps,
        vec![
            PlannedStep { item_id: 1, source: "D:/Download/a.JPG".into(), dest: format!("{P}/Beach Scenes/Morning.JPG"), keep_source: false, numbered: false },
            PlannedStep { item_id: 2, source: "D:/DCIM/Camera/b.jpg".into(), dest: format!("{P}/Beach Scenes/Umbrellas.jpg"), keep_source: false, numbered: false },
        ]
    );
    assert!(r.in_place.is_empty() && r.became_missing.is_empty());
}

#[test]
fn extension_comes_from_the_original_name() {
    let i = BinItem { original_name: "IMG_1.HEIC".into(), ..named(1, &format!("{P}/Copied from Maui/IMG_1.HEIC"), "Crater") };
    assert_eq!(bin_plan(&[i], None, &[], None).steps[0].dest, format!("{P}/Beach Scenes/Crater.HEIC"));
}

#[test]
fn in_place_is_marked_not_moved() {
    let r = bin_plan(&[item(1, &format!("{P}/Beach Scenes/Item 1.jpg")), with_status(2, &format!("{P}/Beach Scenes/Item 2 3.jpg"), ItemStatus::Binned)], None, &[], None);
    assert!(r.steps.is_empty());
    // Only an item not yet marked binned needs marking.
    assert_eq!(r.in_place, vec![1]);
}

/// B10: a Re-sort may give a binned item another category; the next Bin Files moves it.
#[test]
fn binned_but_moved_is_planned_again() {
    let r = bin_plan(&[with_status(1, &format!("{P}/Turtle Beach/Item 1.jpg"), ItemStatus::Binned)], None, &[], None);
    assert_eq!(r.steps[0].dest, format!("{P}/Beach Scenes/Item 1.jpg"));
}

#[test]
fn failed_missing_and_uncategorized_items_get_no_step() {
    let items = [
        BinItem { category_id: None, friendly_name: None, failure_reason: Some("decode".into()), ..with_status(1, "D:/a.mp4", ItemStatus::Failed) },
        with_status(2, "D:/b.jpg", ItemStatus::Missing),
        BinItem { category_id: None, ..with_status(3, "D:/c.jpg", ItemStatus::Analyzed) },
        with_status(4, "D:/d.jpg", ItemStatus::Failed),
    ];
    let r = bin_plan(&items, None, &[], None);
    assert!(r.steps.is_empty() && r.in_place.is_empty() && r.became_missing.is_empty());
    // The report lists Failed and Missing; an item still waiting for a category is not listed.
    let left = left_out_of(&items, &[]);
    assert_eq!(left.iter().map(|l| (l.item_id, l.reason)).collect::<Vec<_>>(), vec![(1, LeftOutReason::Failed), (2, LeftOutReason::Missing), (4, LeftOutReason::Failed)]);
    assert_eq!(left[0].failure.as_deref(), Some("decode"));
}

#[test]
fn gone_source_becomes_missing() {
    let r = bin_plan(&[item(1, "D:/a.jpg"), item(2, "D:/b.jpg")], Some(&set(&["D:/b.jpg"])), &[], None);
    assert_eq!(r.became_missing, vec![1]);
    assert_eq!(r.steps.iter().map(|s| s.item_id).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn gone_binned_file_becomes_missing() {
    let r = bin_plan(&[with_status(1, &format!("{P}/Beach Scenes/Item 1.jpg"), ItemStatus::Binned)], Some(&set(&[])), &[], None);
    assert_eq!(r.became_missing, vec![1]);
    assert!(r.in_place.is_empty());
}

#[test]
fn taken_name_is_numbered() {
    let r = bin_plan(&[named(1, "D:/a.jpg", "Sunset")], Some(&set(&["D:/a.jpg", &format!("{P}/Beach Scenes/sunset.jpg")])), &[], None);
    assert_eq!(r.steps[0].dest, format!("{P}/Beach Scenes/Sunset 2.jpg"));
    assert!(r.steps[0].numbered);
}

/// P8's follow-on ruling: a file another listed project also holds is copied, never taken.
#[test]
fn shared_item_is_copied_not_moved() {
    let r = bin_plan(&[item(1, "D:/DCIM/Camera/a.jpg"), item(2, "D:/DCIM/Camera/b.jpg")], None, &["D:/DCIM/Camera/a.jpg"], None);
    assert!(r.steps.iter().find(|s| s.item_id == 1).unwrap().keep_source);
    assert!(!r.steps.iter().find(|s| s.item_id == 2).unwrap().keep_source);
}

#[test]
fn only_limits_the_plan() {
    let only: HashSet<i64> = [2].into_iter().collect();
    let r = bin_plan(&[item(1, "D:/a.jpg"), item(2, "D:/b.jpg"), with_status(3, "D:/c.jpg", ItemStatus::Failed)], None, &[], Some(&only));
    assert_eq!(r.steps.iter().map(|s| s.item_id).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn next_free_skips_every_taken_number() {
    let taken: HashSet<String> = [format!("{P}/Beach Scenes/Sunset.jpg"), format!("{P}/Beach Scenes/sunset 2.jpg"), format!("{P}/Beach Scenes/Sunset 3.jpg")].iter().map(|p| key(p)).collect();
    let is_taken = |p: &str| taken.contains(&key(p));
    assert_eq!(next_free(&format!("{P}/Beach Scenes/Sunset.jpg"), is_taken), format!("{P}/Beach Scenes/Sunset 4.jpg"));
    assert_eq!(next_free(&format!("{P}/Beach Scenes/Sunset 2.jpg"), is_taken), format!("{P}/Beach Scenes/Sunset 4.jpg"));
    assert_eq!(next_free(&format!("{P}/Beach Scenes/Gecko.jpg"), |_| false), format!("{P}/Beach Scenes/Gecko.jpg"));
}

// ---------------------------------------------------------------------------------------------
// FolderCheckTest and LayoutCheckTest
// ---------------------------------------------------------------------------------------------

fn check(items: &[(&str, bool)], disk: &[&str], removed: &[&str]) -> LayoutProblems {
    check_in(&["Beach Scenes", "Turtle Beach"], items, disk, removed)
}

fn check_in(categories: &[&str], items: &[(&str, bool)], disk: &[&str], removed: &[&str]) -> LayoutProblems {
    let items: Vec<(String, bool)> = items.iter().map(|(p, b)| (p.to_string(), *b)).collect();
    folder_check(P, &list(categories), &items, &list(removed), &list(disk))
}

#[test]
fn clean_is_ok() {
    let a = format!("{P}/Beach Scenes/a.jpg");
    assert!(check(&[(&a, true)], &[&a], &[]).ok());
}

/// B5: a binned file deleted outside the app is reported (the caller marks it Missing) but never blocks.
#[test]
fn missing_binned_is_reported_not_blocking() {
    let a = format!("{P}/Beach Scenes/a.jpg");
    let r = check(&[(&a, true)], &[], &[]);
    assert_eq!(r.missing, vec![a]);
    assert!(r.unexpected.is_empty());
}

#[test]
fn unknown_file_is_unexpected() {
    let stray = format!("{P}/Beach Scenes/IMG_4410.jpg");
    assert_eq!(check(&[], &[&stray], &[]).unexpected, vec![stray]);
}

#[test]
fn removed_file_is_skipped() {
    let sign = format!("{P}/Turtle Beach/Sign.jpg");
    assert!(check(&[], &[&sign], &[&sign]).ok());
}

/// A binned item a Re-sort moved to another category (B10), or a failed item added from there.
#[test]
fn unbinned_item_in_a_category_folder_is_expected() {
    let a = format!("{P}/Turtle Beach/Item 1.jpg");
    assert!(check(&[(&a, false)], &[&a], &[]).ok());
}

#[test]
fn hidden_files_are_ignored() {
    assert!(check(&[], &[&format!("{P}/Beach Scenes/.nomedia"), &format!("{P}/Beach Scenes/.thumbs/x.jpg")], &[]).ok());
}

#[test]
fn files_outside_category_folders_are_ignored() {
    let others = [
        format!("{P}/.binning/project.db"),
        format!("{P}/.binning/thumbs/1.jpg"),
        format!("{P}/Copied from Maui/a.jpg"),
        format!("{P}/Not A Category/x.jpg"),
        format!("{P}/loose.jpg"),
        format!("{P}/notes.txt"),
        "D:/DCIM/Camera/b.jpg".to_string(),
    ];
    assert!(check(&[], &others.iter().map(|s| s.as_str()).collect::<Vec<_>>(), &[]).ok());
}

#[test]
fn case_differs_still_matches() {
    assert!(check(&[(&format!("{P}/Beach Scenes/A.JPG"), true)], &[&format!("{P}/beach scenes/a.jpg")], &[]).ok());
}

#[test]
fn category_names_are_made_safe_like_the_planner() {
    assert_eq!(folder_for("Mama's: Fish"), "Mama's_ Fish");
    let a = format!("{P}/Mama's_ Fish/a.jpg");
    assert_eq!(check_in(&["Mama's: Fish"], &[], &[&a], &[]).unexpected, vec![a]);
}

#[test]
fn category_files_lists_every_category_folder() {
    let d = tempfile::tempdir().unwrap();
    let root = norm(d.path());
    for f in ["Beach Scenes/x.jpg", "Turtle Beach/deep/y.jpg", "Other/z.jpg"] {
        fs::create_dir_all(parent(&format!("{root}/{f}"))).unwrap();
        fs::write(format!("{root}/{f}"), b"x").unwrap();
    }
    // The same folder under two spellings is listed once.
    let files = category_files(&root, &list(&["Beach Scenes", "Turtle Beach", "beach scenes"]));
    assert_eq!(files, vec![format!("{root}/Beach Scenes/x.jpg"), format!("{root}/Turtle Beach/deep/y.jpg")]);
}

// ---------------------------------------------------------------------------------------------
// MoveModelTest
// ---------------------------------------------------------------------------------------------

#[test]
fn final_steps_are_never_resumed() {
    for s in ["Done", "Skipped", "Missing", "Failed"] {
        assert!(is_final(s), "{s}");
    }
    for s in ["Planned", "Copied", "Verified"] {
        assert!(!is_final(s), "{s}");
    }
    // A value written by a newer build must never crash a resume; it is treated as finished.
    assert!(is_final("Teleported"));
}

// ---------------------------------------------------------------------------------------------
// MoveReportTest
// ---------------------------------------------------------------------------------------------

fn run_row(state: &str, kind: &str) -> MoveRun {
    MoveRun { id: 1, kind: kind.into(), started_at: 1, finished_at: Some(2), state: state.into(), message: None, removed_folders: None, in_place: 0, check_ok: Some(true), report_closed: false }
}

fn step_row(id: i64, state: &str) -> MoveStep {
    MoveStep { id, run_id: 1, item_id: id, source: format!("D:/s/{id}.jpg"), dest: format!("{P}/Beach/F{id}.jpg"), state: state.into(), bytes: 0, attempts: 0, keep_source: false, numbered: false, error: None }
}

#[test]
fn done_headline_counts_files_and_folders() {
    let steps = [step_row(1, "Done"), step_row(2, "Done"), MoveStep { dest: format!("{P}/Sunset/F3.jpg"), ..step_row(3, "Done") }];
    let run = MoveRun { removed_folders: Some(format!("{P}/Turtle Beach")), ..run_row("Done", "Bin") };
    let r = MoveReport::of(&run, &steps, vec![]);
    assert_eq!(r.headline, "3 files binned into 2 category folders");
    assert_eq!((r.moved, r.still_to_move, r.check_ok), (3, 0, Some(true)));
    assert_eq!(r.removed_folders, vec!["Turtle Beach"]);
}

#[test]
fn one_file_is_singular() {
    assert_eq!(MoveReport::of(&run_row("Done", "Bin"), &[step_row(1, "Done")], vec![]).headline, "1 file binned into 1 category folder");
}

#[test]
fn stopped_says_how_many_are_left() {
    let run = MoveRun { message: Some("You stopped it.".into()), ..run_row("Stopped", "Bin") };
    let r = MoveReport::of(&run, &[step_row(1, "Done"), step_row(2, "Planned"), step_row(3, "Planned")], vec![]);
    assert_eq!(r.headline, "Bin Files stopped");
    assert_eq!(r.message.as_deref(), Some("You stopped it."));
    assert_eq!((r.moved, r.still_to_move), (1, 2));
}

#[test]
fn skipped_are_listed_with_why() {
    let steps = [step_row(1, "Done"), MoveStep { error: Some(step_errors::COPY_MISMATCH.into()), ..step_row(2, "Skipped") }, step_row(3, "Missing")];
    let r = MoveReport::of(&run_row("Done", "Bin"), &steps, vec![]);
    assert_eq!(r.skipped, vec![SkippedFile { item_id: 2, name: "F2.jpg".into(), reason: step_errors::COPY_MISMATCH.into() }]);
    assert_eq!(r.became_missing, 1);
    assert_eq!(r.headline, "1 file binned into 1 category folder");
}

#[test]
fn nothing_to_move_headline() {
    let left = vec![LeftOut { item_id: 9, reason: LeftOutReason::Failed, name: "bad.mp4".into(), path: "D:/bad.mp4".into(), failure: None }];
    let r = MoveReport::of(&MoveRun { in_place: 12, ..run_row("Done", "Bin") }, &[], left.clone());
    assert_eq!(r.headline, "Nothing to move. 12 files were already in place.");
    assert_eq!(r.left_out, left);
    assert_eq!(MoveReport::of(&run_row("Done", "Bin"), &[], vec![]).headline, "Nothing to move.");
}

#[test]
fn numbered_lists_the_names() {
    let steps = [MoveStep { dest: format!("{P}/Beach/Sunset 2.jpg"), numbered: true, ..step_row(1, "Done") }, step_row(2, "Done")];
    assert_eq!(MoveReport::of(&run_row("Done", "Bin"), &steps, vec![]).numbered, vec!["Sunset 2.jpg"]);
}

#[test]
fn shared_copies_are_counted_apart() {
    let steps = [MoveStep { keep_source: true, ..step_row(1, "Done") }, step_row(2, "Done")];
    let r = MoveReport::of(&run_row("Done", "Bin"), &steps, vec![]);
    assert_eq!((r.copied_shared, r.moved), (1, 1));
    assert_eq!(r.headline, "2 files binned into 1 category folder");
}

#[test]
fn save_headlines() {
    let save = run_row("Done", "Save");
    assert_eq!(MoveReport::of(&save, &[step_row(1, "Done"), step_row(2, "Done")], vec![]).headline, "Saved. 2 files moved and checked.");
    // A Save that only copied a file another project holds says so.
    assert_eq!(MoveReport::of(&save, &[MoveStep { keep_source: true, ..step_row(1, "Done") }], vec![]).headline, "Saved. 1 file copied and checked.");
    let both = [step_row(1, "Done"), step_row(2, "Done"), MoveStep { keep_source: true, ..step_row(3, "Done") }];
    assert_eq!(MoveReport::of(&save, &both, vec![]).headline, "Saved. 2 files moved and 1 copied, all checked.");
    assert_eq!(MoveReport::of(&run_row("Stopped", "Save"), &[step_row(1, "Planned")], vec![]).headline, "Saving stopped");
}

/// "You stopped it after Fire Knife Spin.mp4": the last file that actually moved.
#[test]
fn last_moved_is_the_last_done_file() {
    let steps = [MoveStep { dest: format!("{P}/Beach/A.jpg"), ..step_row(1, "Done") }, MoveStep { dest: format!("{P}/Beach/B.jpg"), ..step_row(2, "Done") }, step_row(3, "Planned")];
    assert_eq!(MoveReport::of(&run_row("Stopped", "Bin"), &steps, vec![]).last_moved.as_deref(), Some("B.jpg"));
}

/// The report's numbers come from the run row, so they are the same after a restart (B8).
#[test]
fn in_place_and_check_come_from_the_run() {
    let r = MoveReport::of(&MoveRun { in_place: 3, check_ok: Some(false), ..run_row("Done", "Bin") }, &[], vec![]);
    assert_eq!((r.in_place, r.check_ok, r.run_id), (3, Some(false), 1));
}

#[test]
fn format_bytes_rounds_to_one_decimal() {
    assert_eq!(format_bytes(3_300_000_000), "3.1 GB");
    assert_eq!(format_bytes(200 * 1024 * 1024), "200 MB");
    assert!(format_bytes(512).ends_with("KB"));
}

// ---------------------------------------------------------------------------------------------
// A project on disk, for the engine and the job
// ---------------------------------------------------------------------------------------------

struct Rig {
    _dir: tempfile::TempDir,
    /// The project folder.
    root: String,
    /// A camera folder on the project's volume.
    cam: String,
    /// A folder that counts as another volume once `other_volume()` is called.
    sd: String,
    db: ProjectDb,
    /// The run the engine tests add their steps to.
    run: i64,
    items: RefCell<i64>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = norm(dir.path());
        let (root, cam, sd) = (format!("{base}/proj"), format!("{base}/DCIM/Camera"), format!("{base}/sd"));
        for d in [&root, &cam, &sd] {
            fs::create_dir_all(d).unwrap();
        }
        let db = ProjectDb::open(std::path::Path::new(&format!("{root}/.binning/project.db")), "Trip").unwrap();
        Rig { _dir: dir, root, cam, sd, db, run: 0, items: RefCell::new(0) }
    }

    /// For the engine tests: an open run to add steps to.
    fn with_run() -> Self {
        let mut rig = Self::new();
        rig.run = rig.db.tx(|t| db::write_run(t, "Bin", &[], &[], &[], true)).unwrap();
        rig
    }

    fn other_volume(&self) {
        let sd = self.sd.clone();
        faults::set(|f| f.other_volume = Some(sd));
    }

    fn put(&self, path: &str, content: &str) {
        fs::create_dir_all(parent(path)).unwrap();
        fs::write(path, content).unwrap();
    }

    fn file(&self, path: &str) -> String {
        let content = format!("data:{}", file_name(path));
        self.put(path, &content);
        content
    }

    fn content(&self, path: &str) -> Option<String> {
        fs::read_to_string(path).ok()
    }

    fn temp_files(&self) -> Vec<String> {
        files::list_files(&format!("{}/.binning/moving", self.root))
    }

    /// An item at `path` in `category` (by id), whether or not the file exists.
    fn item(&self, path: &str, category: Option<i64>, name: Option<&str>, status: ItemStatus) -> i64 {
        let n = {
            let mut n = self.items.borrow_mut();
            *n += 1;
            *n
        };
        let id = self
            .db
            .insert_item(&NewItem { content_hash: format!("h{n}"), path: path.into(), name: file_name(path).into(), kind: ItemKind::Photo, taken_at: None, duration_ms: None, width: None, height: None })
            .unwrap();
        self.db
            .read(|c| {
                c.execute("UPDATE item SET status = ?2, categoryId = ?3, friendlyName = ?4 WHERE id = ?1", rusqlite::params![id, status.as_str(), category, name])?;
                Ok(())
            })
            .unwrap();
        id
    }

    fn sorted(&self, path: &str, category: i64, name: &str) -> i64 {
        self.item(path, Some(category), Some(name), ItemStatus::Sorted)
    }

    fn category(&self, name: &str) -> i64 {
        self.db
            .read(|c| {
                c.execute("INSERT INTO category (name, description, createdBy) VALUES (?1, '', 'Model')", [name])?;
                Ok(c.last_insert_rowid())
            })
            .unwrap()
    }

    fn rename_category(&self, id: i64, name: &str) {
        self.db.read(|c| Ok(c.execute("UPDATE category SET name = ?2 WHERE id = ?1", rusqlite::params![id, name]).map(|_| ())?)).unwrap();
    }

    /// A folder an earlier run put a file in: the only kind Bin Files may remove once empty.
    fn made(&self, folder: &str) {
        self.db
            .read(|c| {
                c.execute("INSERT INTO move_run (kind, startedAt, finishedAt, state, inPlace, reportClosed) VALUES ('Bin', 1, 2, 'Done', 0, 1)", [])?;
                let run = c.last_insert_rowid();
                c.execute("INSERT INTO move_step (runId, itemId, source, dest, state) VALUES (?1, 0, 'D:/old.jpg', ?2, 'Done')", rusqlite::params![run, format!("{folder}/old.jpg")])?;
                Ok(())
            })
            .unwrap();
    }

    /// A step of the rig's run, and the Sorted item it moves. Returns the step's id (the item's id is the same).
    fn step(&self, source: &str, dest: &str) -> i64 {
        self.step_in(source, dest, "Planned", 0, false)
    }

    fn step_in(&self, source: &str, dest: &str, state: &str, attempts: i64, keep_source: bool) -> i64 {
        let item = self.item(source, None, None, ItemStatus::Sorted);
        let bytes = files::size(source);
        let step = self
            .db
            .read(|c| {
                c.execute(
                    "INSERT INTO move_step (runId, itemId, source, dest, state, bytes, attempts, keepSource) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![self.run, item, source, dest, state, bytes, attempts, keep_source],
                )?;
                Ok(c.last_insert_rowid())
            })
            .unwrap();
        assert_eq!(step, item, "the engine tests rely on step ids matching item ids");
        step
    }

    fn step_row(&self, id: i64) -> MoveStep {
        self.db.steps(self.run).unwrap().into_iter().find(|s| s.id == id).unwrap()
    }

    fn state(&self, id: i64) -> String {
        self.step_row(id).state
    }

    fn item_row(&self, id: i64) -> db::Item {
        self.db.item(id).unwrap().unwrap()
    }

    fn status(&self, id: i64) -> ItemStatus {
        self.item_row(id).status
    }

    fn run_engine(&self) -> (MoveOutcome, Vec<MoveProgress>) {
        self.run_engine_with(&MoveControl::new(false), |_| {})
    }

    fn run_engine_with(&self, control: &MoveControl, each: impl Fn(&MoveProgress)) -> (MoveOutcome, Vec<MoveProgress>) {
        let seen = RefCell::new(Vec::new());
        let outcome = Engine { db: &self.db, root: self.root.clone() }
            .run(self.run, "Bin", control, &|p| {
                each(&p);
                seen.borrow_mut().push(p);
            })
            .unwrap();
        (outcome, seen.into_inner())
    }

    fn job<'a>(&'a self, shared: SharedLookup<'a>) -> BinFilesJob<'a> {
        BinFilesJob::new(&self.db, &self.root, shared)
    }

    fn runs(&self) -> Vec<MoveRun> {
        self.db
            .read(|c| {
                let mut s = c.prepare("SELECT id FROM move_run ORDER BY id")?;
                let ids = s.query_map([], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(ids)
            })
            .unwrap()
            .into_iter()
            .map(|id| self.db.move_run(id).unwrap().unwrap())
            .collect()
    }
}

fn nobody(_: &[String]) -> Vec<String> {
    Vec::new()
}

fn go() -> MoveControl {
    MoveControl::new(false)
}

// ---------------------------------------------------------------------------------------------
// MoveEngineTest
// ---------------------------------------------------------------------------------------------

#[test]
fn rename_moves_and_records() {
    let r = Rig::with_run();
    let (src, dest) = (format!("{}/a.jpg", r.cam), format!("{}/Beach/Morning.jpg", r.root));
    let c = r.file(&src);
    let id = r.step(&src, &dest);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert!(!files::exists(&src));
    assert_eq!(r.content(&dest), Some(c));
    assert_eq!(r.state(id), "Done");
    let item = r.item_row(id);
    assert_eq!((item.current_path.as_str(), item.status), (dest.as_str(), ItemStatus::Binned));
    assert_eq!(item.content_hash, files::sha256(&dest).unwrap());
    assert_eq!(faults::get(|f| f.copies), 0, "a move on one volume never copies");
}

#[test]
fn rename_dest_taken_takes_the_next_number() {
    let r = Rig::with_run();
    let (src, dest) = (format!("{}/a.jpg", r.cam), format!("{}/Beach/Morning.jpg", r.root));
    let c = r.file(&src);
    r.put(&dest, "someone else's");
    let id = r.step(&src, &dest);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert_eq!(r.content(&dest).as_deref(), Some("someone else's"));
    let numbered = format!("{}/Beach/Morning 2.jpg", r.root);
    assert_eq!(r.content(&numbered), Some(c));
    assert_eq!(r.step_row(id).dest, numbered);
}

#[test]
fn rename_check_fails_fails_the_run() {
    let r = Rig::with_run();
    let (a_src, b_src) = (format!("{}/a.jpg", r.cam), format!("{}/b.jpg", r.cam));
    r.file(&a_src);
    r.file(&b_src);
    let a = r.step(&a_src, &format!("{}/Beach/A.jpg", r.root));
    let b = r.step(&b_src, &format!("{}/Beach/B.jpg", r.root));
    faults::set(|f| f.rename_lies = true);
    assert!(matches!(r.run_engine().0, MoveOutcome::Failed(_)));
    let step = r.step_row(a);
    assert_eq!((step.state.as_str(), step.error.as_deref()), ("Failed", Some(step_errors::RENAME_CHECK)));
    assert_eq!(r.state(b), "Planned");
    assert_eq!(r.item_row(a).current_path, a_src);
    assert!(files::exists(&a_src));
}

#[test]
fn rename_io_error_skips_and_carries_on() {
    let r = Rig::with_run();
    let src = format!("{}/a.jpg", r.cam);
    r.file(&src);
    let a = r.step(&src, &format!("{}/Beach/A.jpg", r.root));
    faults::set(|f| f.rename_fails = true);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    let step = r.step_row(a);
    assert_eq!((step.state.as_str(), step.error.as_deref()), ("Skipped", Some(step_errors::IO)));
    assert!(files::exists(&src));
    assert_eq!(r.status(a), ItemStatus::Sorted);
}

#[test]
fn copy_across_volumes_moves_and_deletes_the_source() {
    let r = Rig::with_run();
    r.other_volume();
    let (src, dest) = (format!("{}/DCIM/a.mp4", r.sd), format!("{}/Surf/Wave.mp4", r.root));
    let c = r.file(&src);
    files::set_last_modified(&src, 1_000_000_000_000);
    let id = r.step(&src, &dest);
    let (outcome, seen) = r.run_engine();
    assert_eq!(outcome, MoveOutcome::Done);
    assert_eq!(r.content(&dest), Some(c));
    assert_eq!(files::last_modified(&dest), 1_000_000_000_000);
    assert!(!files::exists(&src));
    assert!(r.temp_files().is_empty());
    assert_eq!(r.state(id), "Done");
    assert_eq!(r.item_row(id).content_hash, files::sha256(&dest).unwrap());
    let phases: Vec<MovePhase> = seen.iter().filter_map(|p| p.phase).collect();
    assert_eq!(phases, vec![MovePhase::Copying, MovePhase::Checking, MovePhase::Placing, MovePhase::DeletingOriginal]);
}

#[test]
fn copy_never_overwrites() {
    let r = Rig::with_run();
    r.other_volume();
    let (src, dest) = (format!("{}/a.jpg", r.sd), format!("{}/Beach/A.jpg", r.root));
    let c = r.file(&src);
    r.put(&dest, "keep me");
    r.step(&src, &dest);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert_eq!(r.content(&dest).as_deref(), Some("keep me"));
    assert_eq!(r.content(&format!("{}/Beach/A 2.jpg", r.root)), Some(c));
}

#[test]
fn copy_mismatch_once_retries_and_succeeds() {
    let r = Rig::with_run();
    r.other_volume();
    let (src, dest) = (format!("{}/a.jpg", r.sd), format!("{}/Beach/A.jpg", r.root));
    let c = r.file(&src);
    let id = r.step(&src, &dest);
    faults::set(|f| f.corrupt_copies = 1);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert_eq!(r.content(&dest), Some(c));
    assert_eq!(r.state(id), "Done");
    assert_eq!(faults::get(|f| f.copies), 2);
}

/// B4 (owner): two bad copies skip the file; its original is untouched and the run carries on.
#[test]
fn copy_mismatch_twice_skips_and_keeps_the_source() {
    let r = Rig::with_run();
    r.other_volume();
    let (a_src, b_src) = (format!("{}/a.jpg", r.sd), format!("{}/b.jpg", r.sd));
    let c = r.file(&a_src);
    r.file(&b_src);
    let a = r.step(&a_src, &format!("{}/Beach/A.jpg", r.root));
    let b = r.step(&b_src, &format!("{}/Beach/B.jpg", r.root));
    faults::set(|f| f.corrupt_copies = 2);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    let step = r.step_row(a);
    assert_eq!((step.state.as_str(), step.error.as_deref()), ("Skipped", Some(step_errors::COPY_MISMATCH)));
    assert_eq!(r.content(&a_src), Some(c));
    assert!(!files::exists(&format!("{}/Beach/A.jpg", r.root)));
    assert_eq!(r.status(a), ItemStatus::Sorted);
    assert!(r.temp_files().is_empty());
    assert_eq!(r.state(b), "Done");
}

#[test]
fn copy_space_short_stops_before_copying() {
    let r = Rig::with_run();
    r.other_volume();
    let src = format!("{}/a.mp4", r.sd);
    r.put(&src, &"x".repeat(1000));
    let id = r.step(&src, &format!("{}/Surf/A.mp4", r.root));
    faults::set(|f| f.free = Some(COPY_MARGIN_BYTES + 500));
    assert_eq!(r.run_engine().0, MoveOutcome::StorageFull(500));
    assert_eq!(r.state(id), "Planned");
    assert_eq!(faults::get(|f| f.copies), 0);
    assert!(files::exists(&src));
}

#[test]
fn copy_space_runs_out_deletes_the_half_copy() {
    let r = Rig::with_run();
    r.other_volume();
    let src = format!("{}/a.mp4", r.sd);
    r.put(&src, &"x".repeat(1000));
    let id = r.step(&src, &format!("{}/Surf/A.mp4", r.root));
    faults::set(|f| f.no_space_during_copy = true);
    assert!(matches!(r.run_engine().0, MoveOutcome::StorageFull(_)));
    assert!(r.temp_files().is_empty());
    assert_eq!(files::size(&src), 1000);
    assert_eq!(r.state(id), "Planned");
}

/// P8: another project also holds the source; it is copied and never deleted.
#[test]
fn keep_source_never_deletes_the_source() {
    let r = Rig::with_run();
    let (src, dest) = (format!("{}/a.jpg", r.cam), format!("{}/Beach/A.jpg", r.root));
    let c = r.file(&src);
    let id = r.step_in(&src, &dest, "Planned", 0, true);
    let (outcome, seen) = r.run_engine();
    assert_eq!(outcome, MoveOutcome::Done);
    assert_eq!(r.content(&src), Some(c.clone()));
    assert_eq!(r.content(&dest), Some(c));
    assert_eq!(faults::get(|f| f.copies), 1);
    assert!(seen.iter().all(|p| p.phase != Some(MovePhase::DeletingOriginal)));
    assert_eq!(r.state(id), "Done");
}

#[test]
fn missing_source_marks_missing_and_carries_on() {
    let r = Rig::with_run();
    let b_src = format!("{}/b.jpg", r.cam);
    r.file(&b_src);
    let a = r.step(&format!("{}/a.jpg", r.cam), &format!("{}/Beach/A.jpg", r.root));
    let b = r.step(&b_src, &format!("{}/Beach/B.jpg", r.root));
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert_eq!(r.state(a), "Missing");
    assert_eq!(r.status(a), ItemStatus::Missing);
    assert_eq!(r.state(b), "Done");
}

#[test]
fn low_space_stops_every_run() {
    let r = Rig::with_run();
    let src = format!("{}/a.jpg", r.cam);
    r.file(&src);
    let a = r.step(&src, &format!("{}/Beach/A.jpg", r.root));
    faults::set(|f| f.free = Some(MIN_FREE_BYTES - 1));
    assert_eq!(r.run_engine().0, MoveOutcome::StorageFull(1));
    assert_eq!(r.state(a), "Planned");
    assert!(files::exists(&src));
}

fn two_files(r: &Rig) -> (i64, i64) {
    let (a_src, b_src) = (format!("{}/a.jpg", r.cam), format!("{}/b.jpg", r.cam));
    r.file(&a_src);
    r.file(&b_src);
    (r.step(&a_src, &format!("{}/Beach/A.jpg", r.root)), r.step(&b_src, &format!("{}/Beach/B.jpg", r.root)))
}

/// B2 (owner): Pause finishes the file in progress, then holds before the next one.
#[test]
fn pause_finishes_the_current_file_first() {
    let r = Rig::with_run();
    let (a, b) = two_files(&r);
    let control = go();
    let (paused_tx, paused_rx) = mpsc::channel();
    let paused_tx = std::sync::Mutex::new(paused_tx);
    std::thread::scope(|s| {
        let run = s.spawn(|| {
            Engine { db: &r.db, root: r.root.clone() }
                .run(r.run, "Bin", &control, &|p| {
                    // Pressed while A.jpg is moving.
                    if p.phase == Some(MovePhase::Moving) && p.current.as_deref() == Some("A.jpg") {
                        control.pause();
                    }
                    if p.paused {
                        paused_tx.lock().unwrap().send(p).unwrap();
                    }
                })
                .unwrap()
        });
        let paused = paused_rx.recv().unwrap();
        assert_eq!(paused.next.as_deref(), Some("B.jpg"));
        assert_eq!((r.state(a).as_str(), r.state(b).as_str()), ("Done", "Planned"));
        assert_eq!(r.db.move_run(r.run).unwrap().unwrap().state, "Paused");
        control.resume();
        assert_eq!(run.join().unwrap(), MoveOutcome::Done);
    });
    assert_eq!(r.state(b), "Done");
    assert_eq!(r.db.move_run(r.run).unwrap().unwrap().state, "Running");
}

#[test]
fn stop_finishes_the_current_file_first() {
    let r = Rig::with_run();
    let (a, b) = two_files(&r);
    let control = go();
    let (outcome, _) = r.run_engine_with(&control, |p| {
        if p.phase == Some(MovePhase::Moving) && p.current.as_deref() == Some("A.jpg") {
            control.stop();
        }
    });
    assert_eq!(outcome, MoveOutcome::Stopped);
    assert_eq!((r.state(a).as_str(), r.state(b).as_str()), ("Done", "Planned"));
}

/// B2/B3: a run that was paused when the app closed resumes paused, and names the next file.
#[test]
fn paused_run_stays_paused_until_resumed_or_stopped() {
    for stop in [false, true] {
        let r = Rig::with_run();
        let src = format!("{}/a.jpg", r.cam);
        r.file(&src);
        let a = r.step(&src, &format!("{}/Beach/F1.jpg", r.root));
        let control = MoveControl::new(true);
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        std::thread::scope(|s| {
            let run = s.spawn(|| {
                Engine { db: &r.db, root: r.root.clone() }
                    .run(r.run, "Bin", &control, &|p| {
                        if p.paused {
                            tx.lock().unwrap().send(p).unwrap();
                        }
                    })
                    .unwrap()
            });
            assert_eq!(rx.recv().unwrap().next.as_deref(), Some("F1.jpg"));
            assert_eq!(r.state(a), "Planned");
            if stop {
                control.stop();
                assert_eq!(run.join().unwrap(), MoveOutcome::Stopped);
            } else {
                control.resume();
                assert_eq!(run.join().unwrap(), MoveOutcome::Done);
            }
        });
        assert_eq!(r.state(a), if stop { "Planned" } else { "Done" });
        assert_eq!(files::exists(&src), stop);
    }
}

#[test]
fn third_start_is_skipped() {
    let r = Rig::with_run();
    r.other_volume();
    let src = format!("{}/a.jpg", r.sd);
    r.file(&src);
    let temp = format!("{}/.binning/moving/1.tmp", r.root);
    r.put(&temp, "half");
    let id = r.step_in(&src, &format!("{}/Beach/A.jpg", r.root), "Copied", 2, false);
    assert_eq!(id, 1);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    let step = r.step_row(id);
    assert_eq!((step.state.as_str(), step.error.as_deref()), ("Skipped", Some(step_errors::INTERRUPTED)));
    assert!(files::exists(&src));
    assert!(!files::exists(&temp));
}

#[test]
fn progress_counts_every_step() {
    let r = Rig::with_run();
    r.other_volume();
    let (a_src, b_src) = (format!("{}/a.jpg", r.cam), format!("{}/b.jpg", r.sd));
    r.file(&a_src);
    r.file(&b_src);
    r.step(&a_src, &format!("{}/Beach/A.jpg", r.root));
    r.step(&b_src, &format!("{}/Beach/B.jpg", r.root));
    r.step_in("D:/x/done.jpg", &format!("{}/Beach/C.jpg", r.root), "Done", 1, false);
    let (_, seen) = r.run_engine();
    assert_eq!((seen.last().unwrap().total, seen.last().unwrap().done), (3, 3));
    assert_eq!(seen.first().unwrap().done, 1);
    assert!(seen.iter().any(|p| p.phase == Some(MovePhase::Moving) && p.current.as_deref() == Some("A.jpg")));
    assert!(seen.iter().any(|p| p.phase == Some(MovePhase::Copying) && p.current.as_deref() == Some("B.jpg")));
    assert!(seen.iter().any(|p| p.phase == Some(MovePhase::DeletingOriginal)));
}

// ---- resume

#[test]
fn resume_planned_redoes_the_move() {
    let r = Rig::with_run();
    let (src, dest) = (format!("{}/a.jpg", r.cam), format!("{}/Beach/A.jpg", r.root));
    let c = r.file(&src);
    let id = r.step_in(&src, &dest, "Planned", 1, false);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert_eq!(r.state(id), "Done");
    assert_eq!(r.content(&dest), Some(c));
}

/// The app died between the rename and recording it: the file is found at its target.
#[test]
fn resume_planned_rename_already_done_records_it() {
    let r = Rig::with_run();
    let (src, dest) = (format!("{}/a.jpg", r.cam), format!("{}/Beach/A.jpg", r.root));
    r.put(&dest, "data");
    let id = r.step_in(&src, &dest, "Planned", 1, false);
    r.db.read(|c| Ok(c.execute("UPDATE move_step SET bytes = 4 WHERE id = ?1", [id]).map(|_| ())?)).unwrap();
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert_eq!(r.state(id), "Done");
    assert_eq!(r.item_row(id).current_path, dest);
    assert_eq!(r.content(&dest).as_deref(), Some("data"));
}

/// A first start never claims a file it didn't put there.
#[test]
fn first_start_source_gone_but_target_there_is_missing() {
    let r = Rig::with_run();
    let dest = format!("{}/Beach/A.jpg", r.root);
    r.put(&dest, "data");
    let id = r.step(&format!("{}/a.jpg", r.cam), &dest);
    r.db.read(|c| Ok(c.execute("UPDATE move_step SET bytes = 4 WHERE id = ?1", [id]).map(|_| ())?)).unwrap();
    r.run_engine();
    assert_eq!(r.state(id), "Missing");
    assert_eq!(r.content(&dest).as_deref(), Some("data"));
}

/// A file of another size at the target is not the file that was moving.
#[test]
fn resume_source_gone_and_another_file_at_the_target_is_missing() {
    let r = Rig::with_run();
    let dest = format!("{}/Beach/A.jpg", r.root);
    r.put(&dest, "someone else's");
    let id = r.step_in(&format!("{}/a.jpg", r.cam), &dest, "Planned", 1, false);
    r.db.read(|c| Ok(c.execute("UPDATE move_step SET bytes = 4 WHERE id = ?1", [id]).map(|_| ())?)).unwrap();
    r.run_engine();
    assert_eq!(r.state(id), "Missing");
    assert_eq!(r.content(&dest).as_deref(), Some("someone else's"));
}

#[test]
fn resume_copied_discards_the_temp_and_redoes() {
    let r = Rig::with_run();
    r.other_volume();
    let (src, dest) = (format!("{}/a.jpg", r.sd), format!("{}/Beach/A.jpg", r.root));
    let c = r.file(&src);
    let temp = format!("{}/.binning/moving/1.tmp", r.root);
    r.put(&temp, "half");
    let id = r.step_in(&src, &dest, "Copied", 1, false);
    assert_eq!(r.run_engine().0, MoveOutcome::Done);
    assert_eq!(r.state(id), "Done");
    assert_eq!(r.content(&dest), Some(c));
    assert!(!files::exists(&temp) && !files::exists(&src));
}

#[test]
fn resume_verified_deletes_the_source_only() {
    // Even interrupted twice, a recorded file is finished, never skipped: skipping would leave two copies.
    for attempts in [1, 2] {
        let r = Rig::with_run();
        r.other_volume();
        let (src, dest) = (format!("{}/a.jpg", r.sd), format!("{}/Beach/A.jpg", r.root));
        let c = r.file(&src);
        r.put(&dest, &c);
        let id = r.step_in(&src, &dest, "Verified", attempts, false);
        assert_eq!(r.run_engine().0, MoveOutcome::Done);
        assert!(!files::exists(&src));
        assert_eq!(r.content(&dest), Some(c));
        assert_eq!(r.state(id), "Done");
        assert_eq!(faults::get(|f| f.copies), 0);
    }
}

#[test]
fn resume_verified_keep_source_keeps_it() {
    let r = Rig::with_run();
    let (src, dest) = (format!("{}/a.jpg", r.cam), format!("{}/Beach/A.jpg", r.root));
    let c = r.file(&src);
    r.put(&dest, &c);
    let id = r.step_in(&src, &dest, "Verified", 1, true);
    r.run_engine();
    assert!(files::exists(&src));
    assert_eq!(r.state(id), "Done");
}

/// The destination vanished after it was recorded: the source is kept and the item points back at it.
#[test]
fn resume_verified_dest_gone_keeps_the_source() {
    let r = Rig::with_run();
    r.other_volume();
    let (src, dest) = (format!("{}/a.jpg", r.sd), format!("{}/Beach/A.jpg", r.root));
    r.file(&src);
    let id = r.step_in(&src, &dest, "Verified", 1, false);
    r.db.read(|c| Ok(c.execute("UPDATE item SET currentPath = ?2, status = 'Binned' WHERE id = ?1", rusqlite::params![id, dest]).map(|_| ())?)).unwrap();
    r.run_engine();
    assert!(files::exists(&src));
    assert_eq!(r.state(id), "Failed");
    let item = r.item_row(id);
    assert_eq!((item.current_path, item.status), (src, ItemStatus::Sorted));
}

#[test]
fn resume_both_gone_marks_missing() {
    let r = Rig::with_run();
    let id = r.step_in(&format!("{}/a.jpg", r.sd), &format!("{}/Beach/A.jpg", r.root), "Planned", 1, false);
    r.run_engine();
    assert_eq!(r.state(id), "Missing");
    let id = r.step_in(&format!("{}/b.jpg", r.sd), &format!("{}/Beach/B.jpg", r.root), "Verified", 1, false);
    r.run_engine();
    assert_eq!(r.state(id), "Missing");
}

/// State 2: the card names the file, where it comes from and goes, and the files just finished.
#[test]
fn progress_names_folders_and_recent_files() {
    let r = Rig::with_run();
    for n in 1..=3 {
        let src = format!("{}/s{n}.jpg", r.cam);
        r.put(&src, "x");
        r.step(&src, &format!("{}/Beach/F{n}.jpg", r.root));
    }
    let (_, seen) = r.run_engine();
    let moving = seen.iter().find(|p| p.current.as_deref() == Some("F3.jpg")).unwrap();
    assert_eq!((moving.from.as_deref(), moving.to.as_deref()), (Some("Camera"), Some("Beach")));
    assert_eq!(moving.recent, vec![MovedFile { name: "F1.jpg".into(), folder: "Beach".into() }, MovedFile { name: "F2.jpg".into(), folder: "Beach".into() }]);
}

#[test]
fn progress_recent_keeps_only_the_last_two_and_only_moved_files() {
    let r = Rig::with_run();
    for n in 1..=3 {
        let src = format!("{}/s{n}.jpg", r.cam);
        r.put(&src, "x");
        r.step(&src, &format!("{}/Beach/F{n}.jpg", r.root));
    }
    r.step(&format!("{}/gone.jpg", r.cam), &format!("{}/Beach/F4.jpg", r.root));
    let (_, seen) = r.run_engine();
    assert_eq!(seen.last().unwrap().recent.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["F2.jpg", "F3.jpg"]);
    assert_eq!(seen.last().unwrap().done, 4);
}

/// A resumed run's card shows the files already moved before the crash.
#[test]
fn progress_recent_includes_what_moved_before_a_resume() {
    let r = Rig::with_run();
    r.step_in(&format!("{}/s1.jpg", r.cam), &format!("{}/Beach/F1.jpg", r.root), "Done", 1, false);
    let src = format!("{}/s2.jpg", r.cam);
    r.put(&src, "x");
    r.step(&src, &format!("{}/Beach/F2.jpg", r.root));
    let (_, seen) = r.run_engine();
    assert_eq!(seen.first().unwrap().recent.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["F1.jpg"]);
}

// ---------------------------------------------------------------------------------------------
// BinFilesJobTest
// ---------------------------------------------------------------------------------------------

/// A project with the categories Beach (1) and Turtle Beach (2).
fn trip() -> Rig {
    let r = Rig::new();
    assert_eq!((r.category("Beach"), r.category("Turtle Beach")), (1, 2));
    r
}

#[test]
fn bin_files_moves_every_sorted_item_and_finishes_the_run() {
    let r = trip();
    let (a, b) = (format!("{}/a.jpg", r.cam), format!("{}/Download/b.jpg", r.sd));
    r.put(&a, "A");
    r.put(&b, "B");
    let one = r.sorted(&a, 1, "Morning");
    r.sorted(&b, 2, "Resting");
    let bad = r.item(&format!("{}/bad.mp4", r.cam), None, None, ItemStatus::Failed);
    let job = r.job(&nobody);
    assert_eq!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.content(&format!("{}/Beach/Morning.jpg", r.root)).as_deref(), Some("A"));
    assert_eq!(r.content(&format!("{}/Turtle Beach/Resting.jpg", r.root)).as_deref(), Some("B"));
    assert_eq!((r.status(one), r.status(bad)), (ItemStatus::Binned, ItemStatus::Failed));
    let runs = r.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, "Done");
    assert!(runs[0].finished_at.is_some());
    // Source folders are never removed (B12), even when emptied.
    assert!(std::path::Path::new(&format!("{}/Download", r.sd)).is_dir());
    let report = job.report(runs[0].id).unwrap().unwrap();
    assert_eq!(report.headline, "2 files binned into 2 category folders");
    assert_eq!(report.left_out.iter().map(|l| (l.item_id, l.reason)).collect::<Vec<_>>(), vec![(bad, LeftOutReason::Failed)]);
    assert_eq!(report.check_ok, Some(true));
}

/// B5: an unknown file in a category folder blocks before anything is written or moved.
#[test]
fn unknown_files_block_before_writing() {
    let r = trip();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    let stray = format!("{}/Beach/IMG_4410.jpg", r.root);
    r.put(&stray, "stray");
    let id = r.sorted(&a, 1, "Item 1");
    match r.job(&nobody).bin_files(&go(), &|_| {}).unwrap() {
        MoveOutcome::Blocked(p) => assert_eq!(p.unexpected, vec![stray.clone()]),
        other => panic!("{other:?}"),
    }
    assert!(r.runs().is_empty());
    assert!(files::exists(&a));
    assert_eq!(r.status(id), ItemStatus::Sorted);
    assert_eq!(r.content(&stray).as_deref(), Some("stray"));
}

/// Leaving a file is recorded, so the check skips it from then on; the file itself is untouched.
#[test]
fn left_files_do_not_block() {
    let r = trip();
    let stray = format!("{}/Beach/IMG_4410.jpg", r.root);
    r.put(&stray, "stray");
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    r.sorted(&a, 1, "Item 1");
    let job = r.job(&nobody);
    assert!(matches!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Blocked(_)));
    r.db.add_removed(&[stray.clone()], "left").unwrap();
    assert_eq!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.content(&stray).as_deref(), Some("stray"));
}

/// B5: a binned file deleted outside the app marks its item Missing; Bin Files carries on.
#[test]
fn missing_binned_is_marked_and_carries_on() {
    let r = trip();
    let gone = r.item(&format!("{}/Beach/Item 1.jpg", r.root), Some(1), Some("Item 1"), ItemStatus::Binned);
    let b = format!("{}/b.jpg", r.cam);
    r.put(&b, "B");
    let two = r.sorted(&b, 1, "Item 2");
    assert_eq!(r.job(&nobody).bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!((r.status(gone), r.status(two)), (ItemStatus::Missing, ItemStatus::Binned));
}

/// B12: an empty folder the app made is removed; a non-empty one and source folders stay.
#[test]
fn removes_only_empty_folders_it_made() {
    let r = trip();
    r.category("Sunset");
    // A Re-sort moved items 1 and 2 into Beach; Sunset still holds a file removed from the project.
    let (one, two, old) = (format!("{}/Turtle Beach/Item 1.jpg", r.root), format!("{}/Sunset/Item 2.jpg", r.root), format!("{}/Sunset/Old.jpg", r.root));
    r.put(&one, "1");
    r.put(&two, "2");
    r.put(&old, "removed");
    r.db.add_removed(&[old.clone()], "removed").unwrap();
    r.made(&format!("{}/Turtle Beach", r.root));
    r.made(&format!("{}/Sunset", r.root));
    r.sorted(&one, 1, "Item 1");
    r.sorted(&two, 1, "Item 2");
    let c = format!("{}/c.jpg", r.cam);
    r.put(&c, "3");
    r.sorted(&c, 1, "Item 3");
    let job = r.job(&nobody);
    assert_eq!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert!(!files::exists(&format!("{}/Turtle Beach", r.root)));
    assert!(files::exists(&old));
    assert!(std::path::Path::new(&r.cam).is_dir());
    assert_eq!(job.unread_report().unwrap().unwrap().removed_folders, vec!["Turtle Beach"]);
}

/// A folder the user made inside the project is never removed, even when Bin Files empties it (B12).
#[test]
fn keeps_an_emptied_folder_it_did_not_make() {
    let r = trip();
    let a = format!("{}/Imports/a.jpg", r.root);
    r.put(&a, "A");
    r.sorted(&a, 1, "Item 1");
    assert_eq!(r.job(&nobody).bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert!(std::path::Path::new(&format!("{}/Imports", r.root)).is_dir());
    assert!(!files::exists(&a));
}

/// P8: a Copied from folder is the app's own, removed once Bin Files empties it.
#[test]
fn removes_an_emptied_copied_from_folder() {
    let r = trip();
    let a = format!("{}/Copied from Maui/a.jpg", r.root);
    r.put(&a, "A");
    r.sorted(&a, 1, "Item 1");
    assert_eq!(r.job(&nobody).bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert!(!files::exists(&format!("{}/Copied from Maui", r.root)));
}

/// B9: nothing to move; in-place items are only marked binned, and the report says so.
#[test]
fn nothing_to_move_marks_in_place() {
    let r = trip();
    let a = format!("{}/Beach/Item 1.jpg", r.root);
    r.put(&a, "1");
    let id = r.sorted(&a, 1, "Item 1");
    let job = r.job(&nobody);
    assert_eq!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::NothingToMove(1));
    assert_eq!(r.status(id), ItemStatus::Binned);
    assert_eq!(job.unread_report().unwrap().unwrap().headline, "Nothing to move. 1 file was already in place.");
}

#[test]
fn storage_full_stops_with_a_message() {
    let r = trip();
    r.other_volume();
    let a = format!("{}/a.mp4", r.sd);
    r.put(&a, &"x".repeat(100));
    let id = r.sorted(&a, 1, "Item 1");
    faults::set(|f| f.free = Some(COPY_MARGIN_BYTES));
    assert!(matches!(r.job(&nobody).bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::StorageFull(_)));
    let run = &r.runs()[0];
    assert_eq!(run.state, "Stopped");
    assert!(run.message.as_deref().unwrap().starts_with("Storage is full. Free up "));
    assert!(files::exists(&a));
    assert_eq!(r.status(id), ItemStatus::Sorted);
}

/// B3: an open run is resumed, not planned again.
#[test]
fn an_open_run_is_resumed_not_planned_again() {
    let r = trip();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    let id = r.sorted(&a, 1, "Morning");
    let dest = format!("{}/Beach/Morning.jpg", r.root);
    let steps = [NewStep { item_id: id, source: a.clone(), dest: dest.clone(), bytes: 1, keep_source: false, numbered: false }];
    r.db.tx(|t| db::write_run(t, "Bin", &steps, &[], &[], true)).unwrap();
    assert_eq!(r.job(&nobody).bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.content(&dest).as_deref(), Some("A"));
    assert!(r.db.open_move_run().unwrap().is_none());
    assert_eq!(r.runs().len(), 1);
}

/// A stopped run leaves the rest for the next Bin Files, and checks the files it did move.
#[test]
fn stop_leaves_the_rest_for_the_next_bin_files() {
    let r = trip();
    let (a, b) = (format!("{}/a.jpg", r.cam), format!("{}/b.jpg", r.cam));
    r.put(&a, "A");
    r.put(&b, "B");
    r.sorted(&a, 1, "Item 1");
    let two = r.sorted(&b, 1, "Item 2");
    let job = r.job(&nobody);
    let control = go();
    let outcome = job
        .bin_files(&control, &|p| {
            if p.phase == Some(MovePhase::Moving) && p.current.as_deref() == Some("Item 1.jpg") {
                control.stop();
            }
        })
        .unwrap();
    assert_eq!(outcome, MoveOutcome::Stopped);
    let run = &r.runs()[0];
    assert_eq!(run.state, "Stopped");
    let report = job.report(run.id).unwrap().unwrap();
    assert_eq!((report.still_to_move, report.check_ok), (1, Some(true)));
    assert_eq!(r.status(two), ItemStatus::Sorted);
    assert_eq!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.status(two), ItemStatus::Binned);
}

/// B8: the report's numbers are in the database: a new job (the app restarted) reads the same ones.
#[test]
fn report_survives_a_restart() {
    let r = trip();
    let (one, b) = (format!("{}/Beach/Item 1.jpg", r.root), format!("{}/b.jpg", r.cam));
    r.put(&one, "1");
    r.put(&b, "B");
    r.sorted(&one, 1, "Item 1");
    r.sorted(&b, 1, "Item 2");
    assert_eq!(r.job(&nobody).bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
    let restarted = r.job(&nobody).report(r.runs()[0].id).unwrap().unwrap();
    assert_eq!((restarted.in_place, restarted.check_ok), (1, Some(true)));
}

/// B8: shown until closed, including after a restart; a closed report never comes back.
#[test]
fn unread_report_is_shown_until_closed() {
    let r = trip();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    r.sorted(&a, 1, "Item 1");
    let job = r.job(&nobody);
    assert!(job.unread_report().unwrap().is_none());
    job.bin_files(&go(), &|_| {}).unwrap();
    let report = job.unread_report().unwrap().unwrap();
    assert_eq!(report.headline, "1 file binned into 1 category folder");
    r.db.close_report(report.run_id).unwrap();
    assert!(job.unread_report().unwrap().is_none());
}

#[test]
fn report_names_what_it_left_out() {
    let r = trip();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    r.sorted(&a, 1, "Item 1");
    let bad_path = format!("{}/bad.mp4", r.cam);
    let bad = r.item(&bad_path, None, None, ItemStatus::Failed);
    r.db.set_status(bad, ItemStatus::Failed, Some("decode")).unwrap();
    let job = r.job(&nobody);
    job.bin_files(&go(), &|_| {}).unwrap();
    let left = job.unread_report().unwrap().unwrap().left_out;
    assert_eq!(left.len(), 1);
    assert_eq!((left[0].name.as_str(), left[0].path.as_str(), left[0].failure.as_deref()), ("bad.mp4", bad_path.as_str(), Some("decode")));
}

/// State 1: what would happen, with nothing written, moved or marked.
#[test]
fn preview_changes_nothing() {
    let r = trip();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    let one = r.sorted(&a, 1, "Morning");
    let gone = r.sorted(&format!("{}/gone.jpg", r.cam), 1, "Item 2");
    let preview = r.job(&nobody).preview().unwrap();
    assert_eq!(preview.files, 1);
    assert!(r.runs().is_empty());
    assert!(files::exists(&a));
    assert!(!files::exists(&format!("{}/Beach", r.root)));
    // A gone file is reported, not marked, until a run does it.
    assert_eq!((r.status(one), r.status(gone)), (ItemStatus::Sorted, ItemStatus::Sorted));
}

#[test]
fn preview_counts_files_folders_and_sources_largest_first() {
    let r = trip();
    let download = format!("{}/Download", parent(parent(&r.cam)));
    for (path, category) in [(format!("{download}/d.jpg"), 1), (format!("{}/a.jpg", r.cam), 2), (format!("{}/b.jpg", r.cam), 2)] {
        r.put(&path, "x");
        r.sorted(&path, category, file_name(&path));
    }
    let preview = r.job(&nobody).preview().unwrap();
    assert_eq!((preview.files, preview.folders, preview.other_storage), (3, 2, 0));
    assert_eq!(preview.sources.iter().map(|s| (s.folder.as_str(), s.count)).collect::<Vec<_>>(), vec![("Camera", 2), ("Download", 1)]);
    assert_eq!(preview.sources[0].path, crate::paths::display(&r.cam));
}

/// B1, B6: files on another volume are copied, and the sheet says how much room that takes.
#[test]
fn preview_counts_copies_and_the_space_they_need() {
    let r = trip();
    r.other_volume();
    let (a, s) = (format!("{}/DCIM/a.mp4", r.sd), format!("{}/s.jpg", r.cam));
    r.put(&a, &"x".repeat(100));
    r.put(&s, &"S".repeat(10));
    r.sorted(&a, 1, "Item 1");
    r.sorted(&s, 1, "Item 2");
    faults::set(|f| f.free = Some(5000));
    let s_for_lookup = s.clone();
    let shared = move |paths: &[String]| paths.iter().filter(|p| **p == s_for_lookup).cloned().collect::<Vec<_>>();
    let preview = r.job(&shared).preview().unwrap();
    assert_eq!((preview.files, preview.other_storage, preview.shared), (2, 1, 1));
    assert_eq!((preview.copy_bytes, preview.free_bytes), (110, 5000));
    assert!(preview.sources.is_empty());
}

#[test]
fn preview_counts_what_is_already_in_its_folder() {
    let r = trip();
    let (one, two, c) = (format!("{}/Beach/Item 1.jpg", r.root), format!("{}/Beach/Item 2.jpg", r.root), format!("{}/c.jpg", r.cam));
    for p in [&one, &two, &c] {
        r.put(p, "x");
    }
    r.item(&one, Some(1), Some("Item 1"), ItemStatus::Binned);
    r.sorted(&two, 1, "Item 2");
    r.sorted(&c, 1, "Item 3");
    assert_eq!(r.job(&nobody).preview().unwrap().in_place, 2);
}

#[test]
fn preview_lists_what_stays_with_names() {
    let r = trip();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    r.sorted(&a, 1, "Item 1");
    r.item(&format!("{}/bad.mp4", r.cam), None, None, ItemStatus::Failed);
    r.item(&format!("{}/lost.jpg", r.cam), Some(1), Some("Item 3"), ItemStatus::Missing);
    r.sorted(&format!("{}/gone.jpg", r.cam), 1, "Item 4");
    let left = r.job(&nobody).preview().unwrap().left_out;
    assert_eq!(
        left.iter().map(|l| (l.name.as_str(), l.reason)).collect::<Vec<_>>(),
        vec![("bad.mp4", LeftOutReason::Failed), ("lost.jpg", LeftOutReason::Missing), ("gone.jpg", LeftOutReason::Missing)]
    );
}

#[test]
fn gone_removed_rows_are_dropped() {
    let r = trip();
    r.db.add_removed(&[format!("{}/Beach/gone.jpg", r.root)], "removed").unwrap();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    r.sorted(&a, 1, "Item 1");
    r.job(&nobody).bin_files(&go(), &|_| {}).unwrap();
    assert!(r.db.removed_paths().unwrap().is_empty());
}

// ---- a Save on binned files

impl Rig {
    /// A binned item whose file is in its category folder.
    fn binned(&self, folder: &str, name: &str, category: i64) -> i64 {
        let path = format!("{}/{folder}/{name}.jpg", self.root);
        self.put(&path, &format!("file {name}"));
        self.item(&path, Some(category), Some(name), ItemStatus::Binned)
    }

    /// A binned item whose file is still under its old name.
    fn binned_as(&self, path: &str, category: i64, name: &str) -> i64 {
        self.put(path, "x");
        self.item(path, Some(category), Some(name), ItemStatus::Binned)
    }

    fn plan_save(&self, job: &BinFilesJob, ids: &[i64]) -> Option<i64> {
        self.db.tx(|t| job.plan_save(t, ids)).unwrap()
    }
}

#[test]
fn plan_save_writes_a_run_for_a_binned_file_that_is_now_elsewhere() {
    let r = trip();
    let old = format!("{}/Beach/Old.jpg", r.root);
    let id = r.binned_as(&old, 1, "New");
    let job = r.job(&nobody);
    let run = r.plan_save(&job, &[id]).unwrap();
    assert_eq!(r.db.move_run(run).unwrap().unwrap().kind, "Save");
    assert_eq!(r.db.steps(run).unwrap()[0].dest, format!("{}/Beach/New.jpg", r.root));
    // Nothing moved yet.
    assert_eq!(r.content(&old).as_deref(), Some("x"));
    // What hasn't moved is ready to bin, so a Save stopped half way can't hide behind "All files binned".
    assert_eq!(r.status(id), ItemStatus::Sorted);
    assert_eq!(job.resume(run, &go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.status(id), ItemStatus::Binned);
    assert!(!files::exists(&old));
}

/// P8: another project's file is copied, never moved, by a Save as by Bin Files.
#[test]
fn plan_save_a_file_another_project_holds_is_copied_not_moved() {
    let r = trip();
    let old = format!("{}/Beach/Old.jpg", r.root);
    let id = r.binned_as(&old, 1, "New");
    let everything = |paths: &[String]| paths.to_vec();
    let job = r.job(&everything);
    let run = r.plan_save(&job, &[id]).unwrap();
    assert!(r.db.steps(run).unwrap()[0].keep_source);
    assert_eq!(job.resume(run, &go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.content(&old).as_deref(), Some("x"), "the other project's file stays");
    assert_eq!(r.content(&format!("{}/Beach/New.jpg", r.root)).as_deref(), Some("x"));
}

/// Items not binned only change in the database; their files move at the next Bin Files.
#[test]
fn plan_save_leaves_files_that_are_not_binned() {
    let r = trip();
    let a = format!("{}/a.jpg", r.cam);
    r.put(&a, "A");
    let id = r.sorted(&a, 1, "New");
    assert!(r.plan_save(&r.job(&nobody), &[id]).is_none());
    assert!(r.runs().is_empty());
}

#[test]
fn plan_save_nothing_to_do_when_the_file_is_already_there() {
    let r = trip();
    let id = r.binned("Beach", "Morning", 1);
    assert!(r.plan_save(&r.job(&nobody), &[id]).is_none());
    assert!(r.runs().is_empty());
    assert_eq!(r.status(id), ItemStatus::Binned);
}

#[test]
fn plan_save_only_touches_the_items_it_was_given() {
    let r = trip();
    let one = r.binned_as(&format!("{}/Beach/Old.jpg", r.root), 1, "New");
    let two = r.binned_as(&format!("{}/Beach/Other.jpg", r.root), 1, "Moved Too");
    let run = r.plan_save(&r.job(&nobody), &[one]).unwrap();
    assert_eq!(r.db.steps(run).unwrap().iter().map(|s| s.item_id).collect::<Vec<_>>(), vec![one]);
    assert_eq!(r.status(two), ItemStatus::Binned);
}

#[test]
fn plan_save_a_taken_name_gets_a_number() {
    let r = trip();
    r.binned("Beach", "Sunset", 1);
    let two = r.binned_as(&format!("{}/Beach/Old.jpg", r.root), 1, "Sunset");
    let run = r.plan_save(&r.job(&nobody), &[two]).unwrap();
    assert_eq!(r.db.steps(run).unwrap()[0].dest, format!("{}/Beach/Sunset 2.jpg", r.root));
}

/// A category rename is one move per file, then the empty old folder goes.
#[test]
fn renamed_category_moves_every_file_then_removes_the_old_folder() {
    let r = trip();
    let (a, b) = (r.binned("Beach", "A", 1), r.binned("Beach", "B", 1));
    r.made(&format!("{}/Beach", r.root));
    r.rename_category(1, "Coast");
    let job = r.job(&nobody);
    let run = r.plan_save(&job, &[a, b]).unwrap();
    assert_eq!(job.resume(run, &go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.content(&format!("{}/Coast/A.jpg", r.root)).as_deref(), Some("file A"));
    assert_eq!(r.content(&format!("{}/Coast/B.jpg", r.root)).as_deref(), Some("file B"));
    assert!(!files::exists(&format!("{}/Beach", r.root)));
    let report = job.report(run).unwrap().unwrap();
    assert_eq!(report.headline, "Saved. 2 files moved and checked.");
    assert_eq!(report.check_ok, Some(true));
    assert_eq!(report.removed_folders, vec!["Beach"]);
}

/// Q2: a removed file stays in the old folder, so that folder stays too.
#[test]
fn renamed_category_keeps_the_old_folder_while_it_holds_a_removed_file() {
    let r = trip();
    let a = r.binned("Beach", "A", 1);
    let removed = format!("{}/Beach/Removed.jpg", r.root);
    r.put(&removed, "removed");
    r.db.add_removed(&[removed.clone()], "removed").unwrap();
    r.made(&format!("{}/Beach", r.root));
    r.rename_category(1, "Coast");
    let job = r.job(&nobody);
    let run = r.plan_save(&job, &[a]).unwrap();
    assert_eq!(job.resume(run, &go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.content(&removed).as_deref(), Some("removed"));
    assert_eq!(job.report(run).unwrap().unwrap().check_ok, Some(true));
}

#[test]
fn merged_category_moves_its_files_into_the_folder_that_stays() {
    let r = trip();
    // Turtle Beach was merged into Beach: its item now has category 1.
    let (a, b) = (r.binned("Turtle Beach", "A", 1), r.binned("Beach", "B", 1));
    r.made(&format!("{}/Turtle Beach", r.root));
    r.made(&format!("{}/Beach", r.root));
    let job = r.job(&nobody);
    let run = r.plan_save(&job, &[a, b]).unwrap();
    assert_eq!(r.db.steps(run).unwrap().iter().map(|s| s.item_id).collect::<Vec<_>>(), vec![a]);
    assert_eq!(job.resume(run, &go(), &|_| {}).unwrap(), MoveOutcome::Done);
    assert_eq!(r.content(&format!("{}/Beach/A.jpg", r.root)).as_deref(), Some("file A"));
    assert!(!files::exists(&format!("{}/Turtle Beach", r.root)));
}

// ---------------------------------------------------------------------------------------------
// Windows only (not from the Android app): a file another program holds
// ---------------------------------------------------------------------------------------------

#[cfg(windows)]
mod on_windows {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    /// A file open in a player or an editor is skipped and reported, never half-moved; the rest
    /// of the run carries on, and the next Bin Files moves it once it is free.
    #[test]
    fn a_held_file_is_skipped_reported_and_moved_by_the_next_run() {
        let r = trip();
        let (a, b) = (format!("{}/a.mp4", r.cam), format!("{}/b.jpg", r.cam));
        r.put(&a, "playing");
        r.put(&b, "B");
        let (held_item, free_item) = (r.sorted(&a, 1, "Surf Clip"), r.sorted(&b, 1, "Morning"));
        let held = fs::OpenOptions::new().read(true).share_mode(1).open(&a).unwrap();
        let job = r.job(&nobody);
        assert_eq!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
        assert_eq!(r.content(&a).as_deref(), Some("playing"));
        assert!(!files::exists(&format!("{}/Beach/Surf Clip.mp4", r.root)));
        assert_eq!((r.status(held_item), r.status(free_item)), (ItemStatus::Sorted, ItemStatus::Binned));
        let report = job.unread_report().unwrap().unwrap();
        assert_eq!(report.moved, 1);
        assert_eq!(report.skipped, vec![SkippedFile { item_id: held_item, name: "Surf Clip.mp4".into(), reason: step_errors::IO.into() }]);
        assert_eq!(report.check_ok, Some(true));
        drop(held);
        assert_eq!(job.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
        assert_eq!(r.content(&format!("{}/Beach/Surf Clip.mp4", r.root)).as_deref(), Some("playing"));
        assert!(!files::exists(&a));
    }

    /// Names that differ only in case are one name on Windows: the second file gets a number.
    #[test]
    fn names_that_differ_only_in_case_never_collide() {
        let r = trip();
        let (a, b) = (format!("{}/a.jpg", r.cam), format!("{}/b.jpg", r.cam));
        r.put(&a, "A");
        r.put(&b, "B");
        r.sorted(&a, 1, "Sunset");
        r.sorted(&b, 1, "SUNSET");
        assert_eq!(r.job(&nobody).bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
        assert_eq!(files::list_files(&format!("{}/Beach", r.root)), vec![format!("{}/Beach/SUNSET 2.jpg", r.root), format!("{}/Beach/Sunset.jpg", r.root)]);
        assert_eq!(r.content(&format!("{}/Beach/Sunset.jpg", r.root)).as_deref(), Some("A"));
        assert_eq!(r.content(&format!("{}/Beach/SUNSET 2.jpg", r.root)).as_deref(), Some("B"));
    }

    /// A project deep enough that its files pass 260 characters still bins and checks clean.
    #[test]
    fn a_project_with_long_paths_bins_and_checks_clean() {
        let r = trip();
        let long = "A category name of forty characters long";
        let id = r.category(long);
        let a = format!("{}/a.jpg", r.cam);
        r.put(&a, "A");
        let name = "Sixty characters of friendly name to make this path long ok";
        r.sorted(&a, id, name);
        let deep = BinFilesJob::new(&r.db, &r.root, &nobody);
        assert_eq!(deep.bin_files(&go(), &|_| {}).unwrap(), MoveOutcome::Done);
        assert_eq!(r.content(&format!("{}/{long}/{name}.jpg", r.root)).as_deref(), Some("A"));
        assert_eq!(deep.unread_report().unwrap().unwrap().check_ok, Some(true));
    }
}
