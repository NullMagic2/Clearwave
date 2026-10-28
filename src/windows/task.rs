//! Cancellable bounded JSON commands isolate device and Vulkan driver discovery.
//! Parent UI code polls messages only; subprocess waits and teardown stay on workers.
//! Each task owns a kill-on-close Windows job and a fixed discovery deadline.
use super::OwnedChild;
use crate::{Error, Result};
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

const MAX_DISCOVERY_BYTES: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);

pub struct JsonTask {
    cancelled: Arc<AtomicBool>,
    result: Receiver<Result<Value>>,
}
impl JsonTask {
    pub fn start(arguments: &[&str], timeout: Duration) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let arguments: Vec<_> = arguments.iter().map(|value| value.to_string()).collect();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let (sender, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("windows-discovery".into())
            .spawn(move || {
                let mut command = Command::new(executable);
                command
                    .args(arguments)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                let outcome = run(&mut command, timeout, &stop);
                let _ = sender.send(outcome);
            })?;
        Ok(Self { cancelled, result })
    }
    pub fn poll(&self) -> Option<Result<Value>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(Error::new("Windows discovery worker stopped")))
            }
        }
    }
}
impl Drop for JsonTask {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
fn run(command: &mut Command, timeout: Duration, cancelled: &AtomicBool) -> Result<Value> {
    let mut child = OwnedChild::spawn_background(command)?;
    let output = child
        .process()?
        .stdout
        .take()
        .ok_or_else(|| Error::new("Missing discovery output"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = output
            .take((MAX_DISCOVERY_BYTES + 1) as u64)
            .read_to_end(&mut bytes);
        let _ = sender.send(result.map(|_| bytes));
    });
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::new("Windows discovery cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "Windows device or graphics discovery timed out. Check the device and AMD Vulkan driver, then refresh.",
            ));
        }
        if let Some(status) = child.process()?.try_wait()? {
            if !status.success() {
                return Err(Error::new(format!("Windows discovery failed ({status})")));
            }
            let bytes = match receiver.try_recv() {
                Ok(bytes) => bytes?,
                Err(mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Error::new("Windows discovery output did not close"));
                }
            };
            if bytes.len() > MAX_DISCOVERY_BYTES {
                return Err(Error::new(
                    "Windows discovery output exceeds its size limit",
                ));
            }
            return serde_json::from_slice(&bytes).map_err(Error::from);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_TIMEOUT: Duration = Duration::from_millis(150);
    const TEST_MAX_WAIT: Duration = Duration::from_secs(2);

    fn command(script: &str) -> Command {
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }
    #[test]
    fn discovery_reads_json_and_bounds_stalled_or_cancelled_helpers() {
        let result = run(
            &mut command("echo null"),
            TEST_MAX_WAIT,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(result.is_null());
        let start = Instant::now();
        let error = run(
            &mut command("set /p answer="),
            TEST_TIMEOUT,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < TEST_MAX_WAIT);
        let error = run(
            &mut command("set /p answer="),
            TEST_MAX_WAIT,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
}
