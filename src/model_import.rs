//! Import a locally supplied installer as data into a validated model cache.
//! Chooser helpers and audio workers share this path, keeping extraction off UI threads.
//! Only supported bundle files are extracted; the installer is never executed.
use crate::{Error, Result, export, model_source::ModelSource, network};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const HASH_BUFFER_BYTES: usize = 64 * 1024;
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024 * 1024;
const ARCHIVE_FILES: [&str; 8] = [
    "NvAFX/nv/NVAudioEffects.dll",
    "NvAFX/models/denoiser_48k.wpkg",
    "NvAFX/nv/EULA.pdf",
    "NvAFX/nv/ThirdPartyLicenses.txt",
    "EULA.txt",
    "NvMaxineModels/NvMaxineModels.nvi",
    "NvMaxineClient/nv/common_config.bin",
    "NvMaxineClient/nv/global_config.bin",
];
const BROADCAST_INSTALLER_SHA256: &str =
    "c59e4bcbdaee4b56d12447086ee761ca41244ae3e048d1d2a849abb8e106f684";
const WEIGHTS_FILE: &str = "voice-weights.safetensors";

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn is_installer(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
}
fn fingerprint(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let size = input.metadata()?.len();
    if size == 0 || size > MAX_INSTALLER_BYTES {
        return Err(Error::new(
            "Installer must be nonempty and smaller than 1 GiB",
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; HASH_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_INSTALLER_BYTES {
            return Err(Error::new("Installer exceeds the 1 GiB size limit"));
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn archiver() -> PathBuf {
    #[cfg(windows)]
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable) {
            let tool = PathBuf::from(root).join("7-Zip/7z.exe");
            if tool.is_file() {
                return tool;
            }
        }
    }
    PathBuf::from("7z")
}
pub(crate) fn tool_command(tool: impl AsRef<std::ffi::OsStr>) -> Command {
    #[cfg(target_os = "linux")]
    let command = {
        let mut command = Command::new("setpriv");
        command.args(["--pdeathsig", "KILL", "--"]).arg(tool);
        command
    };
    #[cfg(windows)]
    let command = Command::new(tool);

    command
}
pub(crate) fn run_tool(command: &mut Command) -> Result<ExitStatus> {
    #[cfg(target_os = "linux")]
    let status = command.status().map_err(Error::from);
    #[cfg(windows)]
    let status = crate::windows::OwnedChild::spawn(command)
        .and_then(|mut child| child.process()?.wait().map_err(Error::from));
    status
}
pub fn extract(installer: &Path, destination: &Path) -> Result<()> {
    let installer = installer.canonicalize()?;
    fs::create_dir(destination)?;
    let destination = destination.canonicalize()?;
    let mut command = tool_command(archiver());

    command
        .args(["x", "-y", "-bd"])
        .arg(format!("-o{}", destination.display()))
        .arg(&installer)
        .args(ARCHIVE_FILES)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = run_tool(&mut command).map_err(|error| {
        Error::new(format!(
            "Cannot extract installer: install 7-Zip and try again ({error})"
        ))
    })?;
    if !status.success() {
        return Err(Error::new(
            "7-Zip could not extract the RTX Voice or Broadcast installer",
        ));
    }
    if destination
        .join("NvMaxineModels/NvMaxineModels.nvi")
        .is_file()
    {
        if fingerprint(&installer)? != BROADCAST_INSTALLER_SHA256 {
            return Err(Error::new(
                "This Broadcast installer revision is not supported; select NVIDIA Broadcast 2.3.0.71988672",
            ));
        }
        crate::broadcast_import::prepare(&installer, &destination)?;
    }
    let model = ModelSource::resolve(&destination)?.load()?;
    network::specification(&model)?;
    Ok(())
}

pub fn prepare(installer: &Path) -> Result<PathBuf> {
    prepare_in(
        installer,
        &crate::controls::runtime_directory()?.join("imported-models"),
    )
}
pub fn prepare_in(installer: &Path, cache: &Path) -> Result<PathBuf> {
    if !installer.is_file() || !is_installer(installer) {
        return Err(Error::new(
            "Select an RTX Voice or NVIDIA Broadcast installer executable",
        ));
    }
    let identity = fingerprint(installer)?;
    let destination = cache.join(&identity);
    if destination.exists() {
        ModelSource::resolve(&destination)?.load()?;
        return Ok(destination);
    }
    fs::create_dir_all(cache)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::new(error.to_string()))?
        .as_nanos();
    let staging = Staging(cache.join(format!(".import-{}-{stamp}", std::process::id())));
    extract(installer, &staging.0)?;
    let model = ModelSource::resolve(&staging.0)?.load()?;
    export::safetensors(&model, &staging.0.join(WEIGHTS_FILE))?;
    if fingerprint(installer)? != identity {
        return Err(Error::new(
            "Installer changed during extraction; please try again",
        ));
    }
    if let Err(error) = fs::rename(&staging.0, &destination) {
        if !destination.exists() {
            return Err(error.into());
        }
        ModelSource::resolve(&destination)?.load()?;
    }
    Ok(destination)
}

pub fn report_selection(path: Option<PathBuf>) -> Result<()> {
    let result = path
        .map(|path| {
            if is_installer(&path) {
                prepare(&path)
            } else {
                Ok(path)
            }
        })
        .transpose();
    let response = match result {
        Ok(path) => serde_json::to_string(&path.map(|path| path.to_string_lossy().into_owned()))?,
        Err(error) => serde_json::to_string(&serde_json::json!({"error": error.to_string()}))?,
    };
    if response.len() > crate::controls::MAX_PICKER_RESPONSE_BYTES {
        return Err(Error::new("Selected model path is too long"));
    }
    println!("{response}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installer_identity_rejects_empty_files_and_changes_with_content() {
        let path =
            std::env::temp_dir().join(format!("clearwave-installer-{}.EXE", std::process::id()));
        fs::write(&path, []).unwrap();
        assert!(is_installer(&path));
        assert!(fingerprint(&path).is_err());
        fs::write(&path, b"first").unwrap();
        let before = fingerprint(&path).unwrap();
        fs::write(&path, b"second").unwrap();
        assert_ne!(before, fingerprint(&path).unwrap());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn invalid_cached_bundle_is_reported_without_overwriting_it() {
        let root =
            std::env::temp_dir().join(format!("clearwave-cache-check-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let owned = Staging(root.clone());
        let installer = root.join("installer.exe");
        fs::write(&installer, b"unsupported fixture").unwrap();
        let cache = root.join("cache");
        let cached = cache.join(fingerprint(&installer).unwrap());
        fs::create_dir_all(&cached).unwrap();
        fs::write(cached.join(WEIGHTS_FILE), b"invalid model").unwrap();
        assert!(prepare_in(&installer, &cache).is_err());
        assert_eq!(
            fs::read(cached.join(WEIGHTS_FILE)).unwrap(),
            b"invalid model"
        );
        drop(owned);
    }
}
