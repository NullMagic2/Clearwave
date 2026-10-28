//! Explicit elevated microphone setup with bounded, asynchronous result reporting.
//! The desktop polls one result; UAC, PowerShell and process waits stay on a worker.
//! Setup records its result in Program Files, without changing audio protection.
use super::{Apartment, os_error, wide};
use crate::{Error, Result, storage::bounded_read};
use clearwave_apo::protocol;
use serde::Deserialize;
use std::{
    ffi::OsString,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HANDLE, HWND, MAX_PATH, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            SystemInformation::GetSystemDirectoryW,
            Threading::{GetExitCodeProcess, WaitForSingleObject},
        },
        UI::{
            Shell::{
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
                ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{PCWSTR, w},
};

const MAX_REPORT_BYTES: usize = 16 * 1024;
const SETUP_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_MILLISECONDS: u32 = 50;
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Installed,
    Cancelled,
}
#[derive(Deserialize)]
struct Report {
    request_id: String,
    success: bool,
    message: String,
}

pub struct SetupTask {
    result: Receiver<Result<Outcome>>,
    cancelled: Arc<AtomicBool>,
}
impl SetupTask {
    pub fn start(endpoint: &str, owner: HWND) -> Result<Self> {
        let guid = protocol::endpoint_guid(endpoint)
            .ok_or_else(|| Error::new("Choose a microphone first"))?;
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
        let request = format!(
            "{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| Error::new(e.to_string()))?
                .as_nanos()
        );
        let report = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .ok_or_else(|| Error::new("Windows Program Files is unavailable"))?
            .join("Clearwave")
            .join("APO")
            .join(format!("setup-{guid}.json"));
        let owner = owner.0 as usize;
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let (sender, result) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("microphone-setup".into()).spawn(move || {
            let outcome = (|| {
                let _apartment = Apartment::new()?;
                let parameters = wide(format!(
                    "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\" -Endpoint \"{}\" -Action Install -RequestId \"{}\"",
                    script.display(), guid, request));
                let executable = wide(powershell()?);
                let mut info = SHELLEXECUTEINFOW {
                    cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
                    fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
                    hwnd: HWND(owner as *mut _), lpVerb: w!("runas"),
                    lpFile: PCWSTR(executable.as_ptr()), lpParameters: PCWSTR(parameters.as_ptr()),
                    nShow: SW_HIDE.0, ..Default::default()
                };
                // SAFETY: all launch strings and the structure outlive ShellExecuteEx.
                // The returned process handle transfers exactly once into an RAII owner.
                // Only an explicit desktop action starts this elevated packaged script.
                let process = unsafe {
                    match ShellExecuteExW(&mut info) {
                        Ok(()) => {},
                        Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(Outcome::Cancelled),
                        Err(e) => return Err(os_error(e)),
                    }
                    if info.hProcess.is_invalid() { return Err(Error::new("Setup did not return a process handle")); }
                    OwnedHandle::from_raw_handle(info.hProcess.0)
                };
                let code = wait(HANDLE(process.as_raw_handle()), SETUP_TIMEOUT, &stop)?;
                let bytes = bounded_read(&report, MAX_REPORT_BYTES)
                    .map_err(|_| Error::new(format!("Setup exited with code {code} without a readable result. Check that the full package is extracted and PowerShell is permitted.")))?;
                verify_report(&bytes, &request, code)?;
                if !super::audio::installed(&guid) {
                    return Err(Error::new("Setup finished, but the microphone registration could not be verified"));
                }
                Ok(Outcome::Installed)
            })();
            let _ = sender.send(outcome);
        })?;
        Ok(Self { result, cancelled })
    }
    pub fn poll(&self) -> Option<Result<Outcome>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(Error::new("Microphone setup monitor stopped")))
            }
        }
    }
}
impl Drop for SetupTask {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
fn powershell() -> Result<PathBuf> {
    let mut directory = [0u16; MAX_PATH as usize];
    // SAFETY: Windows writes within this fixed UTF-16 slice and returns its length.
    // Resolve the system executable directly instead of searching the user's PATH.
    // Its final path stays owned until ShellExecuteEx has copied the command.
    let length = unsafe { GetSystemDirectoryW(Some(&mut directory)) } as usize;
    if length == 0 || length >= directory.len() {
        return Err(Error::new("Cannot locate Windows PowerShell"));
    }
    Ok(PathBuf::from(OsString::from_wide(&directory[..length]))
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe"))
}
fn wait(process: HANDLE, timeout: Duration, cancelled: &AtomicBool) -> Result<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::new("Setup monitor closed"));
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                "Microphone setup timed out. An elevated setup already running may still finish.",
            ));
        }
        // SAFETY: the caller owns or borrows a live process handle throughout this loop.
        // Every wait is bounded and runs exclusively on the setup worker thread.
        // Dropping the monitor never terminates a partially applied registry transaction.
        match unsafe { WaitForSingleObject(process, POLL_MILLISECONDS) } {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                unsafe { GetExitCodeProcess(process, &mut code) }.map_err(os_error)?;
                return Ok(code);
            }
            WAIT_TIMEOUT => {}
            _ => return Err(Error::new("Cannot monitor microphone setup")),
        }
    }
}
fn verify_report(bytes: &[u8], request: &str, code: u32) -> Result<()> {
    let report: Report = serde_json::from_slice(bytes)?;
    if report.request_id != request {
        return Err(Error::new("Setup did not return a result for this request"));
    }
    if !report.success || code != 0 {
        return Err(Error::new(report.message));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn results_reject_stale_failed_and_incomplete_setup() {
        let report = br#"{"request_id":"current","success":true,"message":"configured"}"#;
        verify_report(report, "current", 0).unwrap();
        assert!(verify_report(report, "old", 0).is_err());
        assert!(verify_report(report, "current", 1).is_err());
        assert!(verify_report(b"{}", "current", 0).is_err());
        let failed = br#"{"request_id":"current","success":false,"message":"Access denied to microphone settings"}"#;
        assert!(
            verify_report(failed, "current", 1)
                .unwrap_err()
                .to_string()
                .contains("Access denied")
        );
    }
    #[test]
    fn process_monitor_reports_exit_and_bounds_waiting_and_cancellation() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "exit /b 17"])
            .spawn()
            .unwrap();
        let stop = AtomicBool::new(false);
        let code = wait(HANDLE(child.as_raw_handle()), Duration::from_secs(5), &stop).unwrap();
        assert_eq!(code, 17);
        child.wait().unwrap();
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle for this live test.
        // Waiting with a deadline exercises a stall without starting another process.
        // The pseudo-handle is borrowed and must never be closed by the test.
        let current = unsafe { windows::Win32::System::Threading::GetCurrentProcess() };
        assert!(wait(current, Duration::from_millis(1), &stop).is_err());
        stop.store(true, Ordering::Release);
        assert!(wait(current, Duration::from_secs(5), &stop).is_err());
        assert!(powershell().unwrap().ends_with("powershell.exe"));
    }
}
