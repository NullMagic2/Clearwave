//! A cancellable worker owns the GTK chooser helper. No GTK objects cross threads.
use crate::{
    Error, Result,
    i18n::{Language, Text},
};
use gtk::prelude::*;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

// A helper emits one bounded JSON message, small enough to fit in its stdout pipe.
const MAX_RESPONSE: usize = 4095;

pub struct FolderPicker {
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Option<PathBuf>>>,
}
impl FolderPicker {
    pub fn start(initial: PathBuf, language: Language) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let (sender, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("model-folder-picker".into())
            .spawn(move || {
                let outcome = (|| {
                    // Parent death also reaps an unresponsive helper if the GUI is killed.
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
                        .arg(language.text(Text::FolderTitle))
                        .arg(language.text(Text::Select))
                        .arg(language.text(Text::Cancel))
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
        // Never wait for GTK or a child process on the application's UI thread.
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
                let mut response = Vec::new();
                child
                    .stdout
                    .take()
                    .ok_or_else(|| Error::new("Missing folder picker output"))?
                    .take((MAX_RESPONSE + 1) as u64)
                    .read_to_end(&mut response)?;
                return decode_response(&response);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
    }
}
fn decode_response(response: &[u8]) -> Result<Option<PathBuf>> {
    if response.len() > MAX_RESPONSE {
        return Err(Error::new("Folder picker response is too large"));
    }
    let selected: Option<String> = serde_json::from_slice(response)?;
    match selected {
        Some(path) if path.is_empty() || path.contains('\0') || !Path::new(&path).is_absolute() => {
            Err(Error::new("Folder picker returned an invalid local path"))
        }
        value => Ok(value.map(PathBuf::from)),
    }
}

/// Internal helper entry point: this process owns its own GTK main thread.
pub fn run_helper(args: &[PathBuf]) -> Result<()> {
    let [initial, title, accept, cancel] = args else {
        return Err(Error::new("Invalid folder picker arguments"));
    };
    gtk::init().map_err(|e| Error::new(format!("Cannot open GTK3 folder picker: {e}")))?;
    let chooser = gtk::FileChooserDialog::with_buttons(
        Some(&title.to_string_lossy()),
        None::<&gtk::Window>,
        gtk::FileChooserAction::SelectFolder,
        &[
            (cancel.to_string_lossy().as_ref(), gtk::ResponseType::Cancel),
            (accept.to_string_lossy().as_ref(), gtk::ResponseType::Accept),
        ],
    );
    chooser.set_local_only(true);
    chooser.set_create_folders(false);
    chooser.set_default_response(gtk::ResponseType::Accept);
    // Do not pass an empty filename to GTK. Start inside an existing local folder.
    if !initial.as_os_str().is_empty() && initial.is_dir() {
        chooser.set_current_folder(initial);
    } else {
        chooser.set_current_folder(gtk::glib::home_dir());
    }
    chooser.connect_response(|dialog, response| {
        let path = if response == gtk::ResponseType::Accept {
            dialog.filename().map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        if let Ok(message) = serde_json::to_string(&path)
            && message.len() < MAX_RESPONSE
        {
            println!("{message}");
        }
        dialog.hide();
        gtk::main_quit();
    });
    chooser.show_all();
    gtk::main();
    Ok(())
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
        assert!(decode_response(&vec![b' '; MAX_RESPONSE + 1]).is_err());
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
