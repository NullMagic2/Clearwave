//! Bounded local file reads shared by model, audio, and settings loaders.
//! Enforce limits while reading so a growing file cannot bypass a metadata check.
use crate::{Error, Result};
use std::{fs::File, io::Read, path::Path};

pub fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let probe = limit
        .checked_add(1)
        .ok_or_else(|| Error::new("Invalid file size limit"))?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(probe as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::new(format!(
            "File exceeds its {limit}-byte size limit"
        )));
    }
    Ok(bytes)
}
