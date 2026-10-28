//! Dispatch native desktop frontends, offline model tools, and streaming audio.
//! Shared loading and processing paths serve both Linux and Windows interfaces.
#![cfg_attr(all(windows, feature = "windows-gui"), windows_subsystem = "windows")]
mod audio;
mod broadcast;
mod broadcast_import;
mod conditioning;
mod controls;
mod dereverb;
#[cfg(target_os = "linux")]
mod desktop;
mod export;
mod fft;
#[cfg(target_os = "linux")]
mod folder_picker;
mod gpu;
mod i18n;
mod model_import;
mod model_source;
mod network;
mod operators;
mod package;
mod pe;
#[cfg(target_os = "linux")]
mod pipewire;
mod resident;
mod storage;
mod voice_balance;
mod wav;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
use windows::desktop;

use serde_json::json;
use std::{
    fmt,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Debug)]
pub struct Error(String);
impl Error {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<std::str::Utf8Error> for Error {
    fn from(e: std::str::Utf8Error) -> Self {
        Self(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, Error>;

const F32_BYTES: usize = size_of::<f32>();
const MAX_FEATURE_FILE_BYTES: usize = 64 * 1024 * 1024;
const STREAM_CONTROL_INTERVAL_HOPS: usize = 10;

const HELP: &str = concat!(
    "clearwave ",
    env!("CARGO_PKG_VERSION"),
    " — voice processing on AMD Vulkan

Commands:
  gui [BUNDLE_DIRECTORY] [--settings PREFERENCES.json]
  licenses
  import-model INSTALLER.exe CACHE_DIRECTORY
  extract INSTALLER.exe NEW_BUNDLE_DIRECTORY
  inspect BUNDLE_DIRECTORY
  export BUNDLE_DIRECTORY NEW_OUTPUT.safetensors
  gpu-info
  gpu-check BUNDLE_DIRECTORY [ADAPTER_NAME_SUBSTRING]
  graph BUNDLE_DIRECTORY
  operator-check BUNDLE_DIRECTORY [ADAPTER_NAME_SUBSTRING]
  infer-features BUNDLE_DIRECTORY INPUT.f32 NEW_OUTPUT.f32 [ADAPTER_NAME_SUBSTRING]
  infer-features-cpu BUNDLE_DIRECTORY INPUT.f32 NEW_OUTPUT.f32
  wav-features INPUT.wav NEW_FEATURES.f32 [BUNDLE_DIRECTORY]
  denoise BUNDLE_DIRECTORY INPUT.wav NEW_OUTPUT.wav [STRENGTH] [ADAPTER_NAME_SUBSTRING] [ECHO_REDUCTION]
  denoise-cpu BUNDLE_DIRECTORY INPUT.wav NEW_OUTPUT.wav [STRENGTH] [ECHO_REDUCTION]
  denoise-reference BUNDLE_DIRECTORY INPUT.wav NEW_OUTPUT.wav [STRENGTH] [ADAPTER_NAME_SUBSTRING] [ECHO_REDUCTION]
  denoise-reference-cpu BUNDLE_DIRECTORY INPUT.wav NEW_OUTPUT.wav [STRENGTH] [ECHO_REDUCTION]
  audio-bypass INPUT.wav NEW_OUTPUT.wav
  stream BUNDLE_DIRECTORY [STRENGTH] [ADAPTER_NAME_SUBSTRING]
  stream-cpu BUNDLE_DIRECTORY [STRENGTH]
  sources
  live BUNDLE_DIRECTORY SOURCE_NAME_OR_ID [--strength 0..1] [--latency-ms 5..100]
       [--adapter NAME] [--seconds DURATION]
       [--meters true|false] [--control-file STRENGTH.json]
  make-default-mic

Model locations accept a .safetensors file directly, or a folder containing
voice-weights.safetensors or model.safetensors. This mode needs no NVIDIA DLL.
Original 0.5.12.6 packages remain supported: denoiser_48k.wpkg and NVAudioEffects.dll
may be together in the selected folder, or under NvAFX/models and NvAFX/nv.
Broadcast 2.3 folders accept denoiser_48k.trtpkg and nvafxdenoiser.dll together.
Installer import supports RTX Voice 0.5.12.6 and Broadcast 2.3.0.71988672.
Broadcast import may download about 33 MB from NVIDIA if no local model cache exists.
GPU checks execute isolated real weight matrices and compare against CPU f64.
Feature inference reads 320 little-endian F32 values per frame and writes 961 raw mask values.
WAV commands accept 48000 Hz PCM16/24/32 or float32 mono/stereo input.
Stereo is averaged to mono. Output is mono float32 WAV with unchanged sample count.
Strength is 0 (bypass) to 1 (full, default). Vendor audio-output parity is pending.
WAV commands process offline; live creates a temporary PipeWire virtual microphone.
Stream commands read mono 48000 Hz little-endian F32 from stdin and write it to stdout.
Stream EOF flushes the tail; diagnostics go to stderr.
Live preserves system defaults; make-default-mic explicitly selects its virtual input.
On Windows, gui controls a capture APO attached to the existing microphone.
Run Setup once as administrator, then keep selecting the physical microphone in apps.
No Windows playback stream or virtual microphone is created. PipeWire commands are Linux-only.
"
);

fn main() -> ExitCode {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    #[cfg(windows)]
    let gui = args.is_empty() || args.first().is_some_and(|arg| arg.as_os_str() == "gui");

    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            #[cfg(windows)]
            if gui {
                windows::show_error(&e.to_string());
            }
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<PathBuf>) -> Result<()> {
    if args.is_empty() {
        return desktop::launch(&[]);
    }
    let cmd = args.first().and_then(|p| p.to_str()).unwrap_or("--help");
    match (cmd, args.len()) {
        ("--help" | "-h", 1) | ("--help", 0) => {
            print!("{HELP}");
            Ok(())
        }
        ("extract", 3) => extract(&args[1], &args[2]),
        ("import-model", 3) => {
            println!(
                "{}",
                model_import::prepare_in(&args[1], &args[2])?.display()
            );
            Ok(())
        }
        ("gui", 1..) => desktop::launch(&args[1..]),
        #[cfg(target_os = "linux")]
        ("__pick-model-folder", 5..=6) => folder_picker::run_helper(&args[1..]),
        #[cfg(windows)]
        ("__windows-pick-model-folder", 3..=4) => windows::pick_folder(
            &args[1],
            &args[2],
            args.get(3)
                .is_some_and(|mode| mode.as_os_str() == "installer"),
        ),
        #[cfg(windows)]
        ("__windows-devices", 1) => {
            println!("{}", serde_json::to_string(&windows::audio::endpoints()?)?);
            Ok(())
        }
        #[cfg(windows)]
        ("__windows-apo-worker", 5) => windows::audio::run(&args[1..]),
        ("licenses", 1) => {
            println!(
                "Clearwave\n\n{}\nFlag artwork: flag-icons — https://github.com/lipis/flag-icons\n\n{}",
                include_str!("../LICENSE"),
                include_str!("../ui/flags/LICENSE")
            );
            Ok(())
        }
        ("sources", 1) => {
            #[cfg(target_os = "linux")]
            println!("{}", serde_json::to_string_pretty(&pipewire::sources()?)?);
            #[cfg(windows)]
            println!(
                "{}",
                serde_json::to_string_pretty(&windows::audio::endpoints()?)?
            );
            Ok(())
        }
        #[cfg(target_os = "linux")]
        ("make-default-mic", 1) => pipewire::make_default(),
        #[cfg(target_os = "linux")]
        ("live", 3..) => pipewire::live(
            &args[1],
            args[2]
                .to_str()
                .ok_or_else(|| Error::new("Source selector must be UTF-8"))?,
            pipewire::LiveOptions::parse(&args[3..])?,
        ),
        #[cfg(windows)]
        ("live" | "make-default-mic", _) => Err(Error::new(
            "PipeWire commands are Linux-only. Use the Windows interface to attach the capture APO to your microphone.",
        )),
        ("wav-features", 3..=4) => wav_features(&args[1], &args[2], args.get(3)),
        ("audio-bypass", 3) => {
            require_new_output(&args[2])?;
            let input = wav::read(&args[1])?;
            let output = audio::Processor::new()?
                .process(&input.samples, 0.0, |_| Ok(vec![0.0; network::GAINS]))?;
            wav::write_new(&args[2], &output)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"samples":output.len(),"sample_rate":audio::RATE,"scope":"FFT and overlap reconstruction with unity gain"})
                )?
            );
            Ok(())
        }
        ("denoise" | "denoise-reference", 4..=7)
        | ("denoise-cpu" | "denoise-reference-cpu", 4..=6) => denoise(
            &args,
            cmd.ends_with("-cpu"),
            cmd.starts_with("denoise-reference"),
        ),
        ("stream", 2..=4) | ("stream-cpu", 2..=3) => stream_audio(&args, cmd == "stream-cpu"),
        ("inspect", 2) => {
            let m = load(&args[1])?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "format":m.format,"tensors":m.inventory(),"runtime_required":m.runtime_sha256.is_some(),
                    "tensor_count":m.tensors.len(),"total_parameters":m.tensors.iter().map(|t|t.values.len()).sum::<usize>(),
                    "package_sha256":m.package_sha256,"runtime_sha256":m.runtime_sha256,"payload_sha256":m.payload_sha256,
                    "waveform_processing_implemented":true,"feature_graph_implemented":true,"full_denoiser_implemented":false
                }))?
            );
            Ok(())
        }
        ("export", 3) => {
            let m = load(&args[1])?;
            export::safetensors(&m, &args[2])?;
            println!(
                "Exported {} tensors to {}",
                m.tensors.len(),
                args[2].display()
            );
            Ok(())
        }
        ("gpu-info", 1) => {
            println!("{}", serde_json::to_string_pretty(&gpu::adapters())?);
            Ok(())
        }
        ("gpu-check", 2..=3) => {
            let m = load(&args[1])?;
            let report = gpu::check_model(&m, adapter_selector(args.get(2))?)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.passed {
                Ok(())
            } else {
                Err(Error::new("GPU matrix comparison failed"))
            }
        }
        ("graph", 2) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&network::specification(&load(&args[1])?)?)?
            );
            Ok(())
        }
        ("operator-check", 2..=3) => {
            let model = load(&args[1])?;
            let report = network::check_operators(&model, adapter_selector(args.get(2))?)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.passed {
                Ok(())
            } else {
                Err(Error::new("Operator or feature-graph comparison failed"))
            }
        }
        ("infer-features", 4..=5) | ("infer-features-cpu", 4) => {
            let model = load(&args[1])?;
            let backend = if cmd == "infer-features-cpu" {
                network::Backend::Cpu
            } else {
                network::Backend::amd(adapter_selector(args.get(4))?)?
            };
            infer_features(&model, backend, &args[2], &args[3])
        }
        _ => Err(Error::new(format!("Invalid command or arguments.\n{HELP}"))),
    }
}

