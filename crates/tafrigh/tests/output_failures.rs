//! Filesystem conflicts and recovery through the public pipeline.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tafrigh::output::{BuiltinFormat, OutputMode, OutputWriter};
use tafrigh::source::Input;
use tafrigh::{
    Event, ItemStatus, NoProgress, Progress, Result, Segment, Tafrigh, TranscribeEvent, Transcriber,
};

#[derive(Default)]
struct RecordingTranscriber(Mutex<Vec<PathBuf>>);

#[async_trait::async_trait]
impl Transcriber for RecordingTranscriber {
    async fn transcribe(
        &self,
        path: &Path,
        _: &dyn Progress<TranscribeEvent>,
    ) -> Result<Vec<Segment>> {
        self.0.lock().unwrap().push(path.to_owned());
        Ok(vec![Segment::new(
            0.0,
            1.0,
            std::fs::read_to_string(path).unwrap(),
        )])
    }
}

fn media(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

fn writer(dir: &Path) -> OutputWriter {
    OutputWriter::new(dir).formats([BuiltinFormat::Txt])
}

fn assert_text(path: &Path, text: &str) {
    assert_eq!(
        std::fs::read_to_string(path).unwrap().trim(),
        text,
        "{}",
        path.display()
    );
}

#[tokio::test]
async fn long_media_names_are_written_and_skipped_on_retry() {
    for (character, count, bytes) in [
        ("a", 240, 240),
        ("a", 246, 246),
        ("ع", 120, 240),
        ("ع", 123, 246),
    ] {
        let name = character.repeat(count);
        assert_eq!(name.len(), bytes);
        for mode in [
            OutputMode::SideBySide,
            OutputMode::TreeToTree,
            OutputMode::Flat,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let input = media(root, &format!("in/{name}.mp3"), "long recording");
            let out = root.join("out");
            let destination = if mode == OutputMode::SideBySide {
                root.join("in")
            } else {
                out.clone()
            };
            let transcriber = Arc::new(RecordingTranscriber::default());
            let pipeline = Tafrigh::builder(transcriber.clone())
                .output(writer(&out))
                .output_mode(mode)
                .skip_existing(true)
                .build();
            let initial = pipeline
                .run([Input::Path(input.clone())], &NoProgress)
                .await
                .unwrap();
            assert_eq!(initial.failures(), 0, "{mode:?}, {bytes}-byte stem");
            assert!(matches!(
                initial.items[0].status,
                ItemStatus::Transcribed { .. }
            ));
            assert_text(&destination.join(format!("{name}.txt")), "long recording");
            let retry = pipeline
                .run([Input::Path(input)], &NoProgress)
                .await
                .unwrap();
            assert_eq!(retry.failures(), 0);
            assert!(matches!(retry.items[0].status, ItemStatus::Skipped));
            assert_eq!(transcriber.0.lock().unwrap().len(), 1);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_discovery_keeps_tree_root_names_on_retry() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let first = media(root, "first/lectures/talk.mp3", "first recording");
    let second = media(root, "second/lectures/talk.mp3", "second recording");
    let blocked = root.join("first/lectures/unreadable");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let transcriber = Arc::new(RecordingTranscriber::default());
    let pipeline = Tafrigh::builder(transcriber.clone())
        .output(writer(&root.join("out")))
        .output_mode(OutputMode::TreeToTree)
        .recursive(true)
        .skip_existing(true)
        .build();
    let inputs = [
        Input::Path(root.join("first/lectures")),
        Input::Path(root.join("second/lectures")),
    ];
    let initial = pipeline.run(inputs.clone(), &NoProgress).await;
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let initial = initial.unwrap();
    assert_eq!(initial.failed_inputs.len(), 1);
    assert_eq!(initial.items.len(), 1);
    let first_output = "out/lectures/talk.txt";
    let second_output = "out/lectures-2/talk.txt";
    assert_text(&root.join(second_output), "second recording");
    let retry = pipeline.run(inputs.clone(), &NoProgress).await.unwrap();
    assert_eq!(retry.failures(), 0);
    assert!(matches!(
        retry.items[0].status,
        ItemStatus::Transcribed { .. }
    ));
    assert!(matches!(retry.items[1].status, ItemStatus::Skipped));
    assert_text(&root.join(first_output), "first recording");
    assert_text(&root.join(second_output), "second recording");
    assert_eq!(*transcriber.0.lock().unwrap(), [second, first]);
    let retry = pipeline.run(inputs, &NoProgress).await.unwrap();
    assert!(
        retry
            .items
            .iter()
            .all(|item| matches!(item.status, ItemStatus::Skipped))
    );
}

#[tokio::test]
async fn mirrored_directories_take_precedence_in_both_discovery_orders() {
    for extension in ["mp3", "wav"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        media(root, &format!("in/a.{extension}"), "top recording");
        media(root, "in/a.txt/b.mp3", "nested recording");
        let pipeline = Tafrigh::builder(RecordingTranscriber::default())
            .output(writer(&root.join("out")))
            .output_mode(OutputMode::TreeToTree)
            .recursive(true)
            .skip_existing(true)
            .build();
        let report = pipeline
            .run([Input::Path(root.join("in"))], &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 0);
        assert_text(&root.join("out/a-2.txt"), "top recording");
        assert_text(&root.join("out/a.txt/b.txt"), "nested recording");
        let report = pipeline
            .run([Input::Path(root.join("in"))], &NoProgress)
            .await
            .unwrap();
        assert!(
            report
                .items
                .iter()
                .all(|item| matches!(item.status, ItemStatus::Skipped))
        );
    }
}

#[tokio::test]
async fn directories_from_later_inputs_are_reserved_before_any_files() {
    for reverse in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let single = media(root, "single/a.mp3", "single recording");
        media(root, "a.txt/b.mp3", "nested recording");
        media(root, "other/c.mp3", "other recording");
        let mut inputs = [
            Input::Path(single),
            Input::Path(root.join("a.txt")),
            Input::Path(root.join("other")),
        ];
        if reverse {
            inputs.reverse();
        }
        let report = Tafrigh::builder(RecordingTranscriber::default())
            .output(writer(&root.join("out")))
            .output_mode(OutputMode::TreeToTree)
            .build()
            .run(inputs, &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 0);
        assert_text(&root.join("out/a-2.txt"), "single recording");
        assert_text(&root.join("out/a.txt/b.txt"), "nested recording");
    }
}

#[tokio::test]
async fn side_by_side_preserves_directories_for_every_output_variant() {
    for directory in ["a.srt", "a.words.srt", "a.uncompacted.srt", "a.srt.tmp"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let input = media(root, "a.mp3", "recording");
        std::fs::create_dir(root.join(directory)).unwrap();
        let output = OutputWriter::new(root)
            .formats([BuiltinFormat::Srt, BuiltinFormat::WordsSrt])
            .keep_uncompacted(true);
        let report = Tafrigh::builder(RecordingTranscriber::default())
            .output(output)
            .output_mode(OutputMode::SideBySide)
            .build()
            .run([Input::Path(input)], &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 0);
        assert!(root.join(directory).is_dir());
        for extension in [
            "srt",
            "words.srt",
            "uncompacted.srt",
            "uncompacted.words.srt",
        ] {
            assert!(root.join(format!("a-2.{extension}")).is_file());
        }
    }
}

#[tokio::test]
async fn sample_report_avoids_mirrored_directories_and_transcripts() {
    for transcript_collision in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        media(root, "in/sample.csv/lecture.mp3", "nested recording");
        if transcript_collision {
            media(root, "in/sample.mp3", "sample recording");
        }
        let report = Tafrigh::builder(RecordingTranscriber::default())
            .output(OutputWriter::new(root.join("out")).formats([BuiltinFormat::Csv]))
            .output_mode(OutputMode::TreeToTree)
            .recursive(true)
            .sample(3)
            .build()
            .run([Input::Path(root.join("in"))], &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 0);
        assert!(root.join("out/sample.csv/lecture.csv").is_file());
        let sample = if transcript_collision {
            "out/sample-3.csv"
        } else {
            "out/sample-2.csv"
        };
        let text = std::fs::read_to_string(root.join(sample)).unwrap();
        assert!(text.starts_with("item,start,end,text,link,file"));
        assert!(text.contains("nested recording"));
        if transcript_collision {
            let transcript = std::fs::read_to_string(root.join("out/sample-2.csv")).unwrap();
            assert!(transcript.contains("sample recording"));
            assert!(!transcript.starts_with("item,"));
        }
    }
}

#[tokio::test]
async fn unusable_item_destination_does_not_abort_the_batch() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    media(root, "in/blocked/deep/talk.mp3", "blocked recording");
    let healthy = media(root, "in/ok.mp3", "healthy recording");
    media(root, "out/blocked", "obstruction");
    let transcriber = Arc::new(RecordingTranscriber::default());
    let events = Mutex::new(Vec::new());
    let collect = |event: Event| match event {
        Event::ItemFailed { item, .. } => {
            events.lock().unwrap().push(format!("failed {}", item.id));
        }
        Event::ItemFinished { item, .. } => {
            events.lock().unwrap().push(format!("finished {}", item.id));
        }
        _ => {}
    };
    let report = Tafrigh::builder(transcriber.clone())
        .output(writer(&root.join("out")))
        .output_mode(OutputMode::TreeToTree)
        .recursive(true)
        .build()
        .run([Input::Path(root.join("in"))], &collect)
        .await
        .unwrap();
    assert_eq!(report.items.len(), 2);
    assert_eq!(report.failures(), 1);
    assert!(matches!(report.items[0].status, ItemStatus::Failed(_)));
    assert!(matches!(
        report.items[1].status,
        ItemStatus::Transcribed { .. }
    ));
    assert_eq!(*events.lock().unwrap(), ["failed talk", "finished ok"]);
    assert_eq!(*transcriber.0.lock().unwrap(), [healthy]);
    assert_text(&root.join("out/ok.txt"), "healthy recording");
    assert_text(&root.join("out/blocked"), "obstruction");
}

