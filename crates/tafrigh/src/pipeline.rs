//! Running a transcriber over user inputs and writing transcripts.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::Notify;

use crate::error::task_result;
use crate::event::{Event, ItemInfo, Progress, TranscribeEvent};
use crate::output::{
    Layout, OutputMode, OutputWriter, SampleReservoir, SampleRow, Target, unique_name, write_sample,
};
use crate::source::{Input, Media, MediaItem, YtDlp, discover_local};
use crate::{CaptionProvider, Error, ErrorChain, Result, Segment, Transcriber};

/// Transcribes inputs (files, folders, URLs) and writes transcripts.
///
/// ```no_run
/// # async fn demo(transcriber: tafrigh::ChunkedTranscriber) -> tafrigh::Result<()> {
/// use tafrigh::output::OutputWriter;
/// use tafrigh::source::Input;
/// use tafrigh::{NoProgress, Tafrigh};
///
/// let tafrigh = Tafrigh::builder(transcriber)
///     .output(OutputWriter::new("transcripts"))
///     .skip_existing(true)
///     .build();
/// let report = tafrigh
///     .run([Input::parse("lectures/"), Input::parse("https://youtu.be/…")], &NoProgress)
///     .await?;
/// assert_eq!(report.failures(), 0);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Tafrigh {
    backend: Backend,
    output: OutputWriter,
    output_mode: OutputMode,
    ytdlp: YtDlp,
    recursive: bool,
    skip_existing: bool,
    sample: usize,
}

/// Configures a [`Tafrigh`] run; created by [`Tafrigh::builder`].
#[derive(Debug)]
pub struct TafrighBuilder {
    backend: Backend,
    output: OutputWriter,
    output_mode: OutputMode,
    ytdlp: Option<YtDlp>,
    recursive: bool,
    skip_existing: bool,
    sample: usize,
}

/// A run has exactly one backend. Caption runs never construct or invoke an audio transcriber.
enum Backend {
    Media(Arc<dyn Transcriber>),
    Captions(Arc<dyn CaptionProvider>),
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Media(_) => "Media",
            Self::Captions(_) => "Captions",
        })
    }
}

/// A download started ahead of its item. Dropping it cancels the download.
struct PrefetchedDownload(tokio::task::JoinHandle<Result<PathBuf>>);

impl PrefetchedDownload {
    async fn wait(mut self) -> Result<PathBuf> {
        task_result((&mut self.0).await)
    }
}

impl Drop for PrefetchedDownload {
    fn drop(&mut self) {
        // Does nothing once the download has finished.
        self.0.abort();
    }
}

impl TafrighBuilder {
    /// Where and how transcripts are written. Defaults to all formats in the current directory.
    #[must_use]
    pub fn output(mut self, output: OutputWriter) -> Self {
        self.output = output;
        self
    }

    /// How transcripts are placed. Defaults to [`OutputMode::Flat`].
    /// In side-by-side mode the writer's directory is ignored, and `sample.csv` goes in
    /// the current directory. Tree mode separates multiple directory inputs by root name.
    #[must_use]
    pub fn output_mode(mut self, mode: OutputMode) -> Self {
        self.output_mode = mode;
        self
    }

    /// URL handling. Defaults to `yt-dlp` downloading into the output directory.
    #[must_use]
    pub fn ytdlp(mut self, ytdlp: YtDlp) -> Self {
        self.ytdlp = Some(ytdlp);
        self
    }

    /// Include nested directories when resolving local inputs. Defaults to false.
    /// Directory symlinks and a nested output directory are not traversed.
    #[must_use]
    pub fn recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }

    /// Skip items when every requested output file already exists.
    #[must_use]
    pub fn skip_existing(mut self, skip: bool) -> Self {
        self.skip_existing = skip;
        self
    }

    /// Write `sample.csv` with this many random segments from the run (0 disables).
    /// Use a numeric suffix when its path belongs to a transcript or directory.
    #[must_use]
    pub fn sample(mut self, count: usize) -> Self {
        self.sample = count;
        self
    }

    /// Finishes configuration.
    pub fn build(self) -> Tafrigh {
        let ytdlp = self.ytdlp.unwrap_or_else(|| {
            YtDlp::new(if self.output_mode == OutputMode::SideBySide {
                std::path::Path::new(".")
            } else {
                self.output.dir()
            })
        });
        Tafrigh {
            backend: self.backend,
            output: self.output,
            output_mode: self.output_mode,
            ytdlp,
            recursive: self.recursive,
            skip_existing: self.skip_existing,
            sample: self.sample,
        }
    }
}

