//! The Android app's `ProjectRegistryTest` and `RegistrarTest`, run against this port on real
//! folders and real project databases.

use super::*;
use crate::db::ItemStatus;
use crate::paths::norm;

struct World {
    _dir: tempfile::TempDir,
    root: String,
    registry: ProjectRegistry,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let root = norm(dir.path());
    let registry = ProjectRegistry::new(dir.path().join("projects.json"), format!("{root}/Agentic Binning"));
    World { _dir: dir, root, registry }
}

impl World {
    fn create(&self, name: &str) -> ProjectRef {
        self.registry.create(name).unwrap()
    }

    /// A second instance over the same file, as after a restart.
    fn restarted(&self) -> ProjectRegistry {
        ProjectRegistry::new(self.registry.file.clone(), self.registry.projects_root.clone())
    }

    fn list_bytes(&self) -> Vec<u8> {
        std::fs::read(&self.registry.file).unwrap()
    }

    fn corrupt(&self) -> Vec<u8> {
        std::fs::write(&self.registry.file, "{not json").unwrap();
        self.list_bytes()
    }
}

// ---------------------------------------------------------------------------------------------
// ProjectRegistryTest
// ---------------------------------------------------------------------------------------------

#[test]
fn create_makes_layout_and_registers() {
    let w = world();
    let a = w.create("La Jolla Trip");
    assert!(Path::new(&thumbs_dir(&a.path)).is_dir());
    assert_eq!(w.registry.list(), vec![a.clone()]);
    assert!(w.registry.create("La Jolla Trip").is_err(), "an existing folder is never reused");
    assert!(w.registry.create("   ").is_err());
    assert_eq!(w.registry.list(), vec![a]);
}

#[test]
fn current_persists_across_instances() {
    let w = world();
    let (a, b) = (w.create("A"), w.create("B"));
    w.registry.set_current(&a).unwrap();
    assert_eq!(w.restarted().current(), Some(a.clone()));
    w.registry.set_current(&b).unwrap();
    assert_eq!(w.restarted().current(), Some(b.clone()));
    assert_eq!(w.restarted().list(), vec![a.clone(), b]);
    // Clearing forgets the open project and keeps the list.
    w.registry.clear_current().unwrap();
    assert_eq!(w.restarted().current(), None);
    assert_eq!(w.restarted().list().len(), 2);
}

#[test]
fn clear_current_with_no_current_writes_nothing() {
    let w = world();
    w.registry.clear_current().unwrap();
    assert_eq!(w.registry.current(), None);
    assert!(w.registry.list().is_empty());
    assert!(!w.registry.file.exists());
}

#[test]
fn project_containing_matches_only_descendants() {
    let w = world();
    let a = w.create("A");
    w.create("AB");
    assert_eq!(w.registry.project_containing(&format!("{}/Sea lions/x.mp4", a.path)), Some(a));
    assert_eq!(w.registry.project_containing(&format!("{}/Agentic Binning/ABC/x.mp4", w.root)), None);
}

#[test]
fn corrupt_list_reads_as_empty_and_every_write_refuses_and_keeps_the_file() {
    let w = world();
    let before = w.corrupt();
    assert!(w.registry.list().is_empty());
    assert_eq!(w.registry.current(), None);
    let a = ProjectRef { name: "A".into(), path: format!("{}/Agentic Binning/A", w.root) };
    assert!(w.registry.create("A").is_err());
    assert!(!Path::new(&a.path).exists(), "no folder is created when the list can't be updated");
    assert!(w.registry.set_current(&a).is_err());
    assert!(w.registry.clear_current().is_err());
    assert!(w.registry.remove(&a.path).is_err());
    assert!(w.registry.restore(&RemovedProject { project: a, index: 0, was_current: false }).is_err());
    assert_eq!(w.list_bytes(), before);
}

#[test]
fn the_same_folder_by_another_path_adds_no_duplicate() {
    let w = world();
    let a = w.create("A");
    let other = format!("{}/Agentic Binning/x/../A/", w.root);
    let stored = w.registry.set_current(&ProjectRef { name: "Renamed".into(), path: other }).unwrap();
    assert_eq!(stored, a);
    assert_eq!(w.registry.list(), vec![a.clone()]);
    assert_eq!(w.registry.current(), Some(a.clone()));
    assert_eq!(w.registry.find(&format!("{}/Agentic Binning/./A/", w.root)), Some(a));
    assert_eq!(w.registry.find(&format!("{}/Agentic Binning/B", w.root)), None);
}

