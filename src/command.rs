//! CLI control requests and operations that do not transcribe media.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use usage_rs::embedded::{Exit, Outcome};
use usage_rs::install::{Env, OnForeign, Wrote};

use crate::cli::{Cli, Completions};

/// Expand reusable argument files and let Usage render its own help and diagnostics.
pub fn parse() -> Result<Cli, ExitCode> {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Completion words are shell input, not arguments to execute. In particular, never
    // expand @files from a partial command line passed by the completion protocol.
    if let Some(answer) = Cli::completion_request(&argv) {
        return Err(print_exit(&Exit {
            text: answer,
            stderr: false,
            code: 0,
        }));
    }
    let argv = usage_rs::response::expand(argv).map_err(|error| {
        print_exit(&Exit {
            text: format!("error: {error}\n"),
            stderr: true,
            code: 2,
        })
    })?;
    match Cli::embedded_outcome(&argv) {
        Outcome::Parsed(cli) => Ok(cli),
        Outcome::Exit(exit) => Err(print_exit(&exit)),
    }
}

fn print_exit(exit: &Exit) -> ExitCode {
    let result = if exit.stderr {
        io::stderr().lock().write_all(exit.text.as_bytes())
    } else {
        io::stdout().lock().write_all(exit.text.as_bytes())
    };
    match result {
        Ok(()) => ExitCode::from(u8::try_from(exit.code).unwrap_or(1)),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: writing CLI output: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Write a script to stdout or install it, with a preview available before installation.
pub fn completions(args: &Completions) -> anyhow::Result<()> {
    let shell = usage_rs::complete::Shell::from_name(&args.shell)
        .expect("shell choices validated by Usage");
    if !args.install {
        io::stdout()
            .lock()
            .write_all(Cli::completion_script(shell).as_bytes())?;
        return Ok(());
    }
    let env = Env::from_process();
    let plan = if args.dry_run {
        let plan = Cli::completion_install_plan(shell, &env)?;
        println!("Would install completions to {}", plan.path.display());
        plan
    } else {
        let installed = Cli::install_completion(shell, &env, OnForeign::Refuse)?;
        let action = match installed.wrote {
            Wrote::Updated => "Updated",
            Wrote::Unchanged => "Already installed",
            _ => "Installed",
        };
        println!("{action}: {}", installed.plan.path.display());
        installed.plan
    };
    if let Some(instruction) = plan.loading.instruction() {
        println!("{instruction}");
    }
    Ok(())
}

/// Print the portable CLI definition.
pub fn spec() -> anyhow::Result<()> {
    io::stdout().lock().write_all(Cli::to_kdl().as_bytes())?;
    Ok(())
}
