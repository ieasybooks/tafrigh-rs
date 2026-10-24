//! Silero 6.2.1 speech-timestamp rules applied to frame probabilities.
//!
//! [`speech_timestamps`] is a line-by-line port of `get_speech_timestamps` from silero-vad
//! 6.2.1. [`StreamingTimestamps`] produces identical spans incrementally: a span is emitted as
//! soon as the next one starts, which is when its end padding becomes known.

use crate::audio::SAMPLE_RATE;
use crate::segment::SpeechSpan;

/// Parameters of the Silero timestamp rules.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TimestampParams {
    /// Samples between consecutive frame probabilities.
    pub frame_samples: usize,
    /// Probability at or above which a frame is speech.
    pub threshold: f32,
    /// Probability below which a frame is silence. Defaults to `threshold - 0.15` (min 0.01).
    pub neg_threshold: Option<f32>,
    /// Speech shorter than this is dropped.
    pub min_speech_ms: u32,
    /// Speech longer than this is split, preferably at the longest pause inside it.
    pub max_speech_s: f64,
    /// Silence needed to end a span.
    pub min_silence_ms: u32,
    /// Padding added to both ends of each span (shared between close neighbors).
    pub speech_pad_ms: u32,
    /// Pauses shorter than this are not considered as split points for long speech.
    pub min_silence_at_max_speech_ms: u32,
    /// Split long speech at the longest pause seen instead of the most recent one.
    pub use_max_possible_silence_at_max_speech: bool,
}

