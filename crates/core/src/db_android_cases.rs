//! The Android app's database tests, run against this port's SQLite layer: `SortDaoTest` and
//! `MoveDaoTest`. The schema is the Android app's, so the cases carry over one for one.

use super::tests::temp_db;
use super::*;

/// A Pending photo at `C:/x/<hash>.jpg`.
fn pending(db: &ProjectDb, hash: &str) -> i64 {
    db.insert_item(&NewItem { content_hash: hash.into(), path: format!("C:/x/{hash}.jpg"), name: format!("{hash}.jpg"), kind: ItemKind::Photo, taken_at: None, duration_ms: None, width: None, height: None })
        .unwrap()
}

fn analyze(db: &ProjectDb, id: i64, description: &str, tags: &[&str]) {
    let result = AnalysisResult {
        description: description.into(),
        actions: vec![crate::analysis::ActionEntry { t: "0:00".into(), text: "walking".into() }],
        tags: tags.iter().map(|t| t.to_string()).collect(),
        suggested_name: format!("Suggested {id}"),
        shot_type: "wide".into(),
    };
    db.save_analysis(id, &result, "{}", "m", "1", &Profile { stop_reason: "eos".into(), ..Default::default() }).unwrap();
}

/// An Analyzed item: "Waves on sand", tagged waves.
fn analyzed(db: &ProjectDb, hash: &str) -> i64 {
    let id = pending(db, hash);
    analyze(db, id, "Waves on sand", &["waves"]);
    id
}

fn sql(db: &ProjectDb, statement: &str) {
    db.read(|c| Ok(c.execute_batch(statement)?)).unwrap();
}

fn new_category(name: &str) -> NewCategoryEntity {
    NewCategoryEntity { name: name.into(), description: format!("{name} things") }
}

fn assign(id: i64, category: &str, name: &str) -> Assignment {
    Assignment { item_id: id, category_name: category.into(), friendly_name: name.into() }
}

fn into(db: &ProjectDb, id: i64, category: &str, name: &str) {
    assert_eq!(db.apply_chunk(&[new_category(category)], &[assign(id, category, name)]).unwrap(), 1);
}

fn item(db: &ProjectDb, id: i64) -> Item {
    db.item(id).unwrap().unwrap()
}

fn found(db: &ProjectDb, query: &str) -> Vec<i64> {
    db.search(query).unwrap()
}

fn category_names(db: &ProjectDb) -> Vec<String> {
    db.category_infos().unwrap().into_iter().map(|c| c.name).collect()
}

// ---------------------------------------------------------------------------------------------
// SortDaoTest
// ---------------------------------------------------------------------------------------------

#[test]
fn apply_chunk_assigns_and_refreshes_search() {
    let (_d, db) = temp_db();
    let id = analyzed(&db, "a");
    into(&db, id, "Beach", "Waves at noon");
    let saved = item(&db, id);
    assert_eq!((saved.status, saved.friendly_name.as_deref()), (ItemStatus::Sorted, Some("Waves at noon")));
    assert!(saved.category_id.is_some());
    // The category and the new name are searchable, and the analysis text still is.
    for query in ["category:beach", "beach", "friendly_name:noon", "sand", "waves"] {
        assert_eq!(found(&db, query), vec![id], "{query}");
    }
    let created = &db.category_infos().unwrap()[0];
    assert_eq!((created.name.as_str(), created.created_by.as_str(), created.items, created.description.as_str()), ("Beach", "model", 1, "Beach things"));
}

#[test]
fn apply_chunk_keeps_the_users_name() {
    let (_d, db) = temp_db();
    let id = analyzed(&db, "a");
    sql(&db, "UPDATE item SET friendlyName = 'My sunset', nameSetByUser = 1");
    into(&db, id, "Beach", "Model name");
    assert_eq!((item(&db, id).status, item(&db, id).friendly_name.as_deref()), (ItemStatus::Sorted, Some("My sunset")));
    assert_eq!(found(&db, "friendly_name:sunset"), vec![id]);
    assert!(found(&db, "friendly_name:model").is_empty());
    assert_eq!(found(&db, "category:beach"), vec![id]);
}

