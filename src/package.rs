//! Package v1 and weights_v0 decoding. No vendor runtime executes here.
use crate::{Error, Result, pe};
use aes::Aes256;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, path::Path};

const MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct Tensor {
    pub name: String,
    pub values: Vec<f32>,
}

#[derive(Debug)]
pub struct Model {
    pub format: &'static str,
    pub tensors: Vec<Tensor>,
    pub package_sha256: String,
    pub runtime_sha256: Option<String>,
    pub payload_sha256: String,
}

#[derive(Serialize)]
pub struct TensorInfo<'a> {
    pub name: &'a str,
    pub elements: usize,
    pub min: f32,
    pub max: f32,
}

impl Model {
    pub fn load(package: &Path, runtime: &Path) -> Result<Self> {
        let package = read_bounded(package)?;
        let runtime = read_bounded(runtime)?;
        let clear = decode_package(&package, &runtime)?;
        Ok(Self {
            format: "NVnn v1 / weights_v0",
            tensors: parse_weights(&clear)?,
            package_sha256: hash(&package),
            runtime_sha256: Some(hash(&runtime)),
            payload_sha256: hash(&clear),
        })
    }

    pub fn get(&self, name: &str) -> Result<&Tensor> {
        self.tensors
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::new(format!("Tensor not found: {name}")))
    }

    pub fn inventory(&self) -> Vec<TensorInfo<'_>> {
        self.tensors
            .iter()
            .map(|t| TensorInfo {
                name: &t.name,
                elements: t.values.len(),
                min: t.values.iter().copied().fold(f32::INFINITY, f32::min),
                max: t.values.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            })
            .collect()
    }
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    if fs::metadata(path)?.len() > MAX_BYTES {
        return Err(Error::new("File exceeds 512 MiB limit"));
    }
    Ok(fs::read(path)?)
}

pub fn decode_package(bytes: &[u8], runtime: &[u8]) -> Result<Vec<u8>> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != b"NVnn" {
        return Err(Error::new("Unsupported package signature"));
    }
    let version = r.u32()?;
    let flags = r.u32()?;
    let reserved = r.u32()?;
    let kind = r.u32()?;
    let reserved2 = r.u32()?;
    if (version, flags, reserved, kind, reserved2) != (1, 1, 0, 1, 0) {
        return Err(Error::new(
            "Unsupported package variant; expected encrypted v1 weights",
        ));
    }
    let meta_len = r.u32()? as usize;
    let meta = r.take(meta_len)?;
    if meta != b"\x10\0\0\0weig\0\0\0\0\0\0\0\0" {
        return Err(Error::new("Unsupported weights metadata"));
    }
    let iv = r.take(16)?;
    if r.u32()? != 1 || r.u32()? != 0 {
        return Err(Error::new("Only single-segment packages are supported"));
    }
    let size = r.u32()? as usize;
    let encrypted = r.take(size)?;
    if !r.is_empty() || size == 0 || size % 16 != 0 {
        return Err(Error::new("Invalid ciphertext length"));
    }

    // This loader targets the supplied 0.5.12.6 runtime layout.
    // Check its key-derivation instruction sequence before using the table RVA.
    let expected = [
        0x41, 0x8d, 0x50, 0x6d, 0x41, 0x0f, 0xb6, 0x04, 0x09, 0x32, 0xc2,
    ];
    if pe::slice_at_rva(runtime, 0x194c7, expected.len())? != expected {
        return Err(Error::new("Unsupported runtime version/layout"));
    }
    let table = pe::slice_at_rva(runtime, 0x33800, 32)?;
    let mut key = [0u8; 32];
    for (i, byte) in table.iter().enumerate() {
        key[i] = byte ^ (i as u8).wrapping_add(0x6d);
    }
    let mut clear = encrypted.to_vec();
    let decoder = cbc::Decryptor::<Aes256>::new((&key).into(), iv.into());
    let decoded = decoder
        .decrypt_padded_mut::<Pkcs7>(&mut clear)
        .map_err(|_| Error::new("Package padding check failed"))?;
    let len = decoded.len();
    key.fill(0);
    clear.truncate(len);
    if !clear.starts_with(b"weights_v0") {
        return Err(Error::new("Decoded payload is not weights_v0"));
    }
    Ok(clear)
}

pub fn parse_weights(bytes: &[u8]) -> Result<Vec<Tensor>> {
    let mut r = Reader::new(bytes);
    if r.take(10)? != b"weights_v0" {
        return Err(Error::new("Unsupported weight format"));
    }
    let mut tensors = Vec::new();
    let mut names = HashSet::new();
    while !r.is_empty() {
        if tensors.len() >= 4096 {
            return Err(Error::new("Too many tensors"));
        }
        let len = r.u32()? as usize;
        if !(1..=256).contains(&len) {
            return Err(Error::new("Invalid tensor name length"));
        }
        let name = std::str::from_utf8(r.take(len)?)?.to_owned();
        if !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || !names.insert(name.clone())
        {
            return Err(Error::new("Invalid or duplicate tensor name"));
        }
        let elements = r.u32()? as usize;
        if elements == 0 {
            return Err(Error::new("Empty tensor"));
        }
        let len = elements
            .checked_mul(4)
            .ok_or_else(|| Error::new("Tensor size overflow"))?;
        let data = r.take(len)?;
        let values: Vec<f32> = data
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            .collect();
        if values.iter().any(|v| !v.is_finite()) {
            return Err(Error::new("Non-finite tensor values"));
        }
        tensors.push(Tensor { name, values });
    }
    if tensors.is_empty() {
        return Err(Error::new("No tensors in payload"));
    }
    Ok(tensors)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| Error::new("Package offset overflow"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| Error::new("Truncated package or tensor"))?;
        self.offset = end;
        Ok(value)
    }
    fn u32(&mut self) -> Result<u32> {
        let x = self.take(4)?;
        Ok(u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
    }
    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut b = b"weights_v0".to_vec();
        b.extend(1u32.to_le_bytes());
        b.push(b'w');
        b.extend(2u32.to_le_bytes());
        b.extend(1.25f32.to_le_bytes());
        b.extend((-2.0f32).to_le_bytes());
        b
    }
    #[test]
    fn parses_values_without_alignment_assumptions() {
        assert_eq!(parse_weights(&fixture()).unwrap()[0].values, [1.25, -2.0]);
    }
    #[test]
    fn rejects_every_truncation() {
        let b = fixture();
        for end in 0..b.len() {
            assert!(parse_weights(&b[..end]).is_err(), "offset {end}");
        }
    }
    #[test]
    fn rejects_duplicates_and_nonfinite() {
        let b = fixture();
        let mut dup = b.clone();
        dup.extend(&b[10..]);
        assert!(parse_weights(&dup).is_err());
        let mut bad = b;
        let len = bad.len();
        bad[len - 4..].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(parse_weights(&bad).is_err());
    }
    #[test]
    fn rejects_ciphertext_before_loading_runtime() {
        assert!(decode_package(b"NVnn", &[]).is_err());
    }
}
