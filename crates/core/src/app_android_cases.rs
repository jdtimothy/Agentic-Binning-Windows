//! The Android app's `projectmedia` test cases, run against this port: `MediaStatusTest`,
//! `MediaCountsTest` and `CategoryNamesTest`.

use super::*;
use crate::db::tests::{add, temp_db};
use crate::names::clean_name;

fn counts() -> MediaCounts {
    MediaCounts::default()
}

fn status(c: &MediaCounts) -> MediaStatus {
    media_status(c, None, false)
}

fn binning(done: usize, total: usize) -> ActiveJob {
    ActiveJob::Binning { done, total, paused: false }
}

fn saving(done: usize, total: usize) -> ActiveJob {
    ActiveJob::Saving { done, total, paused: false }
}

// ---------------------------------------------------------------------------------------------
// MediaStatusTest
// ---------------------------------------------------------------------------------------------

#[test]
fn empty_project() {
    let s = status(&counts());
    assert_eq!((s.text.as_str(), s.bin_files_enabled), ("Nothing to bin yet", false));
}

#[test]
fn ready_enables_bin_files() {
    let s = status(&MediaCounts { total: 40, ready_to_bin: 34, binned: 6, ..counts() });
    assert_eq!((s.text.as_str(), s.bin_files_enabled, s.ready_to_bin, s.tone), ("34 files ready to bin", true, 34, "Ready"));
    assert_eq!(status(&MediaCounts { total: 1, ready_to_bin: 1, ..counts() }).text, "1 file ready to bin");
}

#[test]
fn waiting_to_be_categorized_blocks_bin_files_and_offers_categorize_now() {
    let s = status(&MediaCounts { total: 44, waiting: 3, failed: 1, ready_to_bin: 34, binned: 6, ..counts() });
    assert_eq!(s.text, "34 files ready to bin · 3 waiting to be categorized");
    assert_eq!((s.bin_files_enabled, s.categorize_now, s.tone), (false, true, "Attention"));
}

#[test]
fn failed_and_missing_do_not_block_bin_files() {
    let s = status(&MediaCounts { total: 40, failed: 1, missing: 2, ready_to_bin: 37, ..counts() });
    assert_eq!(s.text, "37 files ready to bin · 1 failed, 2 missing");
    assert_eq!((s.bin_files_enabled, s.categorize_now), (true, false));
}

#[test]
fn pending_items_block_bin_files_even_when_not_running() {
    let s = status(&MediaCounts { total: 50, pending: 12, ready_to_bin: 38, ..counts() });
    assert_eq!((s.text.as_str(), s.bin_files_enabled), ("38 files ready to bin · 12 waiting to analyze", false));
}

#[test]
fn all_binned() {
    let s = status(&MediaCounts { total: 41, failed: 1, binned: 40, ..counts() });
    assert_eq!((s.text.as_str(), s.bin_files_enabled, s.tone), ("All files binned · 1 failed", false, "Quiet"));
}

/// Owner's ruling (Nov 10): while the last folder check found something, Bin Files stays on with
/// nothing to bin, so it can run the check and show the unknown-files sheet.
#[test]
fn all_binned_last_check_failed_bin_files_runs_the_check() {
    let c = MediaCounts { total: 41, failed: 1, binned: 40, ..counts() };
    let s = media_status(&c, None, true);
    assert_eq!((s.text.as_str(), s.bin_files_enabled, s.tone, s.blocked_reason.clone()), ("All files binned · check the folders", true, "Ready", None));
    // Still off while a job runs or something waits; the check comes with the next Bin Files then.
    assert!(!media_status(&c, Some(ActiveJob::Sorting), true).bin_files_enabled);
    let waiting = media_status(&MediaCounts { pending: 1, ..c.clone() }, None, true);
    assert_eq!((waiting.bin_files_enabled, waiting.blocked_reason.as_deref()), (false, Some("Bin Files waits for analysis to finish")));
    // With files ready, Bin Files is on anyway and its run checks first: no extra words.
    assert_eq!(media_status(&MediaCounts { ready_to_bin: 3, ..c }, None, true).text, "3 files ready to bin · 1 failed");
}

#[test]
fn any_job_disables_bin_files() {
    let c = MediaCounts { total: 40, ready_to_bin: 34, binned: 6, ..counts() };
    let text = |c: &MediaCounts, job| media_status(c, Some(job), false).text;
    assert_eq!(text(&MediaCounts { pending: 11, analyzing: 1, ..c.clone() }, ActiveJob::Analysis), "Analyzing 12 left");
    assert_eq!(text(&c, ActiveJob::Sorting), "Categorizing…");
    assert_eq!(text(&c, saving(1, 3)), "Saving · moving 2 of 3 files");
    assert_eq!(text(&c, saving(0, 1)), "Saving · moving 1 of 1 file");
    assert_eq!(text(&c, saving(0, 0)), "Saving…");
    assert_eq!(text(&c, binning(40, 40)), "Binning · 40 of 40 files");
    for job in [ActiveJob::Analysis, ActiveJob::Sorting, saving(0, 3), binning(0, 3)] {
        let s = media_status(&c, Some(job), false);
        assert_eq!((s.bin_files_enabled, s.tone), (false, "Running"), "{job:?}");
    }
}

