//! Path handling. Every path the app stores or compares uses forward slashes, with no trailing
//! slash, so the move planner's string rules are the same on Windows as on Android. Windows file
//! APIs accept forward slashes; paths are shown to people with backslashes ([`display`]).

use std::path::{Path, PathBuf};

/// Normalizes a path from the OS or the user: backslashes become slashes, the `\\?\` prefix goes,
/// and a trailing slash is dropped (a drive root keeps its slash: `C:/`).
pub fn norm(p: impl AsRef<Path>) -> String {
    let s = p.as_ref().to_string_lossy().replace('\\', "/");
    let s = if let Some(rest) = s.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = s.strip_prefix("//?/") {
        rest.to_string()
    } else {
        s
    };
    let trimmed = s.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return format!("{trimmed}/");
    }
    trimmed.to_string()
}

/// The canonical form of `path` (symlinks, `..` and Windows short names resolved), normalized.
/// A path that does not exist yet is resolved through its nearest existing ancestor, so a new
/// folder compares equal to its parent's spelling.
pub fn canonical(path: &str) -> String {
    if let Ok(p) = std::fs::canonicalize(path) {
        return norm(p);
    }
    let absolute = norm(std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path)));
    let mut head = absolute.as_str();
    loop {
        let up = parent(head);
        if up == head || up.is_empty() {
            return absolute;
        }
        // A drive root keeps its slash, or `C:` would mean the drive's current folder.
        let probe = if up.ends_with(':') { format!("{up}/") } else { up.to_string() };
        if let Ok(p) = std::fs::canonicalize(&probe) {
            return format!("{}/{}", norm(p).trim_end_matches('/'), &absolute[up.len()..].trim_start_matches('/'));
        }
        head = up;
    }
}

/// Case-insensitive comparison key: Windows file systems ignore case.
pub fn key(path: &str) -> String {
    path.to_lowercase()
}

pub fn same_path(a: &str, b: &str) -> bool {
    a == b || key(&canonical(a)) == key(&canonical(b))
}

/// Whether `path` lies inside `ancestor` (whole path segments only, any case).
pub fn is_under(path: &str, ancestor: &str) -> bool {
    let a = format!("{}/", key(ancestor).trim_end_matches('/'));
    key(path).starts_with(&a)
}

/// The part after the last slash.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The part before the last slash (the path itself when it has none).
pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => path,
    }
}

/// The extension without its dot, or "" when the name has none.
pub fn extension(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

/// How a path is shown to people on Windows.
pub fn display(path: &str) -> String {
    if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(norm(r"C:\Users\me\Pictures\"), "C:/Users/me/Pictures");
        assert_eq!(norm(r"\\?\C:\a\b"), "C:/a/b");
        assert_eq!(norm(r"\\?\UNC\server\share\x"), "//server/share/x");
        assert_eq!(norm("C:\\"), "C:/");
        assert_eq!(norm("/"), "/");
        assert!(is_under("C:/A/b/c.jpg", "c:/a"));
        assert!(!is_under("C:/Ab/c.jpg", "C:/A"));
        assert_eq!(file_name("a/b/c.JPG"), "c.JPG");
        assert_eq!(parent("a/b/c.JPG"), "a/b");
        assert_eq!(extension("a/b/c.tar.JPG"), "JPG");
        assert_eq!(extension("a/b/.hidden"), "");
        let d = tempfile::tempdir().unwrap();
        let real = canonical(&norm(d.path()));
        assert_eq!(canonical(&format!("{}/not/there.jpg", norm(d.path()))), format!("{real}/not/there.jpg"));
    }
}