/// Outcome of a run.
#[derive(Debug, Clone, Default)]
pub struct RunReport {
    /// Every item that was attempted, in order.
    pub items: Vec<ItemReport>,
    /// Inputs that could not be resolved: `(input, error)`.
    pub failed_inputs: Vec<(String, String)>,
}

impl RunReport {
    /// Failed inputs plus failed items.
    pub fn failures(&self) -> usize {
        self.failed_inputs.len()
            + self
                .items
                .iter()
                .filter(|item| matches!(item.status, ItemStatus::Failed(_)))
                .count()
    }
}

/// Outcome for one item.
#[derive(Debug, Clone)]
pub struct ItemReport {
    /// The item.
    pub item: ItemInfo,
    /// What happened to it.
    pub status: ItemStatus,
}

/// What happened to an item.
#[derive(Debug, Clone)]
pub enum ItemStatus {
    /// Transcribed and written.
    Transcribed {
        /// Number of segments before compaction.
        segments: usize,
        /// Files written.
        outputs: Vec<PathBuf>,
    },
    /// Skipped because its outputs already existed.
    Skipped,
    /// Failed with this error.
    Failed(String),
}

impl Tafrigh {
    /// Starts configuring a run with `transcriber` (which may be an `Arc<dyn Transcriber>`).
    pub fn builder(transcriber: impl Transcriber + 'static) -> TafrighBuilder {
        Self::with_backend(Backend::Media(Arc::new(transcriber)))
    }

    /// Configures a run that retrieves existing captions from URLs through `provider`.
    /// Caption runs share output handling with media transcription and never download audio.
    pub fn captions(provider: impl CaptionProvider + 'static) -> TafrighBuilder {
        Self::with_backend(Backend::Captions(Arc::new(provider)))
    }

    fn with_backend(backend: Backend) -> TafrighBuilder {
        TafrighBuilder {
            backend,
            output: OutputWriter::new("."),
            output_mode: OutputMode::default(),
            ytdlp: None,
            recursive: false,
            skip_existing: false,
            sample: 0,
        }
    }

    /// Resolves the media items behind an input.
    pub async fn resolve(&self, input: &Input) -> Result<Vec<MediaItem>> {
        if let Backend::Captions(provider) = &self.backend {
            let Input::Url(url) = input else {
                return Err(Error::Source("caption providers require URL inputs".into()));
            };
            return provider.resolve(url, &self.ytdlp).await;
        }
        match input {
            Input::Path(path) => {
                let path = path.clone();
                let recursive = self.recursive;
                let excluded = (self.output_mode != OutputMode::SideBySide)
                    .then(|| self.output.dir().to_owned());
                task_result(
                    tokio::task::spawn_blocking(move || {
                        discover_local(&path, recursive, excluded.as_deref())
                    })
                    .await,
                )
            }
            Input::Url(url) => self.ytdlp.resolve(url).await,
        }
    }

