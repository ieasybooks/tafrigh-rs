//! Public CLI behavior. Helpers must work without credentials, tools or network setup.

use std::path::Path;
use std::process::{Command, Output};

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tafrigh"));
    for key in [
        "TAFRIGH_WIT_TOKENS",
        "TAFRIGH_WIT_LANGUAGE",
        "TAFRIGH_FFMPEG",
        "TAFRIGH_FFPROBE",
        "TAFRIGH_YT_DLP",
        "RUST_LOG",
        "CLICOLOR_FORCE",
    ] {
        command.env_remove(key);
    }
    command.env("NO_COLOR", "1");
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn help_version_and_spec_work_without_credentials_or_tools() {
    for args in [
        vec!["--help"],
        vec!["transcribe", "--help"],
        vec!["--version"],
        vec!["spec"],
        vec!["__usage_spec__"],
    ] {
        let output = command().env("PATH", "").args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        assert_ne!(stdout(&output), "");
        assert!(output.stderr.is_empty(), "{}", stderr(&output));
    }
    let help = stdout(&command().arg("--help").output().unwrap());
    for text in [
        "completions",
        "transcribe",
        "spec",
        "Examples",
        "Output",
        "Segmentation",
        "Wit.ai",
        "@FILE",
    ] {
        assert!(help.contains(text), "missing {text}: {help}");
    }
    let spec = stdout(&command().arg("spec").output().unwrap());
    assert_eq!(
        spec,
        stdout(&command().arg("__usage_spec__").output().unwrap())
    );
}

#[test]
fn help_and_spec_do_not_disclose_token_environment_values() {
    for args in [vec!["--help"], vec!["transcribe", "--help"], vec!["spec"]] {
        let output = command()
            .env("TAFRIGH_WIT_TOKENS", "private-test-value")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(!stdout(&output).contains("private-test-value"));
        assert!(!stderr(&output).contains("private-test-value"));
    }
}

#[test]
fn invalid_arguments_fail_before_tool_setup() {
    for args in [
        vec!["--max-chunk", "0"],
        vec!["--max-chunk", "NaN"],
        vec!["--pad", "-1"],
        vec!["--pad-noise", "2"],
        vec!["--pad", "0", "--pad-noise", "-6"],
        vec!["--resend-gaps", "NaN"],
        vec!["--vad-threshold", "1.1"],
        vec!["--wit-rps", "0"],
        vec!["--wit-attempts", "0"],
        vec!["--wit-concurrency", "0"],
        vec!["--wit-max-audio", "2"],
        vec!["--output-mode", "flat"],
        vec!["--output-mode", "side-by-side", "-o", "out"],
        vec!["--format", "txt", "--words"],
        vec!["--format", "docx"],
        vec!["--yt-dlp-args", "'unclosed"],
        vec!["--typo"],
    ] {
        let output = command()
            .env("PATH", "")
            .args(["transcribe", "recording.mp3", "-w", "test"])
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert_eq!(stdout(&output), "");
        assert!(stderr(&output).contains("error:"));
        assert!(!stderr(&output).contains("required program"));
    }
}