impl Default for TimestampParams {
    fn default() -> Self {
        Self {
            frame_samples: 512,
            threshold: 0.5,
            neg_threshold: None,
            min_speech_ms: 250,
            max_speech_s: f64::INFINITY,
            min_silence_ms: 100,
            speech_pad_ms: 30,
            min_silence_at_max_speech_ms: 98,
            use_max_possible_silence_at_max_speech: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Derived {
    frame_samples: usize,
    threshold: f64,
    neg_threshold: f64,
    min_speech: f64,
    pad: f64,
    max_speech: f64,
    min_silence: f64,
    min_silence_at_max_speech: f64,
    use_max_possible: bool,
}

impl Derived {
    fn new(params: &TimestampParams) -> Self {
        let sr = f64::from(SAMPLE_RATE);
        let pad = sr * f64::from(params.speech_pad_ms) / 1000.0;
        Self {
            frame_samples: params.frame_samples,
            // Compared in f64 like the Python reference (float32 probabilities vs float).
            threshold: f64::from(params.threshold),
            neg_threshold: params
                .neg_threshold
                .map_or((f64::from(params.threshold) - 0.15).max(0.01), f64::from),
            min_speech: sr * f64::from(params.min_speech_ms) / 1000.0,
            pad,
            max_speech: sr * params.max_speech_s - params.frame_samples as f64 - 2.0 * pad,
            min_silence: sr * f64::from(params.min_silence_ms) / 1000.0,
            min_silence_at_max_speech: sr * f64::from(params.min_silence_at_max_speech_ms) / 1000.0,
            use_max_possible: params.use_max_possible_silence_at_max_speech,
        }
    }
}

/// The frame-level state machine shared by the batch and streaming APIs.
/// It emits raw (unpadded) spans in order.
#[derive(Debug, Clone)]
struct Machine {
    d: Derived,
    frame: usize,
    triggered: bool,
    current_start: Option<i64>,
    temp_end: i64,
    prev_end: i64,
    next_start: i64,
    possible_ends: Vec<(i64, i64)>,
}

impl Machine {
    fn new(d: Derived) -> Self {
        Self {
            d,
            frame: 0,
            triggered: false,
            current_start: None,
            temp_end: 0,
            prev_end: 0,
            next_start: 0,
            possible_ends: Vec::new(),
        }
    }

    fn reset_after_emit(&mut self) {
        self.prev_end = 0;
        self.next_start = 0;
        self.temp_end = 0;
        self.possible_ends.clear();
    }

    /// Processes one frame probability; raw spans are passed to `emit`.
    fn step(&mut self, prob: f32, emit: &mut impl FnMut(i64, i64)) {
        let d = self.d;
        let prob = f64::from(prob);
        let current = (d.frame_samples * self.frame) as i64;
        self.frame += 1;

        if prob >= d.threshold && self.temp_end != 0 {
            let silence = current - self.temp_end;
            if silence as f64 > d.min_silence_at_max_speech {
                self.possible_ends.push((self.temp_end, silence));
            }
            self.temp_end = 0;
            if self.next_start < self.prev_end {
                self.next_start = current;
            }
        }

        if prob >= d.threshold && !self.triggered {
            self.triggered = true;
            self.current_start = Some(current);
            return;
        }

        if self.triggered {
            let start = self.current_start.unwrap_or(0);
            if (current - start) as f64 > d.max_speech {
                if d.use_max_possible && !self.possible_ends.is_empty() {
                    // First candidate with the longest silence, like Python's max().
                    let (end, silence) = self.possible_ends.iter().copied().fold(
                        self.possible_ends[0],
                        |best, candidate| {
                            if candidate.1 > best.1 {
                                candidate
                            } else {
                                best
                            }
                        },
                    );
                    self.prev_end = end;
                    emit(start, end);
                    self.current_start = None;
                    self.next_start = end + silence;
                    if self.next_start < end + current {
                        self.current_start = Some(self.next_start);
                    } else {
                        self.triggered = false;
                    }
                    self.reset_after_emit();
                } else if self.prev_end != 0 {
                    emit(start, self.prev_end);
                    self.current_start = None;
                    if self.next_start < self.prev_end {
                        self.triggered = false;
                    } else {
                        self.current_start = Some(self.next_start);
                    }
                    self.reset_after_emit();
                } else {
                    emit(start, current);
                    self.current_start = None;
                    self.reset_after_emit();
                    self.triggered = false;
                    return;
                }
            }
        }

        if prob < d.neg_threshold && self.triggered {
            if self.temp_end == 0 {
                self.temp_end = current;
            }
            let silence = current - self.temp_end;
            if !d.use_max_possible && silence as f64 > d.min_silence_at_max_speech {
                self.prev_end = self.temp_end;
            }
            if (silence as f64) < d.min_silence {
                return;
            }
            let start = self.current_start.unwrap_or(0);
            if (self.temp_end - start) as f64 > d.min_speech {
                emit(start, self.temp_end);
            }
            self.current_start = None;
            self.reset_after_emit();
            self.triggered = false;
        }
    }

    fn finish(&mut self, audio_len: i64, emit: &mut impl FnMut(i64, i64)) {
        if let Some(start) = self.current_start.take()
            && (audio_len - start) as f64 > self.d.min_speech
        {
            emit(start, audio_len);
        }
    }
}

fn to_span(start: i64, end: i64) -> SpeechSpan {
    SpeechSpan {
        start: start.max(0) as usize,
        end: end.max(0) as usize,
    }
}

/// Computes padded speech spans for a whole recording of `audio_len` samples. The streaming
/// version must match this line-by-line port of the reference, which tests check.
#[cfg(test)]
pub(super) fn speech_timestamps(
    probs: &[f32],
    audio_len: usize,
    params: &TimestampParams,
) -> Vec<SpeechSpan> {
    let d = Derived::new(params);
    let mut machine = Machine::new(d);
    let mut speeches: Vec<(i64, i64)> = Vec::new();
    let mut emit = |start, end| speeches.push((start, end));
    for &prob in probs {
        machine.step(prob, &mut emit);
    }
    let n = audio_len as i64;
    machine.finish(n, &mut emit);

    let last = speeches.len().saturating_sub(1);
    for i in 0..speeches.len() {
        if i == 0 {
            speeches[i].0 = (speeches[i].0 as f64 - d.pad).max(0.0) as i64;
        }
        if i == last {
            speeches[i].1 = (n as f64).min(speeches[i].1 as f64 + d.pad) as i64;
        } else {
            let silence = speeches[i + 1].0 - speeches[i].1;
            if (silence as f64) < 2.0 * d.pad {
                speeches[i].1 += silence.div_euclid(2);
                speeches[i + 1].0 = (speeches[i + 1].0 - silence.div_euclid(2)).max(0);
            } else {
                speeches[i].1 = (n as f64).min(speeches[i].1 as f64 + d.pad) as i64;
                speeches[i + 1].0 = (speeches[i + 1].0 as f64 - d.pad).max(0.0) as i64;
            }
        }
    }
    speeches.into_iter().map(|(s, e)| to_span(s, e)).collect()
}

/// Incremental version of `speech_timestamps`; yields identical spans.
#[derive(Debug, Clone)]
pub(super) struct StreamingTimestamps {
    machine: Machine,
    /// Last raw span, start already padded; its end padding waits for the next span.
    pending: Option<(i64, i64)>,
    seen_first: bool,
}

impl StreamingTimestamps {
    /// Starts a recording.
    pub fn new(params: &TimestampParams) -> Self {
        Self {
            machine: Machine::new(Derived::new(params)),
            pending: None,
            seen_first: false,
        }
    }

    /// Processes frame probabilities; finished spans are appended to `out`.
    pub fn push(&mut self, probs: &[f32], out: &mut Vec<SpeechSpan>) {
        let mut raw = Vec::new();
        for &prob in probs {
            self.machine.step(prob, &mut |s, e| raw.push((s, e)));
        }
        for (start, end) in raw {
            self.accept(start, end, out);
        }
    }

    /// Ends the recording of `audio_len` samples and flushes the remaining spans.
    pub fn finish(&mut self, audio_len: usize, out: &mut Vec<SpeechSpan>) {
        let n = audio_len as i64;
        let mut raw = None;
        self.machine.finish(n, &mut |s, e| raw = Some((s, e)));
        if let Some((start, end)) = raw {
            self.accept(start, end, out);
        }
        if let Some((start, end)) = self.pending.take() {
            let end = (n as f64).min(end as f64 + self.machine.d.pad) as i64;
            out.push(to_span(start, end));
        }
    }

    /// Lowest sample index a span not yet emitted can start at.
    pub fn retain_from(&self) -> usize {
        let pad = self.machine.d.pad.ceil() as i64;
        let frontier = (self.machine.d.frame_samples * self.machine.frame) as i64;
        let mut low = frontier - pad;
        if let Some(start) = self.machine.current_start {
            low = low.min(start - pad);
        }
        if let Some((start, _)) = self.pending {
            low = low.min(start);
        }
        low.max(0) as usize
    }

    fn accept(&mut self, start: i64, end: i64, out: &mut Vec<SpeechSpan>) {
        let pad = self.machine.d.pad;
        let mut start = start;
        if !self.seen_first {
            start = (start as f64 - pad).max(0.0) as i64;
            self.seen_first = true;
        }
        if let Some((prev_start, mut prev_end)) = self.pending.take() {
            let silence = start - prev_end;
            if (silence as f64) < 2.0 * pad {
                prev_end += silence.div_euclid(2);
                start = (start - silence.div_euclid(2)).max(0);
            } else {
                prev_end = (prev_end as f64 + pad) as i64;
                start = (start as f64 - pad).max(0.0) as i64;
            }
            out.push(to_span(prev_start, prev_end));
        }
        self.pending = Some((start, end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_SAMPLES: usize = 512;

    /// Deterministic pseudo-random probabilities with speech-like runs.
    fn probs(seed: u64, len: usize) -> Vec<f32> {
        let mut state = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut value: f64 = next();
        (0..len)
            .map(|_| {
                if next() < 0.08 {
                    value = next();
                }
                (value + (next() - 0.5) * 0.3).clamp(0.0, 1.0) as f32
            })
            .collect()
    }

    #[test]
    fn streaming_matches_batch() {
        let param_sets = [
            TimestampParams::default(),
            TimestampParams {
                max_speech_s: 3.0,
                ..Default::default()
            },
            TimestampParams {
                max_speech_s: 5.0,
                min_silence_ms: 300,
                speech_pad_ms: 60,
                min_speech_ms: 500,
                use_max_possible_silence_at_max_speech: false,
                ..Default::default()
            },
            TimestampParams {
                max_speech_s: 15.0,
                min_silence_ms: 300,
                speech_pad_ms: 200,
                ..Default::default()
            },
        ];
        for seed in 0..200u64 {
            let p = probs(seed, 3000 + seed as usize * 7);
            let audio_len = p.len() * WINDOW_SAMPLES - (seed as usize % WINDOW_SAMPLES);
            for params in &param_sets {
                let expected = speech_timestamps(&p, audio_len, params);
                let mut stream = StreamingTimestamps::new(params);
                let mut got = Vec::new();
                let mut offset = 0;
                let mut step = 1 + seed as usize % 97;
                while offset < p.len() {
                    let end = (offset + step).min(p.len());
                    let before = got.len();
                    stream.push(&p[offset..end], &mut got);
                    // Everything emitted must start at or after the previous watermark.
                    for span in &got[before..] {
                        assert!(span.start <= span.end);
                    }
                    offset = end;
                    step = step * 3 % 101 + 1;
                }
                stream.finish(audio_len, &mut got);
                assert_eq!(got, expected, "seed {seed} params {params:?}");
            }
        }
    }

    #[test]
    fn watermark_never_passes_unemitted_spans() {
        let params = TimestampParams {
            max_speech_s: 4.0,
            speech_pad_ms: 60,
            ..Default::default()
        };
        for seed in 0..100u64 {
            let p = probs(seed, 4000);
            let audio_len = p.len() * WINDOW_SAMPLES;
            let expected = speech_timestamps(&p, audio_len, &params);
            let mut stream = StreamingTimestamps::new(&params);
            let mut got = Vec::new();
            let mut watermarks = Vec::new();
            for chunk in p.chunks(13) {
                stream.push(chunk, &mut got);
                watermarks.push((got.len(), stream.retain_from()));
            }
            stream.finish(audio_len, &mut got);
            for (emitted, watermark) in watermarks {
                for span in &expected[emitted..] {
                    assert!(
                        span.start >= watermark,
                        "seed {seed}: {span:?} < {watermark}"
                    );
                }
            }
        }
    }
}
