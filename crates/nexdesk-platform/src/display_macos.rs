use std::{
    ffi::{c_char, c_void},
    ptr::{NonNull, null},
};

use crate::{DisplayMode, InputError};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayIsActive(id: u32) -> u32;
    fn CGDisplayIsInMirrorSet(id: u32) -> u32;
    fn CGDisplayCopyDisplayMode(id: u32) -> *mut c_void;
    fn CGDisplayCopyAllDisplayModes(id: u32, options: *const c_void) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize;
    fn CGDisplayModeGetRefreshRate(mode: *const c_void) -> f64;
    fn CGDisplaySetDisplayMode(id: u32, mode: *const c_void, options: *const c_void) -> i32;
    fn CGDisplayModeIsUsableForDesktopGUI(mode: *const c_void) -> bool;
}

#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCreateUUIDFromDisplayID(id: u32) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFUUIDCreateString(allocator: *const c_void, uuid: *const c_void) -> *mut c_void;
    fn CFStringGetCString(
        text: *const c_void,
        buffer: *mut c_char,
        length: isize,
        encoding: u32,
    ) -> bool;
}

struct Owned(NonNull<c_void>);

impl Owned {
    fn new(pointer: *mut c_void) -> Option<Self> {
        NonNull::new(pointer).map(Self)
    }
    fn get(&self) -> *const c_void {
        self.0.as_ptr()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: Each retained CoreFoundation value is released once.
        unsafe { CFRelease(self.get()) };
    }
}

#[derive(Clone, Debug)]
pub struct Monitor {
    id: u32,
    key: String,
}

impl Monitor {
    pub fn locate(id: u64, platform: i64) -> Option<Self> {
        let id = if platform > 0 {
            u32::try_from(platform).ok()?
        } else {
            u32::try_from(id).ok()?
        };
        let key = identity(id)?;
        Some(Self { id, key })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn current(&self) -> Result<DisplayMode, InputError> {
        self.check()?;
        // SAFETY: The ID was matched on the active display; Copy returns an owned mode.
        let mode = Owned::new(unsafe { CGDisplayCopyDisplayMode(self.id) })
            .ok_or(InputError::Unavailable)?;
        mode_value(mode.get()).ok_or(InputError::Unsupported)
    }

    pub fn modes(&self) -> Result<Vec<DisplayMode>, InputError> {
        let collection = self.collection()?;
        let mut modes: Vec<_> = items(&collection).filter_map(mode_value).collect();
        modes.sort_unstable_by_key(|mode| (mode.width, mode.height, mode.refresh_millihz));
        modes.dedup();
        Ok(modes)
    }

    pub fn apply(&self, requested: DisplayMode) -> Result<DisplayMode, InputError> {
        let collection = self.collection()?;
        let mode = items(&collection)
            .find(|&mode| mode_value(mode) == Some(requested))
            .ok_or(InputError::Unsupported)?;
        // SAFETY: The borrowed mode belongs to the retained collection for this display.
        if unsafe { CGDisplaySetDisplayMode(self.id, mode, null()) } != 0 {
            return Err(InputError::Rejected);
        }
        self.current()
    }

    fn check(&self) -> Result<(), InputError> {
        if identity(self.id).as_deref() != Some(&self.key) {
            return Err(InputError::Unavailable);
        }
        Ok(())
    }

    fn collection(&self) -> Result<Owned, InputError> {
        self.check()?;
        // SAFETY: Copy returns an owned array; default mode selection uses null options.
        Owned::new(unsafe { CGDisplayCopyAllDisplayModes(self.id, null()) })
            .ok_or(InputError::Unavailable)
    }
}

fn items(array: &Owned) -> impl Iterator<Item = *const c_void> + '_ {
    // SAFETY: array contains display modes and is retained for the iteration.
    (0..unsafe { CFArrayGetCount(array.get()) }.min(1024))
        .map(|index| unsafe { CFArrayGetValueAtIndex(array.get(), index) })
        .filter(|mode| !mode.is_null())
}

fn mode_value(mode: *const c_void) -> Option<DisplayMode> {
    // SAFETY: Private callers hold the retained mode or its owning collection.
    let value = unsafe {
        if !CGDisplayModeIsUsableForDesktopGUI(mode) {
            return None;
        }
        DisplayMode {
            width: u32::try_from(CGDisplayModeGetPixelWidth(mode)).ok()?,
            height: u32::try_from(CGDisplayModeGetPixelHeight(mode)).ok()?,
            refresh_millihz: (CGDisplayModeGetRefreshRate(mode) * 1000.0).round() as u32,
        }
    };
    value.valid().then_some(value)
}

fn identity(id: u32) -> Option<String> {
    // SAFETY: Check the active ID, then use owned CoreFoundation UUID and string values.
    unsafe {
        if CGDisplayIsActive(id) == 0 || CGDisplayIsInMirrorSet(id) != 0 {
            return None;
        }
        let identity = Owned::new(CGDisplayCreateUUIDFromDisplayID(id))?;
        let text = Owned::new(CFUUIDCreateString(null(), identity.get()))?;
        let mut buffer = [0u8; 64];
        if !CFStringGetCString(text.get(), buffer.as_mut_ptr().cast(), 64, 0x0800_0100) {
            return None;
        }
        let end = buffer.iter().position(|&byte| byte == 0)?;
        let uuid = std::str::from_utf8(&buffer[..end]).ok()?;
        Some(format!("mac-{uuid}"))
    }
}
