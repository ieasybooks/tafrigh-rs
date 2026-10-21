use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::mpsc;

use super::{ChunkRecognizer, Recognition, Transcriber};
use crate::audio::{
    AudioChunk, Decoder, Padding, SAMPLE_RATE, samples_to_seconds, seconds_to_samples,
};
use crate::error::task_result;
use crate::event::{Progress, TranscribeEvent};
use crate::segment::{Merger, Segmenter, SpeechSpan};
use crate::{Error, ErrorChain, Result, Segment, Word};

/// What to do when a chunk still fails after the recognizer's own retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnChunkError {
    /// Fail the whole file, so a rerun can produce a complete transcript.
    #[default]
    Abort,
    /// Leave the chunk out of the transcript and continue.
    Skip,
}

/// Options for [`ChunkedTranscriber`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkOptions {
    /// Longest chunk sent to the recognizer, in seconds, before padding. Spans longer than
    /// this are split (configure the segmenter with the same limit to split at pauses).
    pub max_chunk_s: f64,
    /// Merge consecutive speech spans up to `max_chunk_s`: fewer requests and more context
    /// per request, but longer segments.
    pub merge: bool,
    /// Audio added on both sides of each chunk; 0.5 s of silence by default, because
    /// recognizers such as Wit drop words that start or end right at the chunk edge.
    pub padding: Option<Padding>,
    /// What to do with chunks that fail after the recognizer's retries.
    pub on_error: OnChunkError,
    /// Requests in flight at once. Defaults to the recognizer's
    /// [`ChunkRecognizer::max_in_flight`].
    pub max_in_flight: Option<usize>,
    /// Resend speech the recognizer skipped: when a chunk's words leave at least this many
    /// seconds of detected speech uncovered, that stretch goes out again as its own request.
    /// Recognizers such as Wit sometimes stop transcribing partway through a chunk. Needs word
    /// timings; `None` disables.
    pub resend_gaps: Option<f64>,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_chunk_s: 15.0,
            merge: false,
            padding: Some(Padding::silence(0.5)),
            on_error: OnChunkError::Abort,
            max_in_flight: None,
            resend_gaps: Some(1.0),
        }
    }
}

/// Decodes a file, segments it into speech chunks and recognizes them with a
/// [`ChunkRecognizer`], all streamed: requests start while the file is still being decoded,
/// and only the audio of unfinished chunks is kept in memory.
pub struct ChunkedTranscriber {
    decoder: Decoder,
    segmenter: Arc<dyn Segmenter>,
    recognizer: Arc<dyn ChunkRecognizer>,
    options: ChunkOptions,
}

