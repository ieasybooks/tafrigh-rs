use std::path::Path;
use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use tafrigh::audio::{AudioChunk, SAMPLE_RATE};
use tafrigh::{BoxError, ChunkRecognizer, Recognition};
use tokio::sync::{mpsc, oneshot};
use transcribe_cpp::{
    Backend, DeviceType, Model, ModelOptions, RunOptions, Session, SessionOptions,
};

use crate::{CohereConfig, CohereError, Device, DownloadProgress};

/// One sample at 16 kHz, in nanoseconds.
const SAMPLE_NANOS: u64 = 1_000_000_000 / SAMPLE_RATE as u64;
/// The longest audio the model accepts per call, padding included: one sample under 400 s.
/// transcribe.cpp advertises 400 s, but its mel frontend adds a centered frame, so exactly
/// 400 s needs one encoder frame more than the model's 5,000.
pub const MAX_AUDIO_DURATION: Duration = Duration::from_nanos(400_000_000_000 - SAMPLE_NANOS);
/// The fewest samples the model accepts: its mel frontend needs two frames, one per 160-sample
/// hop plus the centered one. A shorter chunk lasts under 10 ms, too short for any speech.
const MIN_SAMPLES: usize = 160;
static BACKENDS: OnceLock<Result<(), String>> = OnceLock::new();
static LOGGING: Once = Once::new();

/// A loaded Cohere model. Clones share the same bounded inference queue and native session.
#[derive(Debug, Clone)]
pub struct CohereRecognizer {
    config: CohereConfig,
    backend: String,
    device_name: String,
    max_audio_duration: Duration,
    /// `max_audio_duration` in samples, so chunks are checked exactly.
    max_samples: usize,
    batch_size: usize,
    requests: mpsc::Sender<Request>,
}

struct Request {
    pcm: Vec<f32>,
    /// Where the chunk is, for warnings: its index and source start and end in seconds.
    chunk: (usize, f64, f64),
    reply: oneshot::Sender<Result<Recognition, CohereError>>,
}

impl CohereRecognizer {
    /// Resolves the model, downloading a variant if needed, and opens a session on a blocking thread.
    ///
    /// Requires a Tokio runtime. `auto` tries a GPU and retries loading on CPU if necessary;
    /// `gpu` and `gpu:N` fail if the requested GPU is unavailable or cannot load the model.
    pub async fn load(config: CohereConfig) -> Result<Self, CohereError> {
        Self::load_with_progress(config, |_| {}).await
    }

    /// Like [`Self::load`], reporting download byte counts to a callback.
    ///
    /// Local files and already cached variants do not invoke the callback.
    pub async fn load_with_progress(
        config: CohereConfig,
        progress: impl Fn(DownloadProgress) + Send + Sync,
    ) -> Result<Self, CohereError> {
        config.validate()?;
        let path = config
            .model
            .resolve(config.model_dir.as_deref(), &progress)
            .await?;
        let settings = config.clone();
        let session = tokio::task::spawn_blocking(move || load_session(&settings, &path)).await??;
        let model = session.model();
        let backend = model.backend();
        let device_name = model.device()?.description;
        let limits = session.limits()?;
        let limit_ms = limits.effective_max_audio_ms;
        let max_audio_duration = u64::try_from(limit_ms)
            .ok()
            .filter(|&limit| limit > 0)
            .map_or(MAX_AUDIO_DURATION, |limit| {
                // transcribe.cpp rejects exactly the limit it reports; stay one sample under.
                Duration::from_millis(limit)
                    .saturating_sub(Duration::from_nanos(SAMPLE_NANOS))
                    .min(MAX_AUDIO_DURATION)
            });
        let batch_size = if is_gpu(&model.device()?) {
            config.batch_size
        } else {
            1
        };
        tracing::debug!(
            backend,
            device = %device_name,
            ?limits,
            batch_size,
            "opened Cohere session"
        );
        let (requests, receiver) = mpsc::channel(batch_size);
        let options = RunOptions {
            language: Some(config.language.code().into()),
            timestamps: transcribe_cpp::TimestampKind::None,
            // Cohere hard-codes the <|pnc|> prompt; it has no PNC toggle.
            ..RunOptions::default()
        };
        tokio::spawn(worker(
            session,
            receiver,
            options,
            batch_size,
            config.batch_wait,
        ));
        Ok(Self {
            config,
            backend,
            device_name,
            max_audio_duration,
            max_samples: samples_in(max_audio_duration),
            batch_size,
            requests,
        })
    }

