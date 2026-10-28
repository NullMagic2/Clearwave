//! Enumerate existing capture endpoints and serve the capture APO's PCM packets.
//! The worker owns model inference; no WASAPI capture or render stream is opened.
//! Stopping the desktop-owned process makes the APO return time-aligned dry audio.
use super::{Apartment, os_error, wide};
use crate::{
    Error, Result,
    audio::StreamProcessor,
    controls::{Levels, ProcessingControl},
    network::{Backend, Network},
};
use ::windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, STGM_READ,
                StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
            },
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
        },
    },
    core::PCWSTR,
};
use clearwave_apo::{
    pipe::Pipe,
    protocol::{self, HOP, Packet},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const POLL: Duration = Duration::from_millis(10);
const CONTROL_HOPS: usize = 10;
#[derive(Clone, Deserialize, Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub apo_installed: bool,
}
#[derive(Deserialize, Serialize)]
pub struct Endpoints {
    pub inputs: Vec<Endpoint>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub fn installed(endpoint: &str) -> bool {
    let Some(guid) = protocol::endpoint_guid(endpoint) else {
        return false;
    };
    let key = wide(format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{guid}\FxProperties"
    ));
    let property = wide(protocol::MODE_PROPERTY);
    let mut value = [0u16; 80];
    let mut bytes = size_of_val(&value) as u32;
    // SAFETY: the registry call writes only within the fixed UTF-16 buffer.
    // This reads one capture endpoint's effect class without changing settings.
    // An inaccessible or differently configured endpoint is treated as unattached.
    unsafe {
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(property.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
        .is_err()
        {
            return false;
        }
    }
    let length = value.iter().position(|v| *v == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length]).eq_ignore_ascii_case(protocol::CLSID_TEXT)
}
pub fn endpoints() -> Result<Endpoints> {
    let _apartment = Apartment::new()?;
    let mut result = Endpoints {
        inputs: Vec::new(),
        warnings: Vec::new(),
    };
    // SAFETY: all COM objects are created and released on this initialized thread.
    // Endpoint strings and variants are copied before freeing Windows allocations.
    // Only active capture endpoints are enumerated; no audio stream is started.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER).map_err(os_error)?;
        let devices = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .map_err(os_error)?;
        for index in 0..devices.GetCount().map_err(os_error)? {
            let read = || -> Result<Endpoint> {
                let device = devices.Item(index).map_err(os_error)?;
                let raw = device.GetId().map_err(os_error)?;
                let id = raw
                    .to_string()
                    .map_err(|error| Error::new(error.to_string()));
                CoTaskMemFree(Some(raw.0.cast()));
                let id = id?;
                let store = device.OpenPropertyStore(STGM_READ).map_err(os_error)?;
                let mut value = store
                    .GetValue(&PKEY_Device_FriendlyName)
                    .map_err(os_error)?;
                let raw = PropVariantToStringAlloc(&value);
                let _ = PropVariantClear(&mut value);
                let raw = raw.map_err(os_error)?;
                let name = raw
                    .to_string()
                    .map_err(|error| Error::new(error.to_string()));
                CoTaskMemFree(Some(raw.0.cast()));
                Ok(Endpoint {
                    apo_installed: installed(&id),
                    id,
                    name: name?,
                })
            };
            match read() {
                Ok(endpoint) => result.inputs.push(endpoint),
                Err(error) => result.warnings.push(error.to_string()),
            }
        }
    }
    Ok(result)
}

