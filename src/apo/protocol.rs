//! Fixed-size audio packets connect the capture APO to the existing model worker.
//! Sequence numbers keep delayed audio aligned and reject stale worker responses.
//! This protocol carries microphone PCM only; it has no playback-device routing.
use std::io::{self, Read, Write};

pub const RATE: usize = 48_000;
pub const HOP: usize = 480;
pub const LATENCY: usize = 5_760;
pub const MAX_CHANNELS: usize = 2;
pub const PACKET_BYTES: usize = 16 + HOP * size_of::<f32>();
pub const MAGIC: u32 = 0x43574131;
pub const CLSID_TEXT: &str = "{657B7D18-8316-4DB1-9585-B981D14E615B}";
pub const MODE_PROPERTY: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},6";
pub const NO_AUDIO: u64 = u64::MAX;

#[derive(Clone)]
pub struct Packet {
    pub sequence: u64,
    pub samples: [f32; HOP],
}
impl Default for Packet {
    fn default() -> Self {
        Self {
            sequence: NO_AUDIO,
            samples: [0.0; HOP],
        }
    }
}
impl Packet {
    pub fn write(&self, output: &mut impl Write) -> io::Result<()> {
        let mut bytes = [0u8; PACKET_BYTES];
        bytes[..4].copy_from_slice(&MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&(HOP as u32).to_le_bytes());
        bytes[8..16].copy_from_slice(&self.sequence.to_le_bytes());
        for (value, target) in self
            .samples
            .iter()
            .zip(bytes[16..].as_chunks_mut::<4>().0.iter_mut())
        {
            target.copy_from_slice(&value.to_le_bytes());
        }
        output.write_all(&bytes)
    }

    pub fn read(input: &mut impl Read) -> io::Result<Self> {
        let mut bytes = [0; PACKET_BYTES];
        input.read_exact(&mut bytes)?;
        if u32::from_le_bytes(bytes[..4].try_into().unwrap()) != MAGIC
            || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != HOP as u32
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid APO packet",
            ));
        }
        let mut packet = Self {
            sequence: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            ..Self::default()
        };
        for (sample, bytes) in packet
            .samples
            .iter_mut()
            .zip(bytes[16..].as_chunks::<4>().0.iter())
        {
            *sample = f32::from_le_bytes(*bytes);
            if !sample.is_finite() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Nonfinite APO sample",
                ));
            }
        }
        Ok(packet)
    }
}

pub fn endpoint_guid(endpoint: &str) -> Option<String> {
    let start = endpoint.rfind('{')?;
    let guid = endpoint.get(start..)?;
    if guid.len() != 38 || !guid.ends_with('}') {
        return None;
    }
    if !guid[1..37].bytes().enumerate().all(|(i, byte)| {
        if [8, 13, 18, 23].contains(&i) {
            byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    }) {
        return None;
    }
    Some(guid.to_ascii_lowercase())
}
pub fn pipe_name(guid: &str) -> Option<String> {
    Some(format!(
        r"\\.\pipe\Clearwave.APO.v1.{}",
        endpoint_guid(guid)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_rejects_truncation_invalid_identity_and_nonfinite_audio() {
        let mut packet = Packet {
            sequence: 480,
            ..Packet::default()
        };
        packet.samples[2] = 0.25;
        let mut bytes = Vec::new();
        packet.write(&mut bytes).unwrap();
        assert_eq!(
            Packet::read(&mut bytes.as_slice()).unwrap().samples,
            packet.samples
        );
        assert!(Packet::read(&mut &bytes[..PACKET_BYTES - 1]).is_err());
        bytes[16..20].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(Packet::read(&mut bytes.as_slice()).is_err());
        assert!(endpoint_guid("{invalid}").is_none());
        assert!(
            pipe_name("{657B7D18-8316-4DB1-9585-B981D14E615B}")
                .unwrap()
                .ends_with("{657b7d18-8316-4db1-9585-b981d14e615b}")
        );
    }
}