impl std::fmt::Debug for ChunkedTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkedTranscriber")
            .field("decoder", &self.decoder)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl ChunkedTranscriber {
    /// Pairs a segmenter with a recognizer. Both may also be shared `Arc`s, including
    /// `Arc<dyn Segmenter>` and `Arc<dyn ChunkRecognizer>` chosen at runtime.
    ///
    /// Fails when `options` are out of range or chunks (plus padding) would exceed the
    /// recognizer's [`ChunkRecognizer::max_audio_duration`].
    pub fn new(
        segmenter: impl Segmenter + 'static,
        recognizer: impl ChunkRecognizer + 'static,
        options: ChunkOptions,
    ) -> Result<Self> {
        let segmenter: Arc<dyn Segmenter> = Arc::new(segmenter);
        let recognizer: Arc<dyn ChunkRecognizer> = Arc::new(recognizer);
        if !options.max_chunk_s.is_finite() || options.max_chunk_s <= 0.0 {
            return Err(Error::Config(
                "maximum chunk duration must be positive".into(),
            ));
        }
        if seconds_to_samples(options.max_chunk_s) == 0 {
            return Err(Error::Config(
                "maximum chunk duration must be at least one sample".into(),
            ));
        }
        if let Some(gap) = options.resend_gaps
            && !(gap.is_finite() && seconds_to_samples(gap) > 0)
        {
            return Err(Error::Config(
                "the gap that triggers a resend must be positive".into(),
            ));
        }
        let padding = options.padding.map_or(0.0, |p| p.seconds.max(0.0));
        if let Some(limit) = recognizer.max_audio_duration()
            && options.max_chunk_s + 2.0 * padding > limit + 1e-9
        {
            return Err(Error::Config(format!(
                "chunks of {:.2}s plus {:.2}s padding exceed the recognizer's {limit:.2}s limit",
                options.max_chunk_s,
                2.0 * padding
            )));
        }
        Ok(Self {
            decoder: Decoder::default(),
            segmenter,
            recognizer,
            options,
        })
    }

    /// Uses a custom decoder, e.g. with explicit ffmpeg paths.
    #[must_use]
    pub fn with_decoder(mut self, decoder: Decoder) -> Self {
        self.decoder = decoder;
        self
    }

    /// The options in effect.
    pub fn options(&self) -> &ChunkOptions {
        &self.options
    }

    /// Requests for the speech that `recognized` (the chunk's segments) left out.
    fn resends(&self, request: &Request, recognized: &[Segment], min_gap: usize) -> Vec<Request> {
        let regions = request.missed_speech(recognized, min_gap);
        regions
            .into_iter()
            .map(|region| {
                tracing::debug!(
                    "chunk #{}: no words for {:.2}s–{:.2}s; resending",
                    request.chunk.index,
                    samples_to_seconds(region.start),
                    samples_to_seconds(region.end)
                );
                request.resend(region, self.options.padding)
            })
            .collect()
    }

    /// Applies [`ChunkOptions::on_error`] to a request that failed after all retries; `Err`
    /// stops the file.
    fn failed(
        &self,
        meta: ChunkMeta,
        resent: bool,
        source: crate::BoxError,
        media: &Path,
        progress: &dyn Progress<TranscribeEvent>,
    ) -> Result<()> {
        match self.options.on_error {
            OnChunkError::Abort => Err(Error::Recognition {
                index: meta.index,
                start: meta.start,
                end: meta.end,
                source,
            }),
            OnChunkError::Skip => {
                let error = ErrorChain(&*source).to_string();
                tracing::warn!(
                    "skipping {}chunk #{} ({:.2}s–{:.2}s) of {}: {error}",
                    if resent { "the resend of " } else { "" },
                    meta.index,
                    meta.start,
                    meta.end,
                    media.display()
                );
                if !resent {
                    progress.report(TranscribeEvent::ChunkSkipped {
                        index: meta.index,
                        start: meta.start,
                        end: meta.end,
                        error,
                    });
                }
                Ok(())
            }
        }
    }
}