#[test]
fn apply_chunk_uses_existing_categories_and_writes_only_the_new_ones_it_uses() {
    let (_d, db) = temp_db();
    let (first, second) = (analyzed(&db, "a"), analyzed(&db, "b"));
    assert_eq!(db.apply_chunk(&[new_category("Beach"), new_category("Mountains")], &[assign(first, "Beach", "One")]).unwrap(), 1);
    assert_eq!(db.apply_chunk(&[], &[assign(second, "Beach", "Two")]).unwrap(), 1);
    let categories = db.category_infos().unwrap();
    assert_eq!(categories.len(), 1);
    assert_eq!((categories[0].name.as_str(), categories[0].items), ("Beach", 2));
}

#[test]
fn apply_chunk_is_all_or_nothing() {
    let (_d, db) = temp_db();
    let (a, b) = (analyzed(&db, "a"), analyzed(&db, "b"));
    assert!(db.apply_chunk(&[new_category("Beach")], &[assign(a, "Beach", "One"), assign(b, "Nope", "Two")]).is_err());
    assert!(db.category_infos().unwrap().is_empty());
    let untouched = item(&db, a);
    assert_eq!((untouched.status, untouched.category_id), (ItemStatus::Analyzed, None));
    assert_eq!(untouched.friendly_name.as_deref(), Some("Suggested 1"), "the name the analysis gave it stays");
    assert!(found(&db, "category:beach").is_empty());
    assert_eq!(db.uncategorized().unwrap().len(), 2);
}

