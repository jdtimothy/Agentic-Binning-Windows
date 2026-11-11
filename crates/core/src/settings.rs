//! App settings (a JSON file in the app's data folder) and the Claude API key.

use crate::pipeline::SortChoice;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

/// Which backend analyzes and sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Backend {
    /// GenieX when it is installed, otherwise the simulated models.
    #[default]
    Auto,
    GenieX,
    Simulated,
}

/// The frame counts a clip may be capped at.
pub const FRAME_CAP_OPTIONS: [u32; 5] = [16, 24, 32, 48, 64];

/// The nearest of [`FRAME_CAP_OPTIONS`]; a tie resolves to the lower one.
pub fn snap_frame_cap(n: u32) -> u32 {
    *FRAME_CAP_OPTIONS.iter().min_by_key(|o| (o.abs_diff(n), **o)).unwrap()
}

fn default_frames() -> u32 {
    FRAME_CAP_OPTIONS[0]
}

fn default_sorting() -> SortChoice {
    SortChoice::Lfm3
}

fn default_compute() -> String {
    "npu".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default)]
    pub theme_mode: ThemeMode,
    /// Keeps the computer awake while a run is in progress.
    #[serde(default)]
    pub keep_awake: bool,
    /// Offline-capable default; switches to Claude when a key is first saved.
    #[serde(default = "default_sorting")]
    pub sorting_model: SortChoice,
    #[serde(default = "default_frames")]
    pub max_frames_per_clip: u32,
    /// True once the user picked a sorting model themselves; saving a key then no longer changes it.
    #[serde(default)]
    pub sorting_model_chosen_by_user: bool,
    #[serde(default)]
    pub backend: Backend,
    /// Where `geniex` is, when it is not on the PATH.
    #[serde(default)]
    pub geniex_path: String,
    /// Where `ffmpeg` is, when it is not on the PATH.
    #[serde(default)]
    pub ffmpeg_path: String,
    /// `npu`, `gpu` or `cpu`, passed to `geniex infer --compute`.
    #[serde(default = "default_compute")]
    pub compute: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("defaults")
    }
}

impl AppSettings {
    fn sanitized(mut self) -> Self {
        self.max_frames_per_clip = snap_frame_cap(self.max_frames_per_clip);
        if !["npu", "gpu", "cpu"].contains(&self.compute.as_str()) {
            self.compute = default_compute();
        }
        self
    }
}

/// Reads and writes the settings file. An unreadable file reads as the defaults.
pub struct SettingsStore {
    file: PathBuf,
}

impl SettingsStore {
    pub fn new(file: PathBuf) -> Self {
        Self { file }
    }

    pub fn load(&self) -> AppSettings {
        std::fs::read_to_string(&self.file).ok().and_then(|t| serde_json::from_str::<AppSettings>(&t).ok()).unwrap_or_default().sanitized()
    }

    pub fn save(&self, settings: &AppSettings) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&settings.clone().sanitized())?)?;
        std::fs::rename(tmp, &self.file)
    }
}

const KEY_SERVICE: &str = "Agentic Binning";
const KEY_ACCOUNT: &str = "claude-api-key";

/// Where the Claude API key is kept: the Windows Credential Manager, which encrypts it for the
/// signed-in user. Elsewhere (development builds) a file only the user can read.
pub struct ApiKeyStore {
    #[allow(dead_code)]
    fallback_file: PathBuf,
}

impl ApiKeyStore {
    pub fn new(data_dir: &Path) -> Self {
        Self { fallback_file: data_dir.join("claude-api-key") }
    }

    #[cfg(windows)]
    pub fn get(&self) -> Option<String> {
        keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT).ok()?.get_password().ok().filter(|k| !k.trim().is_empty())
    }

    #[cfg(windows)]
    pub fn set(&self, key: &str) -> Result<(), String> {
        keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT).and_then(|e| e.set_password(key.trim())).map_err(|e| format!("Couldn't save the key: {e}"))
    }

    #[cfg(windows)]
    pub fn clear(&self) -> Result<(), String> {
        match keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("Couldn't remove the key: {e}")),
        }
    }

    #[cfg(not(windows))]
    pub fn get(&self) -> Option<String> {
        let _ = (KEY_SERVICE, KEY_ACCOUNT);
        std::fs::read_to_string(&self.fallback_file).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
    }

    #[cfg(not(windows))]
    pub fn set(&self, key: &str) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        if let Some(dir) = self.fallback_file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&self.fallback_file).map_err(|e| e.to_string())?;
        f.write_all(key.trim().as_bytes()).map_err(|e| e.to_string())
    }

    #[cfg(not(windows))]
    pub fn clear(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.fallback_file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The key with all but its last four characters hidden, for Settings.
    pub fn masked(&self) -> Option<String> {
        self.get().map(|k| {
            let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            format!("••••{tail}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_defaults() {
        let d = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(d.path().join("settings.json"));
        let s = store.load();
        assert_eq!((s.sorting_model, s.max_frames_per_clip, s.compute.as_str(), s.backend), (SortChoice::Lfm3, 16, "npu", Backend::Auto));
        let mut changed = s.clone();
        changed.max_frames_per_clip = 30;
        changed.sorting_model = SortChoice::Claude;
        changed.theme_mode = ThemeMode::Light;
        store.save(&changed).unwrap();
        let back = store.load();
        assert_eq!((back.max_frames_per_clip, back.sorting_model, back.theme_mode), (32, SortChoice::Claude, ThemeMode::Light));
        std::fs::write(d.path().join("settings.json"), "garbage").unwrap();
        assert_eq!(store.load(), AppSettings::default());
        assert_eq!(snap_frame_cap(20), 16);
        assert_eq!(snap_frame_cap(1000), 64);
        assert!(serde_json::to_string(&changed).unwrap().contains("\"sortingModel\":\"Claude\""));
    }

    #[cfg(not(windows))]
    #[test]
    fn api_key_store() {
        let d = tempfile::tempdir().unwrap();
        let keys = ApiKeyStore::new(d.path());
        assert!(keys.get().is_none());
        keys.set(" sk-ant-test-1234 ").unwrap();
        assert_eq!(keys.get().as_deref(), Some("sk-ant-test-1234"));
        assert_eq!(keys.masked().as_deref(), Some("••••1234"));
        keys.clear().unwrap();
        assert!(keys.masked().is_none());
    }
}
