use std::io;

use portable_pty::{Child, CommandBuilder};

pub(super) fn ordinary_user() -> bool {
    // SAFETY: These calls only read the current process credentials.
    unsafe {
        let user = libc::getuid();
        user != 0 && libc::geteuid() == user && libc::getegid() == libc::getgid()
    }
}

pub(super) fn command() -> io::Result<CommandBuilder> {
    let mut command = CommandBuilder::new("/bin/sh");
    // Commands remain interactive without enabling shell job control.
    command.arg("-i");
    command.arg("+m");
    Ok(command)
}

pub(super) struct ProcessScope(libc::pid_t);

impl ProcessScope {
    pub(super) fn new(child: &dyn Child) -> io::Result<Self> {
        let pid = child
            .process_id()
            .and_then(|id| libc::pid_t::try_from(id).ok())
            .filter(|id| *id > 1)
            .ok_or_else(|| io::Error::other("PTY process ID is unavailable"))?;
        // SAFETY: Query only. portable-pty gives its child a separate session.
        if unsafe { libc::getsid(pid) } != pid {
            return Err(io::Error::other("PTY session was not isolated"));
        }
        Ok(Self(pid))
    }

    pub(super) fn terminate(&self) {
        // Stop the session leader and its group while the PID still belongs to our child.
        // SAFETY: The scope was checked at creation and cannot be the local agent's group.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
        for pid in process_ids() {
            // SAFETY: Signal only processes in the PTY's isolated session.
            unsafe {
                if pid > 1 && libc::getsid(pid) == self.0 {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    pub(super) fn has_exited(&self) -> bool {
        // SAFETY: The information buffer is exclusively owned by this call.
        // WNOWAIT leaves the child unreaped until the scope has been cleaned up.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            libc::waitid(
                libc::P_PID,
                self.0 as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            ) == 0
                && info.si_pid() == self.0
        }
    }
}

#[cfg(target_os = "linux")]
fn process_ids() -> Vec<libc::pid_t> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect()
}

#[cfg(target_os = "macos")]
fn process_ids() -> Vec<libc::pid_t> {
    const ALL_PROCESSES: u32 = 1;
    // SAFETY: libproc reports the buffer size in bytes for a null output pointer.
    let estimate = unsafe { libc::proc_listpids(ALL_PROCESSES, 0, std::ptr::null_mut(), 0) };
    if estimate <= 0 || estimate > 4 * 1024 * 1024 {
        return Vec::new();
    }
    let count = (estimate as usize).div_ceil(size_of::<libc::pid_t>()) + 256;
    let mut pids = vec![0; count];
    // SAFETY: pids contains count initialized entries and the byte bound is representable.
    let bytes = unsafe {
        libc::proc_listpids(
            ALL_PROCESSES,
            0,
            pids.as_mut_ptr().cast(),
            (count * size_of::<libc::pid_t>()) as i32,
        )
    };
    pids.truncate((bytes.max(0) as usize) / size_of::<libc::pid_t>());
    pids
}