fn adapter_selector(value: Option<&PathBuf>) -> Result<Option<&str>> {
    value
        .map(|p| {
            p.to_str()
                .ok_or_else(|| Error::new("Adapter name must be UTF-8"))
        })
        .transpose()
}

fn require_new_output(path: &Path) -> Result<()> {
    if path.try_exists()? {
        return Err(Error::new("Output already exists; choose a new file path"));
    }
    Ok(())
}

fn wav_features(input: &Path, output: &Path, model: Option<&PathBuf>) -> Result<()> {
    use std::io::Write;

    require_new_output(output)?;
    let input = wav::read(input)?;
    let mut out = std::io::BufWriter::new(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?,
    );
    let profile = model
        .map(|path| load(path).map(|model| model.profile))
        .transpose()?
        .unwrap_or_default();
    let frames = audio::Processor::for_profile(profile)?.features(&input.samples, |values| {
        for value in values {
            out.write_all(&value.to_le_bytes())?;
        }
        Ok(())
    })?;
    out.flush()?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"frames":frames,"feature_bins":network::FEATURES,"sample_rate":audio::RATE,"hop_samples":audio::HOP,"first_frame_start_sample":-((audio::TRANSFORM - audio::HOP) as i64),"input_channels":input.channels,"scope":"48 kHz spectral features; zero-padded waveform boundaries"})
        )?
    );
    Ok(())
}