#[test]
fn remove_forgets_only_that_entry() {
    let w = world();
    let (a, b, c) = (w.create("A"), w.create("B"), w.create("C"));
    w.registry.set_current(&a).unwrap();
    assert_eq!(w.registry.remove(&b.path).unwrap(), Some(RemovedProject { project: b.clone(), index: 1, was_current: false }));
    assert_eq!(w.registry.list(), vec![a.clone(), c]);
    assert_eq!(w.registry.current(), Some(a.clone()));
    // It survives a restart.
    assert_eq!(w.restarted().list().len(), 2);
    assert_eq!(w.restarted().current(), Some(a));
}

#[test]
fn remove_current_clears_current_and_an_unknown_path_changes_nothing() {
    let w = world();
    let a = w.create("A");
    w.registry.set_current(&a).unwrap();
    let before = w.list_bytes();
    assert_eq!(w.registry.remove(&format!("{}/Agentic Binning/Nope", w.root)).unwrap(), None);
    assert_eq!(w.list_bytes(), before);
    let removed = w.registry.remove(&format!("{}/Agentic Binning/x/../A", w.root)).unwrap();
    assert_eq!(removed, Some(RemovedProject { project: a, index: 0, was_current: true }));
    assert_eq!(w.registry.current(), None);
    assert!(w.registry.list().is_empty());
}

#[test]
fn restore_puts_the_entry_back_at_its_position() {
    let w = world();
    let (a, b, c) = (w.create("A"), w.create("B"), w.create("C"));
    let removed = w.registry.remove(&b.path).unwrap().unwrap();
    w.registry.restore(&removed).unwrap();
    assert_eq!(w.registry.list(), vec![a.clone(), b.clone(), c.clone()]);
    w.registry.restore(&removed).unwrap();
    assert_eq!(w.registry.list().len(), 3, "restoring twice adds nothing");
    // After the list shrank, it goes at the end.
    let removed_c = w.registry.remove(&c.path).unwrap().unwrap();
    w.registry.remove(&b.path).unwrap();
    w.registry.restore(&removed_c).unwrap();
    assert_eq!(w.registry.list(), vec![a, c]);
}

/// Relative path, size and SHA-256 of every file under `dir`.
fn snapshot(dir: &str) -> Vec<(String, i64, String)> {
    files::list_files(dir).into_iter().map(|p| (p[dir.len()..].to_string(), files::size(&p), files::sha256(&p).unwrap())).collect()
}

