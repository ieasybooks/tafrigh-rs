//! Public CLI behavior. Helpers must work without credentials, tools or network setup.

use std::path::Path;
use std::process::{Command, Output};

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tafrigh"));
    for key in [
        "TAFRIGH_WIT_TOKENS",
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
fn removed_language_flags_are_rejected_and_the_old_environment_is_ignored() {
    for flag in ["--subtitle-language", "--wit-language"] {
        let output = command()
            .args(["recording.mp3", "--transcriber", "wit", flag, "ar"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(stderr(&output).contains(flag), "{}", stderr(&output));
    }
    let output = command()
        .env("TAFRIGH_WIT_LANGUAGE", "ar")
        .args(["https://youtu.be/video", "--transcriber", "youtube"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("--language"),
        "{}",
        stderr(&output)
    );
    let spec = stdout(&command().arg("spec").output().unwrap());
    for removed in [
        "--subtitle-language",
        "--wit-language",
        "TAFRIGH_WIT_LANGUAGE",
    ] {
        assert!(!spec.contains(removed), "{spec}");
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
            .args([
                "transcribe",
                "recording.mp3",
                "--transcriber",
                "wit",
                "-w",
                "test",
            ])
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
fn inputs_backend_and_wit_tokens_are_required_for_wit_transcription() {
    for args in [
        vec![],
        vec!["transcribe"],
        vec!["recording.mp3"],
        vec!["recording.mp3", "-w", "test"],
        vec!["recording.mp3", "--transcriber", "wit"],
        vec!["recording.mp3", "--transcriber", "wit", "-w", "   "],
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
fn youtube_requires_an_exact_language_and_youtube_inputs_before_tool_setup() {
    for (input, flags, expected) in [
        ("https://youtu.be/video", vec![], "--language"),
        (
            "https://youtu.be/video",
            vec!["--language", "ar.*"],
            "language code",
        ),
        (
            "https://youtu.be/video",
            vec!["--language", "all"],
            "language code",
        ),
        (
            "https://youtu.be/video",
            vec!["-l", "ar,en"],
            "language code",
        ),
        ("recording.mp3", vec!["--language", "ar"], "YouTube URLs"),
        (
            "https://example.com/video",
            vec!["--language", "ar"],
            "YouTube URLs",
        ),
    ] {
        let output = command()
            .env("PATH", "")
            .args([input, "--transcriber", "youtube"])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(stderr(&output).contains(expected), "{}", stderr(&output));
    }
}

#[test]
fn wit_options_are_rejected_by_youtube_even_when_the_value_is_a_default() {
    let cases: &[(&[&str], &str)] = &[
        (&["--recursive"], "--recursive"),
        (&["-qr"], "--recursive"),
        (&["--no-recursive"], "--recursive"),
        (&["--vad", "silero"], "--vad"),
        (&["--max-chunk=15.0"], "--max-chunk"),
        (&["--merge"], "--merge"),
        (&["--no-merge"], "--merge"),
        (&["--merge", "--no-merge"], "--merge"),
        (&["--vad-threshold", "0.5"], "--vad-threshold"),
        (&["--min-speech-ms", "500"], "--min-speech-ms"),
        (&["--min-silence-ms", "300"], "--min-silence-ms"),
        (&["--speech-pad-ms", "250"], "--speech-pad-ms"),
        (&["--pad", "0.5"], "--pad"),
        (&["--pad-noise", "-6"], "--pad-noise"),
        (&["--resend-gaps", "1.0"], "--resend-gaps"),
        (&["--on-chunk-error", "abort"], "--on-chunk-error"),
        (&["--wit-token", "x"], "--wit-token"),
        (&["-w", "x"], "--wit-token"),
        (&["-wx"], "--wit-token"),
        (&["--wit-rps", "1.0"], "--wit-rps"),
        (&["--wit-concurrency", "18"], "--wit-concurrency"),
        (&["--wit-attempts", "5"], "--wit-attempts"),
        (&["--wit-max-audio", "60"], "--wit-max-audio"),
        (&["--wit-endpoint", "dictation"], "--wit-endpoint"),
        (&["--wit-api-url", "https://api.wit.ai"], "--wit-api-url"),
        (&["--wit-api-version", "20240304"], "--wit-api-version"),
        (&["--wit-audio", "opus"], "--wit-audio"),
        (&["--ffmpeg", "ffmpeg"], "--ffmpeg"),
        (&["--ffprobe", "ffprobe"], "--ffprobe"),
    ];
    for &(flags, option) in cases {
        let output = command()
            .env("PATH", "")
            .env("TAFRIGH_WIT_TOKENS", "environment-token")
            .args([
                "https://youtu.be/video",
                "--transcriber",
                "youtube",
                "-l",
                "ar",
            ])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{flags:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains(&format!("'{option}': isn't used by --transcriber youtube")),
            "{flags:?}: {}",
            stderr(&output)
        );
        assert!(!stderr(&output).contains("environment-token"));

        let output = command()
            .env("PATH", "")
            .args([
                "recording.mp3",
                "--transcriber",
                "wit",
                "-w",
                "test",
                "-l",
                "en",
            ])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{flags:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("required program"),
            "{flags:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn shared_options_and_flag_like_values_work_with_both_backends() {
    for (transcriber, input) in [
        ("wit", "recording.mp3"),
        ("youtube", "https://youtu.be/video"),
    ] {
        let output = command()
            .env("PATH", "")
            .env("TAFRIGH_WIT_TOKENS", "test")
            .args([
                input,
                "--transcriber",
                transcriber,
                "--language",
                "en-US",
                "-q",
                "-vv",
                "-o",
                "out",
                "--output-mode",
                "flat",
                "-f",
                "srt",
                "--words",
                "--min-words",
                "2",
                "--keep-uncompacted",
                "--skip-existing",
                "--sample",
                "1",
                "--download-dir",
                "downloads",
                "--save-metadata",
                "--yt-dlp",
                "missing-yt-dlp",
                "--yt-dlp-args",
                "--ffmpeg ignored --merge",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{transcriber}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("required program"),
            "{transcriber}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn argument_file_options_are_checked_against_the_final_backend() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("preset.args"),
        "--transcriber wit --language ar --no-merge",
    )
    .unwrap();
    for transcriber in ["youtube", "wit"] {
        let output = command()
            .current_dir(dir.path())
            .env("PATH", "")
            .env("TAFRIGH_WIT_TOKENS", "test")
            .args([
                "@preset.args",
                "https://youtu.be/video",
                "--transcriber",
                transcriber,
            ])
            .output()
            .unwrap();
        if transcriber == "youtube" {
            assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
            assert!(
                stderr(&output).contains("'--merge': isn't used by --transcriber youtube"),
                "{}",
                stderr(&output)
            );
        } else {
            assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
            assert!(
                stderr(&output).contains("required program"),
                "{}",
                stderr(&output)
            );
        }
    }
}

#[test]
fn unknown_options_are_rejected_for_both_backends() {
    for transcriber in ["wit", "youtube"] {
        let output = command()
            .env("PATH", "")
            .args([
                "https://youtu.be/video",
                "--transcriber",
                transcriber,
                "-l",
                "ar",
                "--typo",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(stderr(&output).contains("--typo"), "{}", stderr(&output));
    }
}

#[test]
fn local_model_options_are_rejected_by_wit_and_youtube() {
    let dir = tempfile::tempdir().unwrap();
    for (transcriber, required) in [("wit", ["-w", "test"]), ("youtube", ["-l", "ar"])] {
        for (option, value) in [
            ("--model", "x"),
            ("--device", "x"),
            ("--model-dir", "x"),
            ("--cohere-batch-size", "16"),
            ("--threads", "1"),
        ] {
            let output = command()
                .env("PATH", "")
                .args(["https://youtu.be/video", "--transcriber", transcriber])
                .args(required)
                .args([option, value])
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(2),
                "{option}: {}",
                stderr(&output)
            );
            assert!(
                stderr(&output).contains(&format!(
                    "'{option}': isn't used by --transcriber {transcriber}"
                )),
                "{option}: {}",
                stderr(&output)
            );
            std::fs::write(dir.path().join("models.args"), format!("{option} {value}")).unwrap();
            let from_file = command()
                .current_dir(dir.path())
                .env("PATH", "")
                .args(["https://youtu.be/video", "--transcriber", transcriber])
                .args(required)
                .arg("@models.args")
                .output()
                .unwrap();
            assert_eq!(from_file.status.code(), Some(2));
            assert!(stderr(&from_file).contains(&format!(
                "'{option}': isn't used by --transcriber {transcriber}"
            )));
        }
    }
}

/// Runs a Cohere transcription of a missing file without tools on PATH.
fn cohere(args: &[&str]) -> Output {
    command()
        .env("PATH", "")
        .args(["transcribe", "recording.mp3", "--transcriber", "cohere"])
        .args(args)
        .output()
        .unwrap()
}

#[cfg(not(feature = "cohere"))]
#[test]
fn cohere_needs_a_build_with_the_cohere_feature() {
    let output = cohere(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("cargo install tafrigh-cli --features cohere"),
        "{}",
        stderr(&output)
    );
}

#[cfg(feature = "cohere")]
#[test]
fn cohere_rejects_unusable_options_before_loading_a_model() {
    for (args, expected) in [
        (
            vec!["-w", "token"],
            "'--wit-token': isn't used by --transcriber cohere",
        ),
        (
            vec!["--resend-gaps", "1"],
            "'--resend-gaps': isn't used by --transcriber cohere",
        ),
        (
            vec!["--no-words"],
            "'--words': isn't used by --transcriber cohere",
        ),
        (
            vec!["-f", "srt,words-srt"],
            "needs word timings, which --transcriber cohere doesn't provide",
        ),
        (
            vec!["-l", "fr"],
            "expected ar or en with --transcriber cohere",
        ),
        (vec!["--device", "tpu"], "expected auto, cpu, gpu or gpu:N"),
        (vec!["--device", "gpu:"], "expected auto, cpu, gpu or gpu:N"),
        (vec!["--cohere-batch-size", "0"], "must be between 1 and 64"),
        (
            vec!["--cohere-batch-size", "65"],
            "must be between 1 and 64",
        ),
        (
            vec!["--threads", "513"],
            "limited by available CPU parallelism and the native limit",
        ),
        (
            vec!["--model", "missing.gguf"],
            "expected a variant (bf16, f16, q8_0, q6_k, q5_k_m, q4_k_m) or an existing .gguf file",
        ),
        (
            vec!["--max-chunk", "400"],
            "exceeds the 399.9999375s Cohere audio limit",
        ),
        (
            vec!["--vad", "none", "--max-chunk", "400", "--pad", "0"],
            "exceeds the 399.9999375s Cohere audio limit",
        ),
        // 7,999.5 samples of padding round up to 8,000 on each side: one sample over the limit.
        (
            vec!["--vad", "none", "--max-chunk", "399", "--pad", "0.49996875"],
            "exceeds the 399.9999375s Cohere audio limit",
        ),
        (
            vec!["--wit-audio", "wav"],
            "'--wit-audio': isn't used by --transcriber cohere",
        ),
    ] {
        let output = cohere(&args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains(expected),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[cfg(feature = "cohere")]
#[test]
fn cohere_accepts_its_options_and_checks_ffmpeg_before_loading_a_model() {
    let model_dir = tempfile::tempdir().unwrap();
    let output = cohere(&[
        "--model",
        "q4_k_m",
        "--device",
        "cpu",
        "--model-dir",
        model_dir.path().to_str().unwrap(),
        "--cohere-batch-size",
        "24",
        "--threads",
        "1",
        "-l",
        "en",
        "--merge",
        "--max-chunk",
        "30",
        "-f",
        "srt,txt",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("a required program is unavailable"),
        "{}",
        stderr(&output)
    );
    assert_eq!(std::fs::read_dir(model_dir.path()).unwrap().count(), 0);
}

#[cfg(feature = "cohere")]
#[test]
fn cohere_threads_reject_oversubscription_before_tool_checks() {
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let output = cohere(&["--threads", &(available + 1).to_string()]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("limited by available CPU parallelism"));
    for threads in ["0", "1"] {
        let output = cohere(&["--threads", threads]);
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        assert!(stderr(&output).contains("a required program is unavailable"));
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
        .and(header("content-type", "audio/opus"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"is_final":true,"type":"FINAL_TRANSCRIPTION","text":"hello world","speech":{"confidence":0.9,"tokens":[]}}"#
        )).expect(1).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("@lecture one.wav"));
    std::fs::create_dir(dir.path().join("presets")).unwrap();
    std::fs::write(
        dir.path().join("presets/base.args"),
        "--transcriber wit --vad none --max-chunk 1 --pad 0 --resend-gaps 0 -f txt --words --wit-attempts 1",
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
        .env("TAFRIGH_FFMPEG", "missing-ffmpeg")
        .args([
            "@presets/batch.args",
            "@@lecture one.wav",
            "--no-words",
            "-l",
            "en",
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

#[tokio::test]
async fn wit_detects_language_when_omitted_even_with_the_removed_environment_variable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"[{"lang":"en","is_app_for_token":true}]"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/dictation"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"is_final":true,"type":"FINAL_TRANSCRIPTION","text":"hello?","speech":{"confidence":0.9,"tokens":[]}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    write_wav(&dir.path().join("lecture.wav"));
    let mut cmd = command();
    cmd.current_dir(dir.path())
        .env("TAFRIGH_WIT_LANGUAGE", "ar")
        .args([
            "lecture.wav",
            "--transcriber",
            "wit",
            "-w",
            "test-token",
            "--vad",
            "none",
            "--max-chunk",
            "1",
            "--pad",
            "0",
            "--resend-gaps",
            "0",
            "-f",
            "txt",
            "--wit-attempts",
            "1",
            "--wit-api-url",
            &server.uri(),
            "-q",
        ]);
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lecture.txt")).unwrap(),
        "hello?\n"
    );
}

#[cfg(unix)]
mod youtube {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn fake_ytdlp(dir: &Path) -> PathBuf {
        let script = dir.join("yt-dlp");
        std::fs::write(&script, r#"#!/bin/sh
if [ "$1" = --version ]; then printf 'test\n'; exit 0; fi
printf '%s\n' call "$@" >> "$TAFRIGH_TEST_YTDLP_LOG"
flat=no
skip=no
metadata=
template=
language=
previous=
for arg in "$@"; do
  case "$previous" in
    --load-info-json) metadata=$arg ;;
    --output) template=$arg ;;
    --sub-langs) language=$arg ;;
  esac
  case "$arg" in
    --flat-playlist) flat=yes ;;
    --skip-download) skip=yes ;;
    --no-skip-download) skip=no ;;
  esac
  previous=$arg
  url=$arg
done
if [ "$skip" != yes ]; then printf 'ERROR: attempted audio download\n' >&2; exit 1; fi
if [ "$flat" = yes ]; then
  if [ "$TAFRIGH_TEST_CAPTIONS" = playlist ]; then
    printf '%s\n' '{"entries":[{"id":"missing","webpage_url":"https://youtu.be/missing"},{"id":"video","webpage_url":"https://youtu.be/video"}]}'
  else
    printf '%s\n' '{"id":"video","webpage_url":"https://youtu.be/video"}'
  fi
elif [ -n "$metadata" ]; then
  printf 'metadata:%s\n' "$metadata" >> "$TAFRIGH_TEST_YTDLP_LOG"
  if [ "$language" != ar ]; then printf 'ERROR: wrong language\n' >&2; exit 1; fi
  if [ "$TAFRIGH_TEST_CAPTIONS" = nofile ]; then exit 0; fi
  template=${template#subtitle:}
  prefix=${template%'%(ext)s'}
  target=${prefix}${language}.json3
  if [ "$TAFRIGH_TEST_CAPTIONS" = malformed ]; then
    printf 'not json\n' > "$target"
  elif [ "$TAFRIGH_TEST_CAPTIONS" = empty ]; then
    printf '%s\n' '{"events":[]}' > "$target"
  elif [ "$TAFRIGH_TEST_CAPTIONS" = untimed ]; then
    printf '%s\n' '{"events":[{"tStartMs":1000,"dDurationMs":2000,"segs":[{"utf8":"hello world"}]}]}' > "$target"
  else
    printf '%s\n' '{"events":[{"tStartMs":1000,"dDurationMs":2000,"segs":[{"utf8":"hello"},{"utf8":" world","tOffsetMs":700}]}]}' > "$target"
  fi
else
  if [ "$url" = https://youtu.be/missing ]; then
    printf '%s\n' '{"extractor_key":"Youtube","subtitles":{},"automatic_captions":{}}'
  else
    printf '%s\n' '{"id":"video","extractor_key":"Youtube","subtitles":{"ar":[{"ext":"json3","url":"https://www.youtube.com/api/timedtext?lang=ar"}]}}'
    if [ "$TAFRIGH_TEST_CAPTIONS" = failed ]; then
      printf 'ERROR: caption request failed\n' >&2
      exit 1
    fi
  fi
fi
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn caption_command(dir: &Path, scenario: &str) -> Command {
        let mut cmd = command();
        cmd.current_dir(dir)
            .env("PATH", "")
            .env("TAFRIGH_FFMPEG", "missing-ffmpeg")
            .env("TAFRIGH_FFPROBE", "missing-ffprobe")
            .env("TAFRIGH_TEST_YTDLP_LOG", dir.join("calls.log"))
            .env("TAFRIGH_TEST_CAPTIONS", scenario)
            .args([
                "https://youtu.be/video",
                "--transcriber",
                "youtube",
                "--language",
                "ar",
                "--quiet",
                "--yt-dlp",
            ])
            .arg(fake_ytdlp(dir));
        cmd
    }

    #[test]
    fn writes_all_formats_and_available_words_without_audio_or_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let output = caption_command(dir.path(), "timed")
            .args(["--words", "--sample", "1"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(stdout(&output), "");
        for ext in [
            "txt",
            "srt",
            "vtt",
            "csv",
            "tsv",
            "json",
            "words.srt",
            "words.vtt",
            "words.csv",
            "words.tsv",
            "words.json",
        ] {
            assert!(
                dir.path().join(format!("video.{ext}")).is_file(),
                "missing {ext}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("video.txt")).unwrap(),
            "hello world\n"
        );
        let words = std::fs::read_to_string(dir.path().join("video.words.srt")).unwrap();
        assert!(
            words.contains("00:00:01,000 --> 00:00:01,700\nhello"),
            "{words}"
        );
        assert!(
            words.contains("00:00:01,700 --> 00:00:03,000\nworld"),
            "{words}"
        );
        let sample = std::fs::read_to_string(dir.path().join("sample.csv")).unwrap();
        assert!(sample.contains("https://youtu.be/video?t=1,"), "{sample}");
        assert!(!sample.contains(".json3"));
        let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        for path in calls
            .lines()
            .filter_map(|line| line.strip_prefix("metadata:"))
        {
            assert!(!Path::new(path).exists(), "temporary metadata remains");
        }
        assert!(!calls.contains("bestaudio"));
    }

    #[test]
    fn unused_wit_environment_works_but_an_explicit_token_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let output = caption_command(dir.path(), "timed")
            .env("TAFRIGH_WIT_TOKENS", "unused-token")
            .args(["-f", "txt"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(dir.path().join("video.txt").is_file());

        let output = caption_command(dir.path(), "timed")
            .env("TAFRIGH_WIT_TOKENS", "unused-token")
            .args(["-w", "x"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            stderr(&output).contains("'--wit-token': isn't used by --transcriber youtube"),
            "{}",
            stderr(&output)
        );
    }

    #[test]
    fn missing_captions_fail_the_item_and_continue_the_playlist_without_wit() {
        let dir = tempfile::tempdir().unwrap();
        let output = caption_command(dir.path(), "playlist")
            .env("TAFRIGH_WIT_TOKENS", "must-not-use")
            .args(["-f", "txt", "-o", "out"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        assert!(
            stderr(&output).contains("1 transcribed, 1 failed"),
            "{}",
            stderr(&output)
        );
        assert!(stderr(&output).contains("no original captions"));
        assert!(!stderr(&output).contains("setting up Wit"));
        assert!(!dir.path().join("out/missing.txt").exists());
        assert!(dir.path().join("out/video.txt").is_file());
        let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert!(!calls.contains("bestaudio"));
    }

    #[test]
    fn skip_existing_uses_current_names_and_does_not_fetch_captions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("video.txt"), "existing transcript\n").unwrap();
        let output = caption_command(dir.path(), "failed")
            .args(["--skip-existing", "-f", "txt"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("1 skipped"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("video.txt")).unwrap(),
            "existing transcript\n"
        );
        let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert_eq!(calls.lines().filter(|line| *line == "call").count(), 1);
    }

    #[test]
    fn failed_empty_and_malformed_downloads_do_not_create_transcripts() {
        for (scenario, expected) in [
            ("failed", "caption request failed"),
            ("nofile", "reading downloaded"),
            ("empty", "empty captions"),
            ("malformed", "invalid YouTube JSON3"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let output = caption_command(dir.path(), scenario)
                .args(["-f", "txt"])
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(3),
                "{scenario}: {}",
                stderr(&output)
            );
            assert!(
                stderr(&output).contains(expected),
                "{scenario}: {}",
                stderr(&output)
            );
            assert!(!dir.path().join("video.txt").exists());
        }
    }

    #[test]
    fn preserves_download_directory_and_warns_when_words_are_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let output = caption_command(dir.path(), "untimed")
            .args([
                "--download-dir",
                "captions",
                "-f",
                "srt",
                "--words",
                "--yt-dlp-args",
                "--no-skip-download --sub-langs en --sub-format vtt",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("no word timings"));
        assert!(dir.path().join("captions/video.srt").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("captions/video.words.srt")).unwrap(),
            ""
        );
    }
}
