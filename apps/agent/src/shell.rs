use std::{
    io::{self, Read, Write},
    thread,
    time::Duration,
};

use nexdesk_media::ControlPipe;
use nexdesk_session::{
    envelope::{decode, encode},
    terminal::{CHUNK, Reply, Request, Size},
};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{mpsc, watch};

#[allow(unsafe_code)]
#[cfg_attr(unix, path = "shell_unix.rs")]
#[cfg_attr(windows, path = "shell_windows.rs")]
mod platform;

pub(crate) fn ordinary_user() -> bool {
    platform::ordinary_user()
}

pub(crate) async fn serve(mut pipe: ControlPipe, mut stopped: watch::Receiver<bool>) {
    let packet = tokio::select! {
        biased;
        _ = stopped.changed() => return,
        packet = tokio::time::timeout(Duration::from_secs(12), pipe.recv()) => packet.ok().flatten(),
    };
    let Some((id, Request::Open { size }, bytes)) = packet.as_deref().and_then(decode::<Request>)
    else {
        return;
    };
    if !size.valid() || !bytes.is_empty() {
        return;
    }
    let pty = tokio::task::spawn_blocking(move || Pty::start(size));
    let Some(Ok(pty)) = (tokio::select! {
        biased;
        _ = stopped.changed() => None,
        ready = pty => ready.ok(),
    }) else {
        let _ = send(&pipe, id, Reply::Unavailable, Vec::new());
        return;
    };
    if !send(&pipe, id, Reply::Opened, Vec::new()) {
        return;
    }
    let Pty {
        mut process,
        mut output,
        input,
    } = pty;
    let mut process_check = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            biased;
            _ = stopped.changed() => break,
            packet = pipe.recv() => {
                let Some((_, request, bytes)) = packet.as_deref().and_then(decode::<Request>) else { break };
                match request {
                    Request::Input if (1..=CHUNK).contains(&bytes.len()) => {
                        if input.try_send(bytes).is_err() { break }
                    }
                    Request::Resize { size } if size.valid() && bytes.is_empty() => {
                        if process.resize(size).is_err() { break }
                    }
                    _ => break,
                }
            }
            frame = output.recv() => {
                let Some(data) = frame else { break };
                if !send(&pipe, id, Reply::Output, data) { break }
            }
            _ = process_check.tick() => {
                if process.has_exited() { break }
            }
        }
    }
    drop(input);
    drop(output);
    let _ = tokio::task::spawn_blocking(move || drop(process)).await;
    let _ = send(&pipe, id, Reply::Ended, Vec::new());
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn send(pipe: &ControlPipe, id: u64, reply: Reply, data: Vec<u8>) -> bool {
    encode(id, &reply, data).is_some_and(|packet| pipe.send(&packet))
}

struct Pty {
    output: mpsc::Receiver<Vec<u8>>,
    input: mpsc::Sender<Vec<u8>>,
    process: Process,
}

struct Process {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    scope: platform::ProcessScope,
}

impl Pty {
    fn start(size: Size) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if !ordinary_user() {
            return Err(io::Error::other("Shell requires an ordinary user").into());
        }
        let command = command()?;
        let pair = native_pty_system().openpty(dimensions(size))?;
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let (send_output, output) = mpsc::channel(16);
        let (input, mut receive_input) = mpsc::channel::<Vec<u8>>(64);
        #[cfg(windows)]
        let (mut cursor, cursor_ready) = platform::StartupCursor::new(input.clone());
        thread::Builder::new()
            .name("nexdesk-pty-input".into())
            .spawn(move || {
                while let Some(bytes) = receive_input.blocking_recv() {
                    if writer
                        .write_all(&bytes)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        thread::Builder::new()
            .name("nexdesk-pty-output".into())
            .spawn(move || {
                let mut bytes = [0; CHUNK];
                let mut delivering = true;
                while let Ok(size) = reader.read(&mut bytes) {
                    if size == 0 {
                        break;
                    }
                    #[cfg(windows)]
                    cursor.observe(&bytes[..size]);
                    // The ConPTY close operation can emit a final frame after the receiver stops.
                    if delivering {
                        delivering = send_output.blocking_send(bytes[..size].to_vec()).is_ok();
                    }
                }
            })?;
        let mut child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let scope = match platform::ProcessScope::new(child.as_ref()) {
            Ok(scope) => scope,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(output);
                return Err(error.into());
            }
        };
        #[cfg(windows)]
        let _ = cursor_ready.recv_timeout(Duration::from_millis(1500));
        let process = Process {
            master: pair.master,
            child,
            scope,
        };
        Ok(Self {
            process,
            output,
            input,
        })
    }
}

impl Process {
    fn has_exited(&mut self) -> bool {
        #[cfg(unix)]
        {
            self.scope.has_exited()
        }
        #[cfg(windows)]
        {
            self.child.try_wait().is_ok_and(|status| status.is_some())
        }
    }

    fn resize(&self, size: Size) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.master.resize(dimensions(size))?;
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.scope.terminate();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn dimensions(size: Size) -> PtySize {
    PtySize {
        cols: size.cols,
        rows: size.rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn command() -> io::Result<CommandBuilder> {
    let mut command = platform::command()?;
    command.env_clear();
    for name in [
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_CTYPE",
        "PATH",
        "SystemRoot",
        "SystemDrive",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "USERNAME",
        "HOMEDRIVE",
        "HOMEPATH",
        "PATHEXT",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("TERM", "xterm");
    Ok(command)
}
