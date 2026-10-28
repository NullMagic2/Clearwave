//! Windows OS interoperability, native dialogs and owned subprocess lifetimes.
//! Keep raw Windows handles here and in the native desktop module.
//! Model loading, DSP and WASAPI stream callbacks remain safe Rust.
pub mod audio;
pub mod desktop;
pub mod task;

use crate::{Error, Result};
use ::windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HANDLE},
        Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::{BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW},
        },
        UI::{
            Shell::{
                Common::COMDLG_FILTERSPEC, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
                FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
                SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
            },
            WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        },
    },
    core::{PCWSTR, w},
};
use std::{
    marker::PhantomData,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    rc::Rc,
};

pub fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
pub fn os_error(error: ::windows::core::Error) -> Error {
    Error::new(error.to_string())
}

pub fn show_error(message: &str) {
    let message = wide(message);

    // SAFETY: both UTF-16 buffers remain valid until the modal call returns.
    // No pointer is retained by Windows, and there is no unowned HWND argument.
    // This is used only for frontend startup errors, never audio/helper workers.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            ::windows::core::w!("Clearwave"),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    let (source, destination) = (wide(source), wide(destination));
    // SAFETY: both NUL-terminated path buffers live for this synchronous call.
    // MoveFileExW does not retain them and only replaces this caller-owned file.
    // No external handle or directory state is modified by the wrapper.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(os_error)
    }
}

pub struct Apartment {
    thread: PhantomData<Rc<()>>,
}
impl Apartment {
    pub fn new() -> Result<Self> {
        // SAFETY: this guard is created on a new UI/helper thread.
        // A failed initialization returns without creating a guard.
        // Each successful initialization is balanced by this thread's Drop.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                .ok()
                .map_err(os_error)?;
        }
        Ok(Self {
            thread: PhantomData,
        })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: exactly one successful CoInitializeEx belongs to this guard.
        // Callers keep this guard on its creating UI/helper thread.
        // COM interface locals drop before the corresponding apartment guard.
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn pick_folder(initial: &Path, title: &Path, installer: bool) -> Result<()> {
    let _apartment = Apartment::new()?;

    // SAFETY: all interfaces are created and released within this STA thread.
    // Returned shell strings are copied before their CoTaskMemFree call.
    // Windows receives only owned UTF-16 buffers with call-scoped lifetimes.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(os_error)?;
        dialog
            .SetOptions(
                FOS_FORCEFILESYSTEM
                    | FOS_PATHMUSTEXIST
                    | if installer {
                        FOS_FILEMUSTEXIST
                    } else {
                        FOS_PICKFOLDERS
                    },
            )
            .map_err(os_error)?;
        if installer {
            dialog
                .SetFileTypes(&[COMDLG_FILTERSPEC {
                    pszName: w!("*.exe"),
                    pszSpec: w!("*.exe"),
                }])
                .map_err(os_error)?;
        }
        let title = wide(title);
        dialog.SetTitle(PCWSTR(title.as_ptr())).map_err(os_error)?;

        if initial.is_dir() {
            let initial = wide(initial);
            if let Ok(folder) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(initial.as_ptr()), None)
            {
                let _ = dialog.SetFolder(&folder);
            }
        }

        match dialog.Show(None) {
            Ok(()) => {
                let item = dialog.GetResult().map_err(os_error)?;
                let text = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(os_error)?;
                let decoded = text.to_string();
                CoTaskMemFree(Some(text.0.cast()));
                let decoded = decoded.map_err(|e| Error::new(e.to_string()))?;
                crate::model_import::report_selection(Some(PathBuf::from(decoded)))?;
            }
            Err(e) if e.code() == ::windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) => {
                println!("null")
            }
            Err(e) => return Err(os_error(e)),
        }
    }
    Ok(())
}

/// A kill-on-close job owns one helper and its descendants.
/// Dropping the owner kills children even after unexpected parent termination.
/// Reaping runs separately so driver shutdown cannot block the native UI thread.
pub struct OwnedChild {
    pub child: Option<Child>,
    job: Option<OwnedHandle>,
}
impl OwnedChild {
    pub fn process(&mut self) -> Result<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| Error::new("Windows child has already stopped"))
    }
    pub fn stop(&mut self, cleanup: Option<PathBuf>) {
        let job = self.job.take();
        let child = self.child.take();
        if job.is_some() || child.is_some() || cleanup.is_some() {
            reap(child, job, cleanup);
        }
    }

    pub fn spawn(command: &mut Command) -> Result<Self> {
        use std::os::windows::process::CommandExt;
        // SAFETY: this function owns the job until it transfers to OwnedChild.
        // Job-information buffers have the documented structure and explicit size.
        // The child process handle is borrowed only while its std owner is alive.
        unsafe {
            let handle = CreateJobObjectW(None, None).map_err(os_error)?;
            let job = OwnedHandle::from_raw_handle(handle.0);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Err(error) = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) {
                return Err(os_error(error));
            }
            let child = match command
                .creation_flags(CREATE_NO_WINDOW.0 | BELOW_NORMAL_PRIORITY_CLASS.0)
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    return Err(error.into());
                }
            };
            if let Err(error) = AssignProcessToJobObject(handle, HANDLE(child.as_raw_handle())) {
                reap(Some(child), Some(job), None);
                return Err(os_error(error));
            }
            Ok(Self {
                child: Some(child),
                job: Some(job),
            })
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.stop(None);
    }
}

fn reap(child: Option<Child>, job: Option<OwnedHandle>, cleanup: Option<PathBuf>) {
    std::thread::spawn(move || {
        // Closing a kill-on-close job can enter process and driver teardown.
        // The reaper owns every handle, so Stop and Close never wait on that path.
        // Waiting and control-file cleanup follow termination on this worker only.
        drop(job);
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(path) = cleanup {
            let _ = std::fs::remove_file(path);
        }
    });
}
