use std::{
    ffi::c_void,
    ptr::{NonNull, null},
};

use crate::{Injector, Input, InputError, Region};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}

#[repr(C)]
struct Size {
    width: f64,
    height: f64,
}

#[repr(C)]
struct Rect {
    origin: Point,
    size: Size,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayBounds(display: u32) -> Rect;
    fn CGDisplayIsActive(display: u32) -> u32;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *const c_void) -> Point;
    fn CGEventCreateMouseEvent(
        source: *const c_void,
        kind: u32,
        point: Point,
        button: u32,
    ) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
    fn CGEventKeyboardSetUnicodeString(event: *mut c_void, count: usize, text: *const u16);
    fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        count: u32,
        first: i32,
        second: i32,
        third: i32,
    ) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
}

pub fn initialize() {}

pub fn request_permissions() -> bool {
    // SAFETY: Invoked by the foreground operator, with no borrowed objects.
    unsafe {
        let capture = CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess();
        let input = AXIsProcessTrusted() || CGRequestPostEventAccess();
        capture && input
    }
}

pub fn screen_region(id: u64, platform_id: i64, _captured: Region) -> Option<Region> {
    let id = if platform_id > 0 {
        u32::try_from(platform_id).ok()?
    } else {
        u32::try_from(id).ok()?
    };
    // SAFETY: CoreGraphics validates the ID. Bounds are logical display coordinates.
    let rect = unsafe {
        if CGDisplayIsActive(id) == 0 {
            return None;
        }
        CGDisplayBounds(id)
    };
    (rect.size.width > 0.0 && rect.size.height > 0.0).then_some(Region {
        left: rect.origin.x.round() as i32,
        top: rect.origin.y.round() as i32,
        width: rect.size.width.round() as u32,
        height: rect.size.height.round() as u32,
    })
}

pub struct NativeInput {
    point: Point,
    buttons: [bool; 5],
    modifiers: u8,
}

impl NativeInput {
    pub fn open() -> Result<Self, InputError> {
        // SAFETY: The OS permission check is local and never requests elevation.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(InputError::Unavailable);
        }
        // SAFETY: A null source uses the default event source. The new event owns itself.
        let initial = Event::from_raw(unsafe { CGEventCreate(null()) })?;
        // SAFETY: `initial` keeps the event alive for this call.
        let point = unsafe { CGEventGetLocation(initial.0.as_ptr()) };
        Ok(Self {
            point,
            buttons: [false; 5],
            modifiers: 0,
        })
    }
}

impl Injector for NativeInput {
    fn insert_text(&mut self, text: &str) -> Result<(), InputError> {
        // SAFETY: Accessibility is checked before posting any keyboard events.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(InputError::Unavailable);
        }
        for character in text.chars() {
            let mut units = [0; 2];
            let encoded = character.encode_utf16(&mut units);
            for down in [true, false] {
                // SAFETY: The event and the UTF-16 buffer live through the synchronous calls.
                let event =
                    Event::from_raw(unsafe { CGEventCreateKeyboardEvent(null(), 0, down) })?;
                unsafe {
                    CGEventKeyboardSetUnicodeString(
                        event.0.as_ptr(),
                        encoded.len(),
                        encoded.as_ptr(),
                    );
                    CGEventPost(0, event.0.as_ptr());
                }
            }
        }
        Ok(())
    }

    fn send(&mut self, input: Input) -> Result<(), InputError> {
        // SAFETY: A revoked Accessibility permission fails closed and new events use an owned object.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(InputError::Unavailable);
        }
        let event = match input {
            Input::Move { x, y } => {
                self.point = Point {
                    x: f64::from(x),
                    y: f64::from(y),
                };
                let (kind, button) = match self.buttons.iter().position(|down| *down) {
                    Some(0) => (6, 0),
                    Some(1) => (7, 1),
                    Some(index) => (27, index as u32),
                    None => (5, 0),
                };
                // SAFETY: CGPoint and event constants match the CoreGraphics ABI.
                unsafe { CGEventCreateMouseEvent(null(), kind, self.point, button) }
            }
            Input::Button { button, down } => {
                if !(1..=5).contains(&button) {
                    return Err(InputError::Unsupported);
                }
                let index = usize::from(button - 1);
                let kind = match (index, down) {
                    (0, true) => 1,
                    (0, false) => 2,
                    (1, true) => 3,
                    (1, false) => 4,
                    (_, true) => 25,
                    (_, false) => 26,
                };
                self.buttons[index] = down;
                // SAFETY: The button number is within the supported range.
                unsafe { CGEventCreateMouseEvent(null(), kind, self.point, index as u32) }
            }
            Input::Wheel {
                horizontal,
                vertical,
            } => {
                // SAFETY: Pixel scroll event expects two signed axis values.
                unsafe { CGEventCreateScrollWheelEvent2(null(), 0, 2, vertical, horizontal, 0) }
            }
            Input::Key { usage, down } => {
                let code = key_code(usage).ok_or(InputError::Unsupported)?;
                if (0x0007_00e0..=0x0007_00e7).contains(&usage) {
                    let bit = 1 << (usage - 0x0007_00e0);
                    if down {
                        self.modifiers |= bit;
                    } else {
                        self.modifiers &= !bit;
                    }
                }
                // SAFETY: The keycode is a validated hardware key enum, passed by value.
                unsafe { CGEventCreateKeyboardEvent(null(), code, down) }
            }
        };
        let event = Event::from_raw(event)?;
        let mut flags = 0;
        for (index, mask) in [1u64 << 18, 1u64 << 17, 1u64 << 19, 1u64 << 20]
            .into_iter()
            .enumerate()
        {
            if self.modifiers & (0x11 << index) != 0 {
                flags |= mask;
            }
        }
        // SAFETY: Event is retained until CoreGraphics has consumed the post call.
        unsafe {
            CGEventSetFlags(event.0.as_ptr(), flags);
            CGEventPost(0, event.0.as_ptr());
        }
        Ok(())
    }
}

struct Event(NonNull<c_void>);

impl Event {
    fn from_raw(pointer: *mut c_void) -> Result<Self, InputError> {
        NonNull::new(pointer).map(Self).ok_or(InputError::Rejected)
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: Each CGEventCreate* success transfers exactly one reference.
        unsafe { CFRelease(self.0.as_ptr()) };
    }
}

fn key_code(usage: u32) -> Option<u16> {
    if usage >> 16 != 7 {
        return None;
    }
    let id = (usage & 0xffff) as usize;
    Some(match id {
        4..=29 => [
            0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7,
            16, 6,
        ][id - 4],
        30..=39 => [18, 19, 20, 21, 23, 22, 26, 28, 25, 29][id - 30],
        40..=57 => [
            36, 53, 51, 48, 49, 27, 24, 33, 30, 42, 10, 41, 39, 50, 43, 47, 44, 57,
        ][id - 40],
        58..=69 => [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111][id - 58],
        73 => 114,
        74 => 115,
        75 => 116,
        76 => 117,
        77 => 119,
        78 => 121,
        79 => 124,
        80 => 123,
        81 => 125,
        82 => 126,
        83 => 71,
        84..=99 => [
            75, 67, 78, 69, 76, 83, 84, 85, 86, 87, 88, 89, 91, 92, 82, 65,
        ][id - 84],
        224..=231 => [59, 56, 58, 55, 62, 60, 61, 54][id - 224],
        _ => return None,
    })
}
