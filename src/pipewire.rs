//! PipeWire routing through system clients; temporary nodes follow the live process.
use crate::{Error, Result};
use nix::{
    poll::{PollFd, PollFlags, poll},
    sys::{
        signal::{SigSet, Signal},
        signalfd::{SfdFlags, SignalFd},
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    os::fd::AsFd,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const INPUT: &str = "clearwave.input";
const MICROPHONE: &str = "clearwave.microphone";
#[derive(Serialize)]
pub struct Node {
    pub id: u64,
    pub serial: u64,
    pub name: String,
    pub description: String,
    pub media_class: String,
    pub virtual_node: bool,
}
fn parse_nodes(bytes: &[u8]) -> Result<Vec<Node>> {
    let objects: Vec<Value> = serde_json::from_slice(bytes)?;
    Ok(objects
        .iter()
        .filter_map(|object| {
            if object.get("type")?.as_str()? != "PipeWire:Interface:Node" {
                return None;
            }
            let props = object.get("info")?.get("props")?;
            let name = props.get("node.name")?.as_str()?.to_string();
            Some(Node {
                id: object.get("id")?.as_u64()?,
                serial: props
                    .get("object.serial")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                description: props
                    .get("node.description")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_string(),
                media_class: props
                    .get("media.class")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                virtual_node: props
                    .get("node.virtual")
                    .is_some_and(|v| v.as_bool() == Some(true) || v.as_str() == Some("true")),
                name,
            })
        })
        .collect())
}
fn nodes() -> Result<Vec<Node>> {
    let result = Command::new("pw-dump")
        .output()
        .map_err(|e| Error::new(format!("Cannot run pw-dump: {e}")))?;
    if !result.status.success() {
        return Err(Error::new(format!(
            "PipeWire inspection failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    parse_nodes(&result.stdout)
}
pub fn sources() -> Result<Vec<Node>> {
    Ok(nodes()?
        .into_iter()
        .filter(|n| n.media_class.starts_with("Audio/Source"))
        .collect())
}
fn select_source(available: Vec<Node>, selector: &str) -> Result<Node> {
    let mut matches: Vec<_> = available
        .into_iter()
        .filter(|n| {
            n.media_class.starts_with("Audio/Source")
                && (n.name == selector
                    || n.id.to_string() == selector
                    || n.serial.to_string() == selector)
        })
        .collect();
    if matches.len() != 1 {
        return Err(Error::new(
            "Source selector must match exactly one name, ID, or serial from sources",
        ));
    }
    let source = matches.remove(0);
    if source.name.starts_with("clearwave.") || source.name.starts_with("voice-amd.") {
        return Err(Error::new(
            "Choose an input source other than the Clearwave virtual microphone",
        ));
    }
    Ok(source)
}

struct StopSignals {
    fd: SignalFd,
    previous: SigSet,
}
impl StopSignals {
    fn new() -> Result<Self> {
        let previous = SigSet::thread_get_mask().map_err(|e| Error::new(e.to_string()))?;
        let mut mask = SigSet::empty();
        mask.add(Signal::SIGINT);
        mask.add(Signal::SIGTERM);
        mask.thread_block().map_err(|e| Error::new(e.to_string()))?;
        match SignalFd::with_flags(&mask, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC) {
            Ok(fd) => Ok(Self { fd, previous }),
            Err(e) => {
                let _ = previous.thread_set_mask();
                Err(Error::new(e.to_string()))
            }
        }
    }
    fn stopped(&self, milliseconds: u16) -> Result<bool> {
        let mut descriptors = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
        poll(&mut descriptors, milliseconds).map_err(|e| Error::new(e.to_string()))?;
        Ok(self
            .fd
            .read_signal()
            .map_err(|e| Error::new(e.to_string()))?
            .is_some())
    }
}
impl Drop for StopSignals {
    fn drop(&mut self) {
        let _ = self.previous.thread_set_mask();
    }
}

struct Children(Vec<(&'static str, Child)>);
impl Children {
    fn add(&mut self, name: &'static str, command: &mut Command) -> Result<usize> {
        let child = command
            .spawn()
            .map_err(|e| Error::new(format!("Cannot start {name}: {e}")))?;
        let index = self.0.len();
        self.0.push((name, child));
        Ok(index)
    }
    fn check(&mut self) -> Result<()> {
        for (name, child) in &mut self.0 {
            if let Some(status) = child.try_wait()? {
                return Err(Error::new(format!(
                    "{name} stopped unexpectedly ({status})"
                )));
            }
        }
        Ok(())
    }
}
impl Drop for Children {
    fn drop(&mut self) {
        for (_, child) in &mut self.0 {
            let _ = child.kill();
        }
        for (_, child) in &mut self.0 {
            let _ = child.wait();
        }
    }
}
fn child_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    // GNU env restores child signal delivery after the parent's scoped signal mask.
    let mut command = Command::new("setpriv");
    command
        .args(["--pdeathsig", "KILL", "--", "env"])
        .arg("--default-signal=INT,TERM")
        .arg(program)
        .stdin(Stdio::null());
    command
}

pub struct LiveOptions {
    pub strength: f32,
    pub latency_ms: u16,
    pub adapter: Option<String>,
    pub seconds: Option<f64>,
    pub meters: bool,
    pub control_file: Option<PathBuf>,
}
impl LiveOptions {
    pub fn parse(args: &[PathBuf]) -> Result<Self> {
        let mut options = Self {
            strength: 1.0,
            latency_ms: 10,
            adapter: None,
            seconds: None,
            meters: false,
            control_file: None,
        };
        let mut seen = std::collections::HashSet::new();
        for pair in args.chunks(2) {
            if pair.len() != 2 {
                return Err(Error::new("Every live option requires a value"));
            }
            let key = pair[0]
                .to_str()
                .ok_or_else(|| Error::new("Live options must be UTF-8"))?;
            let value = pair[1]
                .to_str()
                .ok_or_else(|| Error::new("Live options must be UTF-8"))?;
            if !seen.insert(key) {
                return Err(Error::new("Duplicate live option"));
            }
            match key {
                "--strength" => {
                    options.strength = value.parse().map_err(|_| Error::new("Invalid strength"))?
                }
                "--latency-ms" => {
                    options.latency_ms = value.parse().map_err(|_| Error::new("Invalid latency"))?
                }
                "--adapter" => options.adapter = Some(value.to_string()),
                "--seconds" => {
                    options.seconds =
                        Some(value.parse().map_err(|_| Error::new("Invalid duration"))?)
                }
                "--meters" => {
                    options.meters = value
                        .parse()
                        .map_err(|_| Error::new("Meters must be true or false"))?
                }
                "--control-file" => options.control_file = Some(pair[1].clone()),
                _ => return Err(Error::new(format!("Unknown live option: {key}"))),
            }
        }
        crate::audio::validate_strength(options.strength)?;
        if !(5..=100).contains(&options.latency_ms) {
            return Err(Error::new("PipeWire latency request must be 5–100 ms"));
        }
        if options
            .seconds
            .is_some_and(|s| !s.is_finite() || !(0.1..=86400.0).contains(&s))
        {
            return Err(Error::new("Live duration must be 0.1–86400 seconds"));
        }
        Ok(options)
    }
}

pub fn live(bundle: &Path, selector: &str, options: LiveOptions) -> Result<()> {
    // Validate assets before making any audio nodes.
    crate::load(bundle)?;
    let available = nodes()?;
    if available
        .iter()
        .any(|n| n.name == INPUT || n.name == MICROPHONE || n.name == "voice-amd.microphone")
    {
        return Err(Error::new(
            "Clearwave audio nodes already exist; stop the other instance first",
        ));
    }
    let source = select_source(available, selector)?;
    if let Some(path) = &options.control_file {
        crate::controls::StrengthControl::read(path)?;
    }
    let signals = StopSignals::new()?;
    let mut children = Children(Vec::new());
    let capture_props = json!({"media.class":"Audio/Sink","node.name":INPUT,"node.description":"Clearwave Processing Input","node.virtual":true,"node.autoconnect":false,"priority.session":0});
    let playback_props = json!({"media.class":"Audio/Source","node.name":MICROPHONE,"node.description":format!("{} – Clearwave", source.description),"node.virtual":true,"node.autoconnect":false,"priority.session":0});
    children.add(
        "virtual microphone",
        child_command("pw-loopback").args([
            "--name",
            "clearwave",
            "--channels",
            "1",
            "--channel-map",
            "[MONO]",
            "--latency",
            &options.latency_ms.to_string(),
            "--capture-props",
            &capture_props.to_string(),
            "--playback-props",
            &playback_props.to_string(),
        ]),
    )?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let microphone = loop {
        if signals.stopped(20)? {
            return Ok(());
        }
        children.check()?;
        let current = nodes()?;
        if let Some(mic) = current.into_iter().find(|n| n.name == MICROPHONE) {
            break mic;
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "Timed out creating the PipeWire virtual microphone",
            ));
        }
    };
    let latency = format!("{}ms", options.latency_ms);
    let format = [
        "--raw",
        "--format",
        "f32",
        "--rate",
        "48000",
        "--channels",
        "1",
        "--channel-map",
        "MONO",
        "--latency",
        &latency,
    ];
    let capture_props = json!({"node.name":"clearwave.capture","node.dont-reconnect":true,"node.dont-fallback":true});
    let capture = children.add(
        "microphone capture",
        child_command("pw-record")
            .args(format)
            .arg("--target")
            .arg(&source.name)
            .arg("--properties")
            .arg(capture_props.to_string())
            .arg("-")
            .stdout(Stdio::piped()),
    )?;
    let input = children.0[capture]
        .1
        .stdout
        .take()
        .ok_or_else(|| Error::new("Missing capture pipe"))?;
    let mut processor = child_command(std::env::current_exe()?);
    processor
        .arg("stream")
        .arg(bundle)
        .arg(options.strength.to_string())
        .stdin(Stdio::from(input))
        .stdout(Stdio::piped());
    if let Some(adapter) = &options.adapter {
        processor.arg(adapter);
    }
    if options.meters {
        processor.env("CLEARWAVE_METERS", "1");
    }
    if let Some(path) = &options.control_file {
        processor.env("CLEARWAVE_CONTROL_FILE", path);
    }
    let processing = children.add("AMD stream processor", &mut processor)?;
    let output = children.0[processing]
        .1
        .stdout
        .take()
        .ok_or_else(|| Error::new("Missing processing pipe"))?;
    children.add("virtual microphone feed", child_command("pw-play").args(format).arg("--target").arg(INPUT).arg("--properties").arg(json!({"node.name":"clearwave.feed","node.dont-reconnect":true,"node.dont-fallback":true}).to_string()).arg("-").stdin(Stdio::from(output)))?;
    println!(
        "{}",
        json!({"event":"live_started","source":source,"virtual_microphone":microphone,"requested_latency_ms":options.latency_ms,"algorithmic_delay_ms":60,"make_default_command":"clearwave make-default-mic","default_changed":false})
    );
    let start = Instant::now();
    let mut last_check = Instant::now();
    loop {
        if signals.stopped(20)? {
            break;
        }
        if options
            .seconds
            .is_some_and(|s| start.elapsed().as_secs_f64() >= s)
        {
            break;
        }
        children.check()?;
        if last_check.elapsed() >= Duration::from_secs(1) {
            if !nodes()?
                .iter()
                .any(|n| n.name == source.name && n.serial == source.serial)
            {
                return Err(Error::new(
                    "Selected microphone disappeared; live processing stopped",
                ));
            }
            last_check = Instant::now();
        }
    }
    drop(children);
    println!(
        "{}",
        json!({"event":"live_stopped","default_changed":false})
    );
    Ok(())
}
pub fn make_default() -> Result<()> {
    let mic = sources()?
        .into_iter()
        .find(|n| n.name == MICROPHONE)
        .ok_or_else(|| {
            Error::new("Start live processing before selecting the virtual microphone as default")
        })?;
    let status = Command::new("wpctl")
        .arg("set-default")
        .arg(mic.id.to_string())
        .status()?;
    if !status.success() {
        return Err(Error::new(
            "WirePlumber could not select the default microphone",
        ));
    }
    println!(
        "{}",
        json!({"default_microphone":mic,"scope":"Default for new application streams; applications pinned to another input need explicit selection"})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_selection_rejects_feedback_and_non_sources() {
        let data = br#"[{"id":12,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"physical","media.class":"Audio/Source","object.serial":99}}},{"id":13,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"clearwave.microphone","media.class":"Audio/Source"}}},{"id":14,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"speaker","media.class":"Audio/Sink"}}}]"#;
        assert_eq!(
            select_source(parse_nodes(data).unwrap(), "99")
                .unwrap()
                .name,
            "physical"
        );
        assert!(select_source(parse_nodes(data).unwrap(), "13").is_err());
        assert!(select_source(parse_nodes(data).unwrap(), "speaker").is_err());
    }
    #[test]
    fn live_options_validate_bounds_and_duplicates() {
        let parse =
            |a: &[&str]| LiveOptions::parse(&a.iter().map(PathBuf::from).collect::<Vec<_>>());
        assert!(parse(&["--strength", "nan"]).is_err());
        assert!(parse(&["--latency-ms", "0"]).is_err());
        assert!(parse(&["--seconds", "inf"]).is_err());
        assert!(parse(&["--adapter"]).is_err());
        assert!(parse(&["--seconds", "1", "--seconds", "2"]).is_err());
        assert!(parse(&["--latency-ms", "10", "--strength", "0.5"]).is_ok());
    }
}
