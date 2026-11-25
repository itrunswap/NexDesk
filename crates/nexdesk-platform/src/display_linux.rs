use sha2::{Digest, Sha256};
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::{
        randr::{
            Connection as OutputConnection, ConnectionExt as Randr, GetCrtcInfoReply,
            GetOutputInfoReply, GetScreenResourcesCurrentReply, ModeFlag, ModeInfo, Rotation,
            SetConfig,
        },
        xproto::{AtomEnum, ConnectionExt as Xproto, Window},
    },
    rust_connection::RustConnection,
};

use crate::{DisplayMode, InputError};

#[derive(Clone, Debug)]
pub struct Monitor {
    output: u32,
    key: String,
}

impl Monitor {
    pub fn locate(id: u64, platform: i64) -> Option<Self> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland")
        {
            return None;
        }
        let snapshot = Snapshot::open()?;
        let candidates: Vec<_> = snapshot
            .resources
            .outputs
            .iter()
            .copied()
            .filter_map(|id| {
                let output = snapshot.output(id)?;
                (output.connection == OutputConnection::CONNECTED && output.crtc != 0)
                    .then_some((id, output))
            })
            .collect();
        let expected = if platform > 0 {
            u32::try_from(platform).ok()?
        } else {
            u32::try_from(id).ok()?
        };
        let (output, details) = candidates
            .iter()
            .find(|(output, _)| *output == expected)
            .or_else(|| (candidates.len() == 1).then(|| &candidates[0]))?;
        let key = snapshot.identity(*output, details);
        Some(Self {
            output: *output,
            key,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn current(&self) -> Result<DisplayMode, InputError> {
        let (snapshot, _, current) = self.snapshot()?;
        snapshot
            .resources
            .modes
            .iter()
            .find(|mode| mode.id == current.mode)
            .map(|mode| mode_value(mode, current.rotation))
            .ok_or(InputError::Unavailable)
    }

    pub fn modes(&self) -> Result<Vec<DisplayMode>, InputError> {
        let (snapshot, output, crtc) = self.snapshot()?;
        let mut modes: Vec<_> = snapshot
            .candidates(&output, &crtc)?
            .into_iter()
            .map(|(_, value)| value)
            .collect();
        modes.sort_unstable_by_key(|mode| (mode.width, mode.height, mode.refresh_millihz));
        modes.dedup();
        Ok(modes)
    }

    pub fn apply(&self, requested: DisplayMode) -> Result<DisplayMode, InputError> {
        let (snapshot, output, crtc) = self.snapshot()?;
        let (native, _) = snapshot
            .candidates(&output, &crtc)?
            .into_iter()
            .find(|(_, value)| *value == requested)
            .ok_or(InputError::Unsupported)?;
        if native != crtc.mode {
            let status = snapshot
                .connection
                .randr_set_crtc_config(
                    output.crtc,
                    CURRENT_TIME,
                    snapshot.resources.config_timestamp,
                    crtc.x,
                    crtc.y,
                    native,
                    crtc.rotation,
                    &crtc.outputs,
                )
                .map_err(|_| InputError::Rejected)?
                .reply()
                .map_err(|_| InputError::Rejected)?;
            if status.status != SetConfig::SUCCESS {
                return Err(InputError::Rejected);
            }
        }
        self.current()
    }

    fn snapshot(&self) -> Result<(Snapshot, GetOutputInfoReply, GetCrtcInfoReply), InputError> {
        let snapshot = Snapshot::open().ok_or(InputError::Unavailable)?;
        let output = snapshot
            .output(self.output)
            .ok_or(InputError::Unavailable)?;
        if output.connection != OutputConnection::CONNECTED
            || output.crtc == 0
            || snapshot.identity(self.output, &output) != self.key
        {
            return Err(InputError::Unavailable);
        }
        let controller = snapshot
            .connection
            .randr_get_crtc_info(output.crtc, snapshot.resources.config_timestamp)
            .map_err(|_| InputError::Unavailable)?
            .reply()
            .map_err(|_| InputError::Unavailable)?;
        if controller.outputs.as_slice() != [self.output] {
            return Err(InputError::Unsupported);
        }
        Ok((snapshot, output, controller))
    }
}

struct Snapshot {
    connection: RustConnection,
    root: Window,
    resources: GetScreenResourcesCurrentReply,
}

impl Snapshot {
    fn open() -> Option<Self> {
        let (connection, screen) = x11rb::connect(None).ok()?;
        let root = connection.setup().roots.get(screen)?.root;
        let resources = connection
            .randr_get_screen_resources_current(root)
            .ok()?
            .reply()
            .ok()?;
        Some(Self {
            connection,
            root,
            resources,
        })
    }

    fn output(&self, output: u32) -> Option<GetOutputInfoReply> {
        self.connection
            .randr_get_output_info(output, self.resources.config_timestamp)
            .ok()?
            .reply()
            .ok()
    }

    fn identity(&self, output: u32, details: &GetOutputInfoReply) -> String {
        let edid = (|| {
            let name = self
                .connection
                .intern_atom(true, b"EDID")
                .ok()?
                .reply()
                .ok()?
                .atom;
            if name == 0 {
                return None;
            }
            let data = self
                .connection
                .randr_get_output_property(output, name, AtomEnum::ANY, 0, 64, false, false)
                .ok()?
                .reply()
                .ok()?;
            (data.format == 8).then_some(data.data)
        })();
        let mut digest = Sha256::new();
        digest.update(&details.name);
        if let Some(edid) = edid {
            digest.update(edid);
        } else {
            digest.update(output.to_le_bytes());
        }
        let bytes: [u8; 32] = digest.finalize().into();
        let mut prefix = [0; 16];
        prefix.copy_from_slice(&bytes[..16]);
        format!("x11-{:032x}", u128::from_be_bytes(prefix))
    }

    fn candidates(
        &self,
        output: &GetOutputInfoReply,
        controller: &GetCrtcInfoReply,
    ) -> Result<Vec<(u32, DisplayMode)>, InputError> {
        let geometry = self
            .connection
            .get_geometry(self.root)
            .map_err(|_| InputError::Unavailable)?
            .reply()
            .map_err(|_| InputError::Unavailable)?;
        let x = u32::try_from(controller.x).map_err(|_| InputError::Unsupported)?;
        let y = u32::try_from(controller.y).map_err(|_| InputError::Unsupported)?;
        // Keep the session within the current X11 framebuffer so other outputs stay unchanged.
        Ok(self
            .resources
            .modes
            .iter()
            .filter(|mode| output.modes.contains(&mode.id))
            .filter_map(|mode| {
                let value = mode_value(mode, controller.rotation);
                (value.valid()
                    && x + value.width <= u32::from(geometry.width)
                    && y + value.height <= u32::from(geometry.height))
                .then_some((mode.id, value))
            })
            .collect())
    }
}

fn mode_value(mode: &ModeInfo, rotation: Rotation) -> DisplayMode {
    let rotated = rotation.contains(Rotation::ROTATE90) || rotation.contains(Rotation::ROTATE270);
    let scan = if mode.mode_flags.contains(ModeFlag::DOUBLE_SCAN) {
        2
    } else {
        1
    };
    let interlaced = if mode.mode_flags.contains(ModeFlag::INTERLACE) {
        2
    } else {
        1
    };
    let pixels = u64::from(mode.htotal) * u64::from(mode.vtotal) * scan;
    let frequency = (u64::from(mode.dot_clock) * 1000 * interlaced)
        .checked_div(pixels)
        .unwrap_or(0);
    DisplayMode {
        width: u32::from(if rotated { mode.height } else { mode.width }),
        height: u32::from(if rotated { mode.width } else { mode.height }),
        refresh_millihz: frequency.min(u64::from(u32::MAX)) as u32,
    }
}
