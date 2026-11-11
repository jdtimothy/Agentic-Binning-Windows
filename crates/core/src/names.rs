//! Making model-written or user-typed text safe as a file or folder name.

/// Characters Windows refuses in a file name (Android's list is the same).
const ILLEGAL: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

fn is_illegal(c: char) -> bool {
    ILLEGAL.contains(&c) || (c as u32) < 0x20
}

fn trim_end_dots(s: &str) -> &str {
    s.trim_end_matches(|c: char| c == '.' || c.is_whitespace())
}

/// Takes the first `max` characters (not bytes).
pub fn take_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Makes a model-written name safe as a file or folder name: illegal characters become spaces,
/// runs of whitespace collapse, and the result is cut at `max` chars with no trailing dot or space.
/// Shared by suggested names (60) and category names (40).
pub fn clean_name(name: &str, max: usize) -> String {
    let replaced: String = name.chars().map(|c| if is_illegal(c) { ' ' } else { c }).collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = trim_end_dots(collapsed.trim());
    trim_end_dots(take_chars(cleaned, max)).to_string()
}

/// Names Windows reserves whatever the extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// True for `CON`, `nul.txt` and the like, which Windows cannot create.
pub fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim();
    RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem))
}

/// A folder or file name that Windows accepts: a reserved device name gets a trailing underscore.
pub fn windows_safe(name: &str) -> String {
    if is_reserved_windows_name(name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// Turns a user-typed project name into a safe folder name (at most 80 chars).
pub fn project_folder_name(project_name: &str) -> String {
    let replaced: String = project_name.chars().map(|c| if is_illegal(c) { '_' } else { c }).collect();
    let trimmed = replaced.trim_matches(|c| c == ' ' || c == '.');
    let cut = take_chars(trimmed, 80).trim_end_matches([' ', '.']);
    windows_safe(cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_names() {
        assert_eq!(clean_name("  Sea: turtle / swimming.  ", 60), "Sea turtle swimming");
        assert_eq!(clean_name("a\tb\n\nc", 60), "a b c");
        assert_eq!(clean_name("abcdef. ghi", 7), "abcdef");
        assert_eq!(clean_name("???", 60), "");
    }

    #[test]
    fn project_folder_names() {
        assert_eq!(project_folder_name(" Maui: 2026. "), "Maui_ 2026");
        assert_eq!(project_folder_name("con"), "con_");
        assert_eq!(project_folder_name("..."), "");
    }
}