#[async_trait]
impl Transcriber for ChunkedTranscriber {
    async fn transcribe(
        &self,
        media: &Path,
        progress: &dyn Progress<TranscribeEvent>,
    ) -> Result<Vec<Segment>> {
        let duration = self.decoder.probe_duration(media).await;
        progress.report(TranscribeEvent::Started { duration });

        let max_in_flight = self
            .options
            .max_in_flight
            .unwrap_or_else(|| self.recognizer.max_in_flight())
            .max(1);
        // Queued chunks hold audio; a short queue is enough to keep requests flowing.
        let (tx, mut rx) = mpsc::channel(max_in_flight.min(64) + 8);
        let producer = Producer {
            decoder: self.decoder.clone(),
            path: media.to_owned(),
            segmenter: self.segmenter.clone(),
            chunker: Chunker::new(
                seconds_to_samples(self.options.max_chunk_s),
                self.options.merge,
                self.options.padding,
            ),
            tx,
        };
        let producer_task = tokio::task::spawn_blocking(move || producer.run());
        let resend_gap = self.options.resend_gaps.map(seconds_to_samples);

        let mut inflight = FuturesUnordered::new();
        let mut segments: Vec<(usize, Segment)> = Vec::new();
        let mut receiving = true;
        let outcome: Result<()> = loop {
            tokio::select! {
                biased;
                Some((request, result)) = inflight.next(), if !inflight.is_empty() => {
                    let request: Request = request;
                    let meta = ChunkMeta::of(&request.chunk);
                    let resent = matches!(request.kind, RequestKind::Resend);
                    match result {
                        Ok(recognition) => {
                            let recognized = meta.recognized(recognition);
                            if resent {
                                tracing::debug!(
                                    "chunk #{} resend {:.2}s–{:.2}s: {} segment(s)",
                                    meta.index, meta.start, meta.end, recognized.segments.len()
                                );
                            } else {
                                progress.report(TranscribeEvent::ChunkRecognized {
                                    index: meta.index,
                                    start: meta.start,
                                    end: meta.end,
                                    empty: recognized.segments.is_empty(),
                                });
                                if let Some(min_gap) = resend_gap
                                    && recognized.timed
                                {
                                    for again in
                                        self.resends(&request, &recognized.segments, min_gap)
                                    {
                                        inflight.push(recognize(self.recognizer.clone(), again));
                                    }
                                }
                            }
                            segments.extend(
                                recognized.segments.into_iter().map(|s| (meta.index, s)),
                            );
                        }
                        Err(source) => {
                            if let Err(err) = self.failed(meta, resent, source, media, progress) {
                                break Err(err);
                            }
                        }
                    }
                }
                message = rx.recv(), if receiving && inflight.len() < max_in_flight => match message {
                    Some(Produced::Chunk(request)) => {
                        let meta = ChunkMeta::of(&request.chunk);
                        progress.report(TranscribeEvent::ChunkQueued {
                            index: meta.index,
                            start: meta.start,
                            end: meta.end,
                        });
                        inflight.push(recognize(self.recognizer.clone(), *request));
                    }
                    Some(Produced::Decoded(seconds)) => {
                        progress.report(TranscribeEvent::Decoded { seconds });
                    }
                    Some(Produced::Segmented(chunks)) => {
                        progress.report(TranscribeEvent::Segmented { chunks });
                    }
                    None => receiving = false,
                },
                else => break Ok(()),
            }
        };
        drop(rx);
        drop(inflight);

        let decoding = task_result(producer_task.await);
        outcome?;
        decoding?;

        // By chunk, then by time: resent stretches land between the chunk's other utterances.
        // Stable, so segments starting together keep their order.
        segments.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.start.total_cmp(&y.start)));
        Ok(segments.into_iter().map(|(_, segment)| segment).collect())
    }
}

/// Audio on its way to the recognizer.
struct Request {
    chunk: AudioChunk,
    /// Where the chunk's audio starts in the source, in samples.
    first_sample: usize,
    /// Padding samples before the audio in `chunk.samples`.
    lead: usize,
    kind: RequestKind,
}

/// Why a request is sent.
enum RequestKind {
    /// A chunk cut from detected speech. `speech` holds its speech spans, in source samples,
    /// for finding what the recognizer skipped.
    Chunk { speech: Vec<SpeechSpan> },
    /// A stretch of a chunk sent again because the recognizer skipped it. Not checked again.
    Resend,
}

async fn recognize(
    recognizer: Arc<dyn ChunkRecognizer>,
    request: Request,
) -> (Request, std::result::Result<Recognition, crate::BoxError>) {
    let result = recognizer.recognize(&request.chunk).await;
    (request, result)
}

