use std::{
    ffi::OsString,
    io,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    ptr,
    sync::mpsc::{Receiver, SyncSender, sync_channel},
};

use portable_pty::{Child, CommandBuilder};
use windows_sys::Win32::{
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        SystemInformation::GetSystemDirectoryW,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

pub(super) fn ordinary_user() -> bool {
    let mut token = ptr::null_mut();
    // SAFETY: The output points to a writable HANDLE and success transfers ownership.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut written = 0;
    // SAFETY: The buffer and size correspond to TOKEN_ELEVATION for this token query.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut written,
        )
    };
    ok != 0 && elevation.TokenIsElevated == 0
}

pub(super) fn command() -> io::Result<CommandBuilder> {
    let mut buffer = [0; 512];
    // SAFETY: The output buffer has exactly the provided capacity in UTF-16 code units.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    let path = PathBuf::from(OsString::from_wide(&buffer[..length])).join("cmd.exe");
    let mut command = CommandBuilder::new(path);
    command.args(["/D", "/Q"]);
    Ok(command)
}

pub(super) struct StartupCursor {
    input: Option<(tokio::sync::mpsc::Sender<Vec<u8>>, SyncSender<()>)>,
    matched: usize,
}

impl StartupCursor {
    pub(super) fn new(input: tokio::sync::mpsc::Sender<Vec<u8>>) -> (Self, Receiver<()>) {
        let (ready, receive) = sync_channel(1);
        (
            Self {
                input: Some((input, ready)),
                matched: 0,
            },
            receive,
        )
    }

    pub(super) fn observe(&mut self, output: &[u8]) {
        const QUERY: &[u8] = b"\x1b[6n";
        if self.input.is_none() {
            return;
        }
        for &byte in output {
            if byte == QUERY[self.matched] {
                self.matched += 1;
                if self.matched == QUERY.len() {
                    // The new session starts in the top-left cell. Answer in the
                    // host's PTY so the remote client's terminal is never queried.
                    if let Some((input, ready)) = self.input.take() {
                        let _ = input.blocking_send(b"\x1b[1;1R".to_vec());
                        let _ = ready.send(());
                    }
                    return;
                }
            } else {
                self.matched = usize::from(byte == QUERY[0]);
            }
        }
    }
}

pub(super) struct ProcessScope(OwnedHandle);

impl ProcessScope {
    pub(super) fn new(child: &dyn Child) -> io::Result<Self> {
        let process = child
            .as_raw_handle()
            .ok_or_else(|| io::Error::other("missing PTY process"))?;
        // SAFETY: No inherited access or name is supplied for the new job.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: Both handles are live and the information buffer has the specified layout.
        let assigned = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
                && AssignProcessToJobObject(job.as_raw_handle(), process) != 0
        };
        if !assigned {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(job))
    }

    pub(super) fn terminate(&self) {
        // SAFETY: The owned handle refers to the job for this PTY only.
        let _ = unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
    }
}