fn denoise(args: &[PathBuf], cpu: bool, reference: bool) -> Result<()> {
    require_new_output(&args[3])?;

    let strength = processing_strength(args.get(4))?;
    let echo_reduction = match args.get(if cpu { 5 } else { 6 }) {
        Some(value) => processing_strength(Some(value))?,
        None => 0.0,
    };
    let input = wav::read(&args[2])?;
    let model = load(&args[1])?;
    let backend = if cpu {
        network::Backend::Cpu
    } else {
        network::Backend::amd(adapter_selector(args.get(5))?)?
    };

    let mut network = network::Network::new(&model, backend)?;
    let processor = audio::Processor::for_profile(model.profile)?;
    let start = std::time::Instant::now();
    let mut output =
        processor.process_with_echo(&input.samples, strength, echo_reduction, |context| {
            network.step(context)
        })?;
    if !reference {
        voice_balance::VoiceBalance::default().process(&mut output)?;
    }
    let elapsed = start.elapsed().as_secs_f64();
    wav::write_new(&args[3], &output)?;
    let rms =
        |x: &[f32]| (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "sample_rate":audio::RATE,"samples":output.len(),"input_channels":input.channels,"output_channels":1,
            "frames":network.steps(),"initial_context_predictions":network.steps().min(audio::lookahead_hops(model.profile)),"adapter":network.backend.name(),"strength":strength,"echo_reduction":echo_reduction,
            "execution_mode":network.execution_mode(),"resident_parameter_bytes":network.resident_parameter_bytes(),
            "voice_balance":if reference { "none" } else { voice_balance::PROFILE },
            "audio_seconds":output.len() as f64 / audio::RATE as f64,"elapsed_ms":elapsed*1000.0,
            "processing_seconds_per_audio_second":elapsed / (output.len() as f64 / audio::RATE as f64),
            "input_rms":rms(&input.samples),"output_rms":rms(&output),
            "output_samples_above_full_scale":output.iter().filter(|v| v.abs()>1.0).count(),
            "scope":"offline waveform processing; vendor output parity, speech quality, and real-time latency are not established"
        }))?
    );
    Ok(())
}

