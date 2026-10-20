//! Terminal progress and log output.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use tafrigh::{Event, RunReport, TranscribeEvent};
use tracing_subscriber::fmt::MakeWriter;

/// Progress bars plus result lines on stderr.
pub struct Ui {
    multi: MultiProgress,
    state: Mutex<State>,
}

/// What is on screen: a spinner while an input resolves, and a bar per item in progress
/// (two at once while one item waits for its last answers and the next one starts).
#[derive(Default)]
struct State {
    spinner: Option<ProgressBar>,
    items: HashMap<Arc<str>, ItemBar>,
}

/// One item's progress bar and what it has seen so far.
struct ItemBar {
    bar: ProgressBar,
    duration: Option<f64>,
    queued: u64,
    segmented: bool,
}

impl Ui {
    /// Draws progress bars on stderr unless `quiet` (or stderr is not a terminal).
    pub fn new(quiet: bool) -> Self {
        let target = if quiet {
            ProgressDrawTarget::hidden()
        } else {
            ProgressDrawTarget::stderr()
        };
        Self {
            multi: MultiProgress::with_draw_target(target),
            state: Mutex::new(State::default()),
        }
    }

    /// A tracing writer that prints above the progress bars.
    pub fn log_writer(&self) -> LogWriter {
        LogWriter(self.multi.clone())
    }

    /// Prints a line above the bars (or plainly when bars are hidden).
    pub fn line(&self, text: impl AsRef<str>) {
        if self.multi.is_hidden() {
            eprintln!("{}", text.as_ref());
        } else {
            let _ = self.multi.println(text.as_ref());
        }
    }

    /// Removes all progress bars.
    pub fn clear(&self) {
        let mut state = self.lock();
        if let Some(spinner) = state.spinner.take() {
            spinner.finish_and_clear();
        }
        for (_, item) in state.items.drain() {
            item.bar.finish_and_clear();
        }
        let _ = self.multi.clear();
    }

    /// Updates the display for a pipeline event.
    pub fn handle(&self, event: Event) {
        let mut state = self.lock();
        match event {
            Event::InputResolving { input } => {
                let spinner = self.multi.add(ProgressBar::new_spinner());
                spinner
                    .set_style(ProgressStyle::with_template("{spinner} {msg}").expect("template"));
                spinner.set_message(format!("Resolving {input}"));
                spinner.enable_steady_tick(Duration::from_millis(120));
                if let Some(old) = state.spinner.replace(spinner) {
                    old.finish_and_clear();
                }
            }
            Event::InputResolved { input, items } => {
                state.clear_spinner();
                if items == 0 {
                    drop(state);
                    self.line(format!("! {input}: nothing to transcribe"));
                }
            }
            Event::InputFailed { error, .. } => {
                state.clear_spinner();
                drop(state);
                self.line(format!("✗ {error}"));
            }
            Event::ItemStarted {
                item,
                position,
                total,
            } => {
                let name = item.title.as_deref().unwrap_or(&item.id);
                let bar = self.multi.add(ProgressBar::new(0));
                bar.set_style(
                    ProgressStyle::with_template(
                        "{prefix} {bar:24.cyan/blue} {pos}/{len} chunks {msg} [{elapsed_precise}]",
                    )
                    .expect("template")
                    .progress_chars("━╸ "),
                );
                bar.set_prefix(format!("[{position}/{total}] {}", truncate(name, 40)));
                bar.enable_steady_tick(Duration::from_millis(250));
                let item_bar = ItemBar {
                    bar,
                    duration: None,
                    queued: 0,
                    segmented: false,
                };
                if let Some(old) = state.items.insert(item.id, item_bar) {
                    old.bar.finish_and_clear();
                }
            }
            Event::Downloading { item } => {
                if let Some(item) = state.items.get(&item.id) {
                    item.bar.set_message("· downloading");
                }
            }
            Event::Transcribing { item, event } => {
                if let Some(item) = state.items.get_mut(&item.id) {
                    item.transcribe_event(&event);
                }
            }
            Event::ItemSkipped { item, reason } => {
                state.clear_item(&item.id);
                drop(state);
                self.line(format!("↷ {} ({reason})", item.id));
            }
            Event::ItemFinished { item, segments, .. } => {
                state.clear_item(&item.id);
                drop(state);
                self.line(format!("✓ {} · {segments} segments", item.id));
            }
            Event::ItemFailed { item, error } => {
                state.clear_item(&item.id);
                drop(state);
                self.line(format!("✗ {}: {error}", item.id));
            }
            _ => {}
        }
    }

    /// Prints how many items were transcribed, skipped and failed.
    pub fn summary(&self, report: &RunReport) {
        let mut transcribed = 0;
        let mut skipped = 0;
        for item in &report.items {
            match item.status {
                tafrigh::ItemStatus::Transcribed { .. } => transcribed += 1,
                tafrigh::ItemStatus::Skipped => skipped += 1,
                tafrigh::ItemStatus::Failed(_) => {}
            }
        }
        let failed = report.failures();
        let mut text = format!("{transcribed} transcribed");
        if skipped > 0 {
            let _ = write!(text, ", {skipped} skipped");
        }
        if failed > 0 {
            let _ = write!(text, ", {failed} failed");
        }
        self.line(text);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn clear_spinner(&mut self) {
        if let Some(spinner) = self.spinner.take() {
            spinner.finish_and_clear();
        }
    }

    fn clear_item(&mut self, id: &str) {
        if let Some(item) = self.items.remove(id) {
            item.bar.finish_and_clear();
        }
    }
}

impl ItemBar {
    fn transcribe_event(&mut self, event: &TranscribeEvent) {
        let bar = &self.bar;
        match event {
            TranscribeEvent::Started { duration } => {
                self.duration = *duration;
                bar.set_message("· decoding");
            }
            TranscribeEvent::Decoded { seconds } if !self.segmented => match self.duration {
                Some(total) => bar.set_message(format!(
                    "· decoded {:.0}%",
                    (seconds / total * 100.0).min(100.0)
                )),
                None => bar.set_message(format!("· decoded {}", clock(*seconds))),
            },
            TranscribeEvent::ChunkQueued { .. } => {
                self.queued += 1;
                bar.set_length(self.queued);
            }
            TranscribeEvent::ChunkRecognized { .. } | TranscribeEvent::ChunkSkipped { .. } => {
                bar.inc(1);
            }
            TranscribeEvent::Segmented { chunks } => {
                self.segmented = true;
                bar.set_length(*chunks as u64);
                bar.set_message("");
            }
            _ => {}
        }
    }
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

/// Writes log lines above the progress bars.
#[derive(Clone)]
pub struct LogWriter(MultiProgress);

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = LogLine;

    fn make_writer(&'a self) -> Self::Writer {
        LogLine {
            multi: self.0.clone(),
            buffer: Vec::new(),
        }
    }
}

/// One buffered log record, printed when dropped.
pub struct LogLine {
    multi: MultiProgress,
    buffer: Vec<u8>,
}

impl Write for LogLine {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogLine {
    fn drop(&mut self) {
        if !self.buffer.is_empty() {
            let buffer = std::mem::take(&mut self.buffer);
            self.multi.suspend(|| {
                let _ = std::io::stderr().write_all(&buffer);
            });
        }
    }
}
