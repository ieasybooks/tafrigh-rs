//! `tafrigh`: transcribe with Wit.ai or retrieve existing [YouTube](https://www.youtube.com) captions.

mod cli;
mod command;
mod setup;
mod ui;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tafrigh::source::Input;
use tafrigh::{ChunkedTranscriber, Event, Tafrigh};
use tafrigh_wit::{WitError, WitRecognizer};
use tracing_subscriber::EnvFilter;

use crate::setup::{Backend, Plan, Run};
use crate::ui::Ui;

// Exit codes, also declared in `cli` for the Usage spec and reference docs: 0 success, 1 error,
// 2 invalid arguments (Usage's own code).
/// The run finished, but some inputs or items failed.
const EXIT_FAILURES: u8 = 3;
/// Interrupted with Ctrl-C.
const EXIT_INTERRUPTED: u8 = 130;

fn main() -> ExitCode {
    let plan = match command::parse() {
        Ok(plan) => plan,
        Err(code) => return code,
    };
    let result = match plan {
        Plan::Transcribe(run) => return transcribe(*run),
        Plan::Completions(args) => command::completions(&args),
        Plan::Spec => command::spec(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn transcribe(run: Run) -> ExitCode {
    let ui = Arc::new(Ui::new(run.quiet));
    init_logging(run.verbose, &ui);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let result = runtime.block_on(async {
        tokio::select! {
            result = start(run, &ui) => result,
            _ = tokio::signal::ctrl_c() => {
                ui.clear();
                eprintln!("interrupted");
                Ok(ExitCode::from(EXIT_INTERRUPTED))
            }
        }
    });
    // Dropping the run closed its channels; decoder threads notice, stop and kill ffmpeg.
    runtime.shutdown_timeout(Duration::from_secs(2));

    result.unwrap_or_else(|err| {
        ui.clear();
        eprintln!("error: {err:#}");
        ExitCode::FAILURE
    })
}

async fn start(run: Run, ui: &Ui) -> anyhow::Result<ExitCode> {
    // Before anything touches the network, so a missing program fails fast.
    let needs_ytdlp = run.inputs.iter().any(Input::is_url);
    let builder = match run.backend {
        Backend::Wit(wit) => {
            setup::check_tools(Some(&wit.decoder), needs_ytdlp.then_some(&run.ytdlp)).await?;
            let segmenter = wit.segmenter.build()?;
            let tokens = wit.wit.tokens.len();
            let recognizer = WitRecognizer::new(wit.wit)
                .await
                .map_err(|err| match err {
                    WitError::Encode(_) => {
                        anyhow::anyhow!("{err} (pass --wit-audio wav to send WAV instead)")
                    }
                    err => err.into(),
                })
                .context("setting up Wit.ai")?;
            tracing::info!(
                "Wit.ai: {tokens} token(s), language {}",
                recognizer.language()
            );
            let transcriber = ChunkedTranscriber::new(segmenter, recognizer, wit.chunks)?
                .with_decoder(wit.decoder);
            Tafrigh::builder(transcriber)
        }
        Backend::Youtube(captions) => {
            setup::check_tools(None, Some(&run.ytdlp)).await?;
            Tafrigh::captions(captions)
        }
    };

    let tafrigh = builder
        .recursive(run.recursive)
        .output(run.output)
        .output_mode(run.output_mode)
        .ytdlp(run.ytdlp)
        .skip_existing(run.skip_existing)
        .sample(run.sample)
        .build();

    let report = tafrigh
        .run(run.inputs, &|event: Event| ui.handle(event))
        .await?;
    ui.summary(&report);
    Ok(if report.failures() > 0 {
        ExitCode::from(EXIT_FAILURES)
    } else {
        ExitCode::SUCCESS
    })
}

fn init_logging(verbose: u8, ui: &Ui) {
    let default = match verbose {
        0 => "warn,tafrigh=info,tafrigh_wit=info,tafrigh_youtube=info,tafrigh_cli=info",
        1 => "warn,tafrigh=debug,tafrigh_wit=debug,tafrigh_youtube=debug,tafrigh_cli=debug",
        _ => "info,tafrigh=trace,tafrigh_wit=trace,tafrigh_youtube=trace,tafrigh_cli=trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(ui.log_writer())
        .init();
}