    /// Processes all inputs in order.
    /// Inputs are resolved before transcription so mirrored directories take precedence
    /// over conflicting transcript file names.
    ///
    /// The next item starts transcribing once the current one has sent all of its audio, so its
    /// requests use the capacity the current one no longer needs while that one waits for its
    /// last answers. At most two items transcribe at once. Their progress events interleave, but
    /// transcripts are written, and finished, failed and skipped items reported, in input order.
    ///
    /// Input and item failures are reported, not returned. The error case is reserved for
    /// problems with the run itself, such as [`Error::ToolUnavailable`]: a missing `ffmpeg` or
    /// `yt-dlp` stops the run instead of failing every item.
    pub async fn run(
        &self,
        inputs: impl IntoIterator<Item = Input>,
        events: &dyn Progress<Event>,
    ) -> Result<RunReport> {
        let mut report = RunReport::default();
        let inputs: Vec<_> = inputs.into_iter().collect();
        let mut queue = self.prepare_queue(&inputs, events, &mut report).await?;
        let mut samples = SampleReservoir::new(self.sample);

        let mut running = FuturesUnordered::new();
        // Items finished out of order wait here until those before them are reported.
        let mut finished = BTreeMap::new();
        let (mut started, mut reported) = (0, 0);
        // Items transcribing; the newest one's signal fires once it has sent all its audio.
        let mut transcribing = 0;
        let mut newest: Option<Arc<Notify>> = None;
        let mut prefetched = None;
        let mut exhausted = false;
        loop {
            if !exhausted && newest.is_none() && transcribing < MAX_TRANSCRIBING {
                match queue.items.pop_front() {
                    Some(next) => {
                        let work = self.start_item(next, &queue, events, &mut prefetched);
                        let active = work.sent.is_some();
                        if let Some(sent) = work.sent {
                            transcribing += 1;
                            newest = Some(sent);
                        }
                        let (order, done) = (started, work.done);
                        running.push(async move { (order, active, done.await) });
                        started += 1;
                        continue;
                    }
                    None => exhausted = true,
                }
            }
            let signal = newest.clone();
            tokio::select! {
                Some((order, active, done)) = running.next() => {
                    let done: Done = done;
                    if active {
                        transcribing -= 1;
                    }
                    finished.insert(order, done);
                    while let Some(done) = finished.remove(&reported) {
                        reported += 1;
                        report.items.push(self.finish_item(done, events, &mut samples)?);
                    }
                }
                () = async {
                    match signal {
                        Some(sent) => sent.notified().await,
                        None => std::future::pending().await,
                    }
                }, if newest.is_some() => newest = None,
                else => break,
            }
        }

        if self.sample > 0 {
            write_sample(
                &queue.layout.lock().unwrap().sample_path()?,
                samples.rows(),
                self.sample,
            )?;
        }
        Ok(report)
    }

    /// Resolve inputs and reserve every known parent directory before choosing file names.
    async fn prepare_queue(
        &self,
        inputs: &[Input],
        events: &dyn Progress<Event>,
        report: &mut RunReport,
    ) -> Result<Queue> {
        let mut layout = Layout::new(self.output_mode, self.output.clone(), inputs);
        let mut resolved = Vec::new();
        for (index, input) in inputs.iter().enumerate() {
            let label = input.to_string();
            events.report(Event::InputResolving {
                input: label.clone(),
            });
            let items = match self.resolve(input).await {
                Ok(items) => items,
                Err(err @ Error::ToolUnavailable { .. }) => return Err(err),
                Err(err) => {
                    let error = ErrorChain(&err).to_string();
                    events.report(Event::InputFailed {
                        input: label.clone(),
                        error: error.clone(),
                    });
                    report.failed_inputs.push((label, error));
                    continue;
                }
            };
            events.report(Event::InputResolved {
                input: label,
                items: items.len(),
            });
            let total = items.len();
            for (position, item) in items.into_iter().enumerate() {
                let directory = if matches!(self.backend, Backend::Captions(_))
                    && self.output_mode == OutputMode::SideBySide
                {
                    Some(self.ytdlp.download_dir().to_owned())
                } else {
                    layout.directory(&item, index)
                };
                let directory = directory.map(|dir| layout.require_directory(&dir));
                resolved.push((item, directory, position + 1, total));
            }
        }
        let mut used_ids = HashSet::<Arc<str>>::new();
        let items = resolved
            .into_iter()
            .map(|(item, directory, position, total)| {
                let info = ItemInfo {
                    id: unique_name(&item.id, |id| used_ids.insert(Arc::from(id))).into(),
                    title: item.title.clone(),
                };
                let destination = match directory {
                    None => Destination::AfterDownload,
                    Some(Ok(dir)) => layout
                        .plan(&item, &dir)
                        .map_or_else(Destination::Unusable, Destination::Planned),
                    Some(Err(err)) => Destination::Unusable(err),
                };
                Queued {
                    item,
                    info,
                    destination,
                    position,
                    total,
                }
            })
            .collect();
        Ok(Queue {
            items,
            layout: Arc::new(Mutex::new(layout)),
        })
    }