impl Request {
    /// Stretches of this chunk's speech that `segments` (its recognized words) leave
    /// uncovered, where at least `min_gap` samples of speech went unrecognized.
    ///
    /// Each stretch runs from the first to the last uncovered speech between two words. When a
    /// stretch would be the whole chunk (nothing was recognized), resending it unchanged would
    /// give the same answer, so its speech spans go out one by one instead.
    fn missed_speech(&self, segments: &[Segment], min_gap: usize) -> Vec<SpeechSpan> {
        let RequestKind::Chunk { speech } = &self.kind else {
            return Vec::new();
        };
        let mut words: Vec<(usize, usize)> = segments
            .iter()
            .flat_map(|segment| &segment.words)
            .map(|word| (seconds_to_samples(word.start), seconds_to_samples(word.end)))
            .collect();
        words.sort_unstable();
        let first = self.first_sample;
        let last = first + self.chunk.samples.len() - 2 * self.lead;

        // Gaps between recognized words, chunk edges included.
        let mut gaps = Vec::new();
        let mut covered_to = first;
        for (start, end) in words {
            if start > covered_to {
                gaps.push(SpeechSpan {
                    start: covered_to,
                    end: start.min(last),
                });
            }
            covered_to = covered_to.max(end);
        }
        if covered_to < last {
            gaps.push(SpeechSpan {
                start: covered_to,
                end: last,
            });
        }

        let mut missed = Vec::new();
        for gap in gaps {
            let parts: Vec<SpeechSpan> = speech
                .iter()
                .map(|span| SpeechSpan {
                    start: span.start.max(gap.start),
                    end: span.end.min(gap.end),
                })
                .filter(|part| !part.is_empty())
                .collect();
            let (Some(head), Some(tail)) = (parts.first(), parts.last()) else {
                continue;
            };
            if parts.iter().map(SpeechSpan::len).sum::<usize>() < min_gap {
                continue;
            }
            let stretch = SpeechSpan {
                start: head.start,
                end: tail.end,
            };
            if stretch.start <= first && stretch.end >= last {
                if parts.len() > 1 {
                    missed.extend(parts.into_iter().filter(|part| part.len() >= min_gap));
                }
            } else {
                missed.push(stretch);
            }
        }
        missed
    }

    /// A request for `region` (source samples within this chunk), padded like the chunk.
    fn resend(&self, region: SpeechSpan, padding: Option<Padding>) -> Request {
        let audio_len = self.chunk.samples.len() - 2 * self.lead;
        let from = region
            .start
            .saturating_sub(self.first_sample)
            .min(audio_len);
        let to = region
            .end
            .saturating_sub(self.first_sample)
            .clamp(from, audio_len);
        let audio = &self.chunk.samples[self.lead + from..self.lead + to];
        let (samples, lead) = pad(audio, padding, self.chunk.index);
        Request {
            chunk: AudioChunk {
                index: self.chunk.index,
                start: samples_to_seconds(self.first_sample + from),
                end: samples_to_seconds(self.first_sample + to),
                lead_in: samples_to_seconds(lead),
                samples,
            },
            first_sample: self.first_sample + from,
            lead,
            kind: RequestKind::Resend,
        }
    }
}

/// Pads `audio`; returns the samples and the padding samples on each side.
fn pad(audio: &[i16], padding: Option<Padding>, index: usize) -> (Vec<i16>, usize) {
    match padding {
        Some(padding) => {
            let (samples, _) = padding.apply(audio, index as u64);
            let lead = (samples.len() - audio.len()) / 2;
            (samples, lead)
        }
        None => (audio.to_vec(), 0),
    }
}

#[derive(Debug, Clone, Copy)]
struct ChunkMeta {
    index: usize,
    start: f64,
    end: f64,
    lead_in: f64,
}

impl ChunkMeta {
    fn of(chunk: &AudioChunk) -> Self {
        Self {
            index: chunk.index,
            start: chunk.start,
            end: chunk.end,
            lead_in: chunk.lead_in,
        }
    }

    fn source_time(&self, seconds_in_chunk: f64) -> f64 {
        (self.start + seconds_in_chunk - self.lead_in).clamp(self.start, self.end)
    }

    fn map_words(&self, words: Vec<Word>) -> Vec<Word> {
        words
            .into_iter()
            .filter_map(|word| {
                if !(word.start.is_finite() && word.end.is_finite()) || word.end < word.start {
                    return None;
                }
                let start = self.source_time(word.start);
                let end = self.source_time(word.end);
                if !(start.is_finite() && end.is_finite()) || end < start {
                    return None;
                }
                let confidence = word.confidence.filter(|confidence| {
                    confidence.is_finite() && (0.0..=1.0).contains(confidence)
                });
                Some(Word {
                    start,
                    end,
                    confidence,
                    ..word
                })
            })
            .collect()
    }

