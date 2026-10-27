//! Saved desktop presets and bounded live control/level messages.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Preset {
    pub bundle: String,
    pub source: String,
    pub adapter: String,
    pub strength: f32,
    pub latency_ms: u16,
}
impl Default for Preset {
    fn default() -> Self {
        Self {
            bundle: String::new(),
            source: String::new(),
            adapter: String::new(),
            strength: 1.0,
            latency_ms: 10,
        }
    }
}
impl Preset {
    pub fn validate(&self) -> Result<()> {
        crate::audio::validate_strength(self.strength)?;
        if !(5..=100).contains(&self.latency_ms)
            || [&self.bundle, &self.source, &self.adapter]
                .iter()
                .any(|v| v.len() > 4096 || v.contains('\0'))
        {
            return Err(Error::new("Invalid preset latency or text field"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct NamedPreset {
    pub name: String,
    pub settings: Preset,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub version: u8,
    #[serde(default)]
    pub language: crate::i18n::Language,
    pub active: Preset,
    pub presets: Vec<NamedPreset>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            language: crate::i18n::Language::default(),
            active: Preset::default(),
            presets: Vec::new(),
        }
    }
}
impl Preferences {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 || self.presets.len() > 32 {
            return Err(Error::new(
                "Unsupported settings version or too many presets",
            ));
        }
        self.active.validate()?;
        let mut names = std::collections::HashSet::new();
        for preset in &self.presets {
            if preset.name.trim().is_empty()
                || preset.name.len() > 64
                || !names.insert(&preset.name)
            {
                return Err(Error::new("Preset names must be unique and 1–64 bytes"));
            }
            preset.settings.validate()?;
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let values = match bounded_read(path, 256 * 1024) {
            Ok(v) => v,
            Err(_) if !path.try_exists()? => {
                // Read installed preferences saved before the application rename.
                let data = gtk::glib::user_data_dir();
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
        let preferences: Self = serde_json::from_slice(&values)?;
        preferences.validate()?;
        Ok(preferences)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() > 256 * 1024 {
            return Err(Error::new("Saved preferences exceed 256 KiB"));
        }
        atomic_write(path, &bytes)
    }
    pub fn store_preset(&mut self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Error::new("Enter a preset name of 1–64 bytes"));
        }
        self.active.validate()?;
        if let Some(existing) = self.presets.iter_mut().find(|p| p.name == name) {
            existing.settings = self.active.clone();
        } else {
            if self.presets.len() == 32 {
                return Err(Error::new("A maximum of 32 saved presets is supported"));
            }
            self.presets.push(NamedPreset {
                name: name.into(),
                settings: self.active.clone(),
            });
        }
        Ok(())
    }
}
pub fn runtime_directory() -> Result<PathBuf> {
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
pub fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::new(
            "Control or settings file exceeds its size limit",
        ));
    }
    Ok(bytes)
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
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
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
        let control: Self = serde_json::from_slice(&bounded_read(path, 1024)?)?;
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
    fn language_preferences_accept_old_files_and_round_trip_each_locale() {
        let old = r#"{"version":1,"active":{},"presets":[]}"#;
        let mut preferences: Preferences = serde_json::from_str(old).unwrap();
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
    fn presets_round_trip_and_reject_invalid_values() {
        let mut p = Preferences::default();
        p.active.source = "mic-1".into();
        p.active.strength = 0.4;
        p.store_preset("Speech").unwrap();
        p.active.strength = 0.8;
        p.store_preset("Speech").unwrap();
        assert_eq!(p.presets.len(), 1);
        assert_eq!(p.presets[0].settings.strength, 0.8);
        let decoded: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.active, p.active);
        p.active.strength = 2.0;
        assert!(p.validate().is_err());
        p.active.strength = 0.8;
        p.presets.push(p.presets[0].clone());
        assert!(p.validate().is_err());
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
