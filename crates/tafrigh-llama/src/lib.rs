#![doc = include_str!("../README.md")]
// Preserve upstream API spellings/attributes and casts for platform-dependent FFI enums.
#![allow(
    clippy::upper_case_acronyms,
    clippy::double_must_use,
    clippy::unnecessary_cast
)]

// Keep the owner of ggml and its transitive native link directives in the link graph.
extern crate transcribe_cpp as _;

/// Raw bindings to the bundled llama.cpp and mtmd, using transcribe.cpp's ggml headers.
#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    unpredictable_function_pointer_comparisons,
    clippy::all
)]
pub mod sys {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

include!(env!("TAFRIGH_LLAMA_BINDINGS"));
