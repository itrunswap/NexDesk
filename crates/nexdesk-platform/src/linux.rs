use std::{
    ffi::{c_char, c_int, c_uint, c_ulong, c_void},
    ptr::{NonNull, null},
    sync::OnceLock,
};

use libloading::Library;

use crate::{Injector, Input, InputError, Region};

type Display = *mut c_void;
type FakeKey = unsafe extern "C" fn(Display, c_uint, c_int, c_ulong) -> c_int;
type FakeMove = unsafe extern "C" fn(Display, c_int, c_int, c_int, c_ulong) -> c_int;
type Close = unsafe extern "C" fn(Display) -> c_int;
type Query = unsafe extern "C" fn(Display, *mut c_int, *mut c_int, *mut c_int, *mut c_int) -> c_int;

static X11: OnceLock<Option<Library>> = OnceLock::new();

pub fn initialize() {
    let _ = x11();
}

fn x11() -> Option<&'static Library> {
    X11.get_or_init(|| {
        // SAFETY: The first call runs before the agent initializes webRTC or opens any displays.
        let library = unsafe { Library::new("libX11.so.6") }.ok()?;
        let initialize =
            unsafe { library.get::<unsafe extern "C" fn() -> c_int>(b"XInitThreads") }.ok()?;
        if unsafe { initialize() } == 0 {
            return None;
        }
        Some(library)
    })
    .as_ref()
}

pub fn request_permissions() -> bool {
    NativeInput::open().is_ok()
}

pub fn screen_region(_id: u64, _platform_id: i64, captured: Region) -> Option<Region> {
    (captured.width > 0 && captured.height > 0).then_some(captured)
}

pub struct NativeInput {
    display: NonNull<c_void>,
    keyboard: FakeKey,
    mouse: FakeKey,
    move_to: FakeMove,
    flush: Close,
    close: Close,
    _xtest: Library,
}

// SAFETY: This connection has one owner. XInitThreads runs before it is opened,
// and calls are serialized by the session worker even if Tokio moves the worker.
unsafe impl Send for NativeInput {}

impl NativeInput {
    pub fn open() -> Result<Self, InputError> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland")
        {
            return Err(InputError::Unavailable);
        }
        // SAFETY: These are standard system SONAMEs. Symbols are copied only while
        // their libraries are held by the returned input adapter.
        unsafe {
            let x11 = x11().ok_or(InputError::Unavailable)?;
            let xtest = Library::new("libXtst.so.6").map_err(|_| InputError::Unavailable)?;
            let open =
                symbol::<unsafe extern "C" fn(*const c_char) -> Display>(x11, b"XOpenDisplay")?;
            let close = symbol::<Close>(x11, b"XCloseDisplay")?;
            let flush = symbol::<Close>(x11, b"XFlush")?;
            let query = symbol::<Query>(&xtest, b"XTestQueryExtension")?;
            let keyboard = symbol::<FakeKey>(&xtest, b"XTestFakeKeyEvent")?;
            let mouse = symbol::<FakeKey>(&xtest, b"XTestFakeButtonEvent")?;
            let move_to = symbol::<FakeMove>(&xtest, b"XTestFakeMotionEvent")?;
            let display = NonNull::new(open(null())).ok_or(InputError::Unavailable)?;
            let (mut event, mut error, mut major, mut minor) = (0, 0, 0, 0);
            if query(
                display.as_ptr(),
                &mut event,
                &mut error,
                &mut major,
                &mut minor,
            ) == 0
            {
                close(display.as_ptr());
                return Err(InputError::Unavailable);
            }
            Ok(Self {
                display,
                keyboard,
                mouse,
                move_to,
                flush,
                close,
                _xtest: xtest,
            })
        }
    }
}

impl Injector for NativeInput {
    fn send(&mut self, input: Input) -> Result<(), InputError> {
        // SAFETY: This method exclusively owns the connection during each call.
        let result = unsafe {
            let display = self.display.as_ptr();
            let ok = match input {
                Input::Move { x, y } => (self.move_to)(display, -1, x.max(0), y.max(0), 0),
                Input::Button { button, down } => {
                    let x_button = match button {
                        1 => 1,
                        2 => 3,
                        3 => 2,
                        4 => 8,
                        5 => 9,
                        _ => return Err(InputError::Unsupported),
                    };
                    (self.mouse)(display, x_button, c_int::from(down), 0)
                }
                Input::Key { usage, down } => (self.keyboard)(
                    display,
                    u32::from(evdev(usage).ok_or(InputError::Unsupported)?) + 8,
                    c_int::from(down),
                    0,
                ),
                Input::Wheel {
                    horizontal,
                    vertical,
                } => {
                    let mut ok = 1;
                    for (ticks, back, forward) in [(horizontal, 6, 7), (vertical, 5, 4)] {
                        if ticks == 0 {
                            continue;
                        }
                        for _ in 0..ticks.unsigned_abs().div_ceil(120).min(10) {
                            let button = if ticks < 0 { back } else { forward };
                            ok &= (self.mouse)(display, button, 1, 0);
                            ok &= (self.mouse)(display, button, 0, 0);
                        }
                    }
                    ok
                }
            };
            (self.flush)(display);
            ok
        };
        if result == 0 {
            Err(InputError::Rejected)
        } else {
            Ok(())
        }
    }
}

impl Drop for NativeInput {
    fn drop(&mut self) {
        // SAFETY: The display was opened by this adapter and is closed exactly once.
        unsafe {
            (self.close)(self.display.as_ptr());
        }
    }
}

unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, InputError> {
    // SAFETY: Each caller uses the corresponding X11 or XTEST C signature.
    unsafe { library.get::<T>(name) }
        .map(|value| *value)
        .map_err(|_| InputError::Unavailable)
}

fn evdev(usage: u32) -> Option<u16> {
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
        68 => 87,
        69 => 88,
        70 => 99,
        71 => 70,
        72 => 119,
        73 => 110,
        74 => 102,
        75 => 104,
        76 => 111,
        77 => 107,
        78 => 109,
        79 => 106,
        80 => 105,
        81 => 108,
        82 => 103,
        83 => 69,
        84 => 98,
        85 => 55,
        86 => 74,
        87 => 78,
        88 => 96,
        89 => 79,
        90 => 80,
        91 => 81,
        92 => 75,
        93 => 76,
        94 => 77,
        95 => 71,
        96 => 72,
        97 => 73,
        98 => 82,
        99 => 83,
        100 => 86,
        101 => 127,
        104..=115 => (id + 79) as u16,
        224..=231 => [29, 42, 56, 125, 97, 54, 100, 126][id - 224],
        _ => return None,
    })
}
