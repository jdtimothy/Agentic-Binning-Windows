//! The file system as the move engine sees it. Nothing here ever replaces an existing file.

use crate::paths;
use sha2::{Digest, Sha256};
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
/// exists. A hard link is tried first because it can never replace a file; file systems without
/// hard links (exFAT, FAT32, some network shares) fall back to a checked rename.
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
    match fs::hard_link(from, to) {
        Ok(()) => match fs::remove_file(from) {
            Ok(()) => Ok(true),
            Err(e) => {
                // The source is held open by another program: undo, so there is still one file.
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
    let mut digest = Sha256::new();
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
    Ok(hex(&digest.finalize()))
}

/// SHA-256 of the file's bytes as lowercase hex.
pub fn sha256(path: &str) -> io::Result<String> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(hex(&digest.finalize()))
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
