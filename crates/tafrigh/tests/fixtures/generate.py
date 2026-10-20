# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "silero-vad==6.2.1",
#     "onnxruntime==1.31.0",
#     "numpy==2.5.4",
# ]
# ///
"""Regenerates the Silero golden fixtures from the Python reference implementation.

The reference is the official silero-vad 6.2.1 package for the timestamp rules
(`get_speech_timestamps`, fed the network's probabilities) and onnxruntime running the bundled
sequence ONNX export from faster-whisper for the network.

`cargo test` only reads the committed fixtures. Rerun this only when the reference changes: a new
Silero version or model, or new cases.

From the repository root, with uv (provided by mise.toml) and ffmpeg on PATH:

    uv run crates/tafrigh/tests/fixtures/generate.py \
        --short jfk.flac --long lecture.wav [--long more.mp3 ...]

uv installs the dependencies listed above, at the versions pinned in `generate.py.lock`, into a
cached environment. After editing the dependencies, refresh the lock with
`uv lock --script crates/tafrigh/tests/fixtures/generate.py`. Fixtures are written next to this
script (`--out` to change), using the bundled model (`--model` to change).
"""

import argparse
import base64
import json
import random
import subprocess
from pathlib import Path

import numpy as np
import onnxruntime
import torch
from silero_vad import get_speech_timestamps

SR = 16_000
WINDOW = 512
CONTEXT = 64
BATCH = 10_000  # frames per network call; the LSTM state carries across calls
MODEL = Path(__file__).resolve().parents[2] / "assets" / "silero_vad_v6.onnx"


def decode_s16(path: Path) -> np.ndarray:
    raw = subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-i", str(path), "-vn", "-ac", "1",
         "-ar", str(SR), "-f", "s16le", "-"],
        check=True, capture_output=True,
    ).stdout
    return np.frombuffer(raw, dtype="<i2")


