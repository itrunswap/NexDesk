use std::time::{Duration, Instant};

use nexdesk_platform::{DisplayMode, Monitor};
use nexdesk_session::collaboration::{Problem, Reply};

const CONFIRM: Duration = Duration::from_secs(15);

struct Change {
    display: Monitor,
    initial: DisplayMode,
    applied: DisplayMode,
    preview: Option<Preview>,
}

struct Preview {
    token: u64,
    deadline: Instant,
    previous: DisplayMode,
}

#[derive(Default)]
pub(crate) struct Resolution {
    change: Option<Change>,
    next: u64,
}

impl Resolution {
    pub fn modes(display: Monitor) -> Result<Reply, Problem> {
        let current = display.current().map_err(|_| Problem::Unavailable)?;
        let mut choices = display.modes().map_err(|_| Problem::Unavailable)?;
        choices.truncate(128);
        Ok(Reply::Modes {
            display: display.key().to_owned(),
            current,
            choices,
        })
    }

    pub fn apply(&mut self, display: Monitor, mode: DisplayMode) -> Result<Reply, Problem> {
        if !mode.valid() {
            return Err(Problem::Invalid);
        }
        self.tick();
        if self
            .change
            .as_ref()
            .is_some_and(|change| change.display.key() != display.key())
        {
            self.restore();
        }
        if self
            .change
            .as_ref()
            .is_some_and(|change| change.preview.is_some())
        {
            return Err(Problem::Busy);
        }
        let before = display.current().map_err(|_| Problem::Unavailable)?;
        if self
            .change
            .as_ref()
            .is_some_and(|change| change.applied != before)
        {
            self.change = None;
        }
        let initial = self.change.as_ref().map_or(before, |change| change.initial);
        let actual = match display.apply(mode) {
            Ok(readback) => readback,
            Err(_) => {
                if before != mode && display.current().ok() == Some(mode) {
                    let _ = display.apply(before);
                }
                return Err(Problem::Unavailable);
            }
        };
        if actual != mode {
            let _ = display.apply(before);
            return Err(Problem::Changed);
        }
        self.next = self.next.wrapping_add(1).max(1);
        let reply = Reply::Preview {
            token: self.next,
            display: display.key().to_owned(),
            mode: actual,
            expires_ms: CONFIRM.as_millis() as u32,
        };
        self.change = Some(Change {
            display,
            initial,
            applied: actual,
            preview: Some(Preview {
                token: self.next,
                deadline: Instant::now() + CONFIRM,
                previous: before,
            }),
        });
        Ok(reply)
    }

    pub fn confirm(&mut self, token: u64) -> Result<Reply, Problem> {
        self.tick();
        let active = self
            .change
            .as_mut()
            .filter(|change| {
                change
                    .preview
                    .as_ref()
                    .is_some_and(|preview| preview.token == token)
            })
            .ok_or(Problem::Expired)?;
        if active.display.current().ok() != Some(active.applied) {
            return Err(Problem::Changed);
        }
        active.preview = None;
        Ok(Reply::Confirmed {
            display: active.display.key().to_owned(),
            mode: active.applied,
        })
    }

    pub fn pending(&self, token: u64) -> Option<DisplayMode> {
        let active = self.change.as_ref()?;
        let preview = active.preview.as_ref()?;
        (preview.token == token && Instant::now() < preview.deadline).then_some(active.applied)
    }

    pub fn rollback(&mut self, token: u64) -> Result<Reply, Problem> {
        let active = self.change.as_mut().ok_or(Problem::Expired)?;
        let preview = active
            .preview
            .as_ref()
            .filter(|preview| preview.token == token)
            .ok_or(Problem::Expired)?;
        if active.display.current().ok() != Some(active.applied) {
            self.change = None;
            return Err(Problem::Changed);
        }
        let current = active
            .display
            .apply(preview.previous)
            .map_err(|_| Problem::Unavailable)?;
        active.applied = current;
        active.preview = None;
        Ok(Reply::Restored { current })
    }

    pub fn tick(&mut self) -> bool {
        if let Some(token) = self
            .change
            .as_ref()
            .and_then(|change| change.preview.as_ref())
            .filter(|pending| pending.deadline <= Instant::now())
            .map(|pending| pending.token)
        {
            if self.rollback(token).is_ok() {
                return true;
            }
            self.restore();
            return true;
        }
        false
    }

    pub fn restore(&mut self) {
        if let Some(active) = self.change.take()
            && active.applied != active.initial
            && active.display.current().ok() == Some(active.applied)
        {
            let _ = active.display.apply(active.initial);
        }
    }
}

impl Drop for Resolution {
    fn drop(&mut self) {
        self.restore();
    }
}