#[test]
fn inputs_and_nonblank_tokens_are_required_only_for_transcription() {
    for args in [
        vec![],
        vec!["transcribe"],
        vec!["recording.mp3"],
        vec!["recording.mp3", "-w", "   "],
    ] {
        let output = command().env("PATH", "").args(&args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn completion_scripts_cover_supported_shells_and_run_without_setup() {
    for shell in ["bash", "elvish", "fish", "nu", "powershell", "zsh"] {
        let output = command()
            .env("PATH", "")
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(output.status.success(), "{shell}: {}", stderr(&output));
        assert!(
            stdout(&output).contains("__complete_word__"),
            "{shell}: {}",
            stdout(&output)
        );
        assert_eq!(stderr(&output), "");
    }
    for args in [
        vec!["completions", "bogus"],
        vec!["completions", "fish", "--dry-run"],
    ] {
        assert_eq!(
            command().args(args).output().unwrap().status.code(),
            Some(2)
        );
    }
}

#[test]
fn completion_protocol_answers_partial_commands_without_expanding_argument_files() {
    let output = command()
        .env("PATH", "")
        .args([
            "__complete_word__",
            "--shell",
            "bash",
            "--words",
            "tafrigh",
            "transcribe",
            "@does-not-exist.args",
            "--output-mode",
            "tr",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("tree-to-tree"),
        "{}",
        stdout(&output)
    );
    assert_eq!(stderr(&output), "");
}

#[test]
fn completion_installation_can_be_previewed_and_preserves_foreign_files() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("fish/completions/tafrigh.fish");
    let install = |preview: bool| {
        let mut command = command();
        command
            .env("XDG_CONFIG_HOME", dir.path())
            .args(["completions", "fish", "--install"]);
        if preview {
            command.arg("--dry-run");
        }
        command.output().unwrap()
    };
    let output = install(true);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains(&target.display().to_string()));
    assert!(!target.exists());
    let output = install(false);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        std::fs::read_to_string(&target)
            .unwrap()
            .contains("__complete_word__")
    );
    assert!(stdout(&install(false)).contains("Already installed"));
    std::fs::write(&target, "# user maintained completions\n").unwrap();
    let output = install(false);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "# user maintained completions\n"
    );
}

#[test]
fn response_files_report_cycles_missing_files_and_bad_quoting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cycle.args"), "@cycle.args").unwrap();
    std::fs::write(dir.path().join("broken.args"), "'unclosed").unwrap();
    for file in ["cycle.args", "missing.args", "broken.args"] {
        let output = command()
            .current_dir(dir.path())
            .arg(format!("@{file}"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(stderr(&output).contains(file), "{}", stderr(&output));
    }
}

fn write_wav(path: &Path) {
    // One second of PCM, mono, 16 kHz, 16 bit. Fixed windows force a recognition request.
    let data_size = 32_000_u32;
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend((36 + data_size).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16_u32.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(16_000_u32.to_le_bytes());
    wav.extend(32_000_u32.to_le_bytes());
    wav.extend(2_u16.to_le_bytes());
    wav.extend(16_u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data_size.to_le_bytes());
    wav.resize(44 + data_size as usize, 0);
    std::fs::write(path, wav).unwrap();
}

#[tokio::test]
async fn transcribes_with_env_credentials_nested_presets_and_explicit_overrides() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/dictation"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"is_final":true,"type":"FINAL_TRANSCRIPTION","text":"hello world","speech":{"confidence":0.9,"tokens":[]}}"#
        )).expect(1).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("@lecture one.wav"));
    std::fs::create_dir(dir.path().join("presets")).unwrap();
    std::fs::write(
        dir.path().join("presets/base.args"),
        "--vad none --max-chunk 1 --pad 0 --resend-gaps 0 -f txt --words --wit-attempts 1",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("presets/batch.args"),
        "@base.args --output-dir old-output",
    )
    .unwrap();
    let mut cmd = command();
    cmd.current_dir(dir.path())
        .env("TAFRIGH_WIT_TOKENS", "test-token")
        .env("TAFRIGH_WIT_LANGUAGE", "en")
        .env("TAFRIGH_FFMPEG", "missing-ffmpeg")
        .args([
            "@presets/batch.args",
            "@@lecture one.wav",
            "--no-words",
            "--ffmpeg",
            "ffmpeg",
            "-o",
            "transcripts",
            "--wit-api-url",
            &server.uri(),
            "--quiet",
        ]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("1 transcribed"));
    assert!(
        std::fs::read_to_string(dir.path().join("transcripts/@lecture one.txt"))
            .unwrap()
            .contains("hello world")
    );
    assert!(!dir.path().join("old-output").exists());
    assert!(!stderr(&output).contains("test-token"));
}