    /// Reports `next` as started and returns its work, which downloads and transcribes it
    /// (or does nothing when its outputs exist). Also starts downloading the item after it.
    fn start_item<'a>(
        &'a self,
        next: Queued,
        queue: &Queue,
        events: &'a dyn Progress<Event>,
        prefetched: &mut Option<(Arc<str>, PrefetchedDownload)>,
    ) -> Started<'a> {
        events.report(Event::ItemStarted {
            item: next.info.clone(),
            position: next.position,
            total: next.total,
        });
        let download = prefetched.take().and_then(|(id, download)| {
            // A prefetch for any other item is dropped, which cancels it.
            (id == next.info.id).then_some(download)
        });
        if let Some(following) = queue.items.front() {
            *prefetched = self
                .prefetch_download(following)
                .map(|download| (following.info.id.clone(), download));
        }
        let Queued {
            item,
            info,
            destination,
            ..
        } = next;
        // Items that cannot start finish at once, without taking a transcription slot.
        let outcome = match destination {
            Destination::Unusable(error) => Outcome::Failed(error),
            Destination::Planned(target) if self.skip_existing && target.exists() => {
                Outcome::Skipped
            }
            destination => {
                let sent = Arc::new(Notify::new());
                let signal = sent.clone();
                let layout = queue.layout.clone();
                let done = Box::pin(async move {
                    let link = item.link.clone();
                    let outcome = self
                        .work(item, &info, destination, &layout, events, download, &signal)
                        .await
                        .unwrap_or_else(Outcome::Failed);
                    // Also when it failed before sending everything: the next item may start.
                    signal.notify_one();
                    Done {
                        info,
                        link,
                        outcome,
                    }
                });
                return Started {
                    done,
                    sent: Some(sent),
                };
            }
        };
        Started::finished(Done {
            info,
            link: item.link,
            outcome,
        })
    }

    /// Downloads and transcribes an item that has started.
    #[expect(
        clippy::too_many_arguments,
        reason = "the item's own state plus the run's shared handles"
    )]
    async fn work(
        &self,
        mut item: MediaItem,
        info: &ItemInfo,
        destination: Destination,
        layout: &Mutex<Layout>,
        events: &dyn Progress<Event>,
        mut download: Option<PrefetchedDownload>,
        sent: &Notify,
    ) -> Result<Outcome> {
        let target = match destination {
            Destination::Planned(target) => target,
            // Side-by-side URLs follow yt-dlp's actual path, including custom output
            // templates. Plan before transcription, so later items claim their names in
            // input order.
            Destination::AfterDownload => {
                let path = self
                    .media_path(&item, info, events, download.take())
                    .await?;
                let target = layout.lock().unwrap().downloaded(&path)?;
                item.media = Media::File(path);
                if self.skip_existing && target.exists() {
                    return Ok(Outcome::Skipped);
                }
                target
            }
            Destination::Unusable(err) => return Err(err),
        };
        let (media, segments) = self
            .transcribe_item(&item, info, events, download, sent)
            .await?;
        Ok(Outcome::Transcribed(Transcript {
            target,
            media,
            segments,
        }))
    }

    fn prefetch_download(&self, next: &Queued) -> Option<PrefetchedDownload> {
        let needed = match &next.destination {
            Destination::Planned(target) => !(self.skip_existing && target.exists()),
            Destination::AfterDownload => true,
            Destination::Unusable(_) => false,
        };
        if !needed
            || matches!(self.backend, Backend::Captions(_))
            || !matches!(next.item.media, Media::Remote { .. })
        {
            return None;
        }
        let ytdlp = self.ytdlp.clone();
        let item = next.item.clone();
        Some(PrefetchedDownload(tokio::spawn(async move {
            ytdlp.download(&item).await
        })))
    }

    /// Downloads if needed and transcribes one item; `sent` fires once all its audio has
    /// gone to the recognizer.
    async fn transcribe_item(
        &self,
        item: &MediaItem,
        info: &ItemInfo,
        events: &dyn Progress<Event>,
        download: Option<PrefetchedDownload>,
        sent: &Notify,
    ) -> Result<(Option<PathBuf>, Vec<Segment>)> {
        let transcriber = match &self.backend {
            Backend::Captions(provider) => {
                events.report(Event::FetchingCaptions { item: info.clone() });
                let segments = provider.fetch(item, &self.ytdlp).await?;
                return Ok((None, segments));
            }
            Backend::Media(transcriber) => transcriber,
        };
        let path = self.media_path(item, info, events, download).await?;
        let progress = |event: TranscribeEvent| {
            if matches!(event, TranscribeEvent::Segmented { .. }) {
                sent.notify_one();
            }
            events.report(Event::Transcribing {
                item: info.clone(),
                event,
            });
        };
        let segments = transcriber.transcribe(&path, &progress).await?;
        Ok((Some(path), segments))
    }

    async fn media_path(
        &self,
        item: &MediaItem,
        info: &ItemInfo,
        events: &dyn Progress<Event>,
        download: Option<PrefetchedDownload>,
    ) -> Result<PathBuf> {
        Ok(match &item.media {
            Media::File(path) => path.clone(),
            Media::Remote { .. } => {
                events.report(Event::Downloading { item: info.clone() });
                match download {
                    Some(download) => download.wait().await?,
                    None => self.ytdlp.download(item).await?,
                }
            }
        })
    }

    /// Writes a transcribed item and reports how it went; only errors that should stop the
    /// whole run are returned.
    fn finish_item(
        &self,
        done: Done,
        events: &dyn Progress<Event>,
        samples: &mut SampleReservoir,
    ) -> Result<ItemReport> {
        let Done {
            info,
            link,
            outcome,
        } = done;
        let written = match outcome {
            Outcome::Skipped => {
                events.report(Event::ItemSkipped {
                    item: info.clone(),
                    reason: "outputs exist".into(),
                });
                return Ok(ItemReport {
                    item: info,
                    status: ItemStatus::Skipped,
                });
            }
            Outcome::Transcribed(transcript) => {
                let segments = transcript.segments.len();
                self.write_item(&info, link.as_deref(), transcript, samples)
                    .map(|outputs| (segments, outputs))
            }
            Outcome::Failed(err) => Err(err),
        };
        let status = match written {
            Ok((segments, outputs)) => {
                events.report(Event::ItemFinished {
                    item: info.clone(),
                    segments,
                    outputs: outputs.clone(),
                });
                ItemStatus::Transcribed { segments, outputs }
            }
            Err(err @ Error::ToolUnavailable { .. }) => return Err(err),
            Err(err) => {
                let error = ErrorChain(&err).to_string();
                events.report(Event::ItemFailed {
                    item: info.clone(),
                    error: error.clone(),
                });
                ItemStatus::Failed(error)
            }
        };
        Ok(ItemReport { item: info, status })
    }

    /// Writes an item's transcripts and adds its segments to the sample; returns the files.
    fn write_item(
        &self,
        info: &ItemInfo,
        link: Option<&str>,
        transcript: Transcript,
        samples: &mut SampleReservoir,
    ) -> Result<Vec<PathBuf>> {
        let Transcript {
            target,
            media,
            segments,
        } = transcript;
        if target.writer.needs_words()
            && !segments.is_empty()
            && segments.iter().all(|s| s.words.is_empty())
        {
            tracing::warn!(
                "{}: the recognizer returned no word timings; word-level files will be empty",
                info.id
            );
        }
        let written = target.write(&segments)?;
        if self.sample > 0 {
            for segment in written.segments {
                samples.push(SampleRow {
                    segment,
                    item: info.id.to_string(),
                    link: link.map(str::to_owned),
                    file: media.clone(),
                });
            }
        }
        Ok(written.files)
    }
}