    /// One segment per utterance, bounded by its words, when every utterance has word
    /// timings; otherwise a single segment spanning the chunk.
    fn recognized(self, recognition: Recognition) -> Recognized {
        let utterances: Vec<(String, Vec<Word>)> = recognition
            .utterances
            .into_iter()
            .filter_map(|utterance| {
                let text = utterance.text.trim();
                (!text.is_empty()).then(|| (text.to_owned(), self.map_words(utterance.words)))
            })
            .collect();
        if utterances.is_empty() {
            return Recognized {
                segments: Vec::new(),
                timed: true,
            };
        }
        if utterances.iter().all(|(_, words)| !words.is_empty()) {
            let segments = utterances
                .into_iter()
                .map(|(text, words)| {
                    let start = words.iter().map(|w| w.start).fold(f64::INFINITY, f64::min);
                    let end = words.iter().map(|w| w.end).fold(start, f64::max);
                    Segment {
                        start,
                        end,
                        text,
                        words,
                    }
                })
                .collect();
            return Recognized {
                segments,
                timed: true,
            };
        }
        let mut text = String::new();
        let mut words = Vec::new();
        for (utterance_text, utterance_words) in utterances {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&utterance_text);
            words.extend(utterance_words);
        }
        let segment = Segment {
            start: self.start,
            end: self.end,
            text,
            words,
        };
        Recognized {
            segments: vec![segment],
            timed: false,
        }
    }
}

/// The segments recognized in a chunk.
#[derive(Debug, PartialEq)]
struct Recognized {
    segments: Vec<Segment>,
    /// Whether word timings show what was recognized where, so skipped speech can be found.
    /// Also true when nothing was recognized.
    timed: bool,
}

enum Produced {
    Decoded(f64),
    Chunk(Box<Request>),
    Segmented(usize),
}

/// Runs on a blocking thread: decode → segment → cut chunks.
struct Producer {
    decoder: Decoder,
    path: PathBuf,
    segmenter: Arc<dyn Segmenter>,
    chunker: Chunker,
    tx: mpsc::Sender<Produced>,
}

const BLOCK_SAMPLES: usize = SAMPLE_RATE as usize; // one second

impl Producer {
    fn run(mut self) -> Result<()> {
        let mut session = self.segmenter.begin()?;
        let mut reader = self.decoder.open(&self.path)?;
        let mut block = Vec::with_capacity(BLOCK_SAMPLES);
        let mut floats = Vec::with_capacity(BLOCK_SAMPLES);
        let mut spans = Vec::new();
        let mut decoded = 0;
        while reader.read(&mut block, BLOCK_SAMPLES)? > 0 {
            decoded += block.len();
            self.chunker.add_audio(&block);
            floats.clear();
            floats.extend(block.iter().map(|&s| f32::from(s) / 32768.0));
            session.push(&floats, &mut spans)?;
            self.send_chunks(&mut spans)?;
            self.send(Produced::Decoded(samples_to_seconds(decoded)))?;
            self.chunker.discard_before(session.retain_from());
        }

        session.finish(&mut spans)?;
        self.send_chunks(&mut spans)?;
        if let Some(request) = self.chunker.finish() {
            self.send(Produced::Chunk(Box::new(request)))?;
        }
        self.send(Produced::Segmented(self.chunker.chunks))
    }

    fn send_chunks(&mut self, spans: &mut Vec<SpeechSpan>) -> Result<()> {
        let tx = &self.tx;
        self.chunker.cut(spans.drain(..), |request| {
            send(tx, Produced::Chunk(Box::new(request)))
        })
    }

    fn send(&self, message: Produced) -> Result<()> {
        send(&self.tx, message)
    }
}

/// Waits for room in the channel, so only a few chunks' audio is ever waiting.
fn send(tx: &mpsc::Sender<Produced>, message: Produced) -> Result<()> {
    tx.blocking_send(message).map_err(|_| Error::Cancelled)
}

/// Cuts speech spans into padded requests, merging consecutive spans when configured, and
/// keeps the decoded audio that requests still to come need.
struct Chunker {
    max_len: usize,
    padding: Option<Padding>,
    merger: Option<Merger>,
    /// Speech spans inside the chunk the merger is still growing.
    parts: Vec<SpeechSpan>,
    pcm: PcmWindow,
    /// Chunks cut so far.
    chunks: usize,
}

impl Chunker {
    fn new(max_len: usize, merge: bool, padding: Option<Padding>) -> Self {
        Self {
            max_len,
            padding,
            merger: merge.then(|| Merger::new(max_len)),
            parts: Vec::new(),
            pcm: PcmWindow::default(),
            chunks: 0,
        }
    }

