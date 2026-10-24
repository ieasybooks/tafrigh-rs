#!/usr/bin/env python3
"""Replay the 2026-10-23 FLEURS accuracy aggregates using only Python's stdlib.

Run: uv run --no-project python score-cohere-fleurs.py --check REPORT.json
The adjacent CSV contains error counts for 428 public ar_eg test clips per run,
not transcripts. Each row is one clip within a recorded inference pass. This recomputes the
reported corpus WER/CER, paired comparisons and sentence-cluster intervals; it
does not rerun inference or rescore text. See the accompanying benchmark report
for dataset attribution, Arabic normalization and the limits of these results.
"""

import argparse
import collections
import csv
import hashlib
import json
from pathlib import Path
import random


FIELDS = (
    "words", "hypothesis_words", "word_errors", "characters", "character_errors",
    "substitutions", "deletions", "insertions",
)
STRICT_FIELDS = (
    "word_errors", "character_errors", "substitutions", "deletions", "insertions",
)
PAIRS = {
    f"{model}-{mode}": (
        f"stack-original-q8_0-defaults-reference-{mode}-r1" if model == "q8_0"
        else f"original-bf16-{mode}",
        f"stack-patched-{model}-{'defaults' if model == 'q8_0' else 'b32t8'}-reference-{mode}-r1",
    )
    for mode in ("silero", "whole") for model in ("q8_0", "bf16")
}


def read_counts(path):
    """Keep sentence IDs as strings: the original bootstrap sorted them lexically."""
    numeric = (*FIELDS, *(f"strict_{key}" for key in STRICT_FIELDS), "same_as_original")
    expected_columns = {"run", "clip_id", "sentence_id", *numeric}
    runs = collections.defaultdict(dict)
    with path.open(newline="", encoding="utf-8") as stream:
        reader = csv.DictReader(stream)
        if set(reader.fieldnames or []) != expected_columns:
            raise ValueError("CSV columns do not match the documented counts format")
        for row in reader:
            for key in numeric:
                row[key] = int(row[key])
                if row[key] < 0:
                    raise ValueError(f"Negative count in {key}")
            if not row["clip_id"].isdecimal() or not row["sentence_id"].isdecimal():
                raise ValueError("Expected public FLEURS numeric clip and sentence IDs")
            run = runs[row.pop("run")]
            if row["clip_id"] in run:
                raise ValueError("Duplicate clip within a run")
            if row["same_as_original"] not in (0, 1):
                raise ValueError("same_as_original must be 0 or 1")
            for prefix in ("", "strict_"):
                errors = sum(row[prefix + key] for key in ("substitutions", "deletions", "insertions"))
                if row[prefix + "word_errors"] != errors:
                    raise ValueError("Word errors do not equal substitutions + deletions + insertions")
                if row["hypothesis_words"] != row["words"] - row[prefix + "deletions"] + row[prefix + "insertions"]:
                    raise ValueError("Hypothesis length disagrees with the edit counts")
            run[row["clip_id"]] = row
    if set(runs) != {name for pair in PAIRS.values() for name in pair}:
        raise ValueError("Expected all eight recorded FLEURS runs")
    first = next(iter(runs.values()))
    for run in runs.values():
        if set(run) != set(first):
            raise ValueError("Runs do not contain identical clip sets")
        for clip, row in run.items():
            for key in ("sentence_id", "words", "characters"):
                if row[key] != first[clip][key]:
                    raise ValueError("Reference metadata differs across paired runs")
    if len(first) != 428 or len({row["sentence_id"] for row in first.values()}) != 283:
        raise ValueError("Expected 428 clips in 283 reference-sentence clusters")
    return runs


def aggregate(rows, strict=False):
    counts = {
        key: sum(row[("strict_" if strict and key in STRICT_FIELDS else "") + key] for row in rows)
        for key in FIELDS
    }
    return {
        **counts,
        "wer": counts["word_errors"] / counts["words"],
        "cer": counts["character_errors"] / counts["characters"],
    }


