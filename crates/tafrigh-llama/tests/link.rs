//! Exercise symbols from both runtimes in one executable without model weights.

#[test]
fn both_runtimes_link_to_one_ggml() {
    assert!(!transcribe_cpp::compiled_version().is_empty());
    assert!(llama_cpp_2::llama_time_us() > 0);
    assert!(llama_cpp_2::max_devices() > 0);
    let missing = tempfile::tempdir().unwrap().path().join("missing.gguf");
    assert!(transcribe_cpp::Model::load(missing).is_err());
    #[cfg(feature = "mtmd")]
    {
        assert!(!llama_cpp_2::mtmd::mtmd_default_marker().is_empty());
        let defaults = llama_cpp_2::mtmd::MtmdContextParams::default();
        assert_eq!(defaults.flash_attn_type, -1);
        assert!(defaults.warmup);
        let params = llama_cpp_2::mtmd::MtmdContextParams {
            flash_attn_type: 0,
            warmup: false,
            ..defaults
        };
        let raw: llama_cpp_2::sys::mtmd_context_params = (&params).into();
        assert_eq!(raw.flash_attn_type, 0);
        assert!(!raw.warmup);
        let round_trip = llama_cpp_2::mtmd::MtmdContextParams::from(raw);
        assert_eq!(round_trip.flash_attn_type, 0);
        assert!(!round_trip.warmup);
    }
}