    fn add_audio(&mut self, samples: &[i16]) {
        self.pcm.samples.extend_from_slice(samples);
    }

    /// Cuts `spans` into requests and passes each to `send` before cutting the next, so a full
    /// channel holds back the audio still to be padded. A chunk the merger is still growing
    /// waits for later spans or [`Self::finish`].
    fn cut(
        &mut self,
        spans: impl Iterator<Item = SpeechSpan>,
        mut send: impl FnMut(Request) -> Result<()>,
    ) -> Result<()> {
        for span in spans {
            // Segmenters should respect the limit; split defensively if one does not.
            let mut start = span.start;
            while start < span.end {
                let piece = SpeechSpan {
                    start,
                    end: (start + self.max_len).min(span.end),
                };
                start = piece.end;
                let Some(merger) = &mut self.merger else {
                    send(self.request(piece, vec![piece]))?;
                    continue;
                };
                match merger.push(piece) {
                    // `piece` starts a new chunk; the finished one holds the parts so far.
                    Some(finished) => {
                        let parts = std::mem::replace(&mut self.parts, vec![piece]);
                        send(self.request(finished, parts))?;
                    }
                    None => self.parts.push(piece),
                }
            }
        }
        Ok(())
    }

    /// The request for the chunk the merger was still growing when the recording ended.
    fn finish(&mut self) -> Option<Request> {
        let span = self.merger.as_mut()?.finish()?;
        let parts = std::mem::take(&mut self.parts);
        Some(self.request(span, parts))
    }

    /// Drops audio before `sample`, except what the chunk being merged still needs.
    fn discard_before(&mut self, sample: usize) {
        let keep = self
            .merger
            .as_ref()
            .and_then(Merger::retain_from)
            .map_or(sample, |start| start.min(sample));
        self.pcm.discard_before(keep);
    }

    /// The request for the chunk covering `span`, which holds the speech spans `speech`.
    fn request(&mut self, span: SpeechSpan, speech: Vec<SpeechSpan>) -> Request {
        let index = self.chunks;
        self.chunks += 1;
        let (samples, lead) = pad(self.pcm.slice(span), self.padding, index);
        Request {
            chunk: AudioChunk {
                index,
                start: samples_to_seconds(span.start),
                end: samples_to_seconds(span.end),
                lead_in: samples_to_seconds(lead),
                samples,
            },
            first_sample: span.start,
            lead,
            kind: RequestKind::Chunk { speech },
        }
    }
}

/// Decoded samples from `offset` onwards.
#[derive(Default)]
struct PcmWindow {
    offset: usize,
    samples: Vec<i16>,
}

impl PcmWindow {
    fn slice(&self, span: SpeechSpan) -> &[i16] {
        debug_assert!(
            span.start >= self.offset,
            "span starts before retained audio"
        );
        let start = span
            .start
            .saturating_sub(self.offset)
            .min(self.samples.len());
        let end = span.end.saturating_sub(self.offset).min(self.samples.len());
        &self.samples[start..end.max(start)]
    }

