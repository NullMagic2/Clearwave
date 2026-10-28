//! A cancellable worker owns the GTK chooser helper. No GTK objects cross threads.
use crate::{
    Error, Result,
    controls::MAX_PICKER_RESPONSE_BYTES,
    i18n::{Language, Text},
};
use gtk::prelude::*;
use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub struct FolderPicker {
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Option<PathBuf>>>,
}
impl FolderPicker {
    pub fn start(initial: PathBuf, language: Language, installer: bool) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let (sender, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("model-folder-picker".into())
            .spawn(move || {
                let outcome = (|| {
                    let child = Command::new("setpriv")
                        .args([
                            "--pdeathsig",
                            "KILL",
                            "--",
                            "env",
                            "--default-signal=INT,TERM",
                        ])
                        .arg(executable)
                        .arg("__pick-model-folder")
                        .arg(initial)
                        .arg(language.text(if installer {
                            Text::InstallerTitle
                        } else {
                            Text::FolderTitle
                        }))
                        .arg(language.text(Text::Select))
                        .arg(language.text(Text::Cancel))
                        .arg(if installer { "installer" } else { "folder" })
                        .env("GIO_USE_VFS", "local")
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()?;
                    wait_for_selection(child, &cancelled)
                })();
                let _ = sender.send(outcome);
            })?;
        Ok(Self { cancel, result })
    }
    pub fn poll(&self) -> Option<Result<Option<PathBuf>>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(Error::new("Folder picker worker stopped")))
            }
        }
    }
}
impl Drop for FolderPicker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
fn wait_for_selection(mut child: Child, cancel: &AtomicBool) -> Result<Option<PathBuf>> {
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(Error::new(format!("Folder picker exited ({status})")));
                }
                // The parent reads only after helper exit, so the full JSON response must fit
                // inside the pipe without waiting for the parent to drain it first.
                // Linux guarantees at least 4096 bytes of pipe capacity for this protocol.
                let mut response = Vec::new();
                child
                    .stdout
                    .take()
                    .ok_or_else(|| Error::new("Missing folder picker output"))?
                    .take((MAX_PICKER_RESPONSE_BYTES + 1) as u64)
                    .read_to_end(&mut response)?;
                return decode_response(&response);
            }
            Ok(None) => std::thread::sleep(CHILD_POLL_INTERVAL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
    }
}
fn decode_response(response: &[u8]) -> Result<Option<PathBuf>> {
    crate::controls::decode_picker_response(response)
}

pub fn run_helper(args: &[PathBuf]) -> Result<()> {
    let [initial, title, accept, cancel, ..] = args else {
        return Err(Error::new("Invalid folder picker arguments"));
    };
    let installer = args
        .get(4)
        .is_some_and(|mode| mode.as_os_str() == "installer");
    gtk::init().map_err(|e| Error::new(format!("Cannot open GTK3 folder picker: {e}")))?;
    let chooser = gtk::FileChooserDialog::with_buttons(
        Some(&title.to_string_lossy()),
        None::<&gtk::Window>,
        if installer {
            gtk::FileChooserAction::Open
        } else {
            gtk::FileChooserAction::SelectFolder
        },
        &[
            (cancel.to_string_lossy().as_ref(), gtk::ResponseType::Cancel),
            (accept.to_string_lossy().as_ref(), gtk::ResponseType::Accept),
        ],
    );
    chooser.set_local_only(true);
    chooser.set_create_folders(false);
    chooser.set_default_response(gtk::ResponseType::Accept);
    if installer {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("*.exe"));
        filter.add_pattern("*.exe");
        filter.add_pattern("*.EXE");
        chooser.add_filter(filter);
    }
    if !initial.as_os_str().is_empty() && initial.is_dir() {
        chooser.set_current_folder(initial);
    } else {
        chooser.set_current_folder(gtk::glib::home_dir());
    }
    let response = chooser.run();
    let path = if response == gtk::ResponseType::Accept {
        chooser.filename()
    } else {
        None
    };
    chooser.hide();
    crate::model_import::report_selection(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picker_protocol_accepts_local_paths_and_cancellation() {
        assert_eq!(
            decode_response(br#""/tmp/model bundle""#).unwrap(),
            Some(PathBuf::from("/tmp/model bundle"))
        );
        assert_eq!(decode_response(b"null\n").unwrap(), None);
        for invalid in [
            br#""relative/path""#.as_slice(),
            br#""""#,
            br#""/tmp/\u0000""#,
            b"not json",
        ] {
            assert!(decode_response(invalid).is_err());
        }
        assert!(decode_response(&vec![b' '; MAX_PICKER_RESPONSE_BYTES + 1]).is_err());
    }
    #[test]
    fn cancelling_picker_reaps_the_owned_child() {
        let child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = nix::unistd::Pid::from_raw(child.id() as i32);
        assert_eq!(
            wait_for_selection(child, &AtomicBool::new(true)).unwrap(),
            None
        );
        assert_eq!(
            nix::sys::signal::kill(pid, None),
            Err(nix::errno::Errno::ESRCH)
        );
    }
}
