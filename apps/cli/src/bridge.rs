//! Versioned, bounded local stdio transport used by the Pi extension.

use std::{
    io::{self, BufRead, Read, Write},
    thread,
    time::Duration,
};

use nexdesk_session::{
    control::{Action, ActionKind},
    pairing::AccessCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{
    Result, Target,
    desktop::{CommandResult, Live},
    render::{Layout, bridge_image},
};

const INPUT_LIMIT: u64 = 4096;
const VERSION: u16 = 1;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Start {
        version: u16,
        code: String,
        cols: u16,
        rows: u16,
    },
    Trust {
        accept: bool,
    },
    Size {
        cols: u16,
        rows: u16,
    },
    Local {
        text: String,
    },
    Keys {
        codes: Vec<u32>,
    },
    Pointer {
        col: u16,
        row: u16,
        button: u8,
        down: bool,
    },
    Release,
    Close,
}

pub(crate) async fn run(target: Target) -> Result<()> {
    let (send, mut commands) = mpsc::channel(32);
    thread::Builder::new()
        .name("nexdesk-bridge-input".into())
        .spawn(move || {
            let mut terminal = io::stdin().lock();
            let mut bytes = Zeroizing::new(Vec::with_capacity(INPUT_LIMIT as usize));
            loop {
                bytes.clear();
                let count = terminal
                    .by_ref()
                    .take(INPUT_LIMIT + 1)
                    .read_until(b'\n', &mut bytes);
                if !matches!(count, Ok(n) if n > 0 && n <= INPUT_LIMIT as usize) {
                    break;
                }
                let command = serde_json::from_slice::<Command>(&bytes);
                bytes.as_mut_slice().fill(0);
                let Ok(command) = command else { break };
                if send.blocking_send(command).is_err() {
                    break;
                }
            }
        })?;
    emit(json!({"event":"ready","version":VERSION}))?;
    let Some(Command::Start {
        version: VERSION,
        code,
        mut cols,
        mut rows,
    }) = tokio::time::timeout(Duration::from_secs(30), commands.recv()).await?
    else {
        return Ok(());
    };
    let code = Zeroizing::new(code);
    let secret = AccessCode::parse(code.as_bytes())?;
    drop(code);
    let mut live = Live::new(
        target
            .connect(
                secret,
                async |identity| {
                    emit(json!({"event":"identity","fingerprint":identity.to_string()})).is_ok()
                        && matches!(
                            tokio::time::timeout(Duration::from_secs(25), commands.recv()).await,
                            Ok(Some(Command::Trust { accept: true }))
                        )
                },
                || {
                    let _ = emit(json!({"event":"waiting"}));
                },
            )
            .await?,
    )?;
    emit(json!({"event":"connected","can_control":live.can_control}))?;
    let mut heartbeats = tokio::time::interval(Duration::from_millis(650));
    let mut last = tokio::time::Instant::now() - Duration::from_secs(1);
    let mut layout: Option<Layout> = None;
    let mut sequence = 0u64;
    let result: Result<()> = async {
        loop {
            tokio::select! {
                packet = live.control.recv() => {
                    if !live.update(packet) { break }
                    if let Some(state) = &live.state {
                        emit(json!({"event":"desktop","width":state.width,"height":state.height,
                            "can_control":state.can_input,"remote_mode":live.remote_mode}))?;
                    }
                }
                _ = heartbeats.tick() => {
                    if live.view.has_ended() { break }
                    live.send(live.action(ActionKind::Heartbeat));
                }
                frame = live.view.next_frame() => {
                    let Some(frame) = frame? else { break };
                    if last.elapsed() >= Duration::from_millis(180) {
                        last = tokio::time::Instant::now();
                        let (image, next) = bridge_image(&frame, live.viewport, cols, rows);
                        layout = Some(next);
                        sequence += 1;
                        emit(json!({"event":"frame","sequence":sequence,"lines":image}))?;
                    }
                }
                command = commands.recv() => match command {
                    Some(Command::Size { cols: width, rows: height }) => {
                        live.release();
                        cols = width;
                        rows = height;
                        layout = None;
                    }
                    Some(Command::Local { text }) if text.len() <= 128 => {
                        match live.command(&text).await {
                            CommandResult::Quit => break,
                            CommandResult::Status(text) => emit(json!({"event":"status","text":text,
                                "remote_mode":live.remote_mode}))?,
                        }
                    }
                    Some(Command::Keys { codes }) if live.remote_mode && (1..=6).contains(&codes.len()) => {
                        if codes.iter().any(|code| !(0x0007_0004..=0x0007_00e7).contains(code)) { break }
                        for &code in &codes { live.transmit(code, true); }
                        for &code in codes.iter().rev() { live.transmit(code, false); }
                    }
                    Some(Command::Pointer { col, row, button, down }) if button <= 5 => {
                        if let Some((x, y)) = layout.and_then(|layout| layout.point(col, row.saturating_add(1))) {
                            let kind = if button == 0 { ActionKind::Move } else { ActionKind::Button };
                            live.pointer(Action { x, y, code: u32::from(button), down, ..live.action(kind) });
                        } else if !down { live.release(); }
                    }
                    Some(Command::Release) => {
                        live.local();
                        emit(json!({"event":"status","text":"Local command mode.","remote_mode":false}))?;
                    }
                    Some(Command::Close) | None => break,
                    _ => {}
                }
            }
        }
        Ok(())
    }.await;
    live.close();
    let _ = emit(json!({"event":"ended"}));
    result
}

fn emit(value: Value) -> io::Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.flush()
}
