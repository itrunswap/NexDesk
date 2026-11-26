use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use crossterm::{
    event::{
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent,
        MouseEventKind,
    },
    terminal,
};
use futures_util::StreamExt;
use nexdesk_media::{ControlPipe, Preset, Quality, ReceivedSound, View};
use nexdesk_session::{
    ConnectedDesktop,
    collaboration::{Capabilities, Problem, Reply, Request, decode, encode},
    control::{Action, ActionKind, ControlMessage, DesktopState, Grant},
};

use crate::{
    Result, input,
    render::{Backend, Layout, Renderer, Viewport},
    terminal::Terminal,
};

pub(crate) const HELP: &str = "help | zoom 1..8 | pan left/right/up/down | monitor [number] | quality auto/clear/smooth [fps] | mute | volume 0..100 | stats | control | quit";

pub(crate) struct Live {
    pub(crate) view: View,
    pub(crate) control: ControlPipe,
    pub(crate) collaboration: Option<ControlPipe>,
    pub(crate) shell: Option<ControlPipe>,
    pub(crate) state: Option<DesktopState>,
    pub(crate) viewport: Viewport,
    pub(crate) can_control: bool,
    pub(crate) remote_mode: bool,
    volume: Arc<AtomicU8>,
    sound: Option<ReceivedSound>,
    keys: BTreeMap<u32, Vec<u32>>,
}

impl Live {
    pub(crate) fn new(connection: ConnectedDesktop) -> Result<Self> {
        let can_control = connection.grant == Grant::Control;
        let audio = connection
            .capabilities
            .unwrap_or_default()
            .includes(Capabilities::AUDIO);
        let mut view = connection.connection.view;
        let pipes = view
            .take_pipes()
            .ok_or_else(|| io::Error::other("missing desktop channels"))?;
        let volume = Arc::new(AtomicU8::new(0));
        let sound = audio.then(|| view.listen_sound(volume.clone())).flatten();
        Ok(Self {
            view,
            control: pipes.control,
            collaboration: pipes.collaboration,
            shell: pipes.shell,
            state: None,
            viewport: Viewport::default(),
            can_control,
            remote_mode: false,
            volume,
            sound,
            keys: BTreeMap::new(),
        })
    }

    pub(crate) fn action(&self, kind: ActionKind) -> Action {
        Action {
            generation: self.state.as_ref().map_or(0, |s| s.generation),
            ..Action::simple(kind)
        }
    }

    pub(crate) fn send(&self, action: Action) -> bool {
        action.valid() && self.control.send(&ControlMessage::Action(action).pack())
    }

    pub(crate) fn release(&mut self) {
        self.keys.clear();
        self.send(self.action(ActionKind::Release));
    }

    pub(crate) fn local(&mut self) {
        self.release();
        self.remote_mode = false;
    }

