//! Local, authenticated-user/LocalService pipe transport outside audio callbacks.
//! Nonblocking handles have bounded polling so worker shutdown never waits on IPC.
//! No endpoint here opens a microphone, render stream, network socket or driver.
use super::protocol::PACKET_BYTES;
use std::{
    io::{self, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{
            ERROR_INVALID_HANDLE, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
            ERROR_PIPE_LISTENING, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
            INVALID_HANDLE_VALUE, LocalFree,
        },
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE,
            OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
        },
        System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_NOWAIT,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            SetNamedPipeHandleState,
        },
    },
    core::PCWSTR,
};

const POLL: Duration = Duration::from_millis(2);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_CLIENTS: u32 = 4;
pub struct Pipe {
    handle: OwnedHandle,
}
impl Pipe {
    pub fn server(name: &str, first: bool) -> io::Result<Self> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let acl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;OW)"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: Windows initializes the descriptor, which lives through creation.
        // LocalFree releases only that allocation; the pipe owns its copied ACL.
        // Remote clients are rejected and no inheritable handle is published.
        let handle = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(acl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
            .map_err(io::Error::other)?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let flags = PIPE_ACCESS_DUPLEX
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    FILE_FLAGS_AND_ATTRIBUTES(0)
                };
            let handle = CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                MAX_CLIENTS,
                (PACKET_BYTES * 2) as u32,
                (PACKET_BYTES * 2) as u32,
                0,
                Some(&attributes),
            );
            let error = io::Error::last_os_error();
            LocalFree(Some(HLOCAL(descriptor.0)));
            if handle == INVALID_HANDLE_VALUE {
                return Err(error);
            }
            OwnedHandle::from_raw_handle(handle.0)
        };
        Ok(Self { handle })
    }
    pub fn connected(&self) -> io::Result<bool> {
        // SAFETY: this pipe owns a live nonblocking server handle.
        // ConnectNamedPipe retains no buffer and uses no overlapped structure.
        // The caller polls on a worker thread, never on an audio callback.
        match unsafe { ConnectNamedPipe(self.raw(), None) } {
            Ok(()) => Ok(true),
            Err(error)
                if error.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) =>
            {
                Ok(true)
            }
            Err(error)
                if error.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_LISTENING.0) =>
            {
                Ok(false)
            }
            Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_NO_DATA.0) => {
                // SAFETY: a client disconnected before the accept poll observed it.
                // Disconnect resets this exclusively owned server instance for reuse.
                // No audio callback or outstanding overlapped operation uses it.
                unsafe { DisconnectNamedPipe(self.raw()) }.map_err(io::Error::other)?;
                Ok(false)
            }
            Err(error) => Err(io::Error::other(error)),
        }
    }
    pub fn client(name: &str) -> io::Result<Self> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: CreateFile returns an owned client handle or an error.
        // Nonblocking byte mode is set before the handle is exposed to callers.
        // The local pipe name is built from a validated endpoint GUID.
        unsafe {
            let handle = CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
            .map_err(io::Error::other)?;
            let result = Self {
                handle: OwnedHandle::from_raw_handle(handle.0),
            };
            SetNamedPipeHandleState(
                result.raw(),
                Some(&(PIPE_NOWAIT | PIPE_READMODE_BYTE)),
                None,
                None,
            )
            .map_err(io::Error::other)?;
            Ok(result)
        }
    }
    fn raw(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }
    pub fn session<'a>(&'a mut self, stop: &'a AtomicBool) -> Session<'a> {
        Session {
            pipe: self,
            stop,
            deadline: Instant::now() + IO_TIMEOUT,
        }
    }
}
pub struct Session<'a> {
    pipe: &'a mut Pipe,
    stop: &'a AtomicBool,
    deadline: Instant,
}
impl Session<'_> {
    fn pending(&self, error: &windows::core::Error) -> io::Result<()> {
        if ![ERROR_NO_DATA, ERROR_PIPE_BUSY]
            .iter()
            .any(|code| error.code() == windows::core::HRESULT::from_win32(code.0))
        {
            return Err(io::Error::other(error.to_string()));
        }
        if self.stop.load(Ordering::Acquire) {
            return Err(io::Error::from_raw_os_error(ERROR_INVALID_HANDLE.0 as i32));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "APO worker timed out",
            ));
        }
        std::thread::sleep(POLL);
        Ok(())
    }
}
impl Read for Session<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut read = 0;
            // SAFETY: ReadFile writes within the borrowed slice synchronously.
            // The pipe handle remains owned by this session's exclusive borrow.
            // Retrying uses worker sleeps, not audio callback work.
            match unsafe { ReadFile(self.pipe.raw(), Some(buffer), Some(&mut read), None) } {
                Ok(()) => return Ok(read as usize),
                Err(error) => self.pending(&error)?,
            }
        }
    }
}
impl Write for Session<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            let mut written = 0;
            // SAFETY: WriteFile borrows the input slice for this synchronous call.
            // The nonblocking pipe does not retain a pointer to the slice.
            // Timeouts and shutdown are checked between worker-only retries.
            match unsafe { WriteFile(self.pipe.raw(), Some(buffer), Some(&mut written), None) } {
                Ok(()) if written != 0 => return Ok(written as usize),
                Ok(()) => self.pending(&windows::core::Error::from_hresult(
                    windows::core::HRESULT::from_win32(ERROR_PIPE_BUSY.0),
                ))?,
                Err(error) => self.pending(&error)?,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Packet;
    use std::sync::Arc;
    #[test]
    fn local_transport_roundtrip_cancellation_and_early_disconnect() {
        let name = format!(r"\\.\pipe\Clearwave.test.{}", std::process::id());
        let mut server = Pipe::server(&name, true).unwrap();
        assert!(!server.connected().unwrap());
        drop(Pipe::client(&name).unwrap());
        assert!(!server.connected().unwrap());
        assert!(!server.connected().unwrap());
        let mut client = Pipe::client(&name).unwrap();
        assert!(server.connected().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut session = server.session(&worker_stop);
            let mut packet = Packet::read(&mut session).unwrap();
            packet.samples.iter_mut().for_each(|v| *v *= 0.5);
            packet.write(&mut session).unwrap();
            worker_stop.store(true, Ordering::Release);
            assert!(Packet::read(&mut session).is_err());
        });
        let mut session = client.session(&stop);
        Packet {
            sequence: 960,
            samples: [0.4; crate::protocol::HOP],
        }
        .write(&mut session)
        .unwrap();
        let reply = Packet::read(&mut session).unwrap();
        assert_eq!(reply.sequence, 960);
        assert_eq!(reply.samples, [0.2; crate::protocol::HOP]);
        worker.join().unwrap();
    }
}