    /// The settings used to load this model.
    pub fn config(&self) -> &CohereConfig {
        &self.config
    }

    /// Native name of the actual compute backend, such as `CPU`, `CUDA0` or `Metal`.
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Description of the device that owns the model weights.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

fn is_gpu(device: &transcribe_cpp::Device) -> bool {
    matches!(device.device_type, DeviceType::Gpu | DeviceType::Igpu)
}

fn load_session(config: &CohereConfig, path: &Path) -> Result<Session, CohereError> {
    // Validate the file before initializing native backends or allocating weights.
    if !std::fs::metadata(path)?.is_file() {
        return Err(CohereError::Config(
            "the model path is not a regular file".into(),
        ));
    }
    // Native and ggml messages go to the `log` crate (target `transcribe_cpp`) instead of
    // stderr, so applications decide what to show.
    LOGGING.call_once(transcribe_cpp::init_logging);
    // Backend registration mutates process-global native state and must precede enumeration.
    BACKENDS
        .get_or_init(|| transcribe_cpp::init_backends_default().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| CohereError::Config(error.clone()))?;
    let gpu = match config.device {
        Device::Cpu => None,
        Device::Auto | Device::Gpu | Device::GpuIndex(_) => {
            let index = match config.device {
                Device::GpuIndex(index) => index,
                _ => 0,
            };
            let selected = transcribe_cpp::devices()
                .into_iter()
                .filter(is_gpu)
                .nth(index);
            if selected.is_none() && config.device != Device::Auto {
                return Err(CohereError::Config(format!(
                    "GPU {index} is unavailable; use a GPU-enabled build and a working driver, or select cpu"
                )));
            }
            selected
        }
    };
    if let Some(gpu) = gpu {
        match open_session(
            config,
            path,
            &ModelOptions {
                backend: Backend::Auto,
                device: Some(gpu),
                ..ModelOptions::default()
            },
        ) {
            Ok(session) => return Ok(session),
            Err(error) if config.device == Device::Auto => {
                tracing::warn!(%error, "Cohere GPU loading failed; retrying on CPU");
            }
            Err(error) => return Err(error),
        }
    }
    open_session(
        config,
        path,
        &ModelOptions {
            backend: Backend::Cpu,
            device: None,
            ..ModelOptions::default()
        },
    )
}

fn open_session(
    config: &CohereConfig,
    path: &Path,
    options: &ModelOptions,
) -> Result<Session, CohereError> {
    let model = Model::load_with(path, options)?;
    if model.arch() != "cohere_asr" {
        return Err(CohereError::Config(format!(
            "expected a Cohere GGUF model, found architecture {:?}",
            model.arch()
        )));
    }
    let capabilities = model.capabilities()?;
    if capabilities.native_sample_rate != SAMPLE_RATE as i32 {
        return Err(CohereError::Config(
            "the model must accept 16 kHz PCM".into(),
        ));
    }
    if !capabilities
        .languages
        .iter()
        .any(|language| language == config.language.code())
    {
        return Err(CohereError::Config(format!(
            "the model does not support language {}",
            config.language
        )));
    }
    let threads = match config.threads {
        0 if options.device.is_some() => gpu_threads(),
        threads => threads,
    };
    Ok(model.session_with(&SessionOptions {
        n_threads: threads as i32,
        ..SessionOptions::default()
    })?)
}

/// CPU threads for a model on a GPU: every core but two. Decoding and Silero VAD run alongside
/// the model; on a 6-core machine, transcribe.cpp's default of every core was about 15% slower
/// than four threads.
fn gpu_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |cores| cores.get().saturating_sub(2).max(1))
}

