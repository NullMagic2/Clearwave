//! Import Broadcast audio data from an existing cache or NVIDIA's pinned archives.
//! HTTP ranges retrieve only the required model and decoder entry, without SDK libraries.
//! The chooser helper owns all I/O and child processes so cancellation remains bounded.
use crate::{Error, Result, broadcast, model_import, storage::bounded_read};
use flate2::read::DeflateDecoder;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
};

const CDN: &str = "https://ngx.download.nvidia.com/257a0e72-47ec-4829-b73e-6edb796c1a71/org/nvidia/team/ngx/models/";
const MODEL_ARCHIVE: &str = "nvbcast_afx_bnr_v0_9/versions/2309/files/160_E658703.zip";
const DECODER_ARCHIVE: &str = "nvbcast/versions/2311/files/160_E658703.zip";
const MODEL_ARCHIVE_BYTES: u64 = 32_389_139;
const DECODER_ARCHIVE_BYTES: u64 = 1_746_290_711;
const MODEL_NAME: &str = "denoiser_48k.trtpkg";
const DECODER_NAME: &str = "nvafxdenoiser.dll";
const ARCHIVE_FOLDER: &str = "160_E658703";
const CACHE_MODEL: &str =
    "nvbcast_afx_bnr_v0_9/versions/2309/files/160_E658703/denoiser_48k.trtpkg";
const CACHE_DECODER: &str = "nvbcast/versions/2311/files/160_E658703/nvafxdenoiser.dll";
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
const MAX_DECODER_BYTES: usize = 2 * 1024 * 1024;
const ZIP_END_BYTES: usize = 22;
const ZIP_TAIL_BYTES: u64 = 65_557;
const ZIP_DIRECTORY_BYTES: usize = 46;
const ZIP_LOCAL_BYTES: usize = 30;
const ZIP_STORED: u16 = 0;
const ZIP_DEFLATE: u16 = 8;

struct Entry {
    method: u16,
    checksum: u32,
    compressed: usize,
    expanded: usize,
    offset: u64,
}