#[test]
fn analyzing_item_without_a_job_still_reads_as_analyzing() {
    let s = status(&MediaCounts { total: 10, pending: 4, analyzing: 1, ready_to_bin: 5, ..counts() });
    assert_eq!((s.text.as_str(), s.bin_files_enabled), ("Analyzing 5 left", false));
}

#[test]
fn bin_files_reason_names_what_blocks_it() {
    let reason = |c: MediaCounts, job| media_status(&c, job, false).blocked_reason;
    assert_eq!(reason(MediaCounts { total: 4, ready_to_bin: 4, ..counts() }, Some(ActiveJob::Sorting)).as_deref(), Some("Bin Files is available when analysis is stopped or done"));
    assert_eq!(reason(MediaCounts { total: 4, pending: 1, ready_to_bin: 3, ..counts() }, None).as_deref(), Some("Bin Files waits for analysis to finish"));
    assert_eq!(reason(MediaCounts { total: 4, waiting: 3, ready_to_bin: 1, ..counts() }, None).as_deref(), Some("Bin Files waits for 3 items to be categorized"));
    assert_eq!(reason(MediaCounts { total: 1, failed: 1, ..counts() }, None).as_deref(), Some("Nothing to bin yet"));
    assert_eq!(reason(MediaCounts { total: 2, binned: 2, ..counts() }, None).as_deref(), Some("All files are binned"));
    // On exactly when the status turns Bin Files on.
    let ready = MediaCounts { total: 5, failed: 1, ready_to_bin: 4, ..counts() };
    assert_eq!(reason(ready.clone(), None), None);
    assert!(status(&ready).bin_files_enabled);
}

/// A move in progress, paused, or before its first file (the folder check).
#[test]
fn moving_status_lines() {
    let c = MediaCounts { total: 251, ready_to_bin: 248, failed: 1, missing: 2, ..counts() };
    assert_eq!(media_status(&c, Some(binning(0, 0)), false).text, "Binning · checking the folders");
    assert_eq!(media_status(&c, Some(binning(12, 248)), false).text, "Binning · 13 of 248 files");
    let paused = media_status(&c, Some(ActiveJob::Binning { done: 37, total: 248, paused: true }), false);
    assert_eq!((paused.text.as_str(), paused.tone, paused.bin_files_enabled), ("Bin Files paused · 37 of 248 files", "Attention", false));
    assert_eq!(media_status(&c, Some(ActiveJob::Saving { done: 1, total: 3, paused: true }), false).text, "Saving paused · 1 of 3 files");
}

#[test]
fn moving_blocks_bin_files_with_its_own_reason() {
    let c = MediaCounts { total: 4, ready_to_bin: 4, ..counts() };
    let reason = |job| media_status(&c, Some(job), false).blocked_reason.unwrap();
    assert_eq!(reason(binning(1, 4)), "Bin Files is running");
    assert_eq!(reason(ActiveJob::Binning { done: 1, total: 4, paused: true }), "Bin Files is running");
    assert_eq!(reason(saving(0, 2)), "Wait for the files to finish moving");
}

// ---------------------------------------------------------------------------------------------
// MediaCountsTest
// ---------------------------------------------------------------------------------------------

#[test]
fn every_status_has_its_own_bucket() {
    let (_d, db) = temp_db();
    for i in 1..=10 {
        add(&db, &format!("{i}.jpg"));
    }
    db.read(|c| {
        c.execute_batch(
            "INSERT INTO category (name, description, createdBy) VALUES ('A', '', 'model'), ('B', '', 'model');
             UPDATE item SET status = 'Analyzing' WHERE id = 3;
             UPDATE item SET status = 'Analyzed' WHERE id = 4;
             UPDATE item SET status = 'Failed' WHERE id = 5;
             UPDATE item SET status = 'Missing' WHERE id = 6;
             UPDATE item SET status = 'Sorted', categoryId = 1 WHERE id = 7;
             UPDATE item SET status = 'Sorted', categoryId = 2 WHERE id = 8;
             UPDATE item SET status = 'Binned', categoryId = 1 WHERE id = 9;
             UPDATE item SET status = 'Analyzed', categorySetByUser = 1 WHERE id = 10;",
        )?;
        Ok(())
    })
    .unwrap();
    let c = counts_of(&db.grid().unwrap());
    // Waiting is only an Analyzed item without a category that the model may still set: item 10,
    // which the user took out of its category, waits for the user, not for sorting.
    assert_eq!(c, MediaCounts { total: 10, pending: 2, analyzing: 1, waiting: 1, failed: 1, missing: 1, ready_to_bin: 2, binned: 1 });
    assert_eq!(counts_of(&[]), counts());
}