def speech_probabilities(session: onnxruntime.InferenceSession, audio: np.ndarray) -> np.ndarray:
    """One probability per 512-sample frame; the last partial frame is zero-padded.

    Each row of the sequence export is the previous frame's last 64 samples followed by the frame.
    """
    frames = np.zeros(-(-len(audio) // WINDOW) * WINDOW, dtype=np.float32)
    frames[:len(audio)] = audio
    frames = frames.reshape(-1, WINDOW)
    context = np.zeros((len(frames), CONTEXT), dtype=np.float32)
    context[1:] = frames[:-1, -CONTEXT:]
    rows = np.concatenate([context, frames], axis=1)
    h = np.zeros((1, 1, 128), dtype=np.float32)
    c = np.zeros((1, 1, 128), dtype=np.float32)
    probs = []
    for start in range(0, len(rows), BATCH):
        out, h, c = session.run(None, {"input": rows[start:start + BATCH], "h": h, "c": c})
        probs.append(out)
    return np.concatenate(probs).astype(np.float32)


class ReplayModel:
    """Stands in for the network in `get_speech_timestamps`, replaying known probabilities.

    silero-vad calls `reset_states()` once, then the model once per 512-sample window.
    """

    def __init__(self, probs: np.ndarray):
        self.probs = [float(p) for p in probs]
        self.calls = 0

    def reset_states(self):
        self.calls = 0

    def __call__(self, chunk, sampling_rate):
        prob = self.probs[self.calls]
        self.calls += 1
        return torch.tensor(prob, dtype=torch.float64)


def speech_timestamps(probs: np.ndarray, audio_len: int, params: dict) -> list:
    model = ReplayModel(probs)
    spans = get_speech_timestamps(torch.zeros(audio_len), model, sampling_rate=SR, **params)
    assert model.calls == len(probs), f"{model.calls} windows for {len(probs)} probabilities"
    return [[int(s["start"]), int(s["end"])] for s in spans]


PARAM_SETS = {
    "silero-defaults": dict(threshold=0.5, min_speech_duration_ms=250, max_speech_duration_s=float("inf"),
                            min_silence_duration_ms=100, speech_pad_ms=30),
    "tafrigh-defaults": dict(threshold=0.5, min_speech_duration_ms=500, max_speech_duration_s=15.0,
                             min_silence_duration_ms=300, speech_pad_ms=60),
    "short-max": dict(threshold=0.5, min_speech_duration_ms=250, max_speech_duration_s=5.0,
                      min_silence_duration_ms=300, speech_pad_ms=60),
    "long-max": dict(threshold=0.5, min_speech_duration_ms=500, max_speech_duration_s=30.0,
                     min_silence_duration_ms=300, speech_pad_ms=60),
    "latest-pause": dict(threshold=0.5, min_speech_duration_ms=250, max_speech_duration_s=8.0,
                         min_silence_duration_ms=200, speech_pad_ms=30,
                         use_max_poss_sil_at_max_speech=False),
    "low-threshold": dict(threshold=0.375, min_speech_duration_ms=250, max_speech_duration_s=10.0,
                          min_silence_duration_ms=100, speech_pad_ms=100),
}


def to_rust_params(params: dict) -> dict:
    max_s = params["max_speech_duration_s"]
    return {
        "threshold": params["threshold"],
        "min_speech_ms": params["min_speech_duration_ms"],
        "max_speech_s": None if max_s == float("inf") else max_s,
        "min_silence_ms": params["min_silence_duration_ms"],
        "speech_pad_ms": params["speech_pad_ms"],
        "use_max_possible_silence_at_max_speech": params.get("use_max_poss_sil_at_max_speech", True),
    }


def case(name, probs_u8, audio_len, params):
    probs = (probs_u8.astype(np.float64) / 255.0).astype(np.float32)
    return {
        "name": name,
        "audio_len": int(audio_len),
        "probs_u8": base64.b64encode(probs_u8.tobytes()).decode(),
        "params": to_rust_params(params),
        "spans": speech_timestamps(probs, audio_len, params),
    }


def random_probs(rng: random.Random, frames: int) -> np.ndarray:
    value, out = rng.random(), []
    for _ in range(frames):
        if rng.random() < 0.06:
            value = rng.random()
        out.append(min(1.0, max(0.0, value + (rng.random() - 0.5) * 0.3)))
    return np.round(np.array(out) * 255).astype(np.uint8)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, default=MODEL)
    parser.add_argument("--short", type=Path, required=True)
    parser.add_argument("--long", type=Path, action="append", required=True)
    parser.add_argument("--out", type=Path, default=Path(__file__).parent)
    args = parser.parse_args()

    session = onnxruntime.InferenceSession(str(args.model), providers=["CPUExecutionProvider"])

    # 1. Network output on real speech, from the same 16-bit samples the Rust code sees.
    short = decode_s16(args.short)
    short.tofile(args.out / "short.s16le")
    short_probs = speech_probabilities(session, short.astype(np.float32) / 32768.0)
    short_probs.astype("<f4").tofile(args.out / "short.probs.f32")
    tafrigh = PARAM_SETS["tafrigh-defaults"]
    short_spans = speech_timestamps(short_probs, len(short), tafrigh)

    # 2. Timestamp rules on long real-speech probabilities and on random probabilities.
    long_audio = np.concatenate([decode_s16(path) for path in args.long])
    long_probs = speech_probabilities(session, long_audio.astype(np.float32) / 32768.0)
    long_u8 = np.round(long_probs * 255).astype(np.uint8)
    cases = [case(f"long/{name}", long_u8, len(long_audio), params)
             for name, params in PARAM_SETS.items()]
    rng = random.Random(1234)
    names = list(PARAM_SETS)
    for i in range(60):
        frames = rng.randrange(500, 3000)
        audio_len = frames * 512 - rng.randrange(0, 512)
        params = PARAM_SETS[names[i % len(names)]]
        cases.append(case(f"random/{i}", random_probs(rng, frames), audio_len, params))

    (args.out / "silero_golden.json").write_text(json.dumps({
        "short": {"params": to_rust_params(tafrigh), "spans": short_spans},
        "cases": cases,
    }))
    print(f"short: {len(short) / SR:.1f}s, {len(short_spans)} spans; "
          f"long: {len(long_audio) / SR:.1f}s; {len(cases)} timestamp cases")


if __name__ == "__main__":
    main()
