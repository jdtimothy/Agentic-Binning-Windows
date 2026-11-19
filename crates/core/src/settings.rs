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

/// A stored choice this build doesn't know (a model that was offered once and dropped, a value
/// from a newer build) reads as the default for that one setting; the other settings are kept.
fn or_default<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn sorting_or_default<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SortChoice, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|_| default_sorting()))
}

fn default_compute() -> String {
    "npu".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default, deserialize_with = "or_default")]
    pub theme_mode: ThemeMode,
    /// Keeps the computer awake while a run is in progress.
    #[serde(default)]
    pub keep_awake: bool,
    /// Offline-capable default; switches to Claude when a key is first saved.
    #[serde(default = "default_sorting", deserialize_with = "sorting_or_default")]
    pub sorting_model: SortChoice,
    #[serde(default = "default_frames")]
    pub max_frames_per_clip: u32,
    /// True once the user picked a sorting model themselves; saving a key then no longer changes it.
    #[serde(default)]
    pub sorting_model_chosen_by_user: bool,
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
    /// Saving a Claude key switches sorting to Claude, unless the user already chose a model.
    pub fn after_key_saved(mut self) -> Self {
        if !self.sorting_model_chosen_by_user {
            self.sorting_model = SortChoice::Claude;
        }
        self
    }

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
        if key.trim().is_empty() {
            return Err("Paste the key first".into());
        }
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
        if key.trim().is_empty() {
            return Err("Paste the key first".into());
        }
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
        assert_eq!((s.sorting_model, s.max_frames_per_clip, s.compute.as_str()), (SortChoice::Lfm3, 16, "npu"));
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

    // The Android app's `SettingsStoreTest` and the parts of `ApiKeyStoreTest` that are not
    // about Android's Keystore. The key store itself is only exercised off Windows: on Windows
    // it is the signed-in user's real Credential Manager, which a test must never write to.

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let d = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(d.path().join("settings.json"));
        (d, store)
    }

    #[test]
    fn defaults_are_lfm_and_16() {
        let (_d, store) = store();
        let s = store.load();
        assert_eq!(s, AppSettings::default());
        assert_eq!((s.sorting_model, s.max_frames_per_clip, s.sorting_model_chosen_by_user, s.keep_awake, s.theme_mode), (SortChoice::Lfm3, 16, false, false, ThemeMode::System));
    }

    #[test]
    fn frame_cap_snaps_to_options() {
        // 20 and 40 are ties, which snap down.
        for (asked, stored) in [(18, 16), (44, 48), (100, 64), (8, 16), (20, 16), (40, 32), (24, 24)] {
            let (_d, store) = store();
            store.save(&AppSettings { max_frames_per_clip: asked, ..Default::default() }).unwrap();
            assert_eq!(store.load().max_frames_per_clip, stored, "{asked}");
        }
    }

    #[test]
    fn a_saved_key_switches_to_claude_only_when_the_user_has_not_chosen() {
        let fresh = AppSettings::default().after_key_saved();
        assert_eq!((fresh.sorting_model, fresh.sorting_model_chosen_by_user), (SortChoice::Claude, false));
        let chosen = AppSettings { sorting_model_chosen_by_user: true, ..Default::default() }.after_key_saved();
        assert_eq!(chosen.sorting_model, SortChoice::Lfm3);
    }

    #[test]
    fn sorting_choices_are_claude_and_lfm_only() {
        assert_eq!(serde_json::to_string(&[SortChoice::Claude, SortChoice::Lfm3]).unwrap(), r#"["Claude","Lfm3_1_7B"]"#);
    }

    /// A choice that was offered once and dropped falls back by itself; the rest is kept.
    #[test]
    fn an_unknown_stored_choice_falls_back_without_losing_the_other_settings() {
        let (d, store) = store();
        std::fs::write(d.path().join("settings.json"), r#"{"sortingModel":"Qwen3_4B","themeMode":"Sepia","backend":"Cloud","maxFramesPerClip":48,"keepAwake":true,"compute":"quantum"}"#).unwrap();
        let s = store.load();
        // "backend" was a setting until GenieX became the only model; a stored one is ignored.
        assert_eq!((s.sorting_model, s.theme_mode), (SortChoice::Lfm3, ThemeMode::System));
        assert_eq!((s.max_frames_per_clip, s.keep_awake, s.compute.as_str()), (48, true, "npu"));
        // A value of the wrong kind too.
        std::fs::write(d.path().join("settings.json"), r#"{"sortingModel":7,"themeMode":"Dark"}"#).unwrap();
        assert_eq!((store.load().sorting_model, store.load().theme_mode), (SortChoice::Lfm3, ThemeMode::Dark));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_blank_key_is_rejected_and_a_key_is_trimmed() {
        let d = tempfile::tempdir().unwrap();
        let keys = ApiKeyStore::new(d.path());
        assert!(keys.set("   ").is_err());
        assert_eq!(keys.get(), None);
        keys.set("  sk-ant-fake-1234  ").unwrap();
        assert_eq!(keys.get().as_deref(), Some("sk-ant-fake-1234"));
        keys.clear().unwrap();
        keys.clear().unwrap();
        assert_eq!(keys.get(), None);
    }
}
