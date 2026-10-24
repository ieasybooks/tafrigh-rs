# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "numpy==2.5.4",
#     "onnxruntime==1.31.0",
#     "kaldi-native-fbank==1.22.4",
#     "kaldiio==2.18.1",
#     "soundfile==0.14.0",
# ]
# ///
"""Generate FireRed golden fixtures from official Python features and ONNX Runtime.

Run `uv run crates/tafrigh/tests/fixtures/firered/generate.py` at the repository root.
The committed PCM is the input; Rust tests never download or invoke Python. Optional
--firered-source and --silero-source paths allow offline generation from the pinned files.
Only the numpy/Kaldi classes are loaded from FireRed; its torch tensor wrapper is unused.
Silero's official probability postprocessor is used with its frame stride changed to 160.
"""

import argparse
import ast
import hashlib
import json
from pathlib import Path
import urllib.request

import numpy as np
import onnxruntime as ort

HERE = Path(__file__).resolve().parent
ASSETS = HERE.parents[2] / "assets"
FIRERED_REV = "c30ec49e8cc69642b0ee65362eba11b9d11c6e54"
SILERO_REV = "5cd7945676eb32225748052e2e6a0580e4686a08"
FIRERED_SHA = "4c156976a7f3ffe26ec669022516028fd59978c685922a5a26ccf4fe72307960"
SILERO_SHA = "15d0f6b3ef9590e22ca2f146e1436f6901da356cb552d3183df4100862b333cb"


def source(path, url, sha):
    raw = path.read_bytes() if path else urllib.request.urlopen(url).read()
    assert hashlib.sha256(raw).hexdigest() == sha, f"source hash mismatch: {url}"
    return ast.parse(raw)


def references(args):
    tree = source(args.firered_source,
                  f"https://raw.githubusercontent.com/FireRedTeam/FireRedVAD/{FIRERED_REV}/fireredvad/core/audio_feat.py",
                  FIRERED_SHA)
    nodes = [n for n in tree.body if not
             (isinstance(n, ast.Import) and any(x.name == "torch" for x in n.names)) and not
             (isinstance(n, ast.ClassDef) and n.name == "AudioFeat")]
    scope = {}
    exec(compile(ast.Module(body=nodes, type_ignores=[]), "official_firered_features", "exec"), scope)
    fbank = scope["KaldifeatFbank"](80, 25, 10, 0)
    cmvn = scope["CMVN"](str(ASSETS / "firered_cmvn.ark"))
    tree = source(args.silero_source,
                  f"https://raw.githubusercontent.com/snakers4/silero-vad/{SILERO_REV}/src/silero_vad/utils_vad.py",
                  SILERO_SHA)
    fn = next(n for n in tree.body if isinstance(n, ast.FunctionDef)
              and n.name == "get_speech_timestamps_from_probs")
    assignment = next(n for n in fn.body if isinstance(n, ast.Assign)
                      and any(isinstance(t, ast.Name) and t.id == "window_size_samples" for t in n.targets))
    assignment.value = ast.Constant(value=160)
    module = ast.fix_missing_locations(ast.Module(body=[fn], type_ignores=[]))
    scope = {"List": list}
    exec(compile(module, "official_silero_timestamps_160_sample_hop", "exec"), scope)
    return fbank, cmvn, scope[fn.name]


def spans(probs, length, maximum, merge, reference):
    raw = reference(probs.tolist(), threshold=0.4, min_speech_duration_ms=500,
                    max_speech_duration_s=maximum, min_silence_duration_ms=300,
                    speech_pad_ms=250, audio_length_samples=length)
    result = []
    for span in raw:
        start, end = span["start"], span["end"]
        if merge and result and end - result[-1][0] <= maximum * 16000:
            result[-1][1] = end
        else:
            result.append([start, end])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firered-source", type=Path)
    parser.add_argument("--silero-source", type=Path)
    args = parser.parse_args()
    for filename, digest in {
        "fireredvad_stream_vad_with_cache.onnx": "b3c97836130dc34fc32d56fab551e88cf9454511de2b3c250a5e6578dee74b93",
        "firered_cmvn.ark": "c87f6f13edf0f0ec7535ddfc9cc3387d9268cb234b70182d566c5e2edf3ca473",
    }.items():
        assert hashlib.sha256((ASSETS / filename).read_bytes()).hexdigest() == digest, filename
    fbank, cmvn, timestamps = references(args)
    options = ort.SessionOptions()
    options.intra_op_num_threads = options.inter_op_num_threads = 1
    session = ort.InferenceSession(str(ASSETS / "fireredvad_stream_vad_with_cache.onnx"),
                                   sess_options=options, providers=["CPUExecutionProvider"])
    cases = []
    for name, pcm, repeat in [("speech", "../short.s16le", 1),
                              ("recitation", "recitation.s16le", 1),
                              ("recitation-long", "recitation.s16le", 5)]:
        audio = np.tile(np.fromfile(HERE / pcm, dtype="<i2"), repeat)
        features = cmvn(fbank((16000, audio))).astype("<f4")
        cache = np.zeros((8, 1, 128, 19), dtype=np.float32)
        probabilities = []
        # Different from Rust's batch size: checks both cache carry and EOF batching.
        for start in range(0, len(features), 20):
            p, cache = session.run(None, {"feat": features[None, start:start + 20, :], "caches_in": cache})
            probabilities.extend(p.reshape(-1))
        probabilities = np.asarray(probabilities, dtype="<f4")
        probabilities.tofile(HERE / f"{name}.probs.f32")
        if repeat == 1:
            features.tofile(HERE / f"{name}.features.f32")
        cases.append(dict(name=name, pcm=pcm, repeat=repeat, samples=len(audio), frames=len(features),
                          pcm_sha256=hashlib.sha256((HERE / pcm).read_bytes()).hexdigest(),
                          spans_15=spans(probabilities, len(audio), 15, False, timestamps),
                          spans_30=spans(probabilities, len(audio), 30, True, timestamps)))
        print(name, len(audio), "samples", len(features), "frames")
    # An exactly constant window becomes zero after DC removal; exercise the log-energy floor.
    constant = np.full(400, 1234, dtype=np.int16)
    cmvn(fbank((16000, constant))).astype("<f4").tofile(HERE / "constant.features.f32")
    (HERE / "golden.json").write_text(json.dumps(dict(
        firered_revision=FIRERED_REV, silero_revision=SILERO_REV,
        feature_options=str(fbank.opts), cases=cases), indent=2) + "\n")


if __name__ == "__main__":
    main()
