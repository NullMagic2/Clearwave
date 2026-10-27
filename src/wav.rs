//! Bounded RIFF/WAVE input and float32 mono output.
use crate::{Error, Result};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
pub struct Audio {
    pub samples: Vec<f32>,
    pub channels: u16,
}
pub fn read(path: &Path) -> Result<Audio> {
    let mut data = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BYTES {
        return Err(Error::new("WAV input exceeds 64 MiB"));
    }
    decode(&data)
}
fn u16_at(data: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([data[i], data[i + 1]])
}
fn u32_at(data: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
}
fn decode(data: &[u8]) -> Result<Audio> {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(Error::new("Expected a little-endian RIFF/WAVE file"));
    }
    let end = 8usize
        .checked_add(u32_at(data, 4) as usize)
        .ok_or_else(|| Error::new("Invalid RIFF size"))?;
    if end != data.len() {
        return Err(Error::new("RIFF length does not match file length"));
    }
    let (mut format, mut audio) = (None, None);
    let mut p = 12;
    while p < end {
        if end - p < 8 {
            return Err(Error::new("Truncated WAV chunk header"));
        }
        let size = u32_at(data, p + 4) as usize;
        let start = p + 8;
        let stop = start
            .checked_add(size)
            .filter(|v| *v <= end)
            .ok_or_else(|| Error::new("Truncated WAV chunk"))?;
        match &data[p..p + 4] {
            b"fmt " => {
                if format.is_some() || size < 16 {
                    return Err(Error::new("Invalid or duplicate WAV format"));
                }
                format = Some(&data[start..stop]);
            }
            b"data" => {
                if audio.is_some() {
                    return Err(Error::new("Multiple WAV data chunks are unsupported"));
                }
                audio = Some(&data[start..stop]);
            }
            _ => {}
        }
        p = stop + size % 2;
        if p > end {
            return Err(Error::new("Missing WAV chunk padding"));
        }
    }
    let fmt = format.ok_or_else(|| Error::new("Missing WAV format"))?;
    let payload = audio.ok_or_else(|| Error::new("Missing WAV samples"))?;
    let (kind, channels, rate, align, bits) = (
        u16_at(fmt, 0),
        u16_at(fmt, 2),
        u32_at(fmt, 4),
        u16_at(fmt, 12),
        u16_at(fmt, 14),
    );
    if rate != 48000 || !(1..=2).contains(&channels) {
        return Err(Error::new(
            "Expected 48000 Hz mono or stereo WAV; convert other sample rates first",
        ));
    }
    if !matches!((kind, bits), (1, 16 | 24 | 32) | (3, 32)) {
        return Err(Error::new("Expected PCM16/24/32 or IEEE float32 WAV"));
    }
    let bytes = usize::from(bits / 8);
    if usize::from(align) != bytes * usize::from(channels)
        || u32_at(fmt, 8) != rate * u32::from(align)
        || payload.is_empty()
        || payload.len() % usize::from(align) != 0
    {
        return Err(Error::new(
            "Invalid WAV sample alignment or empty recording",
        ));
    }
    let mut samples = Vec::with_capacity(payload.len() / usize::from(align));
    for frame in payload.chunks_exact(usize::from(align)) {
        let mut sum = 0.0f64;
        for s in frame.chunks_exact(bytes) {
            let value = match (kind, bits) {
                (1, 16) => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
                (1, 24) => {
                    ((i32::from(s[0]) | i32::from(s[1]) << 8 | i32::from(s[2]) << 16) << 8 >> 8)
                        as f32
                        / 8388608.0
                }
                (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2147483648.0,
                _ => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            };
            if !value.is_finite() || value.abs() > 16.0 {
                return Err(Error::new("WAV samples must be finite and within ±16"));
            }
            sum += f64::from(value);
        }
        samples.push((sum / f64::from(channels)) as f32);
    }
    Ok(Audio { samples, channels })
}
pub fn write_new(path: &Path, samples: &[f32]) -> Result<()> {
    if samples.is_empty() || samples.iter().any(|v| !v.is_finite()) {
        return Err(Error::new("Output audio must be finite and nonempty"));
    }
    let size = samples
        .len()
        .checked_mul(4)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n <= u32::MAX - 48)
        .ok_or_else(|| Error::new("Output is too large for RIFF"))?;
    // The fact chunk gives the sample count for IEEE float WAVE readers.
    let mut out =
        std::io::BufWriter::new(OpenOptions::new().write(true).create_new(true).open(path)?);
    out.write_all(b"RIFF")?;
    out.write_all(&(size + 48).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&3u16.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&48000u32.to_le_bytes())?;
    out.write_all(&192000u32.to_le_bytes())?;
    out.write_all(&4u16.to_le_bytes())?;
    out.write_all(&32u16.to_le_bytes())?;
    out.write_all(b"fact")?;
    out.write_all(&4u32.to_le_bytes())?;
    out.write_all(&(size / 4).to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&size.to_le_bytes())?;
    for value in samples {
        out.write_all(&value.to_le_bytes())?;
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(kind: u16, channels: u16, bits: u16, payload: &[u8]) -> Vec<u8> {
        let mut b = b"RIFF".to_vec();
        b.extend((36 + payload.len() as u32).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(kind.to_le_bytes());
        b.extend(channels.to_le_bytes());
        b.extend(48000u32.to_le_bytes());
        let align = channels * bits / 8;
        b.extend((48000 * u32::from(align)).to_le_bytes());
        b.extend(align.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b.extend(b"data");
        b.extend((payload.len() as u32).to_le_bytes());
        b.extend(payload);
        b
    }
    #[test]
    fn pcm_signed_extremes_and_stereo_downmix() {
        let x = decode(&fixture(1, 2, 16, &[0, 128, 0, 64])).unwrap();
        assert_eq!(x.samples, [-0.25]);
        let x = decode(&fixture(1, 1, 24, &[0, 0, 128, 255, 255, 127])).unwrap();
        assert_eq!(x.samples[0], -1.0);
        assert!(x.samples[1] > 0.99999);
        let x = decode(&fixture(1, 1, 32, &[0, 0, 0, 128])).unwrap();
        assert_eq!(x.samples, [-1.0]);
    }
    #[test]
    fn malformed_wave_and_nonfinite_samples_are_rejected() {
        let b = fixture(3, 1, 32, &f32::NAN.to_le_bytes());
        assert!(decode(&b).is_err());
        let mut b = fixture(1, 1, 16, &[0, 0]);
        b[40..44].copy_from_slice(&999u32.to_le_bytes());
        assert!(decode(&b).is_err());
        let mut b = fixture(1, 1, 16, &[0, 0]);
        b[24..28].copy_from_slice(&44100u32.to_le_bytes());
        assert!(decode(&b).is_err());
    }
}
