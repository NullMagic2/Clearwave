//! WASAPI device enumeration and bounded microphone-to-render audio streaming.
//! CPAL owns Windows audio clients; callbacks only transfer PCM blocks.
//! The process's worker thread owns model loading, inference and level reporting.
#![forbid(unsafe_code)]
use crate::{
    Error, Result,
    audio::{HOP, RATE, StreamProcessor},
    controls::{Levels, ProcessingControl},
    network::{Backend, CONTEXT, Network},
};
use cpal::{
    Device, DeviceId, FromSample, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    time::{Duration, Instant},
};

const DEVICE_SAMPLE_RATE: u32 = RATE as u32;
const MAX_DEVICE_CHANNELS: u16 = 8;
const AUDIO_QUEUE_BLOCKS: usize = 32;
const PLAYBACK_PRIME_HOPS: usize = CONTEXT + 1;
const CONTROL_INTERVAL_HOPS: u64 = 10;
const DEVICE_TIMEOUT: Duration = Duration::from_secs(3);
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Deserialize, Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
}
#[derive(Deserialize, Serialize)]
pub struct Endpoints {
    pub inputs: Vec<Endpoint>,
    pub outputs: Vec<Endpoint>,
    #[serde(default)]
    pub warnings: Vec<String>,
}
fn describe(device: Device) -> Result<Endpoint> {
    Ok(Endpoint {
        id: device.id().map_err(audio_error)?.to_string(),
        name: device.description().map_err(audio_error)?.name().to_owned(),
    })
}
fn audio_error(error: cpal::Error) -> Error {
    Error::new(format!("Windows audio: {error}"))
}
fn collect_endpoints(
    devices: impl Iterator<Item = Result<Endpoint>>,
    warnings: &mut Vec<String>,
) -> Vec<Endpoint> {
    devices
        .filter_map(|device| match device {
            Ok(endpoint) => Some(endpoint),
            Err(error) => {
                warnings.push(error.to_string());
                None
            }
        })
        .collect()
}
pub fn endpoints() -> Result<Endpoints> {
    let host = cpal::default_host();
    let mut warnings = Vec::new();
    let inputs = collect_endpoints(
        host.input_devices()
            .map(|devices| devices.map(describe).collect::<Vec<_>>())
            .unwrap_or_else(|error| vec![Err(audio_error(error))])
            .into_iter(),
        &mut warnings,
    );
    let outputs = collect_endpoints(
        host.output_devices()
            .map(|devices| devices.map(describe).collect::<Vec<_>>())
            .unwrap_or_else(|error| vec![Err(audio_error(error))])
            .into_iter(),
        &mut warnings,
    );
    Ok(Endpoints {
        inputs,
        outputs,
        warnings,
    })
}

