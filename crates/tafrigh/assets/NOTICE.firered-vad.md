# FireRedVAD

The bundled `fireredvad_stream_vad_with_cache.onnx` and `firered_cmvn.ark` are unchanged copies from [FireRedTeam/FireRedVAD](https://github.com/FireRedTeam/FireRedVAD/tree/c30ec49e8cc69642b0ee65362eba11b9d11c6e54/pretrained_models/onnx_models), commit `c30ec49e8cc69642b0ee65362eba11b9d11c6e54`. The upstream normalization file is named `cmvn.ark`. Copyright 2026 Xiaohongshu (authors: Kaituo Xu, Wenpeng Li, Kai Huang, Kun Liu). The model, statistics, and reference feature code are licensed under Apache-2.0; see [LICENSE.firered-vad](LICENSE.firered-vad).

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `fireredvad_stream_vad_with_cache.onnx` | 2,306,042 | `b3c97836130dc34fc32d56fab551e88cf9454511de2b3c250a5e6578dee74b93` |
| `firered_cmvn.ark` | 1,311 | `c87f6f13edf0f0ec7535ddfc9cc3387d9268cb234b70182d566c5e2edf3ca473` |

The Rust frontend implements the settings in upstream `fireredvad/core/audio_feat.py`: 16 kHz signed-PCM scale, 25 ms frames, 10 ms hop, snipped edges, no dither, DC removal, 0.97 pre-emphasis, Povey window, 512-point power FFT, 80 natural-log mel energies from 20 Hz to Nyquist, and fixed CMVN. The FFT uses [RustFFT 6.4.1](https://crates.io/crates/rustfft/6.4.1), licensed MIT OR Apache-2.0. Both detectors use the existing Silero timestamp rules (see [LICENSE.silero-vad](LICENSE.silero-vad)); FireRed uses a 160-sample probability stride. The FireRed ONNX graph runs unchanged on tract with intermediate value-info annotations ignored by the importer.
