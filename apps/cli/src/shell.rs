use std::{
    fmt::Write as _,
    io::{self, Write},
    time::Duration,
};

use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use futures_util::StreamExt;
use nexdesk_session::{
    ConnectedDesktop,
    collaboration::Capabilities,
    control::ActionKind,
    envelope::{decode, encode},
    terminal::{CHUNK, Reply, Request, Size},
};
use vte::{Params, Parser, Perform};

use crate::{Result, desktop::Live, input::shortcut, terminal::Terminal};

pub(crate) async fn run(connected: ConnectedDesktop) -> Result<()> {
    if !connected
        .capabilities
        .unwrap_or_default()
        .includes(Capabilities::SHELL)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the host did not grant Shell",
        )
        .into());
    }
    let mut live = Live::new(connected)?;
    let mut pipe = live
        .shell
        .take()
        .ok_or_else(|| io::Error::other("Shell channel unavailable"))?;
    let _terminal = Terminal::open(false, false)?;
    let mut out = io::stdout();
    let mut keys = EventStream::new();
    let mut parser = Parser::new();
    let mut heartbeats = tokio::time::interval(Duration::from_millis(700));
    let result: Result<()> = async {
        let open = tokio::time::timeout(Duration::from_secs(10), pipe.wait_open()).await?;
        if !open { return Err(io::Error::other("Shell channel did not open").into()) }
        live.send(live.action(ActionKind::Heartbeat));
        let (cols, rows) = terminal::size()?;
        if !send(&pipe, Request::Open { size: dimensions(cols, rows) }, Vec::new()) {
            return Err(io::Error::other("could not open the remote PTY").into());
        }
        let mut opened = false;
        let handshake = tokio::time::sleep(Duration::from_secs(12));
        tokio::pin!(handshake);
        loop {
            tokio::select! {
                _ = &mut handshake, if !opened => return Err(io::Error::other("remote PTY timed out").into()),
                _ = heartbeats.tick() => { live.send(live.action(ActionKind::Heartbeat)); }
                packet = live.control.recv() => if !live.update(packet) { break },
                packet = pipe.recv() => {
                    let Some((1, reply, bytes)) = packet.as_deref().and_then(decode::<Reply>) else { break };
                    match reply {
                        Reply::Opened if !opened && bytes.is_empty() => {
                            opened = true;
                            out.write_all(b"Remote Shell connected. Ctrl+Q disconnects.\r\n")?;
                            out.flush()?;
                        }
                        Reply::Output if opened && (1..=CHUNK).contains(&bytes.len()) => {
                            let mut output = TerminalOutput(String::new());
                            parser.advance(&mut output, &bytes);
                            out.write_all(output.0.as_bytes())?;
                            out.flush()?;
                        }
                        Reply::Unavailable => return Err(io::Error::other("the host could not start a user PTY").into()),
                        _ => break,
                    }
                }
                event = keys.next() => match event.transpose()? {
                    Some(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                        if shortcut(key, 'q') { break }
                        if opened && let Some(bytes) = keyboard(key)
                            && !send(&pipe, Request::Input, bytes) { break }
                    }
                    Some(Event::Resize(cols, rows)) if opened => {
                        let size = dimensions(cols, rows);
                        if !send(&pipe, Request::Resize { size }, Vec::new()) { break }
                    }
                    None => break,
                    _ => {}
                }
            }
        }
        Ok(())
    }.await;
    send(&pipe, Request::Close, Vec::new());
    live.close();
    result
}

fn dimensions(cols: u16, rows: u16) -> Size {
    Size {
        cols: cols.clamp(2, 512),
        rows: rows.clamp(2, 256),
    }
}

fn send(pipe: &nexdesk_media::ControlPipe, request: Request, bytes: Vec<u8>) -> bool {
    encode(1, &request, bytes).is_some_and(|packet| pipe.send(&packet))
}

fn keyboard(key: KeyEvent) -> Option<Vec<u8>> {
    let mut utf8 = [0; 4];
    let mut control = [0; 1];
    let value: &[u8] = match key.code {
        KeyCode::Enter => b"\r",
        KeyCode::Tab => b"\t",
        KeyCode::BackTab => b"\x1b[Z",
        KeyCode::Esc => b"\x1b",
        KeyCode::Backspace => b"\x7f",
        KeyCode::Delete => b"\x1b[3~",
        KeyCode::Insert => b"\x1b[2~",
        KeyCode::Up => b"\x1b[A",
        KeyCode::Down => b"\x1b[B",
        KeyCode::Right => b"\x1b[C",
        KeyCode::Left => b"\x1b[D",
        KeyCode::Home => b"\x1b[H",
        KeyCode::End => b"\x1b[F",
        KeyCode::PageUp => b"\x1b[5~",
        KeyCode::PageDown => b"\x1b[6~",
        KeyCode::Char(c)
            if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() =>
        {
            control[0] = c.to_ascii_lowercase() as u8 - b'a' + 1;
            &control
        }
        KeyCode::Char(c) if !c.is_control() => c.encode_utf8(&mut utf8).as_bytes(),
        _ => return None,
    };
    let mut sequence = Vec::with_capacity(value.len() + 1);
    if key.modifiers.contains(KeyModifiers::ALT) {
        sequence.push(27);
    }
    sequence.extend_from_slice(value);
    Some(sequence)
}

struct TerminalOutput(String);

impl Perform for TerminalOutput {
    fn print(&mut self, character: char) {
        if !character.is_control() {
            self.0.push(character);
        }
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\r' | b'\n' | b'\t' | 8) {
            self.0.push(char::from(byte));
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore
            || !intermediates.is_empty()
            || !matches!(
                action,
                'm' | 'A'
                    | 'B'
                    | 'C'
                    | 'D'
                    | 'E'
                    | 'F'
                    | 'G'
                    | 'H'
                    | 'f'
                    | 'J'
                    | 'K'
                    | 'P'
                    | 'X'
                    | '@'
                    | 'L'
                    | 'M'
                    | 'S'
                    | 'T'
                    | 'd'
                    | 's'
                    | 'u'
            )
            || params.iter().flatten().any(|&value| value > 1024)
            || (action == 'J' && params.iter().flatten().any(|&value| value > 2))
        {
            return;
        }
        self.0.push_str("\x1b[");
        for (position, group) in params.iter().enumerate() {
            if position != 0 {
                self.0.push(';');
            }
            for (index, value) in group.iter().enumerate() {
                if index != 0 {
                    self.0.push(':');
                }
                let _ = write!(self.0, "{value}");
            }
        }
        self.0.push(action);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if !ignore && intermediates.is_empty() && matches!(byte, b'7' | b'8' | b'D' | b'E' | b'M') {
            self.0.push('\x1b');
            self.0.push(char::from(byte));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_output_cannot_set_clipboard_or_query_local_terminal() {
        let mut parser = Parser::new();
        let mut output = TerminalOutput(String::new());
        parser.advance(
            &mut output,
            b"ok\x1b]52;c;YWJj\x07\x1b[6n\x1b[?1049l\x1b[31mred\x1b[0m",
        );
        assert_eq!(output.0, "ok\x1b[31mred\x1b[0m");
    }
}
