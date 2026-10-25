//! A separate executable gives llama the first logging call in a fresh process.

mod support;

#[test]
fn llama_initializes_transcribes_ggml_logger() {
    llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
    let expected = support::ggml_logger();
    assert_ne!(expected.0, 0);
    transcribe_cpp::init_logging();
    assert_eq!(support::ggml_logger(), expected);
}
