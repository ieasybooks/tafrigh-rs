//! `tafrigh`: transcribe audio, video and playlists into subtitles and text with Wit.ai.

mod cli;
mod setup;
mod ui;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tafrigh::source::Input;
use tafrigh::{ChunkedTranscriber, Event, Tafrigh};
use tafrigh_wit::WitRecognizer;
use tracing_subscriber::EnvFilter;

use crate::cli::Args;
use crate::ui::Ui;

// Exit codes, also listed in the --help footer (`cli::HELP_FOOTER`): 0 success, 1 error,
// 2 invalid arguments (clap's own code).
/// The run finished, but some inputs or items failed.
const EXIT_FAILURES: u8 = 3;
/// Interrupted with Ctrl-C.
const EXIT_INTERRUPTED: u8 = 130;

fn main() -> ExitCode {
    let args = Args::parse();
    let ui = Arc::new(Ui::new(args.quiet));
    init_logging(args.verbose, &ui);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let result = runtime.block_on(async {
        tokio::select! {
            result = run(args, &ui) => result,
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

async fn run(args: Args, ui: &Ui) -> anyhow::Result<ExitCode> {
    let inputs: Vec<Input> = args
        .inputs
        .iter()
        .map(String::as_str)
        .map(Input::parse)
        .collect();
    // Before anything touches the network, so a missing program fails fast.
    let decoder = args.tools.decoder();
    let ytdlp = args.tools.ytdlp(&args.output.dir)?;
    setup::check_tools(&decoder, inputs.iter().any(Input::is_url).then_some(&ytdlp)).await?;

    let output = args.output.writer()?;
    let segmenter = args.segmentation.segmenter()?;
    let recognizer = WitRecognizer::new(args.wit.config(args.segmentation.request_seconds()))
        .await
        .context("setting up Wit.ai")?;
    tracing::info!(
        "Wit.ai: {} token(s), language {}",
        args.wit.tokens.len(),
        recognizer.language()
    );
    let transcriber =
        ChunkedTranscriber::new(segmenter, recognizer, args.segmentation.chunk_options())?
            .with_decoder(decoder);

    let tafrigh = Tafrigh::builder(transcriber)
        .recursive(args.recursive)
        .output(output)
        .ytdlp(ytdlp)
        .skip_existing(args.output.skip_existing)
        .sample(args.output.sample)
        .build();

    let report = tafrigh
        .run(inputs, &|event: Event| ui.handle(event))
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
        0 => "warn,tafrigh=info,tafrigh_wit=info,tafrigh_cli=info",
        1 => "warn,tafrigh=debug,tafrigh_wit=debug,tafrigh_cli=debug",
        _ => "info,tafrigh=trace,tafrigh_wit=trace,tafrigh_cli=trace",
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
