//! Installing GenieX and FFmpeg when they are missing, with Windows Package Manager (`winget`).
//!
//! The app never installs anything by itself: a person presses Install in Settings, next to the
//! license of what will be installed, and only then is `winget install` run. Nothing is bundled
//! or redistributed with the app; winget downloads each program from its publisher's own release,
//! as typing the same command in a terminal would. The command is in the activity log.
//!
//! - GenieX: `Qualcomm.GenieX.CLI`, published by Qualcomm. BSD-3-Clause; the QNN runtime inside
//!   it is under the Qualcomm AI Runtime SDK EULA. Its installer puts it under %LOCALAPPDATA%.
//! - FFmpeg: `BtbN.FFmpeg.LGPL`, which has a Windows ARM64 build. LGPL; the app only runs it.
//!
//! winget adds the program's folder to the user's PATH in the registry, which a running app
//! doesn't see in its own environment: [`registry_path_dirs`] reads it again, so the program is
//! found without restarting the app.

use crate::media::{find_program, quiet_command};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Stdio;

/// Something the app can install with winget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Package {
    /// What the window and the log call it.
    pub name: &'static str,
    pub winget_id: &'static str,
}

pub const GENIEX: Package = Package { name: "GenieX", winget_id: "Qualcomm.GenieX.CLI" };
pub const FFMPEG: Package = Package { name: "FFmpeg", winget_id: "BtbN.FFmpeg.LGPL" };

/// The package named by `id` (`geniex` or `ffmpeg`), as the window sends it.
pub fn package(id: &str) -> Option<Package> {
    match id {
        "geniex" => Some(GENIEX),
        "ffmpeg" => Some(FFMPEG),
        _ => None,
    }
}

/// Where `winget` is, if this computer has it (Windows 11 ships it as App Installer).
pub fn winget() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let mut extra = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        extra.push(PathBuf::from(local).join("Microsoft").join("WindowsApps"));
    }
    find_program("winget", None, &extra)
}

/// The arguments for installing `p`. Agreeing to the package's terms is passed on because the
/// person already agreed in the window, where the terms are linked; winget can't ask, as it runs
/// without a console.
pub fn install_args(p: &Package) -> Vec<&'static str> {
    vec![
        "install",
        "--id",
        p.winget_id,
        "--exact",
        "--source",
        "winget",
        "--silent",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ]
}

/// Runs `winget install` for `p`, handing each line it prints to `on_line`. Returns an error
/// sentence when it did not install. "Already installed" counts as installed.
pub fn run(p: &Package, on_line: &dyn Fn(&str)) -> Result<(), String> {
    let Some(winget) = winget() else {
        return Err("Windows Package Manager (winget) isn't on this computer. Install App Installer from the Microsoft Store, or install the program by hand".into());
    };
    let args = install_args(p);
    on_line(&format!("Running: winget {}", args.join(" ")));
    let mut child = quiet_command(&winget).args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("winget couldn't start: {e}"))?;
    let stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || stderr.map(|s| BufReader::new(s).lines().map_while(Result::ok).collect::<Vec<_>>()).unwrap_or_default());
    let mut already = false;
    let mut last = String::new();
    if let Some(out) = child.stdout.take() {
        // winget draws progress with carriage returns; keep only lines with words in them.
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            for part in line.split('\r') {
                let part = part.trim();
                if part.is_empty() || !part.chars().any(char::is_alphabetic) {
                    continue;
                }
                let lower = part.to_lowercase();
                already |= lower.contains("already installed") || lower.contains("no newer package");
                on_line(part);
                last = part.to_string();
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    for line in err_thread.join().unwrap_or_default() {
        if !line.trim().is_empty() {
            on_line(line.trim());
            last = line.trim().to_string();
        }
    }
    if status.success() || already {
        Ok(())
    } else {
        Err(if last.is_empty() { format!("winget ended with code {}", status.code().unwrap_or(-1)) } else { last })
    }
}

/// The folders on the user's and the machine's PATH as the registry holds them now, which
/// includes what an installer added after the app started. Empty off Windows.
pub fn registry_path_dirs() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let keys = [r"HKCU\Environment", r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment"];
    let mut dirs = Vec::new();
    for key in keys {
        let Ok(out) = quiet_command("reg").args(["query", key, "/v", "Path"]).output() else { continue };
        if let Some(value) = parse_reg_path(&String::from_utf8_lossy(&out.stdout)) {
            dirs.extend(value.split(';').map(str::trim).filter(|d| !d.is_empty()).map(|d| PathBuf::from(expand_env(d))));
        }
    }
    dirs
}

/// The value of `Path` in what `reg query <key> /v Path` prints.
fn parse_reg_path(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("Path").or_else(|| line.strip_prefix("PATH"))?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("REG_EXPAND_SZ").or_else(|| rest.strip_prefix("REG_SZ"))?;
        Some(rest.trim().to_string())
    })
}

/// `%NAME%` replaced by the variable's value; unknown names are left as they are.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_are_named_by_what_the_window_sends() {
        assert_eq!(package("geniex"), Some(GENIEX));
        assert_eq!(package("ffmpeg"), Some(FFMPEG));
        assert_eq!(package("anything else"), None);
    }

    /// Installs only what was asked for, from the winget source, and never waits
    /// on a prompt nobody can see.
    #[test]
    fn the_install_command_is_exact_and_needs_no_console() {
        let args = install_args(&GENIEX).join(" ");
        assert_eq!(args, "install --id Qualcomm.GenieX.CLI --exact --source winget --silent --accept-package-agreements --accept-source-agreements --disable-interactivity");
        assert!(install_args(&FFMPEG).contains(&"BtbN.FFmpeg.LGPL"));
    }

    #[test]
    fn the_path_is_read_from_what_reg_prints() {
        let printed = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    %USERPROFILE%\\bin;C:\\Tools\r\n\r\n";
        assert_eq!(parse_reg_path(printed).as_deref(), Some("%USERPROFILE%\\bin;C:\\Tools"));
        assert_eq!(parse_reg_path("    Path    REG_SZ    C:\\A").as_deref(), Some("C:\\A"));
        assert_eq!(parse_reg_path("ERROR: The system was unable to find the specified registry key or value."), None);
    }

    #[test]
    fn variables_in_the_path_are_expanded() {
        std::env::set_var("AB_INSTALL_TEST", "C:\\Here");
        assert_eq!(expand_env("%AB_INSTALL_TEST%\\bin"), "C:\\Here\\bin");
        assert_eq!(expand_env("%AB_NOT_SET_ANYWHERE%\\bin"), "%AB_NOT_SET_ANYWHERE%\\bin");
        assert_eq!(expand_env("50% off"), "50% off");
        assert_eq!(expand_env("plain"), "plain");
    }
}
