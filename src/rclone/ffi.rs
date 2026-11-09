//! Hand-written bindings for the four functions librclone exports.
//!
//! `build.rs` checks `librclone.h` against the C declarations it keeps, and this file
//! against the Rust ones it keeps beside them, so a signature change in a new rclone
//! version fails the build instead of corrupting memory at run time.
//!
//! Go runs its packages' initialisation on a thread of its own from the moment the process
//! starts, at the same time as `main`; a call into a function it exports, from a thread not
//! already running Go code, waits for that to end (`cgocallbackg1` in Go's
//! runtime/cgocall.go, go1.27.3: it waits on `mainInitDoneChan` while `ncgo` is 0).

use std::ffi::{CStr, CString, NulError, c_char, c_int};
use std::sync::{OnceLock, PoisonError, RwLock, TryLockError};

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

/// Whether rclone was shut down. Each call holds it for reading, and shutting down for
/// writing: none runs while rclone shuts down, and none after.
static FINALIZED: RwLock<bool> = RwLock::new(false);

/// Why an rc call was not made.
#[derive(Debug)]
pub enum CallError {
    /// The method or the input has a NUL byte, which a C string cannot hold.
    Nul(NulError),
    /// rclone was shut down.
    ShutDown,
}

impl From<NulError> for CallError {
    fn from(e: NulError) -> Self {
        Self::Nul(e)
    }
}

/// Initializes rclone once per process. Later calls do nothing.
#[allow(unsafe_code)]
pub fn initialize() {
    // SAFETY: RcloneInitialize takes no arguments; OnceLock guarantees a single call.
    INIT.get_or_init(|| unsafe { RcloneInitialize() });
}

/// Waits until Go has initialised its packages (see the module's documentation).
#[allow(unsafe_code)]
pub fn wait_for_go() {
    // SAFETY: RcloneFreeString is `C.free` (librse/main.go), and freeing a null pointer does
    // nothing (C11 7.22.3.3); as an exported function it first waits for Go's initialisation.
    unsafe { RcloneFreeString(std::ptr::null_mut()) }
}

/// Shuts rclone down; later calls are refused. While a call is still running it does
/// nothing, and later calls go on (`Embedded` refuses them itself): the process is about to
/// end anyway.
#[allow(unsafe_code)]
pub fn finalize() {
    let mut finalized = match FINALIZED.try_write() {
        Ok(finalized) => finalized,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    if !*finalized && INIT.get().is_some() {
        // SAFETY: rclone was initialized, and with the lock held for writing no RcloneRPC is
        // running; `finalized` makes every later `rpc` refuse. In rclone v1.75.2
        // RcloneFinalize only runs Go's garbage collector (librclone/librclone.go), so jobs
        // started with `_async` that are still running in Go are no matter either; a later
        // version that frees state would need them stopped first.
        unsafe { RcloneFinalize() }
    }
    *finalized = true;
}

/// Calls one rc method with a JSON input string and returns the HTTP-style status code
/// and the JSON output.
///
/// rclone's first access to its config file sets an environment variable through the C
/// library's `setenv` (Go's `os.Setenv` with cgo), without the lock Rust takes for the
/// environment: that first call must come before rse starts reading the environment from
/// other threads, as `setup_rclone` in `main.rs` makes it.
///
/// # Errors
/// [`CallError::Nul`] when `method` or `input` contains a NUL byte, and
/// [`CallError::ShutDown`] after [`finalize`].
#[allow(unsafe_code)]
pub fn rpc(method: &str, input: &str) -> Result<(i32, String), CallError> {
    initialize();
    let method = CString::new(method)?;
    let input = CString::new(input)?;
    let finalized = FINALIZED.read().unwrap_or_else(PoisonError::into_inner);
    if *finalized {
        return Err(CallError::ShutDown);
    }
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the call, and Go
    // copies them (C.GoString, librse/main.go) and keeps neither. The lock held for reading
    // keeps `finalize` from running meanwhile.
    let ret = unsafe { RcloneRPC(method.as_ptr().cast_mut(), input.as_ptr().cast_mut()) };
    drop(finalized);
    let output = if ret.output.is_null() {
        String::new()
    } else {
        // SAFETY: Go gives the caller a NUL-terminated copy made with C.CString (malloc),
        // valid until it is freed below.
        let s = unsafe { CStr::from_ptr(ret.output) }.to_string_lossy().into_owned();
        // SAFETY: the pointer came from RcloneRPC and is freed exactly once, with the
        // `free` it was allocated for (C.free in RcloneFreeString).
        unsafe { RcloneFreeString(ret.output) };
        s
    };
    Ok((ret.status, output))
}
