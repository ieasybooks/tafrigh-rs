# Audar dependencies and model licenses

The Rust recognizer is MIT. `tafrigh-llama` bundles llama.cpp and mtmd on the ggml runtime supplied by `tafrigh-transcribe`; see the [native runtime notices](../tafrigh-llama/THIRD-PARTY-LICENSES.md) for the MIT native code and MIT OR Apache-2.0 Rust bindings. Shared download dependencies retain their existing licenses: reqwest and dirs are MIT OR Apache-2.0, sha2 is MIT OR Apache-2.0. No new registry dependency is introduced by this recognizer.

Model weights are downloaded separately, never bundled. Decoder filenames, byte lengths and SHA-256 hashes are pinned from the official Hugging Face Git LFS metadata in `src/model.rs`. Every decoder requires its own tier's BF16 mmproj audio encoder; selecting a quantized decoder does not quantize the encoder.

| Tier | Official repository and revision | Model license |
| --- | --- | --- |
| Turbo | [audarai/Audar-ASR-V1-Turbo](https://huggingface.co/audarai/Audar-ASR-V1-Turbo/tree/371428bea487c7aec82b27dc21f8d4324002e98e), `371428bea487c7aec82b27dc21f8d4324002e98e` | [AudarAI Community License v1.0](https://www.audarai.com/license/audarai-community-license-v1.0/) |
| Flash | [audarai/Audar-ASR-V1-Flash](https://huggingface.co/audarai/Audar-ASR-V1-Flash/tree/54274d39245beba38f30048128e231d4176bd91d), `54274d39245beba38f30048128e231d4176bd91d` | [AudarAI Open License v1.0](https://www.audarai.com/license/audarai-open-license-v1.0/) |

Turbo's model card describes research and limited commercial use for qualifying Community Entities; enterprise, large-scale and model-as-a-service use require an AudarAI Enterprise License. Flash's model card permits commercial use, redistribution, fine-tuning and quantization while retaining the license and notices. Consult the linked model licenses for the full conditions. These model terms are documented here and in the user guide; tafrigh does not add a license prompt, account requirement or runtime restriction.

Output repetition repair follows the `detect_and_fix_repetitions` algorithm from qwen-asr 0.0.6 (`inference/utils.py`, copyright 2026 The Alibaba Qwen team) in [Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR), which is Apache-2.0 ([license](licenses/LICENSE-qwen-asr)). The Rust port changes the implementation language and adds Audar output cleanup. The implementation retains the evaluated character-run and repeated-pattern behavior. The speech fixture is Apache-2.0 and has its provenance and license under `tests/fixtures/`.
