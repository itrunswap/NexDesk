use std::io::{self, Write};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};

pub(crate) struct Terminal {
    mouse: bool,
    viewer: bool,
    enhanced: bool,
}

impl Terminal {
    pub(crate) fn open(viewer: bool, mouse: bool) -> io::Result<Self> {
        let mut terminal = Self {
            mouse,
            viewer,
            enhanced: false,
        };
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen)?;
        if viewer {
            terminal.enhanced = cfg!(unix) && supports_keyboard_enhancement().unwrap_or(false);
            if terminal.enhanced {
                execute!(
                    out,
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    )
                )?;
            }
            execute!(out, Hide, EnableFocusChange)?;
            if mouse {
                execute!(out, EnableMouseCapture)?;
            }
        }
        Ok(terminal)
    }

    pub(crate) fn key_releases(&self) -> bool {
        self.enhanced || cfg!(windows)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let mut out = io::stdout();
        if self.mouse {
            let _ = execute!(out, DisableMouseCapture);
        }
        if self.enhanced {
            let _ = execute!(out, PopKeyboardEnhancementFlags);
        }
        if self.viewer {
            let _ = execute!(out, DisableFocusChange);
        }
        let _ = out.write_all(b"\x1b[0m");
        let _ = execute!(out, Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}
