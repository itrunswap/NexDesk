//! Native desktop input adapters. The session owns the authorization and held inputs.
#![deny(unsafe_code)]

#[cfg_attr(target_os = "windows", path = "windows.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[allow(unsafe_code)]
mod native;

mod audio;
mod clipboard;
mod display;

pub use audio::{
    AUDIO_CHANNELS, AUDIO_RATE, AUDIO_SAMPLES, AudioCapture, AudioOutput, AudioStatus,
};
pub use clipboard::Clipboard;
pub use display::{DisplayMode, Monitor};

#[cfg(target_os = "macos")]
pub use native::capture_display_ids;
pub use native::{NativeInput, initialize, request_permissions, screen_region};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn point(self, x: u16, y: u16) -> (i32, i32) {
        let to_axis = |value: u16, start: i32, length: u32| {
            start.saturating_add(
                (u64::from(value) * u64::from(length.saturating_sub(1)) / 65535) as i32,
            )
        };
        (
            to_axis(x, self.left, self.width),
            to_axis(y, self.top, self.height),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Input {
    Move { x: i32, y: i32 },
    Button { button: u8, down: bool },
    Key { usage: u32, down: bool },
    Wheel { horizontal: i32, vertical: i32 },
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("the desktop has not granted input access")]
    Unavailable,
    #[error("this input is unavailable on the host")]
    Unsupported,
    #[error("the operating system rejected desktop input")]
    Rejected,
}

pub trait Injector {
    fn send(&mut self, input: Input) -> Result<(), InputError>;
    fn insert_text(&mut self, _text: &str) -> Result<(), InputError> {
        Err(InputError::Unsupported)
    }
}

/// Tracks only keys/buttons injected by this session and releases them in reverse order.
pub struct InputLease<T: Injector> {
    backend: T,
    held: Vec<Input>,
}

impl<T: Injector> InputLease<T> {
    pub fn new(backend: T) -> Self {
        Self {
            backend,
            held: Vec::new(),
        }
    }

    pub fn send(&mut self, input: Input) -> Result<(), InputError> {
        let (pressed, down) = match input {
            Input::Key { usage, down } => (Input::Key { usage, down: true }, down),
            Input::Button { button, down } => (Input::Button { button, down: true }, down),
            _ => return self.backend.send(input),
        };
        let existing = self.held.iter().position(|item| *item == pressed);
        match (down, existing) {
            (true, None) if self.held.len() < 64 => {
                self.backend.send(input)?;
                self.held.push(pressed);
            }
            (true, Some(_)) if matches!(input, Input::Key { .. }) => self.backend.send(input)?,
            (false, Some(index)) => {
                self.backend.send(input)?;
                self.held.remove(index);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn release(&mut self) {
        while let Some(pressed) = self.held.pop() {
            let released = match pressed {
                Input::Key { usage, .. } => Input::Key { usage, down: false },
                Input::Button { button, .. } => Input::Button {
                    button,
                    down: false,
                },
                _ => continue,
            };
            let _ = self.backend.send(released);
        }
    }

    pub fn insert_text(&mut self, text: &str) -> Result<(), InputError> {
        self.release();
        self.backend.insert_text(text)
    }
}

impl<T: Injector> Drop for InputLease<T> {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Recorder<'a>(&'a mut Vec<Input>);

    impl Injector for Recorder<'_> {
        fn send(&mut self, event: Input) -> Result<(), InputError> {
            self.0.push(event);
            Ok(())
        }
    }

    #[test]
    fn dropping_a_lease_releases_buttons_and_keys_in_reverse_order() -> Result<(), InputError> {
        let mut events = Vec::new();
        let ctrl = |down| Input::Key {
            usage: 0x0007_00e0,
            down,
        };
        let left = |down| Input::Button { button: 1, down };
        {
            let mut lease = InputLease::new(Recorder(&mut events));
            lease.send(ctrl(false))?;
            lease.send(ctrl(true))?;
            lease.send(left(true))?;
            lease.send(left(true))?;
        }
        assert_eq!(events, [ctrl(true), left(true), left(false), ctrl(false)]);
        Ok(())
    }

    #[test]
    fn capture_corners_map_to_an_offset_portrait_monitor() {
        let portrait = Region {
            left: -1080,
            top: -320,
            width: 1080,
            height: 1920,
        };
        assert_eq!(portrait.point(0, 0), (-1080, -320));
        assert_eq!(portrait.point(65535, 65535), (-1, 1599));
    }
}