/// Items that may transcribe at once.
const MAX_TRANSCRIBING: usize = 2;

/// Planned items not started yet.
struct Queue {
    items: VecDeque<Queued>,
    layout: Arc<Mutex<Layout>>,
}

/// An item waiting to start, with its place in its input.
struct Queued {
    item: MediaItem,
    info: ItemInfo,
    destination: Destination,
    position: usize,
    total: usize,
}

/// Where an item's transcripts go.
enum Destination {
    /// Chosen while planning the run.
    Planned(Target),
    /// Beside the downloaded file of a side-by-side URL; chosen once it is downloaded.
    AfterDownload,
    /// Could not be chosen; the item fails without being transcribed.
    Unusable(Error),
}

/// An item's work, and for one that transcribes, the signal that it has sent all its audio.
struct Started<'a> {
    done: Pin<Box<dyn Future<Output = Done> + Send + 'a>>,
    sent: Option<Arc<Notify>>,
}

impl Started<'_> {
    /// Work that is already over and takes no transcription slot.
    fn finished(done: Done) -> Self {
        Self {
            done: Box::pin(std::future::ready(done)),
            sent: None,
        }
    }
}

/// An item whose work is over, waiting for its turn to be written and reported.
struct Done {
    info: ItemInfo,
    /// Link to the original, for the sample.
    link: Option<String>,
    outcome: Outcome,
}

