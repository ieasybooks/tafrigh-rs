use llama_cpp_2::sys;

pub fn ggml_logger() -> (usize, *mut std::ffi::c_void) {
    let mut callback = None;
    let mut userdata = std::ptr::null_mut();
    // SAFETY: both outputs are valid; the test installs loggers on this thread only.
    unsafe { sys::ggml_log_get(&mut callback, &mut userdata) };
    (callback.map_or(0, |value| value as usize), userdata)
}
