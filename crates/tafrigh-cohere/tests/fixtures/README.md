# Arabic speech fixture

`sample1.wav` is Cohere's unmodified 113,964-byte example from `CohereLabs/cohere-transcribe-arabic-07-2026`, distributed under Apache-2.0. It was retrieved from the ungated [Newmetrics mirror at revision 35dbaf071fb0eec23ae1fdcbf671d469bc607e5c](https://huggingface.co/Newmetrics/cohere-transcribe-arabic-07-2026/blob/35dbaf071fb0eec23ae1fdcbf671d469bc607e5c/examples/sample1.wav). Copyright Cohere. The license text is in `LICENSE-APACHE-2.0`.

SHA-256: `14ce25757caa30e7e39a8ec75cb05fdc7838e92ad0c6619e9c0697515779adaa`.

Ordinary tests do not load a model. Set `TAFRIGH_TEST_COHERE_MODEL=/path/to/model.gguf` to enable the integration test. It checks Arabic output, concurrent result delivery, empty word timings and the recognizer's 31-second input limit without pinning exact text across quantizations or devices.
