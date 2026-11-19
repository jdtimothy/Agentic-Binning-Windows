//! The file system as the move engine sees it. Nothing here ever replaces an existing file.

use crate::paths;
use ring::digest::{Context, SHA256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

const BUFFER: usize = 1 << 20;

/// The storage ran out during a copy: the half copy must go and the run stop.
pub fn is_no_space(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(112) | Some(39)))
        || (cfg!(unix) && e.raw_os_error() == Some(28))
}

/// A rename that crosses volumes: the engine copies instead.
pub fn is_cross_device(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::CrossesDevices || (cfg!(windows) && e.raw_os_error() == Some(17)) || (cfg!(unix) && e.raw_os_error() == Some(18))
}

pub fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

pub fn is_file(path: &str) -> bool {
    Path::new(path).is_file()
}

pub fn size(path: &str) -> i64 {
    fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0)
}

/// Milliseconds since the epoch, or 0 when unknown.
pub fn last_modified(path: &str) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn set_last_modified(path: &str, time_ms: i64) {
    if time_ms <= 0 {
        return;
    }
    if let Ok(f) = File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(time_ms as u64));
    }
}

/// The nearest existing ancestor of `path` (or the path itself).
fn existing(path: &str) -> String {
    let mut cur = path.to_string();
    while !Path::new(&cur).exists() {
        let p = paths::parent(&cur).to_string();
        if p == cur || p.is_empty() {
            break;
        }
        cur = p;
    }
    cur
}

/// A name for the volume holding `path`: the drive or share on Windows, the device id elsewhere.
fn volume_of(path: &str) -> Option<String> {
    let e = existing(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&e).ok().map(|m| m.dev().to_string())
    }
    #[cfg(not(unix))]
    {
        let full = paths::canonical(&e).to_lowercase();
        if let Some(rest) = full.strip_prefix("//") {
            let mut parts = rest.split('/');
            return Some(format!("//{}/{}", parts.next()?, parts.next()?));
        }
        full.split(':').next().filter(|d| d.len() == 1).map(|d| d.to_string())
    }
}