    pub(crate) fn update(&mut self, packet: Option<Vec<u8>>) -> bool {
        match packet.as_deref().and_then(ControlMessage::unpack) {
            Some(ControlMessage::State(state)) => {
                if self
                    .state
                    .as_ref()
                    .is_some_and(|old| old.generation != state.generation)
                {
                    self.keys.clear();
                }
                if !state.can_input {
                    self.local();
                }
                self.state = Some(state);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn pointer(&self, action: Action) {
        if self.remote_mode && self.state.as_ref().is_some_and(|s| s.can_input) {
            self.send(action);
        }
    }

    pub(crate) fn transmit(&self, code: u32, down: bool) -> bool {
        self.send(Action {
            code,
            down,
            ..self.action(ActionKind::Key)
        })
    }

    pub(crate) fn stroke(&mut self, key: KeyEvent, key_releases: bool) {
        if !self.remote_mode || !self.state.as_ref().is_some_and(|s| s.can_input) {
            return;
        }
        let Some((code, shifted)) = input::usage(key.code) else {
            return;
        };
        if key.kind == KeyEventKind::Release {
            if let Some(modifiers) = self.keys.remove(&code) {
                self.transmit(code, false);
                for modifier in modifiers.into_iter().rev() {
                    if !self.keys.contains_key(&modifier) {
                        self.transmit(modifier, false);
                    }
                }
            }
            return;
        }
        if self.keys.contains_key(&code) {
            return;
        }
        let modifiers: Vec<_> = input::modifiers(key.modifiers, shifted)
            .filter(|bit| *bit != code && !self.keys.contains_key(bit))
            .collect();
        for &modifier in &modifiers {
            self.transmit(modifier, true);
        }
        self.transmit(code, true);
        if key_releases {
            self.keys.insert(code, modifiers);
        } else {
            self.transmit(code, false);
            for modifier in modifiers.into_iter().rev() {
                self.transmit(modifier, false);
            }
        }
    }

    pub(crate) async fn command(&mut self, command: &str) -> CommandResult {
        self.local();
        let words: Vec<_> = command.split_whitespace().collect();
        let status = match words.as_slice() {
            ["help"] | [] => HELP.into(),
            ["quit"] => return CommandResult::Quit,
            ["control"] => {
                self.remote_mode =
                    self.can_control && self.state.as_ref().is_some_and(|state| state.can_input);
                if self.remote_mode {
                    "Remote keyboard and mouse enabled.".into()
                } else {
                    "The host has not granted keyboard and mouse control.".into()
                }
            }
            ["zoom", value] => match value.parse::<f64>() {
                Ok(zoom) if (1.0..=8.0).contains(&zoom) => {
                    self.viewport.zoom(zoom);
                    format!("Zoom {zoom}x")
                }
                _ => "Use zoom 1 through 8.".into(),
            },
            ["pan", direction] => {
                let (x, y) = match *direction {
                    "left" => (-0.18, 0.0),
                    "right" => (0.18, 0.0),
                    "up" => (0.0, -0.18),
                    "down" => (0.0, 0.18),
                    _ => return CommandResult::Status("Use pan left, right, up or down.".into()),
                };
                self.viewport.pan(x, y);
                "Viewport moved.".into()
            }
            ["monitor"] => self
                .state
                .as_ref()
                .map_or("Waiting for displays.".into(), |state| {
                    state
                        .displays
                        .iter()
                        .enumerate()
                        .map(|(index, display)| {
                            format!(
                                "{} {}{}",
                                index + 1,
                                clean(&display.label),
                                if state.selected == display.id {
                                    " (active)"
                                } else {
                                    ""
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("   ")
                }),
            ["monitor", number] => {
                let index = number.parse::<usize>().ok().and_then(|n| n.checked_sub(1));
                let display = index.and_then(|index| self.state.as_ref()?.displays.get(index));
                if let Some(display) = display {
                    self.send(Action {
                        display: display.id.clone(),
                        ..self.action(ActionKind::Select)
                    });
                    format!("Selecting {}", clean(&display.label))
                } else {
                    "Display number unavailable. Use monitor to list displays.".into()
                }
            }
            ["quality", preset] | ["quality", preset, _] => {
                let fps = words
                    .get(2)
                    .map_or(Some(30), |value| value.parse::<u16>().ok());
                let preset = match *preset {
                    "auto" => Some(Preset::Auto),
                    "clear" => Some(Preset::Clear),
                    "smooth" => Some(Preset::Smooth),
                    _ => None,
                };
                if let Some((preset, fps)) =
                    preset.zip(fps).filter(|(_, fps)| (10..=60).contains(fps))
                {
                    self.quality(Quality::new(preset, fps)).await
                } else {
                    "Use quality auto, clear or smooth and a frame rate from 10 to 60.".into()
                }
            }
            ["mute"] => {
                self.volume.store(0, Ordering::Release);
                "Muted.".into()
            }
            ["volume", value] => match value.parse::<u8>() {
                Ok(value) if value <= 100 && self.sound.is_some() => {
                    self.volume.store(value, Ordering::Release);
                    format!("Volume {value}%")
                }
                _ => "Audio must be requested and granted. Use volume 0 through 100.".into(),
            },
            ["stats"] => match self.view.stats().await {
                Ok(stats) => {
                    let network = stats.ice.as_ref().map_or("ICE pending".into(), |ice| {
                        format!(
                            "{} {:.1} ms",
                            if ice.relayed { "TURN" } else { "Direct" },
                            ice.rtt_ms
                        )
                    });
                    format!(
                        "{network}  {}  {} decoded  {} dropped",
                        stats.video_codec.as_deref().unwrap_or("Video pending"),
                        stats.video_frames,
                        stats.dropped_frames
                    )
                }
                Err(_) => "Media statistics unavailable.".into(),
            },
            _ => "Unknown command. Type help.".into(),
        };
        CommandResult::Status(status)
    }

    async fn quality(&mut self, quality: Quality) -> String {
        let Some(pipe) = self.collaboration.as_mut() else {
            return "Session settings unavailable.".into();
        };
        let request = encode(1, &Request::Quality { quality }, Vec::new());
        if !request.is_some_and(|packet| pipe.send(&packet)) {
            return "Session settings unavailable.".into();
        }
        let reply = tokio::time::timeout(Duration::from_secs(3), pipe.recv())
            .await
            .ok()
            .flatten();
        match reply
            .as_deref()
            .and_then(decode::<std::result::Result<Reply, Problem>>)
        {
            Some((1, Ok(Reply::Quality { quality }), _)) => {
                format!("Quality {:?} · {} FPS", quality.preset, quality.fps)
            }
            _ => "Host did not confirm the quality change.".into(),
        }
    }

    pub(crate) fn close(&mut self) {
        self.local();
        self.send(self.action(ActionKind::Close));
    }
}

pub(crate) enum CommandResult {
    Quit,
    Status(String),
}

pub(crate) async fn run(connected: ConnectedDesktop, backend: Backend) -> Result<()> {
    let mut live = Live::new(connected)?;
    let terminal = Terminal::open(true, live.can_control)?;
    let mut events = EventStream::new();
    let mut out = io::stdout();
    let mut size = terminal::size()?;
    let mut renderer = Renderer::new(backend);
    let mut ui = Ui {
        layout: None,
        command: String::new(),
        status: "Type help for commands.".into(),
    };
    let mut heartbeats = tokio::time::interval(Duration::from_millis(750));
    let mut last = tokio::time::Instant::now() - Duration::from_secs(1);
    let cadence = Duration::from_millis(1000 / backend.rate());
    let result: Result<()> = async {
        renderer.clear(&mut out)?;
        loop {
            tokio::select! {
                packet = live.control.recv() => {
                    if !live.update(packet) { break }
                }
                _ = heartbeats.tick() => {
                    if live.view.has_ended() { break }
                    live.send(live.action(ActionKind::Heartbeat));
                    ui.line(&mut out, &live, backend, size)?;
                }
                frame = live.view.next_frame() => {
                    let Some(frame) = frame? else { break };
                    if last.elapsed() >= cadence && size.0 >= 12 && size.1 >= 5 {
                        last = tokio::time::Instant::now();
                        ui.layout = Some(renderer.draw(&frame, live.viewport, size.0, size.1, &mut out)?);
                        ui.line(&mut out, &live, backend, size)?;
                    }
                }
                event = events.next() => match event.transpose()? {
                    None => break,
                    Some(Event::FocusLost) => live.local(),
                    Some(Event::Resize(cols, rows)) => {
                        live.release();
                        size = (cols, rows);
                        ui.layout = None;
                        renderer.clear(&mut out)?;
                    }
                    Some(Event::Mouse(event)) => ui.mouse(&live, event),
                    Some(Event::Key(key)) if key.kind != KeyEventKind::Release && input::shortcut(key, 'q') => break,
                    Some(Event::Key(key)) if key.kind != KeyEventKind::Release && input::shortcut(key, 'g') => {
                        live.local();
                        ui.command.clear();
                        ui.status = "Local command mode.".into();
                        ui.line(&mut out, &live, backend, size)?;
                    }
                    Some(Event::Key(key)) if live.remote_mode => live.stroke(key, terminal.key_releases()),
                    Some(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                        if key.code == KeyCode::Enter {
                            match live.command(&ui.command).await {
                                CommandResult::Quit => break,
                                CommandResult::Status(status) => ui.status = status,
                            }
                            ui.command.clear();
                            ui.layout = None;
                            renderer.clear(&mut out)?;
                        } else if key.code == KeyCode::Backspace {
                            ui.command.pop();
                        } else if let KeyCode::Char(c) = key.code
                            && (c.is_ascii_graphic() || c == ' ') && ui.command.len() < 128
                        {
                            ui.command.push(c);
                        }
                        ui.line(&mut out, &live, backend, size)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }.await;
    live.close();
    let _ = renderer.clear(&mut out);
    drop(terminal);
    result
}

struct Ui {
    layout: Option<Layout>,
    command: String,
    status: String,
}

impl Ui {
    fn line(
        &self,
        out: &mut impl Write,
        live: &Live,
        backend: Backend,
        size: (u16, u16),
    ) -> io::Result<()> {
        if size.0 < 12 || size.1 < 5 {
            return Ok(());
        }
        let width = usize::from(size.0 - 1);
        let mode = if live.remote_mode {
            "Remote control"
        } else {
            "Local commands"
        };
        let resolution = live
            .state
            .as_ref()
            .map(|state| format!("{}x{}", state.width, state.height))
            .unwrap_or_default();
        let header = format!(
            "NexDesk  {}  {resolution}  {mode}  Ctrl+G commands  Ctrl+Q quit",
            backend.name()
        );
        let footer = if live.remote_mode {
            String::new()
        } else {
            let tail = self.command.len().saturating_sub(width.saturating_sub(2));
            format!("> {}", &self.command[tail..])
        };
        write!(
            out,
            "\x1b[1;1H\x1b[2K{}\x1b[{};1H\x1b[2K{}\x1b[{};1H\x1b[2K{}",
            header.chars().take(width).collect::<String>(),
            size.1 - 1,
            self.status.chars().take(width).collect::<String>(),
            size.1,
            footer.chars().take(width).collect::<String>(),
        )?;
        out.flush()
    }

    fn mouse(&self, live: &Live, event: MouseEvent) {
        let Some((x, y)) = self
            .layout
            .and_then(|layout| layout.point(event.column, event.row))
        else {
            if matches!(event.kind, MouseEventKind::Up(_)) {
                live.send(live.action(ActionKind::Release));
            }
            return;
        };
        let button = |key| match key {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 3,
        };
        let action = match event.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => Action {
                x,
                y,
                ..live.action(ActionKind::Move)
            },
            MouseEventKind::Down(key) => Action {
                x,
                y,
                code: button(key),
                down: true,
                ..live.action(ActionKind::Button)
            },
            MouseEventKind::Up(key) => Action {
                x,
                y,
                code: button(key),
                ..live.action(ActionKind::Button)
            },
            MouseEventKind::ScrollUp => Action {
                y: 120,
                ..live.action(ActionKind::Wheel)
            },
            MouseEventKind::ScrollDown => Action {
                y: -120,
                ..live.action(ActionKind::Wheel)
            },
            MouseEventKind::ScrollLeft => Action {
                x: 120,
                ..live.action(ActionKind::Wheel)
            },
            MouseEventKind::ScrollRight => Action {
                x: -120,
                ..live.action(ActionKind::Wheel)
            },
        };
        live.pointer(action);
    }
}

pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect()
}
