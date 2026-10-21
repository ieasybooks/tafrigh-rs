//! Transcribe files or folders to text using the core Silero pipeline.
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tafrigh::output::{BuiltinFormat, OutputMode, OutputWriter};
use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
use tafrigh::source::Input;
use tafrigh::{ChunkOptions, ChunkedTranscriber, ItemStatus, NoProgress, Tafrigh};
use tafrigh_cohere::{CohereConfig, CohereRecognizer, ModelSource};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args_os().skip(1);
    let model = args
        .next()
        .context("usage: transcribe MODEL OUTPUT_DIR [OPTIONS] INPUT...")?;
    let output = args.next().context("missing output directory")?;
    let model = model.to_str().map_or_else(
        || ModelSource::Local(PathBuf::from(&model)),
        ModelSource::from,
    );
    let mut config = CohereConfig::new(model);
    let mut options = ChunkOptions {
        resend_gaps: None,
        ..ChunkOptions::default()
    };
    let mut inputs = Vec::new();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--merge") => options.merge = true,
            Some(
                flag @ ("--device" | "--batch-size" | "--threads" | "--chunk-seconds"
                | "--batch-wait-ms"),
            ) => {
                let value = args
                    .next()
                    .with_context(|| format!("missing value for {flag}"))?;
                let value = value.to_str().context("option value must be UTF-8")?;
                match flag {
                    "--device" => config.device = value.parse()?,
                    "--batch-size" => config.batch_size = value.parse()?,
                    "--threads" => config.threads = value.parse()?,
                    "--chunk-seconds" => options.max_chunk = Duration::from_secs(value.parse()?),
                    "--batch-wait-ms" => config.batch_wait = Duration::from_millis(value.parse()?),
                    _ => unreachable!(),
                }
            }
            Some(flag) if flag.starts_with("--") => bail!("unknown option: {flag}"),
            _ => inputs.push(Input::Path(arg.into())),
        }
    }
    ensure!(!inputs.is_empty(), "provide at least one file or folder");
    let recognizer = CohereRecognizer::load(config).await?;
    eprintln!(
        "Cohere: {} ({})",
        recognizer.backend(),
        recognizer.device_name()
    );
    let segmenter = SileroSegmenter::new(SileroConfig {
        max_speech: options.max_chunk,
        ..SileroConfig::default()
    })?;
    let transcriber = ChunkedTranscriber::new(segmenter, recognizer, options)?;
    let pipeline = Tafrigh::builder(transcriber)
        .output(OutputWriter::new().formats([BuiltinFormat::Txt]))
        .output_mode(OutputMode::Flat(output.into()))
        .recursive(true)
        .build();
    let report = pipeline.run(inputs, &NoProgress).await?;
    for item in &report.items {
        if let ItemStatus::Failed(error) = &item.status {
            eprintln!("{}", tafrigh::ErrorChain(&**error));
        }
    }
    ensure!(
        report.failures() == 0,
        "{} inputs failed",
        report.failures()
    );
    Ok(())
}