def bootstrap(original, patched, seed, resamples):
    """Sample paired reference-sentence clusters, retaining every clip in a cluster."""
    by_sentence = collections.defaultdict(lambda: [0, 0, 0])
    for clip, old in original.items():
        group = by_sentence[old["sentence_id"]]
        group[0] += old["words"]
        group[1] += old["word_errors"]
        group[2] += patched[clip]["word_errors"]
    clusters = sorted(by_sentence)
    rng = random.Random(seed)
    old_rates, new_rates = [], []
    for _ in range(resamples):
        sample = rng.choices(clusters, k=len(clusters))
        words = sum(by_sentence[sentence][0] for sentence in sample)
        old_rates.append(sum(by_sentence[sentence][1] for sentence in sample) / words)
        new_rates.append(sum(by_sentence[sentence][2] for sentence in sample) / words)

    def interval(values):
        ordered = sorted(values)
        # Match the published scorer: zero-based floor indices, no interpolation.
        return [ordered[int(0.025 * resamples)], ordered[int(0.975 * resamples)]]

    return {
        "method": f"paired bootstrap clustered by reference sentence, {resamples} resamples",
        "wer_95_ci": {"original": interval(old_rates), "patched": interval(new_rates)},
        "paired_wer_difference_95_ci": {
            "patched minus original": interval([new - old for old, new in zip(old_rates, new_rates)])
        },
    }


def summarize(runs, seed, resamples):
    accuracy = {
        name: {
            "normalized": aggregate(list(rows.values())),
            "without_alef_yaa_folding": aggregate(list(rows.values()), strict=True),
        }
        for name, rows in runs.items()
    }
    comparisons = {}
    for label, (old_name, new_name) in PAIRS.items():
        old, new = runs[old_name], runs[new_name]
        comparisons[label] = {
            "identical_normalized_transcripts": sum(row["same_as_original"] for row in new.values()),
            "new_fewer_word_errors": sum(new[clip]["word_errors"] < row["word_errors"] for clip, row in old.items()),
            "new_more_word_errors": sum(new[clip]["word_errors"] > row["word_errors"] for clip, row in old.items()),
            "confidence_intervals": bootstrap(old, new, seed, resamples),
        }
    return {"accuracy": accuracy, "reference_comparisons": comparisons}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("counts", nargs="?", type=Path, default=Path(__file__).with_name("cohere-cuda-2026-10-23-fleurs.csv"))
    parser.add_argument("--seed", type=int, default=20261023)
    parser.add_argument("--resamples", type=int, default=2000)
    parser.add_argument("--check", type=Path, help="Fail unless both accuracy sections equal an existing public report")
    parser.add_argument("--output", type=Path, help="Write aggregate JSON here; otherwise print it")
    args = parser.parse_args()
    if args.resamples < 1:
        parser.error("--resamples must be positive")
    try:
        results = summarize(read_counts(args.counts), args.seed, args.resamples)
        if args.check:
            expected = json.loads(args.check.read_text(encoding="utf-8"))
            for section, actual in results.items():
                if expected[section] != actual:
                    raise ValueError(f"{section} differs from the comparison report")
    except (OSError, ValueError, KeyError) as error:
        parser.error(str(error))
    results = {
        "counts_sha256": hashlib.sha256(args.counts.read_bytes()).hexdigest(),
        "bootstrap": {
            "seed": args.seed,
            "resamples": args.resamples,
            "cluster": "public FLEURS reference sentence ID (sorted as text)",
            "interval": "sorted samples at zero-based floor(0.025*n) and floor(0.975*n)",
            "limits": "One inference pass per setting; each CSV row is one clip. Intervals cover sentence sampling, not GPU run-to-run variation.",
        },
        **results,
    }
    output = json.dumps(results, indent=2) + "\n"
    if args.output:
        args.output.write_text(output, encoding="utf-8")
    else:
        print(output, end="")


if __name__ == "__main__":
    main()
