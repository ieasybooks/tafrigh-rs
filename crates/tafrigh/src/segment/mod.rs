//! Splitting a PCM stream into speech spans.
//!
//! A [`Segmenter`] is shared configuration (and model weights); each recording gets its own
//! [`SegmentationSession`], fed incrementally so spans can be recognized while decoding continues.

#[cfg(feature = "firered")]
pub mod firered;
#[cfg(feature = "silero")]
pub mod silero;

#[cfg(any(feature = "silero", feature = "firered"))]
mod timestamps;

use std::sync::Arc;
use std::time::Duration;

use crate::Result;
use crate::audio::duration_to_samples;

/// A half-open range of samples `[start, end)` at [`crate::audio::SAMPLE_RATE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechSpan {
    /// First sample.
    pub start: usize,
    /// One past the last sample.
    pub end: usize,
}

impl SpeechSpan {
    /// Number of samples.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// True when the span holds no samples.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Creates a [`SegmentationSession`] per recording.
pub trait Segmenter: Send + Sync {
    /// Starts segmenting a new recording.
    fn begin(&self) -> Result<Box<dyn SegmentationSession>>;
}

impl<T: Segmenter + ?Sized> Segmenter for Arc<T> {
    fn begin(&self) -> Result<Box<dyn SegmentationSession>> {
        (**self).begin()
    }
}

impl<T: Segmenter + ?Sized> Segmenter for Box<T> {
    fn begin(&self) -> Result<Box<dyn SegmentationSession>> {
        (**self).begin()
    }
}

/// Incremental segmentation of one recording.
///
/// Spans are emitted in order, never overlap, and are final once emitted.
pub trait SegmentationSession: Send {
    /// Feeds mono 16 kHz samples in `[-1, 1]`. Finalized spans are appended to `out`.
    fn push(&mut self, samples: &[f32], out: &mut Vec<SpeechSpan>) -> Result<()>;

    /// Signals the end of the recording and flushes the remaining spans into `out`.
    fn finish(&mut self, out: &mut Vec<SpeechSpan>) -> Result<()>;

    /// Lowest sample index a span that has not been emitted yet can start at.
    /// Callers may discard audio before this point.
    fn retain_from(&self) -> usize;
}

/// Covers the whole recording with consecutive windows, without voice detection.
#[derive(Debug, Clone, Copy)]
pub struct FixedWindows {
    window: usize,
}

impl FixedWindows {
    /// Windows of `window`; the last one may be shorter.
    pub fn new(window: Duration) -> Result<Self> {
        let window = duration_to_samples(window);
        if window == 0 {
            return Err(crate::Error::Config(
                "the window must be at least one sample".into(),
            ));
        }
        Ok(Self { window })
    }
}

impl Segmenter for FixedWindows {
    fn begin(&self) -> Result<Box<dyn SegmentationSession>> {
        Ok(Box::new(FixedWindowsSession {
            window: self.window,
            start: 0,
            seen: 0,
        }))
    }
}

struct FixedWindowsSession {
    window: usize,
    start: usize,
    seen: usize,
}

impl SegmentationSession for FixedWindowsSession {
    fn push(&mut self, samples: &[f32], out: &mut Vec<SpeechSpan>) -> Result<()> {
        self.seen += samples.len();
        while self.seen - self.start >= self.window {
            out.push(SpeechSpan {
                start: self.start,
                end: self.start + self.window,
            });
            self.start += self.window;
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<SpeechSpan>) -> Result<()> {
        if self.seen > self.start {
            out.push(SpeechSpan {
                start: self.start,
                end: self.seen,
            });
            self.start = self.seen;
        }
        Ok(())
    }

    fn retain_from(&self) -> usize {
        self.start
    }
}

/// Greedily merges consecutive spans while the merged interval stays within `max_len` samples.
///
/// Merging reduces the number of recognizer requests and gives each request more context, at
/// the cost of coarser segments. Silence between merged spans is kept.
#[derive(Debug, Clone)]
pub(crate) struct Merger {
    max_len: usize,
    current: Option<SpeechSpan>,
}

impl Merger {
    /// Merges up to `max_len` samples per span.
    pub fn new(max_len: usize) -> Self {
        Self {
            max_len,
            current: None,
        }
    }

    /// Adds the next span; returns a merged span once it can no longer grow.
    pub fn push(&mut self, span: SpeechSpan) -> Option<SpeechSpan> {
        match &mut self.current {
            Some(current) if span.end.saturating_sub(current.start) <= self.max_len => {
                current.end = span.end;
                None
            }
            slot => slot.replace(span),
        }
    }

    /// Returns the span still being grown, if any.
    pub fn finish(&mut self) -> Option<SpeechSpan> {
        self.current.take()
    }

    /// Start of the span still being grown; its audio must be kept.
    pub fn retain_from(&self) -> Option<usize> {
        self.current.map(|span| span.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> SpeechSpan {
        SpeechSpan { start, end }
    }

    #[test]
    fn merger_respects_max_len() {
        let mut merger = Merger::new(100);
        let mut out = Vec::new();
        for s in [
            span(0, 30),
            span(40, 90),
            span(95, 120),
            span(130, 150),
            span(300, 320),
        ] {
            out.extend(merger.push(s));
        }
        out.extend(merger.finish());
        assert_eq!(out, vec![span(0, 90), span(95, 150), span(300, 320)]);
    }

    #[test]
    fn fixed_windows_cover_everything() {
        let segmenter = FixedWindows::new(Duration::from_secs(1)).unwrap();
        let mut session = segmenter.begin().unwrap();
        let mut out = Vec::new();
        session.push(&vec![0.0; 20_000], &mut out).unwrap();
        session.push(&vec![0.0; 20_000], &mut out).unwrap();
        session.finish(&mut out).unwrap();
        assert_eq!(
            out,
            vec![span(0, 16_000), span(16_000, 32_000), span(32_000, 40_000)]
        );
    }
}
