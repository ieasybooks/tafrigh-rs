//! Output routing and resume behavior through the public pipeline API.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tafrigh::output::{BuiltinFormat, OutputMode, OutputWriter};
use tafrigh::source::Input;
use tafrigh::{
    ItemStatus, NoProgress, Progress, Result, Segment, Tafrigh, TranscribeEvent, Transcriber,
};

#[derive(Default)]
struct CountingTranscriber(AtomicUsize);

#[async_trait::async_trait]
impl Transcriber for CountingTranscriber {
    async fn transcribe(
        &self,
        _: &Path,
        _: &dyn Progress<TranscribeEvent>,
    ) -> Result<Vec<Segment>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Segment::new(0.0, 1.0, "two words")])
    }
}

fn media(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"").unwrap();
    path
}

fn writer() -> OutputWriter {
    OutputWriter::new()
        .formats([BuiltinFormat::Srt, BuiltinFormat::WordsSrt])
        .keep_uncompacted(true)
}

#[tokio::test]
async fn layouts_route_all_variants_and_skip_only_complete_outputs() {
    let modes: [fn(PathBuf) -> OutputMode; 3] = [
        |_| OutputMode::SideBySide,
        OutputMode::TreeToTree,
        OutputMode::Flat,
    ];
    for mode in modes {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        media(&root, "in/a/talk.mp3");
        media(&root, "in/b/talk.mp3");
        let mode = mode(root.join("out"));
        let transcriber = Arc::new(CountingTranscriber::default());
        let tafrigh = Tafrigh::builder(transcriber.clone())
            .recursive(true)
            .output(writer())
            .output_mode(mode.clone())
            .skip_existing(true)
            .build();
        let inputs = [Input::Path(root.join("in"))];
        let report = tafrigh.run(inputs.clone(), &NoProgress).await.unwrap();
        assert_eq!(report.failures(), 0);
        assert_eq!(report.items.len(), 2);
        assert_ne!(report.items[0].item.id, report.items[1].item.id);
        let names = match mode {
            OutputMode::SideBySide => ["in/a/talk", "in/b/talk"],
            OutputMode::TreeToTree(_) => ["out/a/talk", "out/b/talk"],
            OutputMode::Flat(_) => ["out/talk", "out/talk-2"],
        };
        for (item, name) in report.items.iter().zip(names) {
            let ItemStatus::Transcribed { outputs, .. } = &item.status else {
                panic!("{item:?}")
            };
            let expected: Vec<_> = [
                "srt",
                "uncompacted.srt",
                "words.srt",
                "uncompacted.words.srt",
            ]
            .map(|ext| root.join(format!("{name}.{ext}")))
            .into();
            assert_eq!(outputs, &expected);
            assert!(outputs.iter().all(|path| path.is_file()));
        }
        let report = tafrigh.run(inputs.clone(), &NoProgress).await.unwrap();
        assert!(
            report
                .items
                .iter()
                .all(|item| matches!(item.status, ItemStatus::Skipped))
        );
        assert_eq!(transcriber.0.load(Ordering::SeqCst), 2);

        // A missing word-level variant requires retranscribing that item only.
        std::fs::remove_file(root.join(format!("{}.words.srt", names[0]))).unwrap();
        let report = tafrigh.run(inputs, &NoProgress).await.unwrap();
        assert_eq!(report.failures(), 0);
        assert!(matches!(
            report.items[0].status,
            ItemStatus::Transcribed { .. }
        ));
        assert!(matches!(report.items[1].status, ItemStatus::Skipped));
        assert_eq!(transcriber.0.load(Ordering::SeqCst), 3);
    }
}

#[tokio::test]
async fn tree_separates_roots_and_puts_individual_files_at_the_top() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    media(&root, "first/lectures/course/talk.mp3");
    media(&root, "second/lectures/course/talk.mp3");
    let single = media(&root, "single/talk.mp3");
    let report = Tafrigh::builder(CountingTranscriber::default())
        .output(writer())
        .output_mode(OutputMode::TreeToTree(root.join("out")))
        .recursive(true)
        .sample(3)
        .build()
        .run(
            [
                Input::Path(root.join("first/lectures")),
                Input::Path(root.join("second/lectures")),
                Input::Path(single),
            ],
            &NoProgress,
        )
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    for name in [
        "lectures/course/talk.srt",
        "lectures-2/course/talk.srt",
        "talk.srt",
        "sample.csv",
    ] {
        assert!(root.join("out").join(name).is_file(), "{name}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn urls_are_flat_or_beside_the_actual_download_and_keep_their_download_ids() {
    use std::os::unix::fs::PermissionsExt;
    use tafrigh::source::YtDlp;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let script = root.join("yt-dlp");
    std::fs::write(
        &script,
        r#"#!/bin/sh
if [ "$1" = "--flat-playlist" ]; then
  printf '%s\n' '{"id":"remote","webpage_url":"https://example.com/remote"}'
  exit 0
fi
previous=
for arg in "$@"; do
  if [ "$previous" = "-o" ]; then template=$arg; fi
  previous=$arg
done
dir=$(dirname "$template")/custom
mkdir -p "$dir"
touch "$dir/actual-name.wav"
printf '%s\n' "$dir/actual-name.wav"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let local = media(&root, "in/remote.mp3");
    let modes: [fn(PathBuf) -> OutputMode; 3] = [
        |_| OutputMode::SideBySide,
        OutputMode::TreeToTree,
        OutputMode::Flat,
    ];
    for (index, mode) in modes.into_iter().enumerate() {
        let out = root.join(format!("mode-{index}"));
        let downloads = out.join("downloads");
        let mode = mode(out.clone());
        let transcriber = Arc::new(CountingTranscriber::default());
        let tafrigh = Tafrigh::builder(transcriber.clone())
            .output(writer())
            .output_mode(mode.clone())
            .ytdlp(YtDlp::new(&downloads).program(&script))
            .skip_existing(true)
            .build();
        let inputs = [
            Input::Path(local.clone()),
            Input::Url("https://example.com/remote".into()),
        ];
        let report = tafrigh.run(inputs.clone(), &NoProgress).await.unwrap();
        assert_eq!(report.failures(), 0);
        assert_eq!(report.items[1].item.id.as_ref(), "remote-2");
        let expected = if mode == OutputMode::SideBySide {
            downloads.join("custom/actual-name.srt")
        } else {
            out.join("remote-2.srt")
        };
        assert!(expected.is_file(), "{}", expected.display());
        let report = tafrigh.run(inputs, &NoProgress).await.unwrap();
        assert!(
            report
                .items
                .iter()
                .all(|item| matches!(item.status, ItemStatus::Skipped))
        );
        assert_eq!(transcriber.0.load(Ordering::SeqCst), 2);
    }

    // A colliding progress/output name must not change yt-dlp's cache lookup ID.
    media(&root, "cache/remote.wav");
    let report = Tafrigh::builder(CountingTranscriber::default())
        .output(writer())
        .output_mode(OutputMode::Flat(root.join("cached-output")))
        .ytdlp(YtDlp::new(root.join("cache")).program(&script))
        .build()
        .run(
            [
                Input::Path(local),
                Input::Url("https://example.com/remote".into()),
            ],
            &NoProgress,
        )
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert!(root.join("cached-output/remote-2.srt").is_file());
    assert!(!root.join("cache/custom").exists());
}