#[test]
fn remove_preserves_every_file_and_never_recreates_a_missing_folder() {
    let w = world();
    let a = w.create("A");
    for (file, bytes) in [("Beach/sea turtle.jpg", vec![3u8; 2048]), (".binning/project.db", b"database bytes".to_vec()), (".binning/project.db-journal", vec![]), (".binning/thumbs/1.jpg", vec![7u8; 512])] {
        let path = format!("{}/{file}", a.path);
        std::fs::create_dir_all(parent(&path)).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let before = snapshot(&a.path);
    assert_eq!(before.len(), 4);
    w.registry.set_current(&a).unwrap();
    w.registry.remove(&a.path).unwrap().unwrap();
    assert_eq!(snapshot(&a.path), before);
    let b = w.create("B");
    std::fs::remove_dir_all(&b.path).unwrap();
    assert_eq!(w.registry.remove(&b.path).unwrap().map(|r| r.index), Some(0));
    assert!(!Path::new(&b.path).exists());
}

// ---------------------------------------------------------------------------------------------
// RegistrarTest
// ---------------------------------------------------------------------------------------------

/// This Project (open) and La Jolla Trip, both listed, and a camera folder outside them.
struct Media {
    w: World,
    this: ProjectRef,
    other: ProjectRef,
    db: ProjectDb,
    other_db: ProjectDb,
    camera: String,
}

fn media() -> Media {
    let w = world();
    let (this, other) = (w.create("This Project"), w.create("La Jolla Trip"));
    let db = open_database(&this.path, &this.name, false).unwrap();
    let other_db = open_database(&other.path, &other.name, false).unwrap();
    let camera = format!("{}/media", w.root);
    Media { w, this, other, db, other_db, camera }
}

impl Media {
    /// A picture whose bytes depend on `shade`, so two shades are two different files.
    fn picture(&self, path: &str, shade: u8) -> String {
        std::fs::create_dir_all(parent(path)).unwrap();
        image::RgbImage::from_pixel(8, 6, image::Rgb([shade, 10, 10])).save(path).unwrap();
        path.to_string()
    }

    fn cam(&self, name: &str, shade: u8) -> String {
        self.picture(&format!("{}/{name}", self.camera), shade)
    }

    fn add(&self, paths: &[String]) -> RegistrationSummary {
        register(&self.this, &self.db, &self.w.registry, &Ffmpeg::default(), paths, &|_, _| true)
    }

    fn names(&self) -> Vec<String> {
        self.db.items().unwrap().into_iter().map(|i| i.original_name).collect()
    }
}

#[test]
fn duplicate_bytes_in_the_project_are_skipped() {
    let m = media();
    let (first, copy) = (m.cam("a.png", 1), m.cam("copy-of-a.png", 1));
    let s = m.add(&[first, copy]);
    assert_eq!((s.added, s.duplicates), (1, 1));
    assert_eq!(m.names(), vec!["a.png"]);
}

/// A known path is a duplicate before the file is read: it counts even when the file has gone.
#[test]
fn duplicate_path_is_skipped_without_reading_the_file() {
    let m = media();
    let a = m.cam("a.png", 1);
    assert_eq!(m.add(&[a.clone()]).added, 1);
    std::fs::remove_file(&a).unwrap();
    let s = m.add(&[a]);
    assert_eq!((s.added, s.duplicates, s.unreadable), (0, 1, 0));
}

#[test]
fn a_file_inside_another_projects_folder_is_held_naming_that_project() {
    let m = media();
    // Not even a readable picture: it is refused by where it is, before it is read.
    let clip = format!("{}/clip.mp4", m.other.path);
    std::fs::write(&clip, b"").unwrap();
    let s = m.add(&[clip.clone()]);
    assert_eq!(s.held, vec![HeldFile { path: clip.clone(), project: "La Jolla Trip".into(), listed: true }]);
    assert_eq!((s.added, s.unreadable), (0, 0));
    assert!(m.names().is_empty());
    assert_eq!(s.text, "1 in another project (La Jolla Trip)");
}

#[test]
fn a_file_inside_this_projects_folder_is_accepted() {
    let m = media();
    let mine = m.picture(&format!("{}/Imports/mine.png", m.this.path), 1);
    assert_eq!(m.add(&[mine]).added, 1);
}

#[test]
fn a_file_another_listed_project_holds_is_held_and_nothing_is_registered_until_copied() {
    let m = media();
    let (a, b) = (m.cam("a.png", 1), m.cam("b.png", 2));
    // La Jolla Trip takes a.png first.
    register(&m.other, &m.other_db, &m.w.registry, &Ffmpeg::default(), &[a.clone()], &|_, _| true);
    let s = m.add(&[a.clone(), b]);
    assert_eq!(s.held, vec![HeldFile { path: a.clone(), project: "La Jolla Trip".into(), listed: true }]);
    assert_eq!(m.names(), vec!["b.png"]);
    // The same bytes under another name are held too: files are known by content.
    let twin = m.cam("renamed.png", 1);
    assert_eq!(m.add(&[twin]).held.len(), 1);
}

#[test]
fn a_duplicate_in_this_project_wins_over_another_project_holding_it() {
    let m = media();
    let a = m.cam("a.png", 1);
    assert_eq!(m.add(&[a.clone()]).added, 1);
    // Both projects end up knowing these bytes (the other took a copy of them).
    let copy = m.cam("copy.png", 1);
    register(&m.other, &m.other_db, &m.w.registry, &Ffmpeg::default(), &[copy.clone()], &|_, _| true);
    let s = m.add(&[m.cam("third.png", 1)]);
    assert_eq!((s.duplicates, s.held.len()), (1, 0));
}

#[test]
fn an_unreadable_file_is_reported_and_the_others_are_still_added() {
    let m = media();
    let (a, c) = (m.cam("a.png", 1), m.cam("c.png", 3));
    let bad = format!("{}/bad.jpg", m.camera);
    let s = m.add(&[a, bad, c, format!("{}/notes.txt", m.camera)]);
    assert_eq!((s.added, s.unreadable), (2, 2));
    assert_eq!(s.text, "2 added · 2 couldn't be read");
    assert_eq!(m.names(), vec!["a.png", "c.png"]);
}

#[test]
fn progress_is_reported_for_every_file_and_can_stop_the_run() {
    let m = media();
    let files: Vec<String> = (1..=30).map(|i| m.cam(&format!("f{i}.png"), i)).collect();
    let seen = std::cell::RefCell::new(Vec::new());
    let s = register(&m.this, &m.db, &m.w.registry, &Ffmpeg::default(), &files, &|done, total| {
        seen.borrow_mut().push((done, total));
        done < 20
    });
    assert_eq!(*seen.borrow(), (1..=20).map(|d| (d, 30)).collect::<Vec<_>>());
    assert_eq!((s.added, m.names().len()), (20, 20));
    // An empty list reports nothing.
    let none = register(&m.this, &m.db, &m.w.registry, &Ffmpeg::default(), &[], &|_, _| panic!("no progress for no files"));
    assert_eq!(none.text, "No photos or videos found");
}

#[test]
fn added_items_are_pending_with_what_the_file_says_about_itself() {
    let m = media();
    let photo = m.cam("IMG_1.png", 1);
    m.add(&[photo.clone()]);
    let item = &m.db.items().unwrap()[0];
    assert_eq!((item.status, item.kind), (ItemStatus::Pending, ItemKind::Photo));
    assert_eq!(item.content_hash, files::sha256(&photo).unwrap());
    assert_eq!((item.original_path.as_str(), item.current_path.as_str(), item.original_name.as_str()), (photo.as_str(), photo.as_str(), "IMG_1.png"));
    assert_eq!((item.width, item.height), (Some(8), Some(6)));
    assert_eq!((item.category_id, item.friendly_name.clone(), item.attempts), (None, None, 0));
    assert!(item.added_at > 0);
}

/// A project taken off the list still owns its folder: found by its database, not the list.
#[test]
fn a_file_in_an_unlisted_projects_folder_is_held() {
    let m = media();
    let old = m.w.create("Old Trip");
    drop(open_database(&old.path, "Old Trip", false).unwrap());
    m.w.registry.remove(&old.path).unwrap();
    let file = m.picture(&format!("{}/Beach/a.png", old.path), 1);
    let s = m.add(&[file.clone()]);
    assert_eq!(s.held, vec![HeldFile { path: file, project: "Old Trip".into(), listed: false }]);
    assert!(m.names().is_empty());
}

/// A copy has the same bytes as the other project's file, but is no file of that project: once
/// copied into this project it is an ordinary item, and the original stays where it was.
#[test]
fn a_copy_of_a_held_file_is_an_ordinary_item() {
    let m = media();
    let a = m.cam("a.png", 1);
    register(&m.other, &m.other_db, &m.w.registry, &Ffmpeg::default(), &[a.clone()], &|_, _| true);
    let held = m.add(&[a.clone()]).held;
    let result = copy_shared(&m.this.path, &m.db, &Ffmpeg::default(), &held, &|_, _| {});
    assert_eq!((result.copied, result.failed), (1, 0));
    assert_eq!(result.folders, vec!["Copied from La Jolla Trip"]);
    let copy = format!("{}/Copied from La Jolla Trip/a.png", m.this.path);
    let item = &m.db.items().unwrap()[0];
    assert_eq!((item.current_path.as_str(), item.original_path.as_str(), item.status), (copy.as_str(), copy.as_str(), ItemStatus::Pending));
    assert_eq!(files::sha256(&copy).unwrap(), files::sha256(&a).unwrap());
    // Copying it again adds nothing: this project holds those bytes now.
    let again = copy_shared(&m.this.path, &m.db, &Ffmpeg::default(), &held, &|_, _| {});
    assert_eq!(again.copied, 0);
    assert_eq!(m.names().len(), 1);
}
