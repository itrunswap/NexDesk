//! C ABI for validation, preview lifecycle and native texture frames.

use std::{slice, str};

use nexdesk_core::{DEFAULT_LAN_PORT, DeviceId, LanEndpoint, TargetError};

mod collaboration;
mod desktop;
mod file_transfer;
mod preview;
mod rpc;

const ABI_VERSION: u32 = 1;
const MAX_TEXT_BYTES: usize = 128;
const VALID: i32 = 0;
const INVALID_TEXT: i32 = 4;

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_abi_version() -> u32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn nexdesk_default_lan_port() -> u16 {
    DEFAULT_LAN_PORT
}

/// Checks a device ID and returns a status from the C header.
///
/// # Safety
/// For a non-null pointer and a length in 1..=128, the caller must supply that
/// many readable bytes in one allocation for the entire call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_validate_device_id(id: *const u8, id_len: usize) -> i32 {
    // SAFETY: The caller supplies the buffer for this call; it is never retained.
    let Ok(id) = (unsafe { input_text(id, id_len) }) else {
        return INVALID_TEXT;
    };
    validation_status(id.parse::<DeviceId>())
}

/// Checks an IP and a separate port, both represented as UTF-8.
///
/// # Safety
/// Each non-null pointer with a length in 1..=128 must reference a readable
/// buffer of that length in one allocation for the entire call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nexdesk_validate_lan_endpoint(
    address: *const u8,
    address_len: usize,
    port: *const u8,
    port_len: usize,
) -> i32 {
    // SAFETY: The caller owns both buffers for the duration of this call.
    let (Ok(address), Ok(port)) = (unsafe { input_text(address, address_len) }, unsafe {
        input_text(port, port_len)
    }) else {
        return INVALID_TEXT;
    };
    validation_status(LanEndpoint::parse(address, port))
}

fn validation_status<T>(result: Result<T, TargetError>) -> i32 {
    match result {
        Ok(_) => VALID,
        Err(TargetError::DeviceId) => 1,
        Err(TargetError::LanAddress) => 2,
        Err(TargetError::LanPort) => 3,
    }
}

/// The borrowed view must not outlive the native input buffer.
unsafe fn input_text<'a>(data: *const u8, len: usize) -> Result<&'a str, ()> {
    if len == 0 {
        return Ok("");
    }
    if len > MAX_TEXT_BYTES || data.is_null() {
        return Err(());
    }
    // SAFETY: Non-nullness and length are checked above; the ABI caller keeps
    // the provided allocation valid during parsing.
    str::from_utf8(unsafe { slice::from_raw_parts(data, len) }).map_err(|_| ())
}
