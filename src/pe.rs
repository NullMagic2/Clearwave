//! Bounded PE section access for the supported runtime package.
use crate::{Error, Result};

pub fn slice_at_rva(bytes: &[u8], rva: u32, len: usize) -> Result<&[u8]> {
    if bytes.get(..2) != Some(b"MZ") {
        return Err(Error::new("Runtime is not a PE file"));
    }
    let pe = read_u32(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(Error::new("Invalid PE signature"));
    }
    let count = read_u16(bytes, pe + 6)? as usize;
    let opt = read_u16(bytes, pe + 20)? as usize;
    if count > 96 {
        return Err(Error::new("Unreasonable PE section count"));
    }
    for i in 0..count {
        let offset = pe
            .checked_add(24 + opt + i * 40)
            .ok_or_else(|| Error::new("PE offset overflow"))?;
        let start = read_u32(bytes, offset + 12)?;
        let size = read_u32(bytes, offset + 16)? as usize;
        let raw = read_u32(bytes, offset + 20)? as usize;
        if let Some(delta) = rva.checked_sub(start) {
            let delta = delta as usize;
            if delta.checked_add(len).is_some_and(|end| end <= size) {
                let begin = raw
                    .checked_add(delta)
                    .ok_or_else(|| Error::new("PE offset overflow"))?;
                return bytes
                    .get(begin..begin + len)
                    .ok_or_else(|| Error::new("Truncated PE section"));
            }
        }
    }
    Err(Error::new("Requested RVA is not backed by file data"))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let s = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| Error::new("Truncated PE header"))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let s = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| Error::new("Truncated PE header"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_pe_is_rejected() {
        assert!(slice_at_rva(b"MZ", 0, 32).is_err());
    }
    #[test]
    fn unbacked_rva_is_rejected() {
        let mut pe = vec![0; 0x100];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x40;
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        assert!(slice_at_rva(&pe, 0x33800, 32).is_err());
    }
}