pub fn prepare(installer: &Path, destination: &Path) -> Result<()> {
    for root in cache_candidates(installer) {
        let (model, decoder) = if root.join(MODEL_NAME).is_file() {
            (root.join(MODEL_NAME), root.join(DECODER_NAME))
        } else {
            (root.join(CACHE_MODEL), root.join(CACHE_DECODER))
        };
        if model.is_file() && decoder.is_file() && broadcast::load(&model, &decoder).is_ok() {
            fs::write(
                destination.join(MODEL_NAME),
                bounded_read(&model, MAX_ENTRY_BYTES)?,
            )?;
            fs::write(
                destination.join(DECODER_NAME),
                bounded_read(&decoder, MAX_DECODER_BYTES)?,
            )?;
            return Ok(());
        }
    }
    fetch_entry(
        destination,
        DECODER_ARCHIVE,
        DECODER_ARCHIVE_BYTES,
        DECODER_NAME,
        MAX_DECODER_BYTES,
    )?;
    fetch_entry(
        destination,
        MODEL_ARCHIVE,
        MODEL_ARCHIVE_BYTES,
        MODEL_NAME,
        MAX_ENTRY_BYTES,
    )?;
    broadcast::load(
        &destination.join(MODEL_NAME),
        &destination.join(DECODER_NAME),
    )?;
    Ok(())
}
fn cache_candidates(installer: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("CLEARWAVE_BROADCAST_MODELS") {
        paths.push(path.into());
    }
    if let Some(parent) = installer.parent() {
        paths.push(parent.to_owned());
        paths.push(parent.join("Models"));
    }
    if let Some(root) = std::env::var_os("ProgramData") {
        paths.push(PathBuf::from(root).join("NVIDIA/NGX/models"));
    }
    if let Some(root) = std::env::var_os("WINEPREFIX") {
        paths.push(PathBuf::from(root).join("drive_c/ProgramData/NVIDIA/NGX/models"));
    }
    paths
}
fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| Error::new("Truncated ZIP header"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| Error::new("Truncated ZIP header"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}
fn directory(tail: &[u8], archive_bytes: u64) -> Result<&[u8]> {
    let end = tail
        .windows(4)
        .rposition(|value| value == b"PK\x05\x06")
        .ok_or_else(|| Error::new("NVIDIA archive has no ZIP directory"))?;
    if read_u16(tail, end + 4)? != 0
        || read_u16(tail, end + 6)? != 0
        || read_u16(tail, end + 8)? != read_u16(tail, end + 10)?
        || end + ZIP_END_BYTES + usize::from(read_u16(tail, end + 20)?) != tail.len()
    {
        return Err(Error::new("Unsupported NVIDIA ZIP directory layout"));
    }
    let length = u64::from(read_u32(tail, end + 12)?);
    let start = u64::from(read_u32(tail, end + 16)?);
    let base = archive_bytes
        .checked_sub(tail.len() as u64)
        .ok_or_else(|| Error::new("Invalid archive size"))?;
    let offset = start
        .checked_sub(base)
        .ok_or_else(|| Error::new("ZIP directory exceeds the bounded tail"))?
        as usize;
    let length = usize::try_from(length).map_err(|_| Error::new("ZIP directory is too large"))?;
    tail.get(
        offset
            ..offset
                .checked_add(length)
                .ok_or_else(|| Error::new("ZIP directory overflow"))?,
    )
    .ok_or_else(|| Error::new("Truncated ZIP directory"))
}
fn find_entry(directory: &[u8], wanted: &str, limit: usize) -> Result<Entry> {
    let mut cursor = 0;
    let mut found = None;
    while cursor < directory.len() {
        let header = directory
            .get(cursor..cursor.saturating_add(ZIP_DIRECTORY_BYTES))
            .ok_or_else(|| Error::new("Truncated ZIP entry"))?;
        if &header[..4] != b"PK\x01\x02" {
            return Err(Error::new("Invalid ZIP entry signature"));
        }
        let name_bytes = usize::from(read_u16(header, 28)?);
        let extra_bytes = usize::from(read_u16(header, 30)?);
        let comment_bytes = usize::from(read_u16(header, 32)?);
        let start = cursor + ZIP_DIRECTORY_BYTES;
        let name = directory
            .get(start..start.saturating_add(name_bytes))
            .ok_or_else(|| Error::new("Truncated ZIP entry name"))?;
        if name == wanted.as_bytes() {
            if found.is_some() || read_u16(header, 8)? & 1 != 0 || read_u16(header, 34)? != 0 {
                return Err(Error::new("Ambiguous or encrypted ZIP entry"));
            }
            let method = read_u16(header, 10)?;
            let compressed = read_u32(header, 20)? as usize;
            let expanded = read_u32(header, 24)? as usize;
            if ![ZIP_STORED, ZIP_DEFLATE].contains(&method)
                || compressed == 0
                || compressed > limit
                || expanded == 0
                || expanded > limit
            {
                return Err(Error::new("ZIP entry exceeds the supported limits"));
            }
            found = Some(Entry {
                method,
                checksum: read_u32(header, 16)?,
                compressed,
                expanded,
                offset: u64::from(read_u32(header, 42)?),
            });
        }
        cursor = start
            .checked_add(name_bytes + extra_bytes + comment_bytes)
            .filter(|end| *end <= directory.len())
            .ok_or_else(|| Error::new("ZIP entry bounds overflow"))?;
    }
    found.ok_or_else(|| Error::new("The NVIDIA archive is missing the requested audio file"))
}
fn fetch_entry(
    destination: &Path,
    archive: &str,
    bytes: u64,
    name: &str,
    limit: usize,
) -> Result<()> {
    let url = format!("{CDN}{archive}");
    let tail = download_range(destination, &url, bytes - ZIP_TAIL_BYTES, bytes - 1)?;
    let entry = find_entry(
        directory(&tail, bytes)?,
        &format!("{ARCHIVE_FOLDER}/{name}"),
        limit,
    )?;
    let header = download_range(
        destination,
        &url,
        entry.offset,
        entry.offset + ZIP_LOCAL_BYTES as u64 - 1,
    )?;
    if header.get(..4) != Some(b"PK\x03\x04") || read_u16(&header, 8)? != entry.method {
        return Err(Error::new("Invalid local ZIP entry header"));
    }
    let start = entry.offset
        + ZIP_LOCAL_BYTES as u64
        + u64::from(read_u16(&header, 26)?)
        + u64::from(read_u16(&header, 28)?);
    let end = start
        .checked_add(entry.compressed as u64)
        .filter(|end| *end <= bytes)
        .ok_or_else(|| Error::new("ZIP payload exceeds archive bounds"))?;
    let packed = download_range(destination, &url, start, end - 1)?;
    let clear = if entry.method == ZIP_STORED {
        packed
    } else {
        let mut clear = Vec::new();
        DeflateDecoder::new(packed.as_slice())
            .take(entry.expanded as u64 + 1)
            .read_to_end(&mut clear)?;
        clear
    };
    if clear.len() != entry.expanded || crc32fast::hash(&clear) != entry.checksum {
        return Err(Error::new(
            "Downloaded NVIDIA audio file failed its integrity check",
        ));
    }
    fs::write(destination.join(name), clear)?;
    Ok(())
}
fn download_range(destination: &Path, url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
    let count = end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .filter(|value| *value <= MAX_ENTRY_BYTES as u64)
        .ok_or_else(|| Error::new("Invalid download range"))?;
    let output = destination.join(".nvidia-download-part");
    #[cfg(windows)]
    let curl = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32/curl.exe"))
        .ok_or_else(|| Error::new("Cannot locate Windows curl"))?;
    #[cfg(target_os = "linux")]
    let curl = PathBuf::from("curl");
    let mut command = model_import::tool_command(curl);
    command
        .args([
            "--fail",
            "--silent",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "20",
            "--max-time",
            "180",
            "--retry",
            "2",
            "--range",
        ])
        .arg(format!("{start}-{end}"))
        .arg("--max-filesize")
        .arg(count.to_string())
        .arg("--output")
        .arg(&output)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = model_import::run_tool(&mut command).map_err(|error| {
        Error::new(format!(
            "Broadcast model download requires curl and an Internet connection ({error})"
        ))
    })?;
    if !status.success() {
        return Err(Error::new(
            "Could not download the Broadcast audio model from NVIDIA. Check the Internet connection or select an existing model folder.",
        ));
    }
    let data = bounded_read(&output, count as usize)?;
    fs::remove_file(output)?;
    if data.len() as u64 != count {
        return Err(Error::new(
            "NVIDIA's server returned an unexpected download range",
        ));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_truncated_and_unbounded_zip_entries() {
        assert!(directory(&[], 0).is_err());
        assert!(find_entry(&[0; ZIP_DIRECTORY_BYTES], "file", 1024).is_err());
        let mut entry = vec![0; ZIP_DIRECTORY_BYTES];
        entry[..4].copy_from_slice(b"PK\x01\x02");
        entry[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        entry[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(find_entry(&entry, "", 1024).is_err());
        for length in 0..ZIP_DIRECTORY_BYTES {
            assert!(find_entry(&entry[..length], "file", 1024).is_err() || length == 0);
        }
    }
}