/// Whether a rename from `a` to `b` stays on one storage volume; `b` may not exist yet.
pub fn same_volume(a: &str, b: &str) -> bool {
    #[cfg(test)]
    if faults::other_volume(a) != faults::other_volume(b) {
        return false;
    }
    match (volume_of(a), volume_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Moves `from` to `to` (folder and name) in one step; `Ok(false)`, touching nothing, if `to`
/// exists. An error leaves the file where it was.
///
/// On Windows this is one `MoveFileExW` call without the replace flag, which refuses an existing
/// target itself. Elsewhere a hard link is tried first because it can never replace a file; file
/// systems without hard links fall back to a checked rename.
pub fn rename_no_replace(from: &str, to: &str) -> io::Result<bool> {
    #[cfg(test)]
    if let Some(r) = faults::on_rename() {
        return r;
    }
    // A change of letter case only: the "target" is the file itself.
    if paths::key(from) == paths::key(to) && paths::parent(from) == paths::parent(to) {
        fs::rename(from, to)?;
        return Ok(true);
    }
    if Path::new(to).exists() {
        return Ok(false);
    }
    move_no_replace(from, to)
}

/// `path` as the Windows API takes it: backslashes, and the `\\?\` form once it is long, which
/// lifts the 260 character limit. Stored paths are absolute and hold no `.` or `..` parts, as that
/// form requires.
#[cfg(windows)]
fn api_path(path: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let back = path.replace('/', "\\");
    let long = if back.len() < 240 || back.starts_with("\\\\?\\") {
        back
    } else if let Some(share) = back.strip_prefix("\\\\") {
        format!("\\\\?\\UNC\\{share}")
    } else if back.as_bytes().get(1) == Some(&b':') {
        format!("\\\\?\\{back}")
    } else {
        back
    };
    std::ffi::OsStr::new(&long).encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn move_no_replace(from: &str, to: &str) -> io::Result<bool> {
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    const ERROR_FILE_EXISTS: i32 = 80;
    const ERROR_ALREADY_EXISTS: i32 = 183;
    let (from, to) = (api_path(from), api_path(to));
    // No flags: never replace a file, and never copy across volumes (that fails with error 17,
    // which the engine answers with its own checked copy).
    // SAFETY: both pointers are to NUL-terminated UTF-16 buffers that outlive the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(ERROR_FILE_EXISTS) | Some(ERROR_ALREADY_EXISTS) => Ok(false),
        _ => Err(e),
    }
}

#[cfg(not(windows))]
fn move_no_replace(from: &str, to: &str) -> io::Result<bool> {
    match fs::hard_link(from, to) {
        Ok(()) => match fs::remove_file(from) {
            Ok(()) => Ok(true),
            Err(e) => {
                // The source can't be removed: undo, so there is still one file.
                let _ = fs::remove_file(to);
                Err(e)
            }
        },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) if is_cross_device(&e) => Err(e),
        Err(_) => {
            if Path::new(to).exists() {
                return Ok(false);
            }
            fs::rename(from, to)?;
            Ok(true)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A SHA-256 in progress. `ring` asks Windows whether the processor has the ARMv8 hashing
/// instructions and uses them: on a Snapdragon X about 1,200 MB/s against the `sha2` crate's
/// 520, which can't detect them on Windows and so always hashes in software. Measured on a
/// 1.2 GB clip, Nov 2026. Hashing, not reading, was what made copying a large clip slow.
fn hasher() -> Context {
    Context::new(&SHA256)
}

/// Copies `from` to the new file `to` and returns the SHA-256 of the bytes read from `from`.
/// Fails if `to` exists. The copy is flushed to storage before returning, so the check that
/// follows reads what a reboot would.
pub fn copy_hashing(from: &str, to: &str) -> io::Result<String> {
    let mut input = File::open(from)?;
    let mut output = File::options().write(true).create_new(true).open(to)?;
    #[cfg(test)]
    if faults::take(|f| f.no_space_during_copy) {
        output.write_all(b"half")?;
        return Err(io::Error::from(io::ErrorKind::StorageFull));
    }
    let mut digest = hasher();
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
        output.write_all(&buf[..n])?;
    }
    #[cfg(test)]
    if faults::corrupt_this_copy() {
        output.write_all(b"!")?;
    }
    output.flush()?;
    output.sync_all()?;
    Ok(hex(digest.finish().as_ref()))
}

/// SHA-256 of the file's bytes as lowercase hex.
pub fn sha256(path: &str) -> io::Result<String> {
    let mut input = File::open(path)?;
    let mut digest = hasher();
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(hex(digest.finish().as_ref()))
}

/// Deletes a file (never a folder); true if it is gone.
pub fn delete(path: &str) -> bool {
    let p = Path::new(path);
    if p.is_dir() {
        return false;
    }
    fs::remove_file(p).is_ok() || !p.exists()
}

pub fn mkdirs(dir: &str) -> bool {
    Path::new(dir).is_dir() || fs::create_dir_all(dir).is_ok()
}

/// Free bytes on the volume holding `dir` (or its nearest existing parent).
pub fn usable_space(dir: &str) -> i64 {
    #[cfg(test)]
    if let Some(free) = faults::get(|f| f.free) {
        return free;
    }
    fs4::available_space(existing(dir)).map(|b| b.min(i64::MAX as u64) as i64).unwrap_or(i64::MAX)
}

/// Every file under `dir`, recursively, as normalized paths; empty if it doesn't exist.
pub fn list_files(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_string()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = format!("{}/{}", d.trim_end_matches('/'), e.file_name().to_string_lossy());
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => out.push(p),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Removes `dir` only if it is a folder with nothing at all inside; true if it did.
pub fn remove_empty_dir(dir: &str) -> bool {
    match fs::read_dir(dir) {
        Ok(mut it) => it.next().is_none() && fs::remove_dir(dir).is_ok(),
        Err(_) => false,
    }
}

/// Faults a test can switch on for its own thread, to reach the branches a healthy disk never
/// takes: another volume, a full disk, a copy that reads back wrong, a rename that fails.
#[cfg(test)]
pub mod faults {
    use std::cell::RefCell;
    use std::io;

    #[derive(Default)]
    pub struct Faults {
        /// Paths starting with this count as another volume.
        pub other_volume: Option<String>,
        /// What `usable_space` reports.
        pub free: Option<i64>,
        /// This many copies read back with one byte too many.
        pub corrupt_copies: usize,
        /// Copies made so far.
        pub copies: usize,
        /// The next copy runs out of space after writing part of the file.
        pub no_space_during_copy: bool,
        /// Renames report success and move nothing.
        pub rename_lies: bool,
        /// Renames are refused and touch nothing.
        pub rename_fails: bool,
    }

    thread_local! {
        static FAULTS: RefCell<Faults> = RefCell::new(Faults::default());
    }

    pub fn set(change: impl FnOnce(&mut Faults)) {
        FAULTS.with(|f| change(&mut f.borrow_mut()));
    }

    pub fn get<T>(read: impl FnOnce(&Faults) -> T) -> T {
        FAULTS.with(|f| read(&f.borrow()))
    }

    pub(super) fn take(read: impl FnOnce(&Faults) -> bool) -> bool {
        get(read)
    }

    pub(super) fn other_volume(path: &str) -> bool {
        get(|f| f.other_volume.as_deref().is_some_and(|v| path.starts_with(v)))
    }

    pub(super) fn corrupt_this_copy() -> bool {
        FAULTS.with(|f| {
            let mut f = f.borrow_mut();
            f.copies += 1;
            if f.corrupt_copies > 0 {
                f.corrupt_copies -= 1;
                true
            } else {
                false
            }
        })
    }

    pub(super) fn on_rename() -> Option<io::Result<bool>> {
        get(|f| {
            if f.rename_fails {
                Some(Err(io::Error::from(io::ErrorKind::PermissionDenied)))
            } else if f.rename_lies {
                Some(Ok(true))
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Projects store each file's hash, made with the `sha2` crate before the switch to `ring`.
    /// Both must give the same text for the same bytes, or every stored file would look changed.
    #[test]
    fn hashes_match_the_standard_and_what_projects_already_store() {
        use sha2::Digest;
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        let abc = format!("{root}/abc.txt");
        fs::write(&abc, b"abc").unwrap();
        assert_eq!(sha256(&abc).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let empty = format!("{root}/empty.txt");
        fs::write(&empty, b"").unwrap();
        assert_eq!(sha256(&empty).unwrap(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        // Several read buffers and an odd tail, through both the plain hash and the copy.
        let bytes: Vec<u8> = (0..(3 * BUFFER + 4_097)).map(|i| (i * 31 % 251) as u8).collect();
        let big = format!("{root}/big.bin");
        fs::write(&big, &bytes).unwrap();
        let old = hex(&sha2::Sha256::digest(&bytes));
        assert_eq!(sha256(&big).unwrap(), old);
        assert_eq!(copy_hashing(&big, &format!("{root}/big copy.bin")).unwrap(), old);
    }

    #[test]
    fn rename_never_replaces() {
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        let a = format!("{root}/a.txt");
        let b = format!("{root}/sub/b.txt");
        fs::write(&a, b"one").unwrap();
        assert!(mkdirs(&format!("{root}/sub")));
        fs::write(&b, b"two").unwrap();
        assert!(!rename_no_replace(&a, &b).unwrap());
        assert_eq!(fs::read(&b).unwrap(), b"two");
        let c = format!("{root}/sub/c.txt");
        assert!(rename_no_replace(&a, &c).unwrap());
        assert!(!exists(&a) && exists(&c));
        assert!(same_volume(&c, &format!("{root}/not/yet/there")));
    }

    #[test]
    fn copies_and_hashes() {
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        let a = format!("{root}/a.bin");
        fs::write(&a, b"hello").unwrap();
        let h = copy_hashing(&a, &format!("{root}/b.bin")).unwrap();
        assert_eq!(h, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        assert_eq!(sha256(&format!("{root}/b.bin")).unwrap(), h);
        assert!(copy_hashing(&a, &format!("{root}/b.bin")).is_err());
        assert_eq!(list_files(&root).len(), 2);
        assert!(!remove_empty_dir(&root));
        assert!(usable_space(&root) > 0);
    }
}

/// What only Windows does. These run in the Build workflow on GitHub's Windows ARM64 machine and
/// on the owner's PC; a Linux session can't run them.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    /// Other programs may read the file but not delete or rename it: how an editor or a player holds a file.
    const FILE_SHARE_READ: u32 = 1;

    fn root() -> (tempfile::TempDir, String) {
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        (d, root)
    }

    #[test]
    fn rename_keeps_size_time_and_content() {
        let (_d, root) = root();
        let (a, b) = (format!("{root}/camera/a.jpg"), format!("{root}/proj/Beach/Morning.jpg"));
        mkdirs(&format!("{root}/camera"));
        mkdirs(&format!("{root}/proj/Beach"));
        fs::write(&a, b"the picture").unwrap();
        set_last_modified(&a, 1_600_000_000_000);
        let hash = sha256(&a).unwrap();
        assert!(rename_no_replace(&a, &b).unwrap());
        assert!(!exists(&a));
        assert_eq!((size(&b), last_modified(&b), sha256(&b).unwrap()), (11, 1_600_000_000_000, hash));
    }

    #[test]
    fn rename_never_replaces_whatever_the_case_of_the_name() {
        let (_d, root) = root();
        let (a, b) = (format!("{root}/a.jpg"), format!("{root}/Sunset.jpg"));
        fs::write(&a, b"new").unwrap();
        fs::write(&b, b"kept").unwrap();
        for target in [b.clone(), format!("{root}/SUNSET.JPG")] {
            assert!(!rename_no_replace(&a, &target).unwrap(), "{target}");
            assert_eq!((fs::read(&a).unwrap(), fs::read(&b).unwrap()), (b"new".to_vec(), b"kept".to_vec()));
        }
        // NTFS keeps the name's case: the listing still shows the first spelling.
        assert!(list_files(&root).contains(&b));
    }

    #[test]
    fn a_change_of_case_only_renames_the_file_itself() {
        let (_d, root) = root();
        let (a, b) = (format!("{root}/sunset.JPG"), format!("{root}/Sunset.jpg"));
        fs::write(&a, b"x").unwrap();
        assert!(rename_no_replace(&a, &b).unwrap());
        assert_eq!(list_files(&root), vec![b]);
    }

    #[test]
    fn a_file_another_program_holds_is_not_moved_and_stays_whole() {
        let (_d, root) = root();
        let (a, b) = (format!("{root}/a.mp4"), format!("{root}/Clips/a.mp4"));
        mkdirs(&format!("{root}/Clips"));
        fs::write(&a, b"playing").unwrap();
        let held = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&a).unwrap();
        assert!(rename_no_replace(&a, &b).is_err());
        assert!(exists(&a) && !exists(&b), "one file, where it was");
        // It can still be copied: that is how a file another project holds is binned.
        let hash = copy_hashing(&a, &format!("{root}/copy.tmp")).unwrap();
        assert_eq!(sha256(&format!("{root}/copy.tmp")).unwrap(), hash);
        assert!(!delete(&a), "and a held original is never deleted");
        drop(held);
        assert!(rename_no_replace(&a, &b).unwrap());
        assert_eq!(fs::read(&b).unwrap(), b"playing");
    }

    #[test]
    fn paths_longer_than_260_characters_work() {
        let (_d, root) = root();
        let deep = format!("{root}/{}", ["A folder with a long name to make the path long"; 6].join("/"));
        assert!(deep.len() > 300);
        assert!(mkdirs(&deep));
        let (a, b, c) = (format!("{root}/a.jpg"), format!("{deep}/Sea Turtles Resting On The Sand.jpg"), format!("{deep}/Copy.jpg"));
        fs::write(&a, b"deep").unwrap();
        assert!(rename_no_replace(&a, &b).unwrap(), "from a short path to a long one");
        assert!(exists(&b) && is_file(&b));
        assert_eq!(size(&b), 4);
        let hash = copy_hashing(&b, &c).unwrap();
        assert_eq!(sha256(&c).unwrap(), hash);
        assert!(!rename_no_replace(&c, &b).unwrap(), "a long path is not replaced either");
        let d = format!("{deep}/Moved.jpg");
        assert!(rename_no_replace(&c, &d).unwrap(), "between two long paths");
        assert_eq!(list_files(&deep), vec![d.clone(), b.clone()]);
        assert!(same_volume(&a, &b));
        assert!(delete(&b) && delete(&d));
        assert!(remove_empty_dir(&deep));
    }

    #[test]
    fn a_copy_reads_back_the_same_and_can_carry_the_time() {
        let (_d, root) = root();
        let (a, b) = (format!("{root}/a.bin"), format!("{root}/b.bin"));
        fs::write(&a, vec![7u8; 3 * BUFFER + 11]).unwrap();
        let hash = copy_hashing(&a, &b).unwrap();
        assert_eq!((sha256(&b).unwrap(), size(&b)), (hash, (3 * BUFFER + 11) as i64));
        set_last_modified(&b, 1_500_000_000_000);
        assert_eq!(last_modified(&b), 1_500_000_000_000);
        assert!(copy_hashing(&a, &b).is_err(), "a copy never replaces a file");
    }

    #[test]
    fn names_windows_reserves_or_trims_are_made_safe() {
        let (_d, root) = root();
        for (typed, folder) in [("con", "con_"), ("NUL", "NUL_"), ("Trip. ", "Trip"), ("What? When: Now*", "What_ When_ Now_")] {
            let name = crate::names::project_folder_name(typed);
            assert_eq!(name, folder);
            let dir = format!("{root}/{name}");
            assert!(mkdirs(&dir), "{name}");
            fs::write(format!("{dir}/a.jpg"), b"x").unwrap();
            assert_eq!(list_files(&dir), vec![format!("{dir}/a.jpg")]);
        }
    }

    #[test]
    fn a_folder_that_does_not_exist_yet_is_on_its_parents_volume() {
        let (_d, root) = root();
        assert!(same_volume(&root, &format!("{root}/not/yet/there")));
        assert!(usable_space(&format!("{root}/not/yet/there")) > 0);
        assert_eq!(volume_of(&root).map(|v| v.len()), Some(1), "a drive letter");
    }
}
