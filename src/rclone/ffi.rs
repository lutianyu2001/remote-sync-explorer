//! Hand-written bindings for the four functions librclone exports.
//!
//! `build.rs` checks `librclone.h` against these declarations, so a signature change in a
//! new rclone version fails the build instead of corrupting memory at run time.

use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::OnceLock;

#[repr(C)]
struct RpcReturn {
    output: *mut c_char,
    status: c_int,
}

#[allow(unsafe_code)]
unsafe extern "C" {
    fn RcloneInitialize();
    fn RcloneFinalize();
    fn RcloneRPC(method: *mut c_char, input: *mut c_char) -> RpcReturn;
    fn RcloneFreeString(s: *mut c_char);
}

static INIT: OnceLock<()> = OnceLock::new();

/// Initializes rclone once per process. Later calls do nothing.
#[allow(unsafe_code)]
pub fn initialize() {
    // SAFETY: RcloneInitialize takes no arguments; OnceLock guarantees a single call.
    INIT.get_or_init(|| unsafe { RcloneInitialize() });
}

/// Shuts rclone down. No RPC may be made afterwards.
#[allow(unsafe_code)]
pub fn finalize() {
    if INIT.get().is_some() {
        // SAFETY: rclone was initialized; the caller guarantees no further RPCs.
        unsafe { RcloneFinalize() }
    }
}

/// Calls one rc method with a JSON input string and returns the HTTP-style status code
/// and the JSON output.
///
/// # Errors
/// Fails only when `method` or `input` contains a NUL byte.
#[allow(unsafe_code)]
pub fn rpc(method: &str, input: &str) -> Result<(i32, String), std::ffi::NulError> {
    initialize();
    let method = CString::new(method)?;
    let input = CString::new(input)?;
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the call; rclone
    // only reads them. The returned output is owned by rclone and freed below.
    let ret = unsafe { RcloneRPC(method.as_ptr().cast_mut(), input.as_ptr().cast_mut()) };
    let output = if ret.output.is_null() {
        String::new()
    } else {
        // SAFETY: rclone returns a NUL-terminated C string allocated with malloc.
        let s = unsafe { CStr::from_ptr(ret.output) }.to_string_lossy().into_owned();
        // SAFETY: the pointer came from RcloneRPC and is freed exactly once.
        unsafe { RcloneFreeString(ret.output) };
        s
    };
    Ok((ret.status, output))
}