#[test]
fn active_job_follows_the_phase() {
    let state = |phase: Phase| {
        let mut s = crate::pipeline::Pipeline::new(|_| {}, || {}).state();
        s.phase = phase;
        s
    };
    assert_eq!(active_job(&state(Phase::Categorizing)), Some(ActiveJob::Sorting));
    for phase in [Phase::LoadingModel, Phase::Preparing, Phase::Describing, Phase::Paused, Phase::Releasing] {
        assert_eq!(active_job(&state(phase)), Some(ActiveJob::Analysis), "{phase:?}");
    }
    assert_eq!(active_job(&state(Phase::Idle)), None);
    assert_eq!(active_job(&state(Phase::Stopped)), None);
    // A move is Binning or Saving, with its progress.
    let moving = |phase: Phase, kind: &str, done: usize, total: usize| {
        let mut s = state(phase);
        s.move_progress =
            Some(crate::moves::MoveProgress { kind: kind.into(), done, total, current: None, phase: None, paused: phase == Phase::MovingPaused, from: None, to: None, next: None, recent: vec![] });
        s
    };
    assert_eq!(active_job(&moving(Phase::Moving, "Bin", 12, 248)), Some(binning(12, 248)));
    assert_eq!(active_job(&moving(Phase::MovingPaused, "Bin", 12, 248)), Some(ActiveJob::Binning { done: 12, total: 248, paused: true }));
    assert_eq!(active_job(&moving(Phase::Moving, "Save", 1, 3)), Some(saving(1, 3)));
    // Before the first progress (the folder check), a move is Binning with nothing counted yet.
    assert_eq!(active_job(&state(Phase::Moving)), Some(binning(0, 0)));
    // A paused analysis still blocks Bin Files.
    let s = media_status(&MediaCounts { total: 5, pending: 2, ready_to_bin: 3, ..counts() }, active_job(&state(Phase::Paused)), false);
    assert_eq!((s.bin_files_enabled, s.text.as_str()), (false, "Analyzing 2 left"));
}

// ---------------------------------------------------------------------------------------------
// CategoryNamesTest
// ---------------------------------------------------------------------------------------------

/// Editing uses the sorter's key, so the two can never disagree about a clash.
fn key(name: &str) -> String {
    validator::normalize(name)
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn key_ignores_case_spacing_punctuation_and_plurals() {
    assert_eq!(key("Sea Lion"), key("sea lions"));
    assert_eq!(key("Sea Lion"), key("Sea-lions!"));
    assert_eq!(key("Turtle Beach"), key("turtle beaches"));
    assert_eq!(key("Berry"), key("Berries"));
    assert_eq!(key("Fox"), key("Foxes"));
    assert_eq!(key("Glass"), "glass");
    assert_eq!(key("Mama’s Fish House"), key("Mama's Fish House"));
    assert_eq!(key("Beach"), key("The Beach"));
    assert_eq!(key("Cafe Visit"), key("Café Visits"));
}

/// Pinned on purpose: an earlier draft's key treated these as the same name; the sorter's does not.
#[test]
fn key_differences_follow_the_sorter() {
    assert_ne!(key("Sea Lion"), key("SEALIONS"));
    assert_ne!(key("Mama's Fish House"), key("Mamas Fish House"));
    assert_ne!(key("Sea Lion Beach"), key("Sea Lions Beach"));
}

#[test]
fn clash_finds_the_existing_name() {
    let existing = names(&["Turtle Beach", "Beach Scenes", "Wedding Events"]);
    assert_eq!(clash("turtle beaches", &existing, None).as_deref(), Some("Turtle Beach"));
    assert_eq!(clash("Snorkeling", &existing, None), None);
    // A blank name never clashes.
    assert_eq!(clash(" !! ", &names(&["A"]), None), None);
}

#[test]
fn clash_ignores_the_category_being_renamed() {
    let existing = names(&["Beach Scene", "Wedding Events"]);
    assert_eq!(clash("Beach Scenes", &existing, Some("Beach Scene")), None);
    assert_eq!(clash("wedding event", &existing, Some("Beach Scene")).as_deref(), Some("Wedding Events"));
}

#[test]
fn clean_trims_collapses_and_caps_at_the_sorters_limit() {
    let clean = |name: &str| clean_name(name, validator::MAX_CATEGORY_NAME);
    assert_eq!(clean("  Sea   Turtles "), "Sea Turtles");
    assert_eq!(clean(&"x".repeat(200)).chars().count(), 40);
    assert_eq!(clean("Beach/Sunset?"), "Beach Sunset");
    assert_eq!(clean("Trip..."), "Trip");
}