fn processing_strength(value: Option<&PathBuf>) -> Result<f32> {
    let strength = match value.and_then(|p| p.to_str()) {
        Some(s) => s
            .parse::<f32>()
            .map_err(|_| Error::new("Strength must be a number from 0 to 1"))?,
        None if value.is_some() => return Err(Error::new("Strength must be UTF-8")),
        None => 1.0,
    };
    audio::validate_strength(strength)?;
    Ok(strength)
}

fn stream_audio(args: &[PathBuf], cpu: bool) -> Result<()> {
    use std::io::{Read, Write};
    let mut strength = processing_strength(args.get(2))?;
    let control = std::env::var_os("CLEARWAVE_CONTROL_FILE").map(PathBuf::from);
    let meters = std::env::var_os("CLEARWAVE_METERS").is_some_and(|v| v == "1");
    let model = load(&args[1])?;
    let backend = if cpu {
        network::Backend::Cpu
    } else {
        network::Backend::amd(adapter_selector(args.get(3))?)?
    };

    let mut network = network::Network::new(&model, backend)?;
    let mut processor = audio::StreamProcessor::microphone(model.profile)?;
    eprintln!(
        "{}",
        json!({"event":"stream_ready","adapter":network.backend.name(),"algorithmic_delay_samples":audio::algorithmic_delay_samples(model.profile),"sample_rate":audio::RATE,"voice_balance":voice_balance::PROFILE})
    );

    let mut reader = std::io::stdin().lock();
    let mut writer =
        std::io::BufWriter::with_capacity(audio::HOP * F32_BYTES, std::io::stdout().lock());
    let mut predict = |context: &[f32]| network.step(context);
    let mut bytes = [0u8; audio::HOP * F32_BYTES];
    let mut hops = 0usize;
    let mut input_levels = controls::Levels::default();
    let mut output_levels = controls::Levels::default();
    let start = std::time::Instant::now();

    loop {
        let mut used = 0;
        while used < bytes.len() {
            match reader.read(&mut bytes[used..]) {
                Ok(0) => break,
                Ok(count) => used += count,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        if used == 0 {
            break;
        }
        if used % F32_BYTES != 0 {
            return Err(Error::new("Raw audio ended inside a float32 sample"));
        }
        let samples: Vec<_> = bytes[..used]
            .as_chunks::<F32_BYTES>()
            .0
            .iter()
            .copied()
            .map(f32::from_le_bytes)
            .collect();
        if hops.is_multiple_of(STREAM_CONTROL_INTERVAL_HOPS)
            && let Some(path) = &control
        {
            match controls::ProcessingControl::read(path) {
                Ok(next) => {
                    strength = next.strength;
                    processor.set_echo_reduction(next.echo_reduction)?;
                    processor.set_bass(next.bass_db)?;
                }
                Err(e) => eprintln!(
                    "{}",
                    json!({"event":"control_error","message":e.to_string()})
                ),
            }
        }
        let output = processor.push(&samples, strength, &mut predict)?;
        if meters {
            input_levels.add(&samples);
            output_levels.add(&output);
        }
        for value in output {
            writer.write_all(&value.to_le_bytes())?;
        }
        writer.flush()?;
        hops += 1;
        if meters && hops.is_multiple_of(STREAM_CONTROL_INTERVAL_HOPS) {
            let (input_rms, input_peak) = input_levels.take();
            let (output_rms, output_peak) = output_levels.take();
            eprintln!(
                "{}",
                json!({"event":"levels","input_rms":input_rms,"input_peak":input_peak,"output_rms":output_rms,"output_peak":output_peak,"strength":strength,"samples":processor.samples()})
            );
        }
    }
    for value in processor.finish(strength, &mut predict)? {
        writer.write_all(&value.to_le_bytes())?;
    }
    writer.flush()?;
    eprintln!(
        "{}",
        serde_json::to_string(
            &json!({"event":"stream_finished","samples":processor.samples(),"frames":network.steps(),"initial_context_predictions":network.steps().min(audio::lookahead_hops(model.profile)),"adapter":network.backend.name(),"execution_mode":network.execution_mode(),"resident_parameter_bytes":network.resident_parameter_bytes(),"algorithmic_delay_samples":audio::algorithmic_delay_samples(model.profile),"sample_rate":audio::RATE,"wall_elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"scope":"bounded raw float32 stream; wall time includes input/output waits; PipeWire routing is not created"})
        )?
    );
    Ok(())
}

fn infer_features(
    model: &package::Model,
    backend: network::Backend,
    input: &Path,
    output: &Path,
) -> Result<()> {
    use std::io::{BufWriter, Write};
    let bytes = storage::bounded_read(input, MAX_FEATURE_FILE_BYTES)?;
    if bytes.is_empty() || bytes.len() % (network::FEATURES * F32_BYTES) != 0 {
        return Err(Error::new(
            "Input must contain complete frames of 320 little-endian F32 values",
        ));
    }
    let features: Vec<f32> = bytes
        .as_chunks::<F32_BYTES>()
        .0
        .iter()
        .copied()
        .map(f32::from_le_bytes)
        .collect();
    if features.iter().any(|x| !x.is_finite()) {
        return Err(Error::new("Feature file contains non-finite values"));
    }

    let frames = features.len() / network::FEATURES;
    let mut net = network::Network::new(model, backend)?;
    let mut writer = BufWriter::new(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?,
    );
    let start = std::time::Instant::now();
    for frame in 0..frames {
        let mut context = [0.0; network::CONTEXT * network::FEATURES];
        for i in 0..network::CONTEXT {
            let source = frame as isize + i as isize - (network::CONTEXT / 2) as isize;
            if (0..frames as isize).contains(&source) {
                let source = source as usize * network::FEATURES;
                context[i * network::FEATURES..(i + 1) * network::FEATURES]
                    .copy_from_slice(&features[source..source + network::FEATURES]);
            }
        }
        for value in net.step(&context)? {
            writer.write_all(&value.to_le_bytes())?;
        }
    }
    writer.flush()?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"frames":frames,"feature_bins":network::FEATURES,"gain_bins":network::GAINS,"adapter":net.backend.name(),"execution_mode":net.execution_mode(),"resident_parameter_bytes":net.resident_parameter_bytes(),"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"scope":"raw feature-level noise-mask inference; vendor audio-output parity pending"})
        )?
    );
    Ok(())
}

fn load(bundle: &Path) -> Result<package::Model> {
    model_source::ModelSource::resolve(bundle)?.load()
}
fn extract(installer: &Path, destination: &Path) -> Result<()> {
    model_import::extract(installer, destination)?;
    println!("Bundle ready: {} tensors", load(destination)?.tensors.len());
    Ok(())
}
