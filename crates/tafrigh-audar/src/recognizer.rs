use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tafrigh::audio::{AudioChunk, SAMPLE_RATE};
use tafrigh::{BoxError, ChunkRecognizer, Recognition};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

use crate::{AudarConfig, AudarError, DownloadProgress, MAX_THREADS, runtime};

/// Audio encoder window, including any silence added around the source chunk.
pub const MAX_AUDIO_DURATION: Duration = Duration::from_secs(30);
const MAX_SAMPLES: usize = 30 * SAMPLE_RATE as usize;
// The frontend needs at least two 10 ms frames, including its centered frame.
const MIN_SAMPLES: usize = 160;
const REQUESTS: usize = 2;

/// A loaded Audar decoder and audio encoder. Clones share one inference worker and queue.
#[derive(Debug, Clone)]
pub struct AudarRecognizer {
    config: AudarConfig,
    backend: String,
    device_name: String,
    threads: usize,
    queue: Arc<Queue>,
}

#[derive(Debug)]
struct Queue {
    sender: Option<mpsc::Sender<Request>>,
    capacity: Arc<Semaphore>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Queue {
    fn drop(&mut self) {
        // Free native contexts on their owner thread before process/GPU teardown.
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) struct Request {
    pub pcm: Vec<f32>,
    pub chunk: (usize, f64, f64),
    pub reply: oneshot::Sender<Result<Recognition, AudarError>>,
    // Held through both preparation and decoding, bounding all files together.
    _capacity: OwnedSemaphorePermit,
}

impl AudarRecognizer {
    /// Resolve both model files and load them on a dedicated worker. Requires Tokio.
    /// Automatic device selection retries on CPU if GPU loading fails.
    pub async fn load(config: AudarConfig) -> Result<Self, AudarError> {
        Self::load_with_progress(config, |_| {}).await
    }

    /// Like [`Self::load`], reporting bytes for each model file downloaded on first use.
    pub async fn load_with_progress(
        config: AudarConfig,
        progress: impl Fn(DownloadProgress) + Send + Sync,
    ) -> Result<Self, AudarError> {
        config.validate()?;
        let files = config
            .model
            .resolve(config.model_dir.as_deref(), &progress)
            .await?;
        let threads = if config.threads == 0 {
            std::thread::available_parallelism()
                .map_or(1, std::num::NonZero::get)
                .clamp(1, MAX_THREADS)
        } else {
            config.threads
        };
        let (sender, receiver) = mpsc::channel(1);
        let (ready, loaded) = oneshot::channel();
        let settings = config.clone();
        let runtime = tokio::runtime::Handle::current();
        let worker = std::thread::Builder::new()
            .name("audar".into())
            .spawn(move || {
                runtime::worker(&settings, &files, threads, receiver, ready, &runtime);
            })?;
        let queue = Arc::new(Queue {
            sender: Some(sender),
            capacity: Arc::new(Semaphore::new(REQUESTS)),
            worker: Some(worker),
        });
        let loaded = loaded.await.map_err(|_| AudarError::WorkerStopped)??;
        Ok(Self {
            config,
            backend: loaded.backend,
            device_name: loaded.device_name,
            threads,
            queue,
        })
    }

    /// Settings used to load the model.
    pub fn config(&self) -> &AudarConfig {
        &self.config
    }

    /// Actual native backend, such as CPU, CUDA0 or Metal.
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Description of the selected compute device.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Positive thread count used for decoding, prefill and audio encoding.
    pub fn threads(&self) -> usize {
        self.threads
    }

    fn requests(&self) -> &mpsc::Sender<Request> {
        self.queue.sender.as_ref().expect("live queue")
    }
}

#[async_trait]
impl ChunkRecognizer for AudarRecognizer {
    async fn recognize(&self, chunk: &AudioChunk) -> Result<Recognition, BoxError> {
        if chunk.samples.len() > MAX_SAMPLES {
            return Err(Box::new(AudarError::Config(
                "audio including padding exceeds the Audar limit of 30s".into(),
            )));
        }
        if chunk.samples.len() < MIN_SAMPLES {
            return Ok(Recognition::default());
        }
        let requests = self.requests();
        let capacity = tokio::select! {
            capacity = self.queue.capacity.clone().acquire_owned() => capacity.map_err(|_| AudarError::WorkerStopped)?,
            () = requests.closed() => return Err(Box::new(AudarError::WorkerStopped)),
        };
        // Both in-flight and queue capacity are reserved before float PCM allocation.
        let permit = requests
            .reserve()
            .await
            .map_err(|_| AudarError::WorkerStopped)?;
        let (reply, mut result) = oneshot::channel();
        permit.send(Request {
            pcm: pcm_f32(&chunk.samples),
            chunk: (chunk.index, chunk.start, chunk.end),
            reply,
            _capacity: capacity,
        });
        // A reserved permit can send after the receiver exits; queued senders then remain
        // alive without a worker. Observe closure too, preserving an already delivered reply.
        let answer = tokio::select! {
            biased;
            answer = &mut result => answer.map_err(|_| AudarError::WorkerStopped)?,
            () = requests.closed() => result.try_recv().map_err(|_| AudarError::WorkerStopped)?,
        };
        Ok(answer?)
    }

    fn word_timings(&self) -> bool {
        false
    }
    fn max_audio_duration(&self) -> Option<Duration> {
        Some(MAX_AUDIO_DURATION)
    }
    fn max_in_flight(&self) -> usize {
        REQUESTS
    }
}

fn pcm_f32(samples: &[i16]) -> Vec<f32> {
    #[cfg(test)]
    tests::converting_pcm();
    samples
        .iter()
        .map(|&sample| f32::from(sample) / 32768.0)
        .collect()
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
    fn fake_recognizer() -> (AudarRecognizer, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::channel(1);
        let recognizer = AudarRecognizer {
            config: AudarConfig::local("fake.gguf", "mmproj.gguf"),
            backend: "fake".into(),
            device_name: "fake".into(),
            threads: 1,
            queue: Arc::new(Queue {
                sender: Some(requests),
                capacity: Arc::new(Semaphore::new(REQUESTS)),
                worker: None,
            }),
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

    #[tokio::test]
    async fn accepts_exactly_30_seconds_but_rejects_one_more_sample_before_queueing() {
        let (recognizer, mut receiver) = fake_recognizer();
        assert_eq!(
            recognizer.max_audio_duration(),
            Some(Duration::from_secs(30))
        );
        let samples = 30 * SAMPLE_RATE as usize;
        let boundary = chunk(vec![0; samples]);
        let worker = tokio::spawn(async move {
            let request = receiver.recv().await.unwrap();
            assert_eq!(request.pcm.len(), samples);
            request.reply.send(Ok(Recognition::text("نص"))).unwrap();
        });
        assert_eq!(
            recognizer.recognize(&boundary).await.unwrap(),
            Recognition::text("نص")
        );
        worker.await.unwrap();

        // The closed queue would return WorkerStopped if this reached inference.
        let error = recognizer
            .recognize(&chunk(vec![0; samples + 1]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Audar limit of 30s"), "{error}");
    }

    #[test]
    fn chunking_accepts_29_seconds_with_default_padding_and_rejects_more() {
        let (recognizer, _receiver) = fake_recognizer();
        let window = Duration::from_secs(29);
        let mut options = tafrigh::ChunkOptions {
            max_chunk: window,
            resend_gaps: None,
            ..tafrigh::ChunkOptions::default()
        };
        assert!(
            tafrigh::ChunkedTranscriber::new(
                FixedWindows::new(window).unwrap(),
                recognizer.clone(),
                options.clone(),
            )
            .is_ok()
        );
        options.max_chunk += Duration::from_nanos(62_500);
        let error = tafrigh::ChunkedTranscriber::new(
            FixedWindows::new(window).unwrap(),
            recognizer,
            options,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("recognizer's 30s limit"),
            "{error}"
        );
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
            .recognize(&chunk(vec![0; MAX_SAMPLES + 1]))
            .await
            .unwrap_err();
        assert!(
            long.to_string().contains("exceeds the Audar limit"),
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
                    Err(AudarError::Config("too short".into()))
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
    async fn cancelling_a_request_waiting_for_capacity_does_not_block_the_queue() {
        let (mut recognizer, _) = fake_recognizer();
        let (sender, mut receiver) = mpsc::channel(1);
        Arc::get_mut(&mut recognizer.queue).unwrap().sender = Some(sender);
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
        Arc::get_mut(&mut recognizer.queue).unwrap().sender = Some(sender);
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
            error.downcast_ref::<AudarError>(),
            Some(AudarError::WorkerStopped)
        ));
        assert!(recognizer.requests().is_closed());
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
                    Err(AudarError::Config("inference failed".into()))
                };
                request.reply.send(answer).unwrap();
                drop(receiver);
            });
            let answer = recognizer.recognize(&chunk(vec![1; MIN_SAMPLES])).await;
            if success {
                assert_eq!(answer.unwrap(), Recognition::text("completed"));
            } else {
                assert!(matches!(
                    answer.unwrap_err().downcast_ref::<AudarError>(),
                    Some(AudarError::Config(message)) if message == "inference failed"
                ));
            }
            worker.await.unwrap();
        }
    }

    #[tokio::test]
    async fn prefetches_the_second_request_and_keeps_each_files_results_ordered() {
        let (recognizer, mut receiver) = fake_recognizer();
        let worker = tokio::spawn(async move {
            for _ in 0..2 {
                let first = receiver.recv().await.unwrap();
                // Do not answer until another request arrives: without prefetch this stalls.
                let second = receiver.recv().await.unwrap();
                for request in [second, first] {
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
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let positive = directory.path().join("positive.wav");
        let negative = directory.path().join("negative.wav");
        std::fs::write(
            &positive,
            chunk(vec![1; 2 * SAMPLE_RATE as usize]).wav_bytes(),
        )
        .unwrap();
        std::fs::write(
            &negative,
            chunk(vec![-1; 2 * SAMPLE_RATE as usize]).wav_bytes(),
        )
        .unwrap();
        let transcriber = ChunkedTranscriber::new(
            FixedWindows::new(Duration::from_secs(1)).unwrap(),
            recognizer,
            ChunkOptions {
                max_chunk: Duration::from_secs(1),
                merge: false,
                padding: None,
                resend_gaps: None,
                ..Default::default()
            },
        )
        .unwrap();
        let (positive, negative) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                transcriber.transcribe(&positive, &NoProgress),
                transcriber.transcribe(&negative, &NoProgress)
            )
        })
        .await
        .unwrap();
        for (result, label) in [(positive, "positive"), (negative, "negative")] {
            assert_eq!(
                result
                    .unwrap()
                    .iter()
                    .map(|segment| segment.text.clone())
                    .collect::<Vec<_>>(),
                (0..2)
                    .map(|index| format!("{label} {index}"))
                    .collect::<Vec<_>>()
            );
        }
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn holds_global_capacity_while_requests_are_prepared_or_decoding() {
        PCM_CONVERSIONS.set(0);
        let (recognizer, mut receiver) = fake_recognizer();
        let audio = chunk(vec![1; MIN_SAMPLES]);
        let mut first = Box::pin(recognizer.recognize(&audio));
        let mut second = Box::pin(recognizer.recognize(&audio));
        let mut third = Box::pin(recognizer.recognize(&audio));
        let mut active = Vec::new();
        for future in [&mut first, &mut second] {
            assert!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(
                    future.as_mut(),
                    cx
                )))
                .await
                .is_pending()
            );
            active.push(receiver.recv().await.unwrap());
        }
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(
                third.as_mut(),
                cx
            )))
            .await
            .is_pending()
        );
        assert_eq!(PCM_CONVERSIONS.get(), 2);
        assert!(receiver.try_recv().is_err());
        drop(active.pop());
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(
                third.as_mut(),
                cx
            )))
            .await
            .is_pending()
        );
        assert_eq!(PCM_CONVERSIONS.get(), 3);
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn converts_pcm_without_clipping_or_offset() {
        assert_eq!(
            pcm_f32(&[i16::MIN, 0, 16384, i16::MAX]),
            [-1.0, 0.0, 0.5, 32767.0 / 32768.0]
        );
    }
}
