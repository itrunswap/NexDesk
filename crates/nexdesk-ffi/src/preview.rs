use std::{
    collections::HashMap,
    ffi::c_void,
    net::SocketAddr,
    ptr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering::Relaxed},
    },
    thread,
    time::Duration,
};

use nexdesk_core::LanEndpoint;
use nexdesk_media::PixelFrame;
use nexdesk_session::{
    SessionError, connect,
    pairing::{AccessCode, HostIdentity, PairingError},
};
use tokio::sync::oneshot;

use crate::desktop::DesktopContext;
use crate::input_text;

const MAX_VIEWERS: usize = 4;
pub(crate) const CONNECTING: u32 = 1;
pub(crate) const STREAMING: u32 = 2;
pub(crate) const STOPPED: u32 = 3;
pub(crate) const REJECTED: u32 = 4;
pub(crate) const HOST_CHANGED: u32 = 5;
pub(crate) const UNREACHABLE: u32 = 6;
pub(crate) const MEDIA_FAILED: u32 = 7;
pub(crate) const BUSY: u32 = 8;

static VIEWERS: OnceLock<Mutex<HashMap<u64, Arc<Viewer>>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Viewer {
    status: AtomicU32,
    latest: Mutex<Option<PublishedFrame>>,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
    presented: AtomicU64,
    last_presented: AtomicU64,
    last_texture_wait_us: AtomicU64,
    pub(crate) desktop: Option<DesktopContext>,
}

struct PublishedFrame {
    sequence: u64,
    frame: Arc<PixelFrame>,
}

#[repr(C)]
pub struct PreviewFrame {
    pixels: *const u8,
    width: u32,
    height: u32,
}

fn viewers() -> &'static Mutex<HashMap<u64, Arc<Viewer>>> {
    VIEWERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn lookup(id: u64) -> Option<Arc<Viewer>> {
    viewers().lock().ok()?.get(&id).cloned()
}

impl Viewer {
    pub(crate) fn create(
        desktop: Option<DesktopContext>,
        cancel: oneshot::Sender<()>,
    ) -> Option<(u64, Arc<Self>)> {
        let id = NEXT_ID.fetch_add(1, Relaxed);
        let viewer = Arc::new(Self {
            status: AtomicU32::new(CONNECTING),
            latest: Mutex::new(None),
            cancel: Mutex::new(Some(cancel)),
            presented: AtomicU64::new(0),
            last_presented: AtomicU64::new(0),
            last_texture_wait_us: AtomicU64::new(0),
            desktop,
        });
        let mut registry = viewers().lock().ok()?;
        if registry.len() >= MAX_VIEWERS || id == 0 || registry.contains_key(&id) {
            return None;
        }
        registry.insert(id, viewer.clone());
        Some((id, viewer))
    }

    pub(crate) fn publish(&self, sequence: u64, frame: PixelFrame) -> bool {
        let Ok(mut latest) = self.latest.lock() else {
            return false;
        };
        *latest = Some(PublishedFrame {
            sequence,
            frame: Arc::new(frame),
        });
        self.set_status(STREAMING);
        true
    }

    pub(crate) fn set_status(&self, status: u32) {
        self.status.store(status, Relaxed);
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = None;
        }
    }
}

/// Establishes a read-only session asynchronously; zero indicates invalid arguments or capacity.
///
/// # Safety
/// Each non-null text pointer with a length in 1..=128 must point at a single
/// readable allocation of that length for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m1_connect(
    address: *const u8,
    address_len: usize,
    port: u16,
    code: *const u8,
    code_len: usize,
    identity: *const u8,
    identity_len: usize,
) -> u64 {
    // SAFETY: Text is borrowed only while parsing caller-provided buffers during this call.
    let (address, code, identity) = unsafe {
        (
            input_text(address, address_len),
            input_text(code, code_len),
            input_text(identity, identity_len),
        )
    };
    let (Ok(address), Ok(code), Ok(identity)) = (address, code, identity) else {
        return 0;
    };
    let Ok(address) = LanEndpoint::parse(address, &port.to_string()) else {
        return 0;
    };
    let Ok(code) = AccessCode::parse(code.as_bytes()) else {
        return 0;
    };
    let trusted = if identity.trim().is_empty() {
        None
    } else if let Ok(host) = identity.parse() {
        Some(host)
    } else {
        return 0;
    };
    let (cancel, canceled) = oneshot::channel();
    let Some((id, viewer)) = Viewer::create(None, cancel) else {
        return 0;
    };
    if thread::Builder::new()
        .name("nexdesk-viewer".into())
        .spawn(move || run_viewer(viewer, address.socket_addr(), code, trusted, canceled))
        .is_err()
    {
        nexdesk_m1_disconnect(id);
        return 0;
    }
    id
}

fn run_viewer(
    viewer: Arc<Viewer>,
    address: SocketAddr,
    code: AccessCode,
    trusted: Option<HostIdentity>,
    canceled: oneshot::Receiver<()>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let status = match runtime {
        Ok(runtime) => runtime.block_on(async {
            tokio::select! {
                _ = canceled => STOPPED,
                result = receive(&viewer, address, code, trusted) => result,
            }
        }),
        Err(_) => MEDIA_FAILED,
    };
    viewer.set_status(status);
    viewer.clear();
}