struct ClientCount(Arc<AtomicUsize>);
impl Drop for ClientCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
pub fn run(args: &[PathBuf]) -> Result<()> {
    if args.len() != 4 {
        return Err(Error::new("Invalid Windows APO worker arguments"));
    }
    let endpoint = args[1]
        .to_str()
        .ok_or_else(|| Error::new("Invalid capture endpoint"))?;
    if !installed(endpoint) {
        return Err(Error::new(
            "Attach Clearwave to this microphone using Setup, then reopen the recording application",
        ));
    }
    let name =
        protocol::pipe_name(endpoint).ok_or_else(|| Error::new("Invalid capture endpoint GUID"))?;
    let model = Arc::new(crate::load(&args[0])?);
    let selector = args[3]
        .to_str()
        .ok_or_else(|| Error::new("Invalid adapter name"))?
        .to_owned();
    let clients = Arc::new(AtomicUsize::new(0));
    let mut listener = Pipe::server(&name, true)?;
    eprintln!(
        "{}",
        json!({"event":"started", "transport":"capture-apo", "sample_rate":protocol::RATE, "latency_samples":protocol::LATENCY})
    );
    loop {
        if !listener.connected()? {
            std::thread::sleep(POLL);
            continue;
        }
        let pipe = listener;
        listener = Pipe::server(&name, false)?;
        if clients.load(Ordering::Acquire) >= (clearwave_apo::pipe::MAX_CLIENTS - 1) as usize {
            drop(pipe);
            continue;
        }
        clients.fetch_add(1, Ordering::AcqRel);
        let count = ClientCount(clients.clone());
        let model = model.clone();
        let selector = selector.clone();
        let control = args[2].clone();
        std::thread::Builder::new()
            .name("windows-apo-model".into())
            .spawn(move || {
                let _count = count;
                if let Err(error) = process_client(pipe, &model, &selector, &control) {
                    eprintln!(
                        "{}",
                        json!({"event":"apo_client_stopped", "message":error.to_string()})
                    );
                }
            })?;
    }
}
fn process_client(
    mut pipe: Pipe,
    model: &crate::package::Model,
    selector: &str,
    control_path: &std::path::Path,
) -> Result<()> {
    let mut network = Network::new(
        model,
        Backend::amd((!selector.is_empty()).then_some(selector))?,
    )?;
    let mut processor = StreamProcessor::microphone(model.profile)?;
    let stop = AtomicBool::new(false);
    let mut expected = protocol::NO_AUDIO;
    let mut emitted = 0u64;
    let mut hops = 0usize;
    let mut control = ProcessingControl::read(control_path)?;
    let mut levels = Levels::default();
    let mut pending = std::collections::VecDeque::new();
    loop {
        let mut session = pipe.session(&stop);
        let input = Packet::read(&mut session)?;
        if input.sequence == protocol::NO_AUDIO || input.sequence > u64::MAX - HOP as u64 {
            return Err(Error::new("Invalid APO sequence"));
        }
        if input.sequence != expected {
            network.reset();
            processor = StreamProcessor::microphone(model.profile)?;
            emitted = input.sequence;
            pending.clear();
        }
        if hops.is_multiple_of(CONTROL_HOPS) {
            control = ProcessingControl::read(control_path)?;
        }
        processor.set_echo_reduction(control.echo_reduction)?;
        let output = processor.push(&input.samples, control.strength, &mut |features| {
            network.step(features)
        })?;
        expected = input.sequence + HOP as u64;
        let mut response = Packet::default();
        pending.extend(output);
        if pending.len() >= HOP {
            response.sequence = emitted;
            for value in &mut response.samples {
                *value = pending
                    .pop_front()
                    .ok_or_else(|| Error::new("Incomplete APO output packet"))?;
            }
            emitted += HOP as u64;
            levels.add(&response.samples);
        }
        response.write(&mut session)?;
        hops += 1;
        if hops.is_multiple_of(CONTROL_HOPS) {
            let (rms, peak) = levels.take();
            eprintln!(
                "{}",
                json!({"event":"levels", "output_rms":rms, "output_peak":peak})
            );
        }
    }
}

pub fn setup(endpoint: &str) -> Result<()> {
    let guid =
        protocol::endpoint_guid(endpoint).ok_or_else(|| Error::new("Choose a microphone first"))?;
    let directory = std::env::current_exe()?
        .parent()
        .ok_or_else(|| Error::new("Missing application directory"))?
        .to_path_buf();
    let script = directory.join("setup-apo.ps1");
    if !script.is_file() {
        return Err(Error::new(
            "setup-apo.ps1 is missing. Extract the complete Windows package.",
        ));
    }
    let parameters = wide(format!(
        "-NoProfile -NoExit -File \"{}\" -Endpoint \"{}\" -Action Install",
        script.display(),
        guid
    ));
    // SAFETY: Windows owns the elevated setup process and its confirmation dialog.
    // Arguments contain a validated GUID and a quoted Windows file path only.
    // Setup runs explicitly from the button, never during discovery or capture.
    unsafe {
        use ::windows::{
            Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
            core::w,
        };
        let result = ShellExecuteW(
            None,
            w!("runas"),
            w!("powershell.exe"),
            PCWSTR(parameters.as_ptr()),
            None,
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            return Err(Error::new(
                "Microphone setup was cancelled or could not start",
            ));
        }
    }
    Ok(())
}