#[async_trait]
impl ChunkRecognizer for CohereRecognizer {
    async fn recognize(&self, chunk: &AudioChunk) -> Result<Recognition, BoxError> {
        if chunk.samples.len() > self.max_samples {
            return Err(Box::new(CohereError::Config(format!(
                "audio including padding exceeds the Cohere limit of {:?}",
                self.max_audio_duration
            ))));
        }
        // The model rejects such short audio, which can't hold speech anyway. Treating it as
        // silence keeps a short last chunk, such as the end of fixed windows without padding,
        // from failing the whole recording.
        if chunk.samples.len() < MIN_SAMPLES {
            return Ok(Recognition::default());
        }
        // Wait for queue capacity before allocating another float PCM buffer.
        // Concurrent files can otherwise retain prepared buffers outside the queue.
        let permit = self
            .requests
            .reserve()
            .await
            .map_err(|_| CohereError::WorkerStopped)?;
        let (reply, mut result) = oneshot::channel();
        permit.send(Request {
            pcm: pcm_f32(&chunk.samples),
            chunk: (chunk.index, chunk.start, chunk.end),
            reply,
        });
        // A reserved permit can enqueue after the worker has dropped its receiver.
        // In that case the queued reply sender stays alive, but nobody can answer it.
        let answer = tokio::select! {
            biased;
            answer = &mut result => answer.map_err(|_| CohereError::WorkerStopped)?,
            () = self.requests.closed() => {
                // A reply may arrive between polling `result` and observing closure.
                result.try_recv().map_err(|_| CohereError::WorkerStopped)?
            }
        };
        Ok(answer?)
    }

    fn word_timings(&self) -> bool {
        false
    }

    fn max_audio_duration(&self) -> Option<Duration> {
        Some(self.max_audio_duration)
    }

    fn max_in_flight(&self) -> usize {
        // Keep one batch ready while the session works on the current one.
        // Reserving before conversion bounds request float PCM to two batches across all files.
        // Each file also retains its in-flight i16 chunks and the core's producer queue.
        2 * self.batch_size
    }
}

/// Whole samples in `duration`.
fn samples_in(duration: Duration) -> usize {
    usize::try_from(duration.as_nanos() / u128::from(SAMPLE_NANOS)).unwrap_or(usize::MAX)
}

fn pcm_f32(samples: &[i16]) -> Vec<f32> {
    #[cfg(test)]
    tests::converting_pcm();
    samples
        .iter()
        .map(|&sample| f32::from(sample) / 32768.0)
        .collect()
}

/// What one utterance of a batch produced. When the model repeats itself or runs out of
/// tokens, the chunk keeps the text produced until then (transcribe.cpp drops the repeats),
/// with a warning, rather than failing; other errors fail the chunk.
fn outcome(
    result: Result<transcribe_cpp::Transcript, transcribe_cpp::Error>,
    (index, start, end): (usize, f64, f64),
) -> Result<Recognition, CohereError> {
    let (partial, problem) = match result {
        Ok(transcript) => return Ok(recognition(&transcript.text)),
        Err(transcribe_cpp::Error::OutputRepetition {
            partial: Some(partial),
            ..
        }) => (
            partial,
            "the model started repeating itself; kept the text before the repeats",
        ),
        Err(transcribe_cpp::Error::OutputTruncated {
            partial: Some(partial),
            ..
        }) => (
            partial,
            "the model reached its token limit; kept the text up to it",
        ),
        Err(error) => return Err(CohereError::Inference(Arc::new(error))),
    };
    tracing::warn!("chunk #{index} ({start:.2}s–{end:.2}s): {problem}");
    Ok(recognition(&partial.text))
}

fn recognition(text: &str) -> Recognition {
    if text.trim().is_empty() {
        Recognition::default()
    } else {
        Recognition::text(text.trim())
    }
}