enum Outcome {
    Skipped,
    Transcribed(Transcript),
    Failed(Error),
}

/// What an item produced, and where it goes.
struct Transcript {
    target: Target,
    /// The media file transcribed; `None` for captions.
    media: Option<PathBuf>,
    segments: Vec<Segment>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::Duration;

    use super::*;
    use crate::NoProgress;

    /// A transcriber that always fails with the error its function builds.
    struct Failing(fn() -> Error);

    #[async_trait::async_trait]
    impl Transcriber for Failing {
        async fn transcribe(
            &self,
            _media: &Path,
            _progress: &dyn Progress<TranscribeEvent>,
        ) -> Result<Vec<Segment>> {
            Err((self.0)())
        }
    }

    #[cfg(unix)]
    struct RecordingTranscriber {
        log: std::path::PathBuf,
    }

    #[cfg(unix)]
    #[async_trait::async_trait]
    impl Transcriber for RecordingTranscriber {
        async fn transcribe(
            &self,
            media: &Path,
            _progress: &dyn Progress<TranscribeEvent>,
        ) -> Result<Vec<Segment>> {
            use std::io::Write;

            let id = media.file_stem().and_then(|stem| stem.to_str()).unwrap();
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log)
                .unwrap();
            writeln!(log, "transcribe start {id}").unwrap();
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(250)))
                .await
                .unwrap();
            let mut log = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.log)
                .unwrap();
            writeln!(log, "transcribe end {id}").unwrap();
            Ok(vec![Segment::new(0.0, 1.0, id)])
        }
    }

    #[tokio::test]
    async fn missing_tools_stop_the_run_while_other_errors_fail_the_item() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let media = dir.join("a.mp3");
        std::fs::write(&media, b"").unwrap();
        let output = OutputWriter::new(dir.join("out"));

        let broken_file = Tafrigh::builder(Failing(|| Error::Decode {
            path: "a.mp3".into(),
            message: "Invalid data found when processing input".into(),
        }))
        .output(output.clone())
        .build();
        let report = broken_file
            .run([Input::Path(media.clone())], &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 1);

        let no_ffmpeg = Tafrigh::builder(Failing(|| {
            Error::tool(
                "ffmpeg",
                Path::new("ffmpeg"),
                std::io::ErrorKind::NotFound.into(),
            )
        }))
        .output(output)
        .build();
        let err = no_ffmpeg
            .run([Input::Path(media)], &NoProgress)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ToolUnavailable { tool: "ffmpeg", .. }),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prefetches_the_next_remote_download_during_transcription() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let log = dir.join("events.log");
        let script = dir.join("yt-dlp.sh");
        let script_body = r#"#!/bin/sh