fn configuration(device: &Device, input: bool) -> Result<SupportedStreamConfig> {
    let configs: Vec<_> = if input {
        device
            .supported_input_configs()
            .map_err(audio_error)?
            .collect()
    } else {
        device
            .supported_output_configs()
            .map_err(audio_error)?
            .collect()
    };
    configs
        .into_iter()
        .filter(|c| {
            c.channels() > 0
                && c.channels() <= MAX_DEVICE_CHANNELS
                && c.min_sample_rate() <= DEVICE_SAMPLE_RATE
                && c.max_sample_rate() >= DEVICE_SAMPLE_RATE
                && matches!(
                    c.sample_format(),
                    SampleFormat::F32 | SampleFormat::I16 | SampleFormat::I32
                )
        })
        .min_by_key(|c| (c.sample_format() != SampleFormat::F32, c.channels()))
        .map(|c| c.with_sample_rate(DEVICE_SAMPLE_RATE)).ok_or_else(|| Error::new(
        "This device has no supported 48 kHz PCM stream. Select 48000 Hz in Windows Sound settings or choose another device."))
}
fn input_stream<T>(
    device: &Device,
    config: StreamConfig,
    sender: SyncSender<Vec<f32>>,
    failed: Arc<AtomicBool>,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let callback_failed = failed.clone();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                for block in data.chunks(HOP * channels) {
                    let mono: Vec<f32> = block
                        .chunks_exact(channels)
                        .map(|frame| {
                            frame.iter().map(|v| v.to_sample::<f32>()).sum::<f32>()
                                / channels as f32
                        })
                        .collect();
                    if sender.try_send(mono).is_err() {
                        callback_failed.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            },
            move |_| {
                failed.store(true, Ordering::Relaxed);
            },
            Some(DEVICE_TIMEOUT),
        )
        .map_err(audio_error)
}
fn output_stream<T>(
    device: &Device,
    config: StreamConfig,
    receiver: Receiver<Vec<f32>>,
    failed: Arc<AtomicBool>,
    underruns: Arc<AtomicU64>,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let mut current = Vec::new().into_iter();
    let mut started = false;
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data, _| {
                for frame in data.chunks_exact_mut(channels) {
                    if current.len() == 0 {
                        match receiver.try_recv() {
                            Ok(block) => {
                                current = block.into_iter();
                                started = true;
                            }
                            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                                if started {
                                    underruns.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    let value = current.next().unwrap_or(0.0).clamp(-1.0, 1.0);
                    frame.fill(T::from_sample(value));
                }
            },
            move |_| {
                failed.store(true, Ordering::Relaxed);
            },
            Some(DEVICE_TIMEOUT),
        )
        .map_err(audio_error)
}

/// Process one explicit microphone and one explicit render endpoint.
/// The desktop owns this internal command's job and atomic processing-control file.
/// Failure ends this stream without selecting another device or changing defaults.
pub fn run(args: &[PathBuf]) -> Result<()> {
    if args.len() != 5 {
        return Err(Error::new("Invalid Windows audio worker arguments"));
    }
    let text = |i: usize| {
        args[i]
            .to_str()
            .ok_or_else(|| Error::new("Invalid device identifier"))
    };
    let input_id: DeviceId = text(1)?.parse().map_err(audio_error)?;
    let output_id: DeviceId = text(2)?.parse().map_err(audio_error)?;
    if input_id == output_id {
        return Err(Error::new("Input and output must be different endpoints"));
    }
    let host = cpal::default_host();
    let input = host
        .device_by_id(&input_id)
        .ok_or_else(|| Error::new("Selected microphone is unavailable"))?;
    let output = host
        .device_by_id(&output_id)
        .ok_or_else(|| Error::new("Selected output is unavailable"))?;
    let input_config = configuration(&input, true)?;
    let output_config = configuration(&output, false)?;
    let model = crate::load(&args[0])?;
    let selector = text(4)?;
    let mut network = Network::new(
        &model,
        Backend::amd((!selector.is_empty()).then_some(selector))?,
    )?;
    let mut processor = StreamProcessor::new(model.profile)?;
    let mut control = ProcessingControl::read(&args[3])?;
    processor.set_echo_reduction(control.echo_reduction)?;
    let (capture_send, capture_recv) = mpsc::sync_channel(AUDIO_QUEUE_BLOCKS);
    let (render_send, render_recv) = mpsc::sync_channel(AUDIO_QUEUE_BLOCKS);
    let failed = Arc::new(AtomicBool::new(false));
    let underruns = Arc::new(AtomicU64::new(0));
    macro_rules! capture {
        ($t:ty) => {
            input_stream::<$t>(&input, input_config.config(), capture_send, failed.clone())?
        };
    }
    let capture = match input_config.sample_format() {
        SampleFormat::F32 => capture!(f32),
        SampleFormat::I16 => capture!(i16),
        SampleFormat::I32 => capture!(i32),
        _ => return Err(Error::new("Unsupported capture format")),
    };
    macro_rules! render {
        ($t:ty) => {
            output_stream::<$t>(
                &output,
                output_config.config(),
                render_recv,
                failed.clone(),
                underruns.clone(),
            )?
        };
    }
    let render = match output_config.sample_format() {
        SampleFormat::F32 => render!(f32),
        SampleFormat::I16 => render!(i16),
        SampleFormat::I32 => render!(i32),
        _ => return Err(Error::new("Unsupported output format")),
    };
    // Playback starts with enough bounded silence for the model context.
    // The extra hops absorb scheduling differences between the device threads.
    // This is buffering, not fabricated signal for the denoised-output meter.
    render_send
        .send(vec![0.0; HOP * PLAYBACK_PRIME_HOPS])
        .map_err(|_| Error::new("Audio output closed"))?;
    render.play().map_err(audio_error)?;
    capture.play().map_err(audio_error)?;
    eprintln!(
        "{}",
        json!({"event":"started","adapter":network.backend.name(),"sample_rate":RATE})
    );
    let mut pending = VecDeque::new();
    let mut levels = Levels::default();
    let mut hops = 0u64;
    let mut last_capture = Instant::now();
    loop {
        if failed.load(Ordering::Relaxed) {
            return Err(Error::new(
                "Windows audio stopped or its bounded input queue overran. Restart with an available device.",
            ));
        }
        let block = match capture_recv.recv_timeout(CAPTURE_POLL_INTERVAL) {
            Ok(block) => {
                last_capture = Instant::now();
                block
            }
            Err(mpsc::RecvTimeoutError::Timeout) if last_capture.elapsed() < DEVICE_TIMEOUT => {
                continue;
            }
            Err(_) => return Err(Error::new("Microphone stopped delivering audio")),
        };
        pending.extend(block);
        while pending.len() >= HOP {
            if hops.is_multiple_of(CONTROL_INTERVAL_HOPS) {
                control = ProcessingControl::read(&args[3])?;
                processor.set_echo_reduction(control.echo_reduction)?;
            }
            let hop: Vec<_> = pending.drain(..HOP).collect();
            let processed = processor.push(&hop, control.strength, &mut |c| network.step(c))?;
            levels.add(&processed);
            if !processed.is_empty() {
                render_send
                    .try_send(processed)
                    .map_err(|_| Error::new("Processed audio output queue overran"))?;
            }
            hops += 1;
            if hops.is_multiple_of(CONTROL_INTERVAL_HOPS) {
                let (rms, peak) = levels.take();
                eprintln!(
                    "{}",
                    json!({"event":"levels","output_rms":rms,"output_peak":peak,
                    "underrun_samples":underruns.load(Ordering::Relaxed)})
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_endpoint_does_not_hide_healthy_microphones() {
        let devices = [
            Ok(Endpoint {
                id: "first".into(),
                name: "First".into(),
            }),
            Err(Error::new("Unavailable audio endpoint")),
            Ok(Endpoint {
                id: "second".into(),
                name: "Second".into(),
            }),
        ];
        let mut warnings = Vec::new();
        let result = collect_endpoints(devices.into_iter(), &mut warnings);
        assert_eq!(
            result
                .iter()
                .map(|endpoint| endpoint.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(warnings, ["Unavailable audio endpoint"]);
    }
}
