use std::{mem::size_of, ptr::null};

use windows_sys::Win32::{
    Graphics::Gdi::{
        DEVMODEW, DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS,
        EnumDisplayDevicesW, EnumDisplaySettingsExW,
    },
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE,
            MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
            MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
            MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP,
            MOUSEINPUT, SendInput,
        },
        WindowsAndMessaging::{
            GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN,
        },
    },
};

use crate::{Injector, Input, InputError, Region};

pub fn initialize() {
    // SAFETY: The process calls this before creating any windows or capturers.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn request_permissions() -> bool {
    true
}

pub fn screen_region(id: u64, _platform_id: i64, _captured: Region) -> Option<Region> {
    let id = u32::try_from(id).ok()?;
    let mut device = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..DISPLAY_DEVICEW::default()
    };
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..DEVMODEW::default()
    };
    // SAFETY: Both initialized structs have SDK sizes. DeviceName is NUL-terminated by Windows.
    let position = unsafe {
        if EnumDisplayDevicesW(null(), id, &mut device, 0) == 0
            || device.StateFlags & DISPLAY_DEVICE_ACTIVE == 0
            || EnumDisplaySettingsExW(
                device.DeviceName.as_ptr(),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
                0,
            ) == 0
        {
            return None;
        }
        mode.Anonymous1.Anonymous2.dmPosition
    };
    (mode.dmPelsWidth > 0 && mode.dmPelsHeight > 0).then_some(Region {
        left: position.x,
        top: position.y,
        width: mode.dmPelsWidth,
        height: mode.dmPelsHeight,
    })
}

pub struct NativeInput;

impl NativeInput {
    pub fn open() -> Result<Self, InputError> {
        Ok(Self)
    }
}

impl Injector for NativeInput {
    fn insert_text(&mut self, text: &str) -> Result<(), InputError> {
        let events: Vec<_> = text
            .encode_utf16()
            .flat_map(|unit| {
                [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP].map(|flags| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wScan: unit,
                            dwFlags: flags,
                            ..Default::default()
                        },
                    },
                })
            })
            .collect();
        if events.is_empty() {
            return Ok(());
        }
        // SAFETY: All elements are initialized Win32 keyboard INPUT values.
        let accepted = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        if accepted as usize != events.len() {
            return Err(InputError::Rejected);
        }
        Ok(())
    }

    fn send(&mut self, input: Input) -> Result<(), InputError> {
        match input {
            Input::Move { x, y } => {
                // SAFETY: Read-only Win32 metrics, with checked nonzero extents.
                let (left, top, width, height) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN),
                    )
                };
                if width < 1 || height < 1 {
                    return Err(InputError::Unavailable);
                }
                let axis = |point: i32, start: i32, pixels: i32| {
                    let length = i64::from(pixels);
                    let offset = (i64::from(point) - i64::from(start)).clamp(0, length - 1);
                    ((offset * 65536 + length / 2) / length).min(65535) as i32
                };
                emit(mouse(
                    MOUSEEVENTF_VIRTUALDESK | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE,
                    axis(x, left, width),
                    axis(y, top, height),
                    0,
                ))
            }
            Input::Button { button, down } => {
                let (flags, value) = match (button, down) {
                    (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (1, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (3, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (3, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (4 | 5, true) => (MOUSEEVENTF_XDOWN, u32::from(button - 3)),
                    (4 | 5, false) => (MOUSEEVENTF_XUP, u32::from(button - 3)),
                    _ => return Err(InputError::Unsupported),
                };
                emit(mouse(flags, 0, 0, value))
            }
            Input::Wheel {
                horizontal,
                vertical,
            } => {
                if vertical != 0 {
                    emit(mouse(MOUSEEVENTF_WHEEL, 0, 0, vertical as u32))?;
                }
                if horizontal != 0 {
                    emit(mouse(MOUSEEVENTF_HWHEEL, 0, 0, horizontal as u32))?;
                }
                Ok(())
            }
            Input::Key { usage, down } => {
                let mut flags = if down { 0 } else { KEYEVENTF_KEYUP };
                let (vk, code) = match usage {
                    0x0007_0046 => (0x2c, 0),
                    0x0007_0048 => (0x13, 0),
                    _ => {
                        let scan = scan_code(usage).ok_or(InputError::Unsupported)?;
                        flags |= KEYEVENTF_SCANCODE;
                        if scan >> 8 == 0xe0 {
                            flags |= KEYEVENTF_EXTENDEDKEY;
                        }
                        (0, scan & 0xff)
                    }
                };
                emit(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk,
                            wScan: code,
                            dwFlags: flags,
                            ..KEYBDINPUT::default()
                        },
                    },
                })
            }
        }
    }
}

fn mouse(flags: u32, dx: i32, dy: i32, value: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: value,
                dwFlags: flags,
                ..MOUSEINPUT::default()
            },
        },
    }
}

fn emit(input: INPUT) -> Result<(), InputError> {
    // SAFETY: The pointer refers to one initialized INPUT for the duration of this call.
    if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } == 1 {
        Ok(())
    } else {
        Err(InputError::Rejected)
    }
}

fn scan_code(usage: u32) -> Option<u16> {
    if usage >> 16 != 7 {
        return None;
    }
    let id = (usage & 0xffff) as usize;
    Some(match id {
        4..=29 => [
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
            17, 45, 21, 44,
        ][id - 4],
        30..=39 => (id - 28) as u16,
        40..=57 => [
            28, 1, 14, 15, 57, 12, 13, 26, 27, 43, 86, 39, 40, 41, 51, 52, 53, 58,
        ][id - 40],
        58..=67 => (id + 1) as u16,
        68 => 0x57,
        69 => 0x58,
        71 => 0x46,
        73..=82 => [
            0xe052, 0xe047, 0xe049, 0xe053, 0xe04f, 0xe051, 0xe04d, 0xe04b, 0xe050, 0xe048,
        ][id - 73],
        83..=99 => [
            0x45, 0xe035, 0x37, 0x4a, 0x4e, 0xe01c, 0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48,
            0x49, 0x52, 0x53,
        ][id - 83],
        100 => 0x56,
        101 => 0xe05d,
        224..=231 => [0x1d, 0x2a, 0x38, 0xe05b, 0xe01d, 0x36, 0xe038, 0xe05c][id - 224],
        _ => return None,
    })
}
