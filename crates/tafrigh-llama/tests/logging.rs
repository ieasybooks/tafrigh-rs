//! llama's safe and native logging setup must preserve transcribe's ggml sink.

mod support;

use llama_cpp_2::{send_logs_to_tracing, sys, LogOptions};
use support::ggml_logger;

unsafe extern "C" fn discard(
    _level: sys::ggml_log_level,
    _text: *const std::ffi::c_char,
    _userdata: *mut std::ffi::c_void,
) {
}

#[test]
fn llama_logging_preserves_the_shared_ggml_logger() {
    transcribe_cpp::init_logging();
    let expected = ggml_logger();
    assert_ne!(expected.0, 0);
    send_logs_to_tracing(LogOptions::default());
    assert_eq!(ggml_logger(), expected);
    let mut userdata = 0_u8;
    // SAFETY: discard never dereferences the pointer, which is valid for this call.
    unsafe { sys::llama_log_set(Some(discard), std::ptr::from_mut(&mut userdata).cast()) };
    assert_eq!(ggml_logger(), expected);
}