if [ "$1" = "--flat-playlist" ]; then
  printf '%s\n' '{"entries":[{"id":"item1","title":"One","webpage_url":"https://example.com/item1"},{"id":"item2","title":"Two","webpage_url":"https://example.com/item2"}]}'
  exit 0
fi
template=
previous=
for arg in "$@"; do
  if [ "$previous" = "-o" ]; then template=$arg; fi
  previous=$arg
done
url=
for arg in "$@"; do url=$arg; done
case "$url" in
  *item1*) id=item1 ;;
  *item2*) id=item2 ;;
  *) exit 1 ;;
esac
path=$(printf '%s' "$template" | sed 's/%(id)s/'"$id"'/g; s/%(ext)s/wav/g')
mkdir -p "$(dirname "$path")"
printf '%s download start %s\n' "$(date +%s)" "$id" >> "__LOG__"
printf 'audio' > "$path"
printf '%s\n' "$path"
"#
        .replace("__LOG__", log.to_str().unwrap());
        std::fs::write(&script, script_body).unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let output =
            OutputWriter::new(dir.join("outputs")).formats([crate::output::BuiltinFormat::Txt]);
        let transcriber = RecordingTranscriber { log: log.clone() };
        let tafrigh = Tafrigh::builder(transcriber)
            .output(output)
            .ytdlp(YtDlp::new(dir.join("downloads")).program(&script))
            .build();
        let events = Mutex::new(Vec::new());
        let collect = |event: Event| events.lock().unwrap().push(event);
        let report = tafrigh
            .run(
                [Input::Url("https://example.com/playlist".into())],
                &collect,
            )
            .await
            .unwrap();
        assert_eq!(report.items.len(), 2);
        assert!(
            report
                .items
                .iter()
                .all(|item| matches!(item.status, ItemStatus::Transcribed { .. }))
        );

        let log_text = std::fs::read_to_string(&log).unwrap();
        let item2_download = log_text.find("download start item2").unwrap();
        let item1_end = log_text.find("transcribe end item1").unwrap();
        assert!(item2_download < item1_end, "{log_text}");

        let item_events: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                Event::ItemStarted { item, .. } => Some(("started", item.id.to_string())),
                Event::Downloading { item } => Some(("downloading", item.id.to_string())),
                Event::ItemFinished { item, .. } => Some(("finished", item.id.to_string())),
                _ => None,
            })
            .collect();
        assert_eq!(
            item_events,
            vec![
                ("started", "item1".into()),
                ("downloading", "item1".into()),
                ("finished", "item1".into()),
                ("started", "item2".into()),
                ("downloading", "item2".into()),
                ("finished", "item2".into()),
            ]
        );
    }

    /// Reports that all audio was sent, then takes as many milliseconds as the number after
    /// `_` in the file name to finish.
    #[derive(Default)]
    struct Draining {
        log: Mutex<Vec<String>>,
        active: AtomicUsize,
        peak: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Transcriber for Draining {
        async fn transcribe(
            &self,
            media: &Path,
            progress: &dyn Progress<TranscribeEvent>,
        ) -> Result<Vec<Segment>> {
            let id = media.file_stem().unwrap().to_string_lossy().into_owned();
            let millis: u64 = id.split('_').nth(1).unwrap().parse().unwrap();
            let now = self.active.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(now, SeqCst);
            self.log.lock().unwrap().push(format!("start {id}"));
            progress.report(TranscribeEvent::Segmented { chunks: 1 });
            tokio::time::sleep(Duration::from_millis(millis)).await;
            self.log.lock().unwrap().push(format!("end {id}"));
            self.active.fetch_sub(1, SeqCst);
            Ok(vec![Segment::new(0.0, 1.0, &id)])
        }
    }

    #[tokio::test]
    async fn next_items_start_while_one_waits_for_its_last_answers() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let inputs: Vec<Input> = ["a_300", "b_50", "c_50", "d_50"]
            .into_iter()
            .map(|name| {
                let path = dir.join(format!("{name}.mp3"));
                std::fs::write(&path, b"").unwrap();
                Input::Path(path)
            })
            .collect();
        let transcriber = Arc::new(Draining::default());
        let tafrigh = Tafrigh::builder(transcriber.clone())
            .output(OutputWriter::new(dir.join("out")).formats([crate::output::BuiltinFormat::Txt]))
            .build();
        let finished = Mutex::new(Vec::new());
        let collect = |event: Event| {
            if let Event::ItemFinished { item, .. } = event {
                finished.lock().unwrap().push(item.id.to_string());
            }
        };
        let report = tafrigh.run(inputs, &collect).await.unwrap();

        // The quick items run one after another while the first one finishes; never more
        // than two at once.
        assert_eq!(
            *transcriber.log.lock().unwrap(),
            [
                "start a_300",
                "start b_50",
                "end b_50",
                "start c_50",
                "end c_50",
                "start d_50",
                "end d_50",
                "end a_300"
            ]
        );
        assert_eq!(transcriber.peak.load(SeqCst), 2);
        // Results still come in input order.
        let order = ["a_300", "b_50", "c_50", "d_50"];
        assert_eq!(*finished.lock().unwrap(), order);
        let ids: Vec<&str> = report.items.iter().map(|item| &*item.item.id).collect();
        assert_eq!(ids, order);
        assert!(dir.join("out/d_50.txt").exists());
    }

    #[tokio::test]
    async fn skipped_items_keep_their_place_and_are_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let names = ["a_200", "b_10", "c_10", "d_10"];
        // b and d were transcribed by an earlier run.
        for done in ["b_10", "d_10"] {
            std::fs::write(out.join(format!("{done}.txt")), "earlier run").unwrap();
        }
        let inputs: Vec<Input> = names
            .into_iter()
            .map(|name| {
                let path = dir.join(format!("{name}.mp3"));
                std::fs::write(&path, b"").unwrap();
                Input::Path(path)
            })
            .collect();
        let transcriber = Arc::new(Draining::default());
        let tafrigh = Tafrigh::builder(transcriber.clone())
            .output(OutputWriter::new(&out).formats([crate::output::BuiltinFormat::Txt]))
            .skip_existing(true)
            .build();
        let outcomes = Mutex::new(Vec::new());
        let collect = |event: Event| match event {
            Event::ItemFinished { item, .. } => {
                outcomes
                    .lock()
                    .unwrap()
                    .push(format!("finished {}", item.id));
            }
            Event::ItemSkipped { item, .. } => {
                outcomes
                    .lock()
                    .unwrap()
                    .push(format!("skipped {}", item.id));
            }
            _ => {}
        };
        let report = tafrigh.run(inputs, &collect).await.unwrap();

        // c starts while a finishes; skipped items never reach the transcriber.
        assert_eq!(
            *transcriber.log.lock().unwrap(),
            ["start a_200", "start c_10", "end c_10", "end a_200"]
        );
        assert_eq!(
            *outcomes.lock().unwrap(),
            [
                "finished a_200",
                "skipped b_10",
                "finished c_10",
                "skipped d_10"
            ]
        );
        let statuses: Vec<(&str, bool)> = report
            .items
            .iter()
            .map(|item| (&*item.item.id, matches!(item.status, ItemStatus::Skipped)))
            .collect();
        assert_eq!(
            statuses,
            [
                ("a_200", false),
                ("b_10", true),
                ("c_10", false),
                ("d_10", true)
            ]
        );
        assert_eq!(
            std::fs::read_to_string(out.join("b_10.txt")).unwrap(),
            "earlier run"
        );
        assert!(out.join("c_10.txt").exists());
    }
}