async fn receive(
    viewer: &Viewer,
    address: SocketAddr,
    code: AccessCode,
    trusted: Option<HostIdentity>,
) -> u32 {
    let mut session = match connect(address, code, trusted).await {
        Ok(session) => session,
        Err(SessionError::Pairing(PairingError::IdentityChanged)) => return HOST_CHANGED,
        Err(SessionError::Rejected | SessionError::Pairing(_)) => return REJECTED,
        Err(SessionError::Unreachable) => return UNREACHABLE,
        Err(SessionError::Busy) => return BUSY,
        Err(_) => return MEDIA_FAILED,
    };
    let mut sequence = 1;
    loop {
        match tokio::time::timeout(Duration::from_secs(8), session.view.next_frame()).await {
            Ok(Ok(Some(frame))) => {
                if !viewer.publish(sequence, frame) {
                    return MEDIA_FAILED;
                }
                sequence += 1;
            }
            Ok(Ok(None)) => return STOPPED,
            Ok(Err(_)) => return MEDIA_FAILED,
            Err(_) if session.view.has_ended() => return STOPPED,
            Err(_) if sequence == 1 => return MEDIA_FAILED,
            Err(_) => continue,
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_m1_status(id: u64) -> u32 {
    lookup(id).map_or(STOPPED, |viewer| viewer.status.load(Relaxed))
}

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_m1_disconnect(id: u64) {
    let viewer = viewers()
        .lock()
        .ok()
        .and_then(|mut viewers| viewers.remove(&id));
    if let Some(viewer) = viewer
        && let Ok(mut cancel) = viewer.cancel.lock()
        && let Some(sender) = cancel.take()
    {
        let _ = sender.send(());
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_m1_frame_sequence(id: u64) -> u64 {
    lookup(id)
        .and_then(|viewer| {
            viewer
                .latest
                .lock()
                .ok()?
                .as_ref()
                .map(|frame| frame.sequence)
        })
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_m1_presented_frames(id: u64) -> u64 {
    lookup(id).map_or(0, |viewer| viewer.presented.load(Relaxed))
}

/// Width in the high word and height in the low word, or zero before the first frame.
#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_m1_frame_size(id: u64) -> u64 {
    lookup(id)
        .and_then(|viewer| {
            viewer.latest.lock().ok()?.as_ref().map(|latest| {
                (u64::from(latest.frame.width) << 32) | u64::from(latest.frame.height)
            })
        })
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_m1_texture_wait_us(id: u64) -> u64 {
    lookup(id).map_or(0, |viewer| viewer.last_texture_wait_us.load(Relaxed))
}

/// Leases the newest RGBA frame to the texture callback. Zero return means no frame.
///
/// # Safety
/// When non-null, `output` must be aligned and writable for one `PreviewFrame`.
/// A returned handle must be released exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m1_acquire_frame(
    id: u64,
    output: *mut PreviewFrame,
) -> *const c_void {
    if output.is_null() {
        return ptr::null();
    }
    let Some((viewer, sequence, frame)) = acquire_latest(id) else {
        return ptr::null();
    };
    let pixels = frame.rgba.as_ptr();
    mark_presented(&viewer, sequence, &frame);
    // SAFETY: Caller provides a writable struct. The Arc lease keeps pixels valid.
    unsafe {
        output.write(PreviewFrame {
            pixels,
            width: frame.width,
            height: frame.height,
        })
    };
    Arc::into_raw(frame).cast::<c_void>()
}

fn acquire_latest(id: u64) -> Option<(Arc<Viewer>, u64, Arc<PixelFrame>)> {
    let viewer = lookup(id)?;
    let (sequence, frame) = {
        let guard = viewer.latest.lock().ok()?;
        let latest = guard.as_ref()?;
        (latest.sequence, latest.frame.clone())
    };
    Some((viewer, sequence, frame))
}

fn mark_presented(viewer: &Viewer, sequence: u64, frame: &PixelFrame) {
    if viewer.last_presented.fetch_max(sequence, Relaxed) < sequence {
        viewer.presented.fetch_add(1, Relaxed);
        viewer.last_texture_wait_us.store(
            frame
                .decoded_at
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
            Relaxed,
        );
    }
}

/// Writes straight into the macOS texture's locked BGRA surface. A resolution
/// change between allocation and acquisition returns false and skips that draw.
///
/// # Safety
/// The caller must exclusively own `length` writable bytes at `destination`
/// for the duration of this call. No pointer escapes the call.
#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m1_copy_bgra(
    id: u64,
    width: u32,
    height: u32,
    destination: *mut u8,
    stride: u32,
    length: usize,
) -> bool {
    let Some((viewer, sequence, frame)) = acquire_latest(id) else {
        return false;
    };
    let Some(required) = (stride as usize).checked_mul(height as usize) else {
        return false;
    };
    if destination.is_null()
        || width != frame.width
        || height != frame.height
        || !(width * 4..=65_536).contains(&stride)
        || required > length
    {
        return false;
    }
    // SAFETY: Caller has locked the OS buffer for writing; the required prefix
    // is bounded by its declared size and the validated decoded dimensions.
    let target = unsafe { std::slice::from_raw_parts_mut(destination, required) };
    let written = frame.copy_bgra(target, stride);
    if written {
        mark_presented(&viewer, sequence, &frame);
    }
    written
}

/// Releases a lease acquired by the texture callback.
///
/// # Safety
/// The handle must be null or returned by `nexdesk_m1_acquire_frame` and not yet released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_m1_release_frame(frame: *const c_void) {
    if !frame.is_null() {
        // SAFETY: The caller transfers the matching Arc reference exactly once.
        drop(unsafe { Arc::from_raw(frame.cast::<PixelFrame>()) });
    }
}
