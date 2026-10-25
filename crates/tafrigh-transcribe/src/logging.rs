//! Routing the native log sink into the [`log`] crate facade.
//!
//! The first call to [`init_logging`] or [`disable_logging`] configures the
//! process-wide native and ggml sink; later calls do nothing. Callers may call
//! either function before every model load. The shared initialization completes
//! before any caller proceeds, so model loading never replaces a running sink.
//! With logging enabled, diagnostics go through `log` and the application's
//! `env_logger`/`tracing-log`/etc. installation.

use std::ffi::CStr;
use std::sync::Once;

use crate::sys;

static LOGGING: Once = Once::new();

/// The C log callback: map the level and forward the message to `log`.
extern "C" fn log_trampoline(
    level: sys::transcribe_log_level,
    msg: *const std::os::raw::c_char,
    _userdata: *mut std::os::raw::c_void,
) {
    if msg.is_null() {
        return;
    }
    // SAFETY: `msg` is a NUL-terminated string valid for this call.
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let text = text.trim_end_matches('\n');
    use sys::transcribe_log_level as L;
    let level = match level {
        L::TRANSCRIBE_LOG_LEVEL_ERROR => log::Level::Error,
        L::TRANSCRIBE_LOG_LEVEL_WARN => log::Level::Warn,
        L::TRANSCRIBE_LOG_LEVEL_DEBUG => log::Level::Debug,
        // INFO and CONT (continuation fragments) both surface at info.
        L::TRANSCRIBE_LOG_LEVEL_INFO | L::TRANSCRIBE_LOG_LEVEL_CONT => log::Level::Info,
        // NONE or any unknown level: drop.
        _ => return,
    };
    log::log!(target: "transcribe_cpp", level, "{text}");
}

/// Route native and ggml diagnostics into the [`log`] crate.
///
/// The first call to this function or [`disable_logging`] wins; later calls do
/// nothing. Callers may call this before every model load.
pub fn init_logging() {
    LOGGING.call_once(|| {
        // SAFETY: the shared Once installs the sink before model loading starts.
        unsafe { sys::transcribe_log_set(Some(log_trampoline), std::ptr::null_mut()) };
    });
}

/// Silence the native library entirely (drops both library and ggml messages,
/// including the stderr default).
///
/// The first call to this function or [`init_logging`] wins; later calls do
/// nothing. Callers may call this before every model load.
pub fn disable_logging() {
    LOGGING.call_once(|| {
        // SAFETY: the shared Once installs the sink before model loading starts.
        unsafe { sys::transcribe_log_set(None, std::ptr::null_mut()) };
    });
}