    fn discard_before(&mut self, sample: usize) {
        let droppable = sample.saturating_sub(self.offset).min(self.samples.len());
        // Drain in large steps to keep this linear overall.
        if droppable >= SAMPLE_RATE as usize * 10 || droppable == self.samples.len() {
            self.samples.drain(..droppable);
            self.offset += droppable;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoxError, Utterance};

    struct NoopRecognizer;

    #[async_trait::async_trait]
    impl ChunkRecognizer for NoopRecognizer {
        async fn recognize(&self, _chunk: &AudioChunk) -> Result<Recognition, BoxError> {
            Ok(Recognition::default())
        }
    }

    fn word(start: f64, end: f64, text: &str) -> Word {
        Word {
            start,
            end,
            text: text.into(),
            confidence: None,
        }
    }

    const META: ChunkMeta = ChunkMeta {
        index: 0,
        start: 10.0,
        end: 20.0,
        lead_in: 1.0,
    };

    #[test]
    fn utterances_with_words_become_tight_segments() {
        let recognition = Recognition {
            utterances: vec![
                Utterance {
                    text: " first part ".into(),
                    // Times are relative to the padded audio: 1s of lead-in.
                    words: vec![word(1.5, 2.0, "first"), word(2.1, 2.6, "part")],
                },
                Utterance::default(),
                Utterance {
                    text: "second".into(),
                    words: vec![word(7.0, 7.4, "second")],
                },
            ],
        };
        let segments = META.recognized(recognition).segments;
        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].start, segments[0].end), (10.5, 11.6));
        assert_eq!(segments[0].text, "first part");
        assert_eq!((segments[1].start, segments[1].end), (16.0, 16.4));
    }

    #[test]
    fn missing_timings_fall_back_to_one_chunk_segment() {
        let recognition = Recognition {
            utterances: vec![
                Utterance {
                    text: "timed".into(),
                    words: vec![word(1.0, 1.5, "timed")],
                },
                Utterance {
                    text: "untimed".into(),
                    words: vec![],
                },
            ],
        };
        let segments = META.recognized(recognition).segments;
        assert_eq!(segments.len(), 1);
        assert_eq!((segments[0].start, segments[0].end), (10.0, 20.0));
        assert_eq!(segments[0].text, "timed untimed");
        assert_eq!(segments[0].words.len(), 1);
        assert_eq!(
            META.recognized(Recognition::text("  ")),
            Recognized {
                segments: vec![],
                timed: true
            }
        );
    }

    #[test]
    fn invalid_word_timings_are_dropped_and_confidence_is_sanitized() {
        let mut out_of_range = word(1.0, 1.5, "bad confidence");
        out_of_range.confidence = Some(1.1);
        let mut non_finite_confidence = word(2.0, 2.5, "non-finite confidence");
        non_finite_confidence.confidence = Some(f32::NAN);
        let mut valid = word(3.0, 3.5, "valid");
        valid.confidence = Some(0.75);
        let words = META.map_words(vec![
            word(f64::NAN, 1.0, "nan start"),
            word(1.0, f64::INFINITY, "infinite end"),
            word(2.0, 1.0, "reversed"),
            out_of_range,
            non_finite_confidence,
            valid,
        ]);
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text, "bad confidence");
        assert_eq!(words[0].confidence, None);
        assert_eq!(words[1].text, "non-finite confidence");
        assert_eq!(words[1].confidence, None);
        assert_eq!(words[2].text, "valid");
        assert_eq!(words[2].confidence, Some(0.75));
    }

    #[test]
    fn utterances_with_only_invalid_words_use_chunk_fallback() {
        let recognized = META.recognized(Recognition {
            utterances: vec![Utterance {
                text: "invalid timings".into(),
                words: vec![word(f64::NAN, 1.0, "invalid")],
            }],
        });
        assert_eq!(
            recognized,
            Recognized {
                segments: vec![Segment::new(10.0, 20.0, "invalid timings")],
                timed: false
            }
        );
    }

    const S: usize = SAMPLE_RATE as usize;

    /// A chunk of `len` samples starting at source sample `first`, padded with `lead` zeros on
    /// each side; sample values count up from 1 so slices are recognizable.
    fn request(first: usize, len: usize, lead: usize, speech: &[(usize, usize)]) -> Request {
        let mut samples = vec![0; lead];
        samples.extend((1..=len).map(|i| i as i16));
        samples.extend(std::iter::repeat_n(0, lead));
        Request {
            chunk: AudioChunk {
                index: 3,
                start: samples_to_seconds(first),
                end: samples_to_seconds(first + len),
                lead_in: samples_to_seconds(lead),
                samples,
            },
            first_sample: first,
            lead,
            kind: RequestKind::Chunk {
                speech: speech
                    .iter()
                    .map(|&(start, end)| SpeechSpan { start, end })
                    .collect(),
            },
        }
    }

    /// A segment holding one word per `(start, end)`, in seconds.
    fn words(spans: &[(f64, f64)]) -> Vec<Segment> {
        vec![Segment {
            start: spans[0].0,
            end: spans[spans.len() - 1].1,
            text: "x".into(),
            words: spans.iter().map(|&(s, e)| word(s, e, "x")).collect(),
        }]
    }

    fn span(start: usize, end: usize) -> SpeechSpan {
        SpeechSpan { start, end }
    }

    #[test]
    fn finds_speech_the_recognizer_skipped() {
        // One span of 10s starting at 100s; words stop at 103s.
        let single = request(100 * S, 10 * S, 0, &[(100 * S, 110 * S)]);
        let recognized = words(&[(100.2, 101.0), (101.0, 103.0)]);
        assert_eq!(
            single.missed_speech(&recognized, S),
            [span(103 * S, 110 * S)]
        );
        // Gaps shorter than the threshold are left alone.
        assert_eq!(single.missed_speech(&recognized, 8 * S), []);
        let covered = words(&[(100.0, 104.5), (105.0, 109.5)]);
        assert_eq!(single.missed_speech(&covered, S), Vec::<SpeechSpan>::new());
        // Nothing recognized in a single span: resending it would repeat the request.
        assert_eq!(single.missed_speech(&[], S), Vec::<SpeechSpan>::new());

        // Merged chunk 0–8s with speech at 0–1s, 2–4s and 6–8s; words cover the first span.
        let merged = request(0, 8 * S, 0, &[(0, S), (2 * S, 4 * S), (6 * S, 8 * S)]);
        // The stretch starts where the words stop, inside the first span.
        assert_eq!(
            merged.missed_speech(&words(&[(0.1, 0.9)]), S),
            [span(9 * S / 10, 8 * S)]
        );
        // Silence between spans does not count: 1–2s and 4–6s hold no speech.
        let tail_only = words(&[(0.0, 4.0), (6.0, 8.0)]);
        assert_eq!(
            merged.missed_speech(&tail_only, S / 2),
            Vec::<SpeechSpan>::new()
        );
        // Nothing recognized: each span long enough goes out on its own.
        assert_eq!(
            merged.missed_speech(&[], S + 1),
            [span(2 * S, 4 * S), span(6 * S, 8 * S)]
        );
    }

    #[test]
    fn resends_a_padded_slice_of_the_chunk() {
        let chunk = request(50 * S, 4 * S, 100, &[(50 * S, 54 * S)]);
        let resend = chunk.resend(span(51 * S, 53 * S), Some(Padding::silence(0.5)));
        assert_eq!(resend.chunk.index, 3);
        assert!((resend.chunk.start - 51.0).abs() < 1e-9);
        assert!((resend.chunk.end - 53.0).abs() < 1e-9);
        assert_eq!(resend.lead, S / 2);
        assert!((resend.chunk.lead_in - 0.5).abs() < 1e-9);
        assert_eq!(resend.chunk.samples.len(), 3 * S);
        // The audio is the original chunk's 1s–3s.
        assert_eq!(resend.chunk.samples[S / 2], (S + 1) as i16);
        assert_eq!(resend.chunk.samples[S / 2 + 2 * S - 1], (3 * S) as i16);
        assert!(matches!(resend.kind, RequestKind::Resend));
        // Regions reaching past the chunk are clamped to it.
        let clamped = chunk.resend(span(53 * S, 60 * S), None);
        assert_eq!(clamped.chunk.samples.len(), S);
    }

    #[test]
    fn cutting_stops_at_the_first_request_that_cannot_be_sent() {
        // One second of audio in 1ms chunks would be a thousand padded requests.
        let mut chunker = Chunker::new(S / 1000, false, Some(Padding::silence(0.5)));
        chunker.add_audio(&vec![0; S]);
        let mut sent = 0;
        let result = chunker.cut(std::iter::once(span(0, S)), |_| {
            sent += 1;
            Err(Error::Cancelled)
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!((sent, chunker.chunks), (1, 1));
    }

    #[test]
    fn rejects_chunk_limit_below_one_sample() {
        let segmenter = crate::segment::FixedWindows::new(1.0).unwrap();
        let error = ChunkedTranscriber::new(
            segmenter,
            NoopRecognizer,
            ChunkOptions {
                max_chunk_s: 1e-6,
                ..ChunkOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("at least one sample"));
    }
}