async fn collect_batch(
    receiver: &mut mpsc::Receiver<Request>,
    first: Request,
    size: usize,
    wait: Duration,
) -> Vec<Request> {
    let mut requests = vec![first];
    let deadline = tokio::time::Instant::now() + wait;
    while requests.len() < size {
        match receiver.try_recv() {
            Ok(request) => requests.push(request),
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => {
                match tokio::time::timeout_at(deadline, receiver.recv()).await {
                    Ok(Some(request)) => requests.push(request),
                    _ => break,
                }
            }
        }
    }
    requests.retain(|request| !request.reply.is_closed());
    requests
}

async fn worker(
    mut session: Session,
    mut receiver: mpsc::Receiver<Request>,
    options: RunOptions,
    batch_size: usize,
    batch_wait: Duration,
) {
    while let Some(first) = receiver.recv().await {
        let requests = collect_batch(&mut receiver, first, batch_size, batch_wait).await;
        if requests.is_empty() {
            continue;
        }
        let options = options.clone();
        let task = tokio::task::spawn_blocking(move || {
            tracing::debug!(batch_size = requests.len(), "recognizing Cohere batch");
            let pcm: Vec<&[f32]> = requests
                .iter()
                .map(|request| request.pcm.as_slice())
                .collect();
            match session.run_batch(&pcm, &options) {
                Ok(results) if results.len() == requests.len() => {
                    for (request, result) in requests.into_iter().zip(results) {
                        let _ = request.reply.send(outcome(result, request.chunk));
                    }
                }
                Ok(_) => {
                    for request in requests {
                        let _ = request.reply.send(Err(CohereError::WorkerStopped));
                    }
                }
                Err(error) => {
                    let error = Arc::new(error);
                    for request in requests {
                        let _ = request
                            .reply
                            .send(Err(CohereError::Inference(error.clone())));
                    }
                }
            }
            session
        });
        match task.await {
            Ok(returned) => session = returned,
            Err(error) => {
                tracing::error!(%error, "Cohere inference worker failed");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use tafrigh::segment::FixedWindows;
    use tafrigh::{ChunkOptions, ChunkedTranscriber, NoProgress, Transcriber};

    use super::*;

    thread_local! {
        static PCM_CONVERSIONS: Cell<usize> = const { Cell::new(0) };
        static ON_PCM_CONVERSION: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    pub(super) fn converting_pcm() {
        PCM_CONVERSIONS.set(PCM_CONVERSIONS.get() + 1);
        let hook = ON_PCM_CONVERSION.with_borrow_mut(Option::take);
        if let Some(hook) = hook {
            hook();
        }
    }

    /// A recognizer whose requests arrive on the returned receiver instead of a model.
    fn fake_recognizer() -> (CohereRecognizer, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::channel(8);
        let recognizer = CohereRecognizer {
            config: CohereConfig::new("fake.gguf"),
            backend: "fake".into(),
            device_name: "fake".into(),
            max_audio_duration: MAX_AUDIO_DURATION,
            max_samples: samples_in(MAX_AUDIO_DURATION),
            batch_size: 1,
            requests,
        };
        (recognizer, receiver)
    }

    fn chunk(samples: Vec<i16>) -> AudioChunk {
        AudioChunk {
            index: 0,
            start: 0.0,
            end: 0.0,
            lead_in: 0.0,
            samples,
        }
    }

    #[test]
    fn the_audio_limit_is_one_sample_under_what_the_encoder_holds() {
        // transcribe.cpp's frame counts: one mel frame per 160 samples plus a centered one,
        // then an encoder that halves them three times, rounding up, and holds 5,000.
        let encoder_frames = |samples: usize| (samples / 160 + 1).div_ceil(8);
        let max = samples_in(MAX_AUDIO_DURATION);
        assert_eq!(max, 6_399_999);
        assert_eq!(encoder_frames(max), 5_000);
        assert_eq!(encoder_frames(max + 1), 5_001);
    }

    #[tokio::test]
    async fn audio_too_short_for_the_model_is_silence_and_too_long_audio_fails() {
        let (recognizer, receiver) = fake_recognizer();
        // Any request that reaches the worker now fails.
        drop(receiver);
        let short = recognizer.recognize(&chunk(vec![1; MIN_SAMPLES - 1])).await;
        assert_eq!(short.unwrap(), Recognition::default());
        assert!(
            recognizer
                .recognize(&chunk(vec![1; MIN_SAMPLES]))
                .await
                .is_err()
        );
        let long = recognizer
            .recognize(&chunk(vec![0; samples_in(MAX_AUDIO_DURATION) + 1]))
            .await
            .unwrap_err();
        assert!(
            long.to_string().contains("exceeds the Cohere limit"),
            "{long}"
        );
    }

    #[tokio::test]
    async fn a_tiny_last_window_does_not_fail_the_recording() {
        let (recognizer, mut receiver) = fake_recognizer();
        // Answers like transcribe.cpp, which rejects audio shorter than MIN_SAMPLES.
        let worker = tokio::spawn(async move {
            let mut lengths = Vec::new();
            while let Some(request) = receiver.recv().await {
                lengths.push(request.pcm.len());
                let reply = if request.pcm.len() < MIN_SAMPLES {
                    Err(CohereError::Config("too short".into()))
                } else {
                    Ok(Recognition::text("نص"))
                };
                let _ = request.reply.send(reply);
            }
            lengths
        });
        // 30.005 s without padding: one full 30 s window and an 80-sample tail.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.wav");
        std::fs::write(&path, chunk(vec![0; 480_080]).wav_bytes()).unwrap();
        let options = ChunkOptions {
            max_chunk: Duration::from_secs(30),
            merge: true,
            padding: None,
            resend_gaps: None,
            ..ChunkOptions::default()
        };
        let windows = FixedWindows::new(Duration::from_secs(30)).unwrap();
        let transcriber = ChunkedTranscriber::new(windows, recognizer, options).unwrap();
        let segments = transcriber.transcribe(&path, &NoProgress).await.unwrap();
        assert_eq!(segments.len(), 1);
        drop(transcriber);
        let lengths = tokio::time::timeout(Duration::from_secs(10), worker).await;
        assert_eq!(lengths.unwrap().unwrap(), [480_000]);
    }

    #[tokio::test]
    async fn prepares_the_next_batch_before_the_current_batch_finishes() {
        let (mut recognizer, _) = fake_recognizer();
        recognizer.batch_size = 4;
        let (sender, mut receiver) = mpsc::channel(4);
        recognizer.requests = sender;
        let worker = tokio::spawn(async move {
            let mut requests = Vec::new();
            // Hold the first four answers until the next four requests arrive.
            // With only one batch in flight, this can never finish.
            for _ in 0..8 {
                requests.push(receiver.recv().await.unwrap());
            }
            for request in requests.into_iter().rev() {
                request
                    .reply
                    .send(Ok(Recognition::text(format!("chunk {}", request.chunk.0))))
                    .unwrap();
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.wav");
        std::fs::write(&path, chunk(vec![0; 8 * SAMPLE_RATE as usize]).wav_bytes()).unwrap();
        let options = ChunkOptions {
            max_chunk: Duration::from_secs(1),
            merge: false,
            padding: None,
            resend_gaps: None,
            ..ChunkOptions::default()
        };
        let windows = FixedWindows::new(Duration::from_secs(1)).unwrap();
        let transcriber = ChunkedTranscriber::new(windows, recognizer, options).unwrap();
        let segments = tokio::time::timeout(
            Duration::from_secs(10),
            transcriber.transcribe(&path, &NoProgress),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            (0..8).map(|i| format!("chunk {i}")).collect::<Vec<_>>()
        );
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_request_waiting_for_capacity_does_not_block_the_queue() {
        let (mut recognizer, _) = fake_recognizer();
        let (sender, mut receiver) = mpsc::channel(1);
        recognizer.requests = sender;
        let audio = chunk(vec![1; MIN_SAMPLES]);
        let mut first = Box::pin(recognizer.recognize(&audio));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(
                first.as_mut(),
                cx
            )))
            .await
            .is_pending()
        );
        let mut waiting = Box::pin(recognizer.recognize(&audio));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(
                waiting.as_mut(),
                cx
            )))
            .await
            .is_pending()
        );
        drop(waiting);
        let request = receiver.recv().await.unwrap();
        request.reply.send(Ok(Recognition::text("first"))).unwrap();
        assert_eq!(first.await.unwrap(), Recognition::text("first"));
        let mut next = Box::pin(recognizer.recognize(&audio));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(
                next.as_mut(),
                cx
            )))
            .await
            .is_pending()
        );
        let request = receiver
            .try_recv()
            .expect("cancelled waiter must release capacity");
        request.reply.send(Ok(Recognition::text("next"))).unwrap();
        assert_eq!(next.await.unwrap(), Recognition::text("next"));
    }

    #[tokio::test]
    async fn callers_wait_for_queue_capacity_before_converting_pcm() {
        PCM_CONVERSIONS.set(0);
        let (mut recognizer, _) = fake_recognizer();
        let (sender, _receiver) = mpsc::channel(1);
        recognizer.requests = sender;
        let audio = chunk(vec![1; MIN_SAMPLES]);
        let mut queued = Box::pin(recognizer.recognize(&audio));
        let mut waiting = Box::pin(recognizer.recognize(&audio));
        for future in [&mut queued, &mut waiting] {
            let poll = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(std::future::Future::poll(future.as_mut(), cx))
            })
            .await;
            assert!(poll.is_pending());
        }
        assert_eq!(PCM_CONVERSIONS.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn worker_stopping_during_pcm_conversion_does_not_hang() {
        let (recognizer, receiver) = fake_recognizer();
        ON_PCM_CONVERSION.set(Some(Box::new(move || drop(receiver))));
        let audio = chunk(vec![1; MIN_SAMPLES]);
        let error = tokio::time::timeout(Duration::from_secs(1), recognizer.recognize(&audio))
            .await
            .expect("a stopped worker must not leave its caller waiting")
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CohereError>(),
            Some(CohereError::WorkerStopped)
        ));
        assert!(recognizer.requests.is_closed());
    }

    #[tokio::test]
    async fn completed_reply_is_preserved_when_the_worker_exits() {
        // Covers replies through the first select arm. Preserving a reply delivered between
        // branch polls is a cross-thread race this current-thread test cannot schedule.
        for success in [true, false] {
            let (recognizer, mut receiver) = fake_recognizer();
            let worker = tokio::spawn(async move {
                let request = receiver.recv().await.unwrap();
                let answer = if success {
                    Ok(Recognition::text("completed"))
                } else {
                    Err(CohereError::Config("inference failed".into()))
                };
                request.reply.send(answer).unwrap();
                drop(receiver);
            });
            let answer = recognizer.recognize(&chunk(vec![1; MIN_SAMPLES])).await;
            if success {
                assert_eq!(answer.unwrap(), Recognition::text("completed"));
            } else {
                assert!(matches!(
                    answer.unwrap_err().downcast_ref::<CohereError>(),
                    Some(CohereError::Config(message)) if message == "inference failed"
                ));
            }
            worker.await.unwrap();
        }
    }

    #[tokio::test]
    async fn overlapping_files_keep_their_own_ordered_results() {
        let (mut recognizer, mut receiver) = fake_recognizer();
        recognizer.batch_size = 2;
        let worker = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..8 {
                requests.push(receiver.recv().await.unwrap());
            }
            for request in requests.into_iter().rev() {
                let label = if request.pcm[0] > 0.0 {
                    "positive"
                } else {
                    "negative"
                };
                request
                    .reply
                    .send(Ok(Recognition::text(format!(
                        "{label} {}",
                        request.chunk.0
                    ))))
                    .unwrap();
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let positive = directory.path().join("positive.wav");
        let negative = directory.path().join("negative.wav");
        std::fs::write(
            &positive,
            chunk(vec![1; 4 * SAMPLE_RATE as usize]).wav_bytes(),
        )
        .unwrap();
        std::fs::write(
            &negative,
            chunk(vec![-1; 4 * SAMPLE_RATE as usize]).wav_bytes(),
        )
        .unwrap();
        let options = ChunkOptions {
            max_chunk: Duration::from_secs(1),
            merge: false,
            padding: None,
            resend_gaps: None,
            ..ChunkOptions::default()
        };
        let transcriber = ChunkedTranscriber::new(
            FixedWindows::new(Duration::from_secs(1)).unwrap(),
            recognizer,
            options,
        )
        .unwrap();
        let results = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                transcriber.transcribe(&positive, &NoProgress),
                transcriber.transcribe(&negative, &NoProgress)
            )
        })
        .await
        .unwrap();
        for (result, label) in [(results.0, "positive"), (results.1, "negative")] {
            assert_eq!(
                result
                    .unwrap()
                    .iter()
                    .map(|segment| segment.text.clone())
                    .collect::<Vec<_>>(),
                (0..4).map(|i| format!("{label} {i}")).collect::<Vec<_>>()
            );
        }
        worker.await.unwrap();
    }

    #[test]
    fn converts_pcm_without_clipping_or_offset() {
        assert_eq!(
            pcm_f32(&[i16::MIN, 0, 16384, i16::MAX]),
            [-1.0, 0.0, 0.5, 32767.0 / 32768.0]
        );
    }

    #[test]
    fn empty_text_has_no_utterance_and_nonempty_text_has_no_words() {
        assert_eq!(recognition(" \n ").utterances, []);
        let result = recognition(" مرحبا بالعالم. ");
        assert_eq!(result.utterances.len(), 1);
        assert_eq!(result.utterances[0].text, "مرحبا بالعالم.");
        assert_eq!(result.utterances[0].words, []);
    }

    #[test]
    fn repetition_and_token_limit_stops_keep_the_text_before_them() {
        let partial = |text: &str| {
            Some(Box::new(transcribe_cpp::Transcript {
                text: text.into(),
                ..Default::default()
            }))
        };
        let repeated = transcribe_cpp::Error::OutputRepetition {
            message: "loop".into(),
            partial: partial("قال الشيخ"),
        };
        assert_eq!(
            outcome(Err(repeated), (3, 1.0, 2.0)).unwrap(),
            Recognition::text("قال الشيخ")
        );
        let truncated = transcribe_cpp::Error::OutputTruncated {
            message: "budget".into(),
            partial: partial("نص طويل"),
        };
        assert_eq!(
            outcome(Err(truncated), (0, 0.0, 1.0)).unwrap(),
            Recognition::text("نص طويل")
        );
        let other = transcribe_cpp::Error::InputTooLong("too long".into());
        assert!(outcome(Err(other), (0, 0.0, 1.0)).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn partial_batches_flush_and_cancelled_requests_are_dropped() {
        let (sender, mut receiver) = mpsc::channel(4);
        let (reply, result) = oneshot::channel();
        let first = Request {
            pcm: vec![0.0],
            chunk: (0, 0.0, 0.0),
            reply,
        };
        let (cancelled, cancelled_result) = oneshot::channel();
        drop(cancelled_result);
        sender
            .send(Request {
                pcm: vec![1.0],
                chunk: (1, 0.0, 0.0),
                reply: cancelled,
            })
            .await
            .unwrap();
        let requests = collect_batch(&mut receiver, first, 4, Duration::from_millis(10)).await;
        assert_eq!(requests.len(), 1);
        requests
            .into_iter()
            .next()
            .unwrap()
            .reply
            .send(Ok(Recognition::text("done")))
            .unwrap();
        assert_eq!(result.await.unwrap().unwrap(), Recognition::text("done"));
    }

    #[tokio::test]
    async fn full_batches_leave_extra_requests_queued() {
        let (sender, mut receiver) = mpsc::channel(3);
        let mut results = Vec::new();
        for _ in 0..3 {
            let (reply, result) = oneshot::channel();
            results.push(result);
            sender
                .send(Request {
                    pcm: vec![0.0],
                    chunk: (0, 0.0, 0.0),
                    reply,
                })
                .await
                .unwrap();
        }
        let first = receiver.recv().await.unwrap();
        let requests = collect_batch(&mut receiver, first, 2, Duration::from_secs(1)).await;
        assert_eq!(requests.len(), 2);
        assert!(receiver.try_recv().is_ok());
    }
}
