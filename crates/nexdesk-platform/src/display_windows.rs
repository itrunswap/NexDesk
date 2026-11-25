use std::{
    mem::size_of,
    ptr::{null, null_mut},
};

use sha2::{Digest, Sha256};
use windows_sys::Win32::Graphics::Gdi::{
    CDS_TEST, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICE_ACTIVE,
    DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplaySettingsExW,
};

use crate::{DisplayMode, InputError};

const DEVICE_INTERFACE_NAME: u32 = 1;

#[derive(Clone, Debug)]
pub struct Monitor {
    adapter: [u16; 32],
    key: String,
}

impl Monitor {
    pub fn locate(id: u64, _: i64) -> Option<Self> {
        let mut adapter = device_info();
        // SAFETY: The caller passes a source index and both output buffers have Win32's required size.
        if unsafe { EnumDisplayDevicesW(null(), u32::try_from(id).ok()?, &mut adapter, 0) } == 0
            || adapter.StateFlags & DISPLAY_DEVICE_ACTIVE == 0
        {
            return None;
        }
        let key = identity(&adapter.DeviceName)?;
        Some(Self {
            adapter: adapter.DeviceName,
            key,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn current(&self) -> Result<DisplayMode, InputError> {
        Ok(mode_value(&self.current_native()?))
    }

    pub fn modes(&self) -> Result<Vec<DisplayMode>, InputError> {
        let current = self.current_native()?;
        let mut values: Vec<_> = self.candidates(&current).map(|(_, value)| value).collect();
        values.sort_unstable_by_key(|m| (m.width, m.height, m.refresh_millihz));
        values.dedup();
        Ok(values)
    }

    pub fn apply(&self, requested: DisplayMode) -> Result<DisplayMode, InputError> {
        let current = self.current_native()?;
        if mode_value(&current) == requested {
            return Ok(requested);
        }
        let (mode, _) = self
            .candidates(&current)
            .find(|(_, value)| *value == requested)
            .ok_or(InputError::Unsupported)?;
        // SAFETY: The adapter was matched to its physical monitor and mode comes from
        // EnumDisplaySettingsExW. The test leaves Windows unchanged.
        unsafe {
            if ChangeDisplaySettingsExW(self.adapter.as_ptr(), &mode, null_mut(), CDS_TEST, null())
                != DISP_CHANGE_SUCCESSFUL
                || ChangeDisplaySettingsExW(self.adapter.as_ptr(), &mode, null_mut(), 0, null())
                    != DISP_CHANGE_SUCCESSFUL
            {
                return Err(InputError::Rejected);
            }
        }
        self.current()
    }

    fn current_native(&self) -> Result<DEVMODEW, InputError> {
        if identity(&self.adapter).as_deref() != Some(&self.key) {
            return Err(InputError::Unavailable);
        }
        self.enum_mode(ENUM_CURRENT_SETTINGS)
            .ok_or(InputError::Unavailable)
    }

    fn candidates<'a>(
        &'a self,
        current: &'a DEVMODEW,
    ) -> impl Iterator<Item = (DEVMODEW, DisplayMode)> + 'a {
        (0..1024)
            .map_while(|index| self.enum_mode(index))
            .filter_map(move |mode| {
                // SAFETY: DEVMODEW was filled by the display-mode API.
                let orientation = unsafe { mode.Anonymous1.Anonymous2.dmDisplayOrientation };
                let current_orientation =
                    unsafe { current.Anonymous1.Anonymous2.dmDisplayOrientation };
                let flags = unsafe { mode.Anonymous2.dmDisplayFlags };
                let current_flags = unsafe { current.Anonymous2.dmDisplayFlags };
                let value = mode_value(&mode);
                (value.valid()
                    && orientation == current_orientation
                    && mode.dmBitsPerPel == current.dmBitsPerPel
                    && flags == current_flags)
                    .then_some((mode, value))
            })
    }

    fn enum_mode(&self, index: u32) -> Option<DEVMODEW> {
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: The initialized DEVMODEW and NUL-terminated adapter buffer remain valid.
        (unsafe { EnumDisplaySettingsExW(self.adapter.as_ptr(), index, &mut mode, 0) } != 0)
            .then_some(mode)
    }
}

fn device_info() -> DISPLAY_DEVICEW {
    DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    }
}

fn identity(adapter: &[u16; 32]) -> Option<String> {
    let mut monitor = device_info();
    // SAFETY: Fixed, NUL-terminated adapter name and a correctly sized result buffer.
    if unsafe { EnumDisplayDevicesW(adapter.as_ptr(), 0, &mut monitor, DEVICE_INTERFACE_NAME) } == 0
    {
        return None;
    }
    let id: Vec<_> = monitor
        .DeviceID
        .iter()
        .copied()
        .take_while(|ch| *ch != 0)
        .flat_map(u16::to_le_bytes)
        .collect();
    if id.is_empty() {
        return None;
    }
    let digest = Sha256::digest(&id);
    Some(format!(
        "win-{:x}",
        u128::from_be_bytes(digest[..16].try_into().ok()?)
    ))
}

fn mode_value(mode: &DEVMODEW) -> DisplayMode {
    DisplayMode {
        width: mode.dmPelsWidth,
        height: mode.dmPelsHeight,
        refresh_millihz: mode.dmDisplayFrequency.saturating_mul(1000),
    }
}