#[tokio::test]
async fn existing_outputs_are_skipped_only_when_all_requested_files_exist() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let input = media(root, "in/talk.mp3", "new recording");
    let txt = media(root, "out/talk.txt", "existing text");
    let srt = media(root, "out/talk.srt", "existing subtitles");
    let transcriber = Arc::new(RecordingTranscriber::default());
    let output =
        OutputWriter::new(root.join("out")).formats([BuiltinFormat::Txt, BuiltinFormat::Srt]);
    let pipeline = Tafrigh::builder(transcriber.clone())
        .output(output.clone())
        .skip_existing(true)
        .build();
    let report = pipeline
        .run([Input::Path(input.clone())], &NoProgress)
        .await
        .unwrap();
    assert!(matches!(report.items[0].status, ItemStatus::Skipped));
    assert!(transcriber.0.lock().unwrap().is_empty());
    assert_text(&txt, "existing text");
    assert_text(&srt, "existing subtitles");

    std::fs::remove_file(&srt).unwrap();
    let report = pipeline
        .run([Input::Path(input.clone())], &NoProgress)
        .await
        .unwrap();
    assert!(matches!(
        report.items[0].status,
        ItemStatus::Transcribed { .. }
    ));
    assert_text(&txt, "new recording");
    assert!(
        std::fs::read_to_string(&srt)
            .unwrap()
            .contains("new recording")
    );

    // Without the flag, rewrite existing output at the same destination.
    std::fs::write(&input, "updated recording").unwrap();
    let report = Tafrigh::builder(transcriber.clone())
        .output(output)
        .build()
        .run([Input::Path(input)], &NoProgress)
        .await
        .unwrap();
    assert!(matches!(
        report.items[0].status,
        ItemStatus::Transcribed { .. }
    ));
    assert_text(&txt, "updated recording");
    assert!(
        std::fs::read_to_string(&srt)
            .unwrap()
            .contains("updated recording")
    );
    assert_eq!(transcriber.0.lock().unwrap().len(), 2);
    let mut files: Vec<_> = std::fs::read_dir(root.join("out"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(files, ["talk.srt", "talk.txt"]);
}
