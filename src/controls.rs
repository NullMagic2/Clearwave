//! Shared desktop options, persisted settings, and bounded live control messages.
//! Platform frontends share validation and storage without sharing native widgets.
use crate::{Error, Result, storage::bounded_read};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub const DESKTOP_EVENT_CAPACITY: usize = 64;
pub const UI_REFRESH_INTERVAL: Duration = Duration::from_millis(100);
pub const METER_STALE_AFTER: Duration = Duration::from_millis(600);
pub const MAX_CHILD_EVENT_BYTES: usize = 16 * 1024;
pub const MAX_PICKER_RESPONSE_BYTES: usize = 4095;
pub const DEFAULT_LATENCY_MS: u16 = 10;
pub const MIN_LATENCY_MS: u16 = 5;
pub const MAX_LATENCY_MS: u16 = 100;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_PREFERENCES_BYTES: usize = 256 * 1024;
const MAX_CONTROL_BYTES: usize = 1024;
const PREFERENCES_VERSION: u8 = 2;

pub struct DesktopOptions {
    pub settings: PathBuf,
    pub bundle: Option<PathBuf>,
}
impl DesktopOptions {
    pub fn parse(args: &[PathBuf]) -> Result<Self> {
        let mut settings = runtime_directory()?.join("desktop.json");
        let mut bundle = None;
        let mut arguments = args.iter();

        while let Some(argument) = arguments.next() {
            if argument.as_os_str() == "--settings" {
                settings = arguments
                    .next()
                    .ok_or_else(|| Error::new("--settings requires a file path"))?
                    .clone();
            } else if bundle.is_none() {
                bundle = Some(argument.clone());
            } else {
                return Err(Error::new(
                    "Usage: gui [BUNDLE_DIRECTORY] [--settings PREFERENCES.json]",
                ));
            }
        }

        if settings.is_relative() {
            settings = std::env::current_dir()?.join(settings);
        }
        Ok(Self { settings, bundle })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSettings {
    pub bundle: String,
    pub source: String,
    pub adapter: String,
    pub render: String,
    pub strength: f32,
    pub latency_ms: u16,
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            bundle: String::new(),
            source: String::new(),
            adapter: String::new(),
            render: String::new(),
            strength: 1.0,
            latency_ms: DEFAULT_LATENCY_MS,
        }
    }
}
impl AudioSettings {
    pub fn validate(&self) -> Result<()> {
        crate::audio::validate_strength(self.strength)?;
        if !(MIN_LATENCY_MS..=MAX_LATENCY_MS).contains(&self.latency_ms)
            || [&self.bundle, &self.source, &self.adapter, &self.render]
                .iter()
                .any(|v| v.len() > MAX_TEXT_BYTES || v.contains('\0'))
        {
            return Err(Error::new("Invalid audio settings latency or text field"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub version: u8,
    #[serde(default)]
    pub language: crate::i18n::Language,
    pub active: AudioSettings,
    #[serde(default, skip_serializing, rename = "presets")]
    _legacy_presets: serde::de::IgnoredAny,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            language: crate::i18n::Language::default(),
            active: AudioSettings::default(),
            _legacy_presets: serde::de::IgnoredAny,
        }
    }
}
impl Preferences {
    pub fn validate(&self) -> Result<()> {
        if self.version != PREFERENCES_VERSION {
            return Err(Error::new("Unsupported settings version"));
        }
        self.active.validate()
    }
    fn decode(values: &[u8]) -> Result<Self> {
        let mut preferences: Self = serde_json::from_slice(values)?;
        if preferences.version == 1 {
            preferences.version = PREFERENCES_VERSION;
        }
        preferences.validate()?;
        Ok(preferences)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let values = match bounded_read(path, MAX_PREFERENCES_BYTES) {
            Ok(v) => v,
            Err(_) if !path.try_exists()? => {
                #[cfg(target_os = "linux")]
                let data = gtk::glib::user_data_dir();
                #[cfg(target_os = "linux")]
                if path == data.join("clearwave/runtime/desktop.json") {
                    let legacy = data.join("rtx-voice-amd/runtime/desktop.json");
                    if legacy.is_file() {
                        return Self::load(&legacy);
                    }
                }
                return Ok(Self::default());
            }
            Err(e) => return Err(e),
        };
        Self::decode(&values)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() > MAX_PREFERENCES_BYTES {
            return Err(Error::new("Saved preferences exceed 256 KiB"));
        }
        atomic_write(path, &bytes)
    }
}

pub fn decode_picker_response(response: &[u8]) -> Result<Option<PathBuf>> {
    if response.len() > MAX_PICKER_RESPONSE_BYTES {
        return Err(Error::new("Model picker response is too large"));
    }
    let value: serde_json::Value = serde_json::from_slice(response)?;
    if let Some(error) = value.get("error").and_then(|error| error.as_str()) {
        return Err(Error::new(error));
    }
    let path: Option<String> = serde_json::from_value(value)?;
    match path {
        Some(path) if path.is_empty() || path.contains('\0') || !Path::new(&path).is_absolute() => {
            Err(Error::new("Model picker returned an invalid local path"))
        }
        path => Ok(path.map(PathBuf::from)),
    }
}

pub fn runtime_directory() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|path| PathBuf::from(path).join("Clearwave/runtime"))
            .ok_or_else(|| Error::new("Windows LOCALAPPDATA is unavailable"))
    }
    #[cfg(target_os = "linux")]
    {
        let executable = std::env::current_exe()?;
        let parent = executable
            .parent()
            .ok_or_else(|| Error::new("Executable has no parent directory"))?;
        let project = if parent.file_name().is_some_and(|name| name == "bin") {
            parent.parent().unwrap_or(parent)
        } else {
            parent
        };
        if project == Path::new("/usr") || project == Path::new("/usr/local") {
            return Ok(gtk::glib::user_data_dir().join("clearwave/runtime"));
        }
        Ok(project.join("runtime"))
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("Settings path needs a parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::new(e.to_string()))?
        .as_nanos();
    let temporary = parent.join(format!(".settings-{}-{stamp}.tmp", std::process::id()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        crate::windows::replace_file(&temporary, path)?;
        #[cfg(not(windows))]
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrengthControl {
    pub strength: f32,
}
impl StrengthControl {
    pub fn read(path: &Path) -> Result<f32> {
        let control: Self = serde_json::from_slice(&bounded_read(path, MAX_CONTROL_BYTES)?)?;
        crate::audio::validate_strength(control.strength)?;
        Ok(control.strength)
    }
    pub fn write(path: &Path, strength: f32) -> Result<()> {
        crate::audio::validate_strength(strength)?;
        atomic_write(path, &serde_json::to_vec(&Self { strength })?)
    }
}
#[derive(Default)]
pub struct Levels {
    squares: f64,
    count: usize,
    peak: f32,
}
impl Levels {
    pub fn add(&mut self, samples: &[f32]) {
        for sample in samples {
            self.squares += f64::from(*sample).powi(2);
            self.peak = self.peak.max(sample.abs());
        }
        self.count += samples.len();
    }
    pub fn take(&mut self) -> (f32, f32) {
        let rms = if self.count == 0 {
            0.0
        } else {
            (self.squares / self.count as f64).sqrt() as f32
        };
        let peak = self.peak;
        *self = Self::default();
        (rms, peak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picker_reports_import_errors_and_preserves_local_paths() {
        let path = std::env::temp_dir().join("RTX Voice installer.EXE");
        let response = serde_json::to_vec(&Some(path.to_string_lossy().as_ref())).unwrap();
        assert_eq!(decode_picker_response(&response).unwrap(), Some(path));
        let error = decode_picker_response(br#"{"error":"Install 7-Zip to import this model"}"#)
            .unwrap_err();
        assert!(error.to_string().contains("7-Zip"));
        assert_eq!(decode_picker_response(b"null").unwrap(), None);
    }
    #[test]
    fn desktop_options_preserve_bundle_and_relative_settings_override() {
        let args = ["first.json", "--settings", "settings/desktop.json"].map(PathBuf::from);
        let options = DesktopOptions::parse(&args).unwrap();
        assert_eq!(options.bundle, Some(PathBuf::from("first.json")));
        assert_eq!(
            options.settings,
            std::env::current_dir()
                .unwrap()
                .join("settings/desktop.json")
        );

        assert!(DesktopOptions::parse(&[PathBuf::from("--settings")]).is_err());
        assert!(DesktopOptions::parse(&["one", "two"].map(PathBuf::from)).is_err());
        let args = ["--settings", "one", "bundle", "--settings", "two"].map(PathBuf::from);
        let options = DesktopOptions::parse(&args).unwrap();
        assert_eq!(
            options.settings,
            std::env::current_dir().unwrap().join("two")
        );
        assert_eq!(options.bundle, Some(PathBuf::from("bundle")));
    }
    #[test]
    fn language_preferences_accept_old_files_and_round_trip_each_locale() {
        let old = r#"{"version":1,"active":{},"presets":[]}"#;
        let mut preferences = Preferences::decode(old.as_bytes()).unwrap();
        assert_eq!(preferences.language, crate::i18n::Language::EnglishBritish);
        for language in crate::i18n::Language::ALL {
            preferences.language = language;
            let decoded: Preferences =
                serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
            decoded.validate().unwrap();
            assert_eq!(decoded.language, language);
        }
        let unsupported = old.replace("\"version\":1", "\"version\":1,\"language\":\"unknown\"");
        assert!(serde_json::from_str::<Preferences>(&unsupported).is_err());
    }
    #[test]
    fn old_preferences_keep_active_settings_and_drop_named_presets() {
        let old = br#"{"version":1,"language":"pt-BR","active":{"bundle":"/models","source":"mic-1","adapter":"AMD","strength":0.4,"latency_ms":20},"presets":[{"name":"Speech","settings":{"strength":0.8}}]}"#;
        let mut p = Preferences::decode(old).unwrap();
        assert_eq!(p.version, 2);
        assert_eq!(p.active.source, "mic-1");
        assert_eq!(p.active.bundle, "/models");
        assert_eq!(p.active.adapter, "AMD");
        assert_eq!(p.active.latency_ms, 20);
        assert_eq!(p.active.strength, 0.4);
        assert_eq!(p.language, crate::i18n::Language::PortugueseBrazil);
        let saved = serde_json::to_value(&p).unwrap();
        assert!(saved.get("presets").is_none());
        let decoded = Preferences::decode(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(decoded.active, p.active);
        p.active.strength = 2.0;
        assert!(p.validate().is_err());
        assert!(Preferences::decode(br#"{"version":99,"active":{}}"#).is_err());
    }
    #[test]
    fn level_measurements_are_real_and_reset_per_window() {
        let mut l = Levels::default();
        l.add(&[-0.5, 0.5]);
        assert_eq!(l.take(), (0.5, 0.5));
        assert_eq!(l.take(), (0.0, 0.0));
    }
    #[test]
    fn control_requires_finite_bounded_strength() {
        for payload in [r#"{"strength":1.5}"#, r#"{"strength":0.5,"unexpected":1}"#] {
            let parsed = serde_json::from_str::<StrengthControl>(payload);
            assert!(
                parsed.is_err()
                    || crate::audio::validate_strength(parsed.unwrap().strength).is_err()
            );
        }
    }
}