#[test]
fn uncategorized_is_only_analyzed_items_the_model_may_place_in_id_order() {
    let (_d, db) = temp_db();
    let waiting = pending(&db, "a");
    analyze(&db, waiting, "Waves on sand", &["waves", "sand"]);
    let sorted = analyzed(&db, "b");
    let failed = pending(&db, "c");
    db.set_status(failed, ItemStatus::Failed, Some("decode")).unwrap();
    pending(&db, "d");
    into(&db, sorted, "Beach", "Done");
    // An item the user took out of its category waits for the user, not for the sorter.
    let users = analyzed(&db, "u");
    sql(&db, &format!("UPDATE item SET categorySetByUser = 1 WHERE id = {users}"));
    let later = analyzed(&db, "z");
    let list = db.uncategorized().unwrap();
    assert_eq!(list.iter().map(|i| i.id).collect::<Vec<_>>(), vec![waiting, later]);
    let only = &list[0];
    assert_eq!((only.kind, only.description.as_str(), only.suggested_name.as_str(), only.name_set_by_user), (ItemKind::Photo, "Waves on sand", "Suggested 1", false));
    assert_eq!(only.actions_json, r#"[{"t":"0:00","text":"walking"}]"#);
    assert_eq!(only.tags, vec!["sand", "waves"]);
}

#[test]
fn saving_an_analysis_again_keeps_the_name_the_category_and_the_status() {
    let (_d, db) = temp_db();
    let id = analyzed(&db, "a");
    into(&db, id, "Beach", "Waves at noon");
    // Re-analysis (after a prompt change, say) must not send it back to sorting.
    analyze(&db, id, "A heron wading", &["bird"]);
    assert_eq!(item(&db, id).status, ItemStatus::Sorted);
    assert!(item(&db, id).category_id.is_some());
    assert!(db.uncategorized().unwrap().is_empty());
    assert_eq!(found(&db, "category:beach"), vec![id]);
    assert_eq!(found(&db, "friendly_name:noon"), vec![id]);
    assert_eq!(found(&db, "heron"), vec![id]);
    assert!(found(&db, "sand").is_empty(), "the old description is gone from search");
    assert_eq!(db.tags_for(id).unwrap(), vec!["bird"]);
}

#[test]
fn saving_an_analysis_resets_attempts() {
    let (_d, db) = temp_db();
    let id = pending(&db, "h");
    db.mark_analyzing(id).unwrap();
    db.mark_analyzing(id).unwrap();
    assert_eq!(item(&db, id).attempts, 2);
    analyze(&db, id, "Something", &["x"]);
    assert_eq!((item(&db, id).attempts, item(&db, id).status), (0, ItemStatus::Analyzed));
}

#[test]
fn apply_chunk_skips_an_item_categorized_meanwhile_and_never_overwrites() {
    let (_d, db) = temp_db();
    let (first, second) = (analyzed(&db, "a"), analyzed(&db, "b"));
    into(&db, first, "Beach", "One");
    // A second run (or a stale reply) assigns the same item again.
    assert_eq!(db.apply_chunk(&[new_category("Food")], &[assign(first, "Food", "Lunch"), assign(second, "Food", "Dinner")]).unwrap(), 1);
    assert_eq!(item(&db, first).friendly_name.as_deref(), Some("One"));
    assert_eq!(found(&db, "category:beach"), vec![first]);
    assert_eq!(found(&db, "category:food"), vec![second]);
}

#[test]
fn apply_chunk_skips_an_item_whose_category_the_user_set() {
    let (_d, db) = temp_db();
    let id = analyzed(&db, "u");
    sql(&db, "UPDATE item SET categorySetByUser = 1");
    assert_eq!(db.apply_chunk(&[new_category("Beach")], &[assign(id, "Beach", "Model name")]).unwrap(), 0);
    assert_eq!((item(&db, id).category_id, item(&db, id).status), (None, ItemStatus::Analyzed));
    // Nothing used the new category, so it was not written.
    assert!(db.category_infos().unwrap().is_empty());
}

#[test]
fn apply_chunk_can_add_items_to_a_users_category() {
    let (_d, db) = temp_db();
    sql(&db, "INSERT INTO category (name, description, createdBy) VALUES ('Beach', 'Mine', 'user')");
    let id = analyzed(&db, "a");
    assert_eq!(db.apply_chunk(&[], &[assign(id, "Beach", "Waves at noon")]).unwrap(), 1);
    let category = &db.category_infos().unwrap()[0];
    assert_eq!((category.created_by.as_str(), category.description.as_str(), category.items), ("user", "Mine", 1));
    assert_eq!(item(&db, id).category_id, Some(category.id));
}

/// An item removed meanwhile is skipped; the rest of the chunk is written.
#[test]
fn apply_chunk_skips_an_unknown_item() {
    let (_d, db) = temp_db();
    let a = analyzed(&db, "a");
    assert_eq!(db.apply_chunk(&[new_category("Beach")], &[assign(a, "Beach", "One"), assign(9999, "Beach", "Ghost")]).unwrap(), 1);
    assert_eq!(item(&db, a).status, ItemStatus::Sorted);
}

#[test]
fn sort_runs_start_finish_and_list_finished_newest_first() {
    let (_d, db) = temp_db();
    let first = db.start_sort_run("m", "1", 5).unwrap();
    db.finish_sort_run(first, 4, 2, "done", None).unwrap();
    let second = db.start_sort_run("m", "1", 5).unwrap();
    db.finish_sort_run(second, 0, 0, "partial", Some("Stopped")).unwrap();
    let open = db.start_sort_run("m", "1", 5).unwrap();
    let runs = db.recent_sort_runs().unwrap();
    assert_eq!(runs.iter().map(|r| r.id).collect::<Vec<_>>(), vec![second, first], "finished runs only");
    assert_eq!((runs[1].sorted, runs[1].new_categories, runs[1].outcome.as_str(), runs[1].items), (4, 2, "done", 5));
    // Rows a process death left open are closed; finished ones are left alone.
    assert_eq!(db.close_open_sort_runs("Interrupted").unwrap(), 1);
    let runs = db.recent_sort_runs().unwrap();
    assert_eq!((runs[0].id, runs[0].outcome.as_str(), runs[0].message.as_deref()), (open, "partial", Some("Interrupted")));
    assert_eq!((runs[2].outcome.as_str(), runs[2].message.as_deref()), ("done", None));
    assert_eq!(db.close_open_sort_runs("Interrupted").unwrap(), 0);
}

#[test]
fn prepare_resort_deletes_empty_model_categories_and_refreshes_search() {
    let (_d, db) = temp_db();
    let (a, b) = (analyzed(&db, "a"), analyzed(&db, "b"));
    into(&db, a, "Beach", "One");
    into(&db, b, "Food", "Two");
    assert_eq!(db.prepare_resort(true).unwrap(), 2);
    assert_eq!((item(&db, a).status, item(&db, a).category_id), (ItemStatus::Analyzed, None));
    assert!(db.category_infos().unwrap().is_empty());
    assert!(found(&db, "category:beach").is_empty());
    assert_eq!(found(&db, "friendly_name:one"), vec![a], "the name stays until the sorter gives another");
    assert_eq!(db.uncategorized().unwrap().len(), 2);
}

/// Locks an item as the detail sheet does: the user's name and the user's category.
fn lock(db: &ProjectDb, id: i64, status: &str) {
    sql(db, &format!("UPDATE item SET nameSetByUser = 1, categorySetByUser = 1, status = '{status}' WHERE id = {id}"));
}

#[test]
fn prepare_resort_keeping_edits_leaves_locked_items_sorted_or_binned() {
    let (_d, db) = temp_db();
    let (locked, open, binned_locked, binned_open) = (analyzed(&db, "a"), analyzed(&db, "b"), analyzed(&db, "c"), analyzed(&db, "d"));
    into(&db, locked, "Beach", "One");
    into(&db, open, "Food", "Two");
    into(&db, binned_locked, "Beach", "Three");
    into(&db, binned_open, "Food", "Four");
    lock(&db, locked, "Sorted");
    lock(&db, binned_locked, "Binned");
    sql(&db, &format!("UPDATE item SET status = 'Binned' WHERE id = {binned_open}"));
    let beach = item(&db, locked).category_id;
    assert_eq!(db.prepare_resort(true).unwrap(), 2);
    assert_eq!((item(&db, locked).status, item(&db, locked).category_id), (ItemStatus::Sorted, beach));
    assert_eq!((item(&db, binned_locked).status, item(&db, binned_locked).category_id), (ItemStatus::Binned, beach));
    assert!(item(&db, locked).category_set_by_user);
    // A Re-sort reaches an unlocked binned item; its file stays put until Bin Files.
    assert_eq!((item(&db, binned_open).status, item(&db, binned_open).category_id), (ItemStatus::Analyzed, None));
    assert_eq!(item(&db, binned_open).current_path, "C:/x/d.jpg");
    assert_eq!(db.uncategorized().unwrap().iter().map(|i| i.id).collect::<Vec<_>>(), vec![open, binned_open]);
    assert_eq!(category_names(&db), vec!["Beach"]);
}

#[test]
fn prepare_resort_without_keeping_edits_clears_locks() {
    let (_d, db) = temp_db();
    let locked = analyzed(&db, "a");
    into(&db, locked, "Beach", "One");
    lock(&db, locked, "Sorted");
    assert_eq!(db.prepare_resort(false).unwrap(), 1);
    let cleared = item(&db, locked);
    assert!(!cleared.name_set_by_user && !cleared.category_set_by_user);
    assert_eq!((cleared.status, cleared.category_id), (ItemStatus::Analyzed, None));
    assert!(db.category_infos().unwrap().is_empty());
}

/// A failed item set by hand is Sorted with both locks but has no analysis for a sorter to read.
/// A Re-sort leaves it as it is, with or without Keep my edits, and keeps it searchable.
#[test]
fn prepare_resort_leaves_an_item_without_analysis_sorted() {
    for keep_edits in [false, true] {
        let (_d, db) = temp_db();
        let hand = pending(&db, "hand");
        sql(&db, "INSERT INTO category (name, description, createdBy) VALUES ('Mine', '', 'user')");
        let unlocked_category = if keep_edits { 0 } else { 1 };
        sql(&db, &format!("UPDATE item SET status = 'Sorted', categoryId = 1, friendlyName = 'Broken Card', nameSetByUser = 1, categorySetByUser = {unlocked_category}"));
        db.tx(|t| refresh_fts(t, hand)).unwrap();
        assert_eq!(db.prepare_resort(keep_edits).unwrap(), 0, "keep edits: {keep_edits}");
        assert_eq!((item(&db, hand).status, item(&db, hand).category_id), (ItemStatus::Sorted, Some(1)));
        assert!(db.uncategorized().unwrap().is_empty());
        assert_eq!(found(&db, "friendly_name:broken"), vec![hand]);
        assert_eq!(category_names(&db), vec!["Mine"]);
    }
}

// ---------------------------------------------------------------------------------------------
// MoveDaoTest
// ---------------------------------------------------------------------------------------------

/// A Sorted item at `D:/DCIM/Camera/<hash>.jpg`.
fn sorted(db: &ProjectDb, hash: &str) -> i64 {
    let id = db
        .insert_item(&NewItem { content_hash: hash.into(), path: format!("D:/DCIM/Camera/{hash}.jpg"), name: format!("{hash}.jpg"), kind: ItemKind::Photo, taken_at: None, duration_ms: None, width: None, height: None })
        .unwrap();
    db.set_status(id, ItemStatus::Sorted, None).unwrap();
    id
}

fn step(item: i64, source: &str, dest: &str) -> NewStep {
    NewStep { item_id: item, source: source.into(), dest: dest.into(), bytes: 10, keep_source: false, numbered: false }
}

fn run(db: &ProjectDb, steps: &[NewStep], in_place: &[i64], missing: &[i64]) -> i64 {
    db.tx(|t| write_run(t, "Bin", steps, in_place, missing, true)).unwrap()
}

fn one_step(db: &ProjectDb) -> (i64, i64, i64) {
    let a = sorted(db, "a");
    let run_id = run(db, &[step(a, "D:/DCIM/Camera/a.jpg", "C:/p/Beach/A.jpg")], &[], &[]);
    (a, run_id, db.steps(run_id).unwrap()[0].id)
}

#[test]
fn write_run_writes_steps_and_marks_items_together() {
    let (_d, db) = temp_db();
    let (a, b, c, d) = (sorted(&db, "a"), sorted(&db, "b"), sorted(&db, "c"), sorted(&db, "d"));
    let run_id = run(&db, &[step(a, "D:/a", "C:/p/Beach/A.jpg"), step(b, "D:/b", "C:/p/Beach/B.jpg")], &[c], &[d]);
    let steps = db.steps(run_id).unwrap();
    assert_eq!(steps.iter().map(|s| s.item_id).collect::<Vec<_>>(), vec![a, b]);
    assert!(steps.iter().all(|s| s.run_id == run_id && s.state == "Planned" && s.attempts == 0 && s.bytes == 10));
    assert_eq!((item(&db, c).status, item(&db, d).status), (ItemStatus::Binned, ItemStatus::Missing));
    let open = db.open_move_run().unwrap().unwrap();
    assert_eq!((open.id, open.in_place, open.state.as_str()), (run_id, 1, "Running"));
}

#[test]
fn record_moved_updates_item_and_step_together() {
    let (_d, db) = temp_db();
    let (a, run_id, step_id) = one_step(&db);
    db.record_moved(step_id, a, "C:/p/Beach/A.jpg", Some("newhash"), "Done").unwrap();
    let moved = item(&db, a);
    assert_eq!((moved.status, moved.content_hash.as_str(), moved.current_path.as_str()), (ItemStatus::Binned, "newhash", "C:/p/Beach/A.jpg"));
    assert_eq!(moved.original_path, "D:/DCIM/Camera/a.jpg", "where it came from is kept");
    assert_eq!(db.steps(run_id).unwrap()[0].state, "Done");
}

/// Two items can't share a hash: one that would clash keeps its own.
#[test]
fn record_moved_keeps_the_hash_on_a_clash() {
    let (_d, db) = temp_db();
    let (a, _run, step_id) = one_step(&db);
    sorted(&db, "taken");
    db.record_moved(step_id, a, "C:/p/Beach/A.jpg", Some("taken"), "Done").unwrap();
    assert_eq!(item(&db, a).content_hash, "a");
}

#[test]
fn record_missing_and_record_reverted_mark_item_and_step() {
    let (_d, db) = temp_db();
    let (a, run_id, step_id) = one_step(&db);
    db.record_missing(step_id, a).unwrap();
    assert_eq!((item(&db, a).status, db.steps(run_id).unwrap()[0].state.as_str()), (ItemStatus::Missing, "Missing"));
    db.record_reverted(step_id, a, "D:/DCIM/Camera/a.jpg", "io").unwrap();
    let step = &db.steps(run_id).unwrap()[0];
    assert_eq!((item(&db, a).status, step.state.as_str(), step.error.as_deref()), (ItemStatus::Sorted, "Failed", Some("io")));
}

#[test]
fn start_step_counts_attempts() {
    let (_d, db) = temp_db();
    let (_a, run_id, step_id) = one_step(&db);
    assert_eq!((db.start_step(step_id).unwrap(), db.start_step(step_id).unwrap()), (1, 2));
    db.set_step_dest(step_id, "C:/p/Beach/A 2.jpg").unwrap();
    db.set_step_state(step_id, "Skipped", Some("copy-mismatch")).unwrap();
    let s = &db.steps(run_id).unwrap()[0];
    assert_eq!((s.dest.as_str(), s.error.as_deref(), s.attempts), ("C:/p/Beach/A 2.jpg", Some("copy-mismatch"), 2));
}

#[test]
fn open_run_finds_running_and_paused() {
    let (_d, db) = temp_db();
    let (a, first, _) = one_step(&db);
    db.finish_move_run(first, "Done", None, &["C:/p/Turtle Beach".to_string()], Some(true)).unwrap();
    assert!(db.open_move_run().unwrap().is_none());
    let second = run(&db, &[step(a, "C:/p/B/A.jpg", "C:/p/C/A.jpg")], &[], &[]);
    db.set_run_state(second, "Paused", None).unwrap();
    assert_eq!(db.open_move_run().unwrap().map(|r| r.id), Some(second));
    assert_eq!(db.move_run(first).unwrap().unwrap().removed_folders.as_deref(), Some("C:/p/Turtle Beach"));
}

/// The newest finished run's folder check keeps Bin Files on while it found something.
#[test]
fn last_check_ok_follows_the_newest_finished_run() {
    let (_d, db) = temp_db();
    assert_eq!(db.last_check_ok().unwrap(), None);
    let (_a, first, _) = one_step(&db);
    db.finish_move_run(first, "Done", None, &[], Some(false)).unwrap();
    assert_eq!(db.last_check_ok().unwrap(), Some(false));
    let open = run(&db, &[], &[], &[]);
    assert_eq!(db.last_check_ok().unwrap(), Some(false), "a run still going doesn't count");
    db.finish_move_run(open, "Done", None, &[], Some(true)).unwrap();
    assert_eq!(db.last_check_ok().unwrap(), Some(true));
}

/// The newest finished run's report is unread until closed; closing it closes older ones too.
#[test]
fn unread_run_is_the_newest_until_closed() {
    let (_d, db) = temp_db();
    assert!(db.unread_move_run().unwrap().is_none());
    let (_a, first, _) = one_step(&db);
    assert!(db.unread_move_run().unwrap().is_none(), "a run still going has no report");
    db.finish_move_run(first, "Stopped", None, &[], Some(true)).unwrap();
    let second = run(&db, &[], &[], &[]);
    db.finish_move_run(second, "Done", None, &[], Some(false)).unwrap();
    let unread = db.unread_move_run().unwrap().unwrap();
    assert_eq!((unread.id, unread.check_ok), (second, Some(false)));
    db.close_report(second).unwrap();
    assert!(db.unread_move_run().unwrap().is_none());
}

#[test]
fn binned_folders_are_where_a_step_put_a_file() {
    let (_d, db) = temp_db();
    let (a, b) = (sorted(&db, "a"), sorted(&db, "b"));
    let run_id = run(&db, &[step(a, "D:/a", "C:/p/Beach/A.jpg"), step(b, "D:/b", "C:/p/Sunset/B.jpg")], &[], &[]);
    db.record_moved(db.steps(run_id).unwrap()[0].id, a, "C:/p/Beach/A.jpg", None, "Done").unwrap();
    assert_eq!(db.binned_folders().unwrap(), vec!["C:/p/Beach"]);
}

#[test]
fn removed_paths_add_and_drop() {
    let (_d, db) = temp_db();
    db.add_removed(&["C:/p/Beach/x.jpg".to_string(), "C:/p/Beach/y.jpg".to_string()], "removed").unwrap();
    db.add_removed(&["C:/p/Beach/y.jpg".to_string()], "left").unwrap();
    assert_eq!(db.removed_paths().unwrap(), vec!["C:/p/Beach/x.jpg", "C:/p/Beach/y.jpg"]);
    db.drop_removed(&["C:/p/Beach/x.jpg".to_string()]).unwrap();
    assert_eq!(db.removed_paths().unwrap(), vec!["C:/p/Beach/y.jpg"]);
}

#[test]
fn bin_items_and_category_names_read_the_planners_input() {
    let (_d, db) = temp_db();
    let a = analyzed(&db, "a");
    into(&db, a, "Beach", "Waves");
    let items = db.bin_items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!((items[0].id, items[0].friendly_name.as_deref(), items[0].original_name.as_str(), items[0].status), (a, Some("Waves"), "a.jpg", ItemStatus::Sorted));
    assert_eq!(db.category_names().unwrap(), vec![(items[0].category_id.unwrap(), "Beach".to_string())]);
}
