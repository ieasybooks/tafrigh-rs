//! Pipeline behavior for external caption providers.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tafrigh::output::{BuiltinFormat, Format, OutputMode, OutputWriter};
use tafrigh::source::{Input, Media, MediaItem, YtDlp};
use tafrigh::{CaptionProvider, Error, Event, ItemStatus, Result, Segment, Tafrigh};

#[derive(Default)]
struct RecordedCaptions {
    fetched: Mutex<Vec<String>>,
}

#[async_trait]
impl CaptionProvider for RecordedCaptions {
    async fn resolve(&self, url: &str, _ytdlp: &YtDlp) -> Result<Vec<MediaItem>> {
        assert_eq!(url, "https://captions.example/playlist");
        Ok(["skip", "missing", "written"]
            .into_iter()
            .map(|id| MediaItem {
                id: id.into(),
                title: None,
                media: Media::Remote {
                    url: format!("https://captions.example/{id}"),
                },
                link: Some(format!("https://captions.example/{id}")),
            })
            .collect())
    }

    async fn fetch(&self, item: &MediaItem, _ytdlp: &YtDlp) -> Result<Vec<Segment>> {
        self.fetched.lock().unwrap().push(item.id.to_string());
        if &*item.id == "missing" {
            return Err(Error::Source("no captions for this item".into()));
        }
        Ok(vec![Segment::new(1.0, 2.0, "existing transcript")])
    }
}

#[tokio::test]
async fn external_caption_providers_share_layouts_skipping_and_item_failures() {
    for mode in [
        OutputMode::SideBySide,
        OutputMode::Flat,
        OutputMode::TreeToTree,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let downloads = directory.path().join("downloads");
        let outputs = directory.path().join("outputs");
        let expected = if mode == OutputMode::SideBySide {
            &downloads
        } else {
            &outputs
        };
        std::fs::create_dir_all(expected).unwrap();
        std::fs::write(expected.join("skip.txt"), "already written\n").unwrap();
        let provider = Arc::new(RecordedCaptions::default());
        let shared: Arc<dyn CaptionProvider> = provider.clone();
        let pipeline = Tafrigh::captions(shared)
            .output(
                OutputWriter::new(&outputs)
                    .formats([Arc::new(BuiltinFormat::Txt) as Arc<dyn Format>]),
            )
            .output_mode(mode)
            .ytdlp(YtDlp::new(&downloads).program(directory.path().join("no-yt-dlp")))
            .skip_existing(true)
            .build();
        let fetching = Mutex::new(Vec::new());
        let report = pipeline
            .run(
                [Input::parse("https://captions.example/playlist")],
                &|event| {
                    if let Event::FetchingCaptions { item } = event {
                        fetching.lock().unwrap().push(item.id.to_string());
                    }
                },
            )
            .await
            .unwrap();

        assert_eq!(report.failures(), 1);
        assert_eq!(report.items.len(), 3);
        assert!(matches!(report.items[0].status, ItemStatus::Skipped));
        assert!(matches!(report.items[1].status, ItemStatus::Failed(_)));
        assert!(matches!(
            report.items[2].status,
            ItemStatus::Transcribed { .. }
        ));
        assert_eq!(*provider.fetched.lock().unwrap(), ["missing", "written"]);
        assert_eq!(*fetching.lock().unwrap(), ["missing", "written"]);
        assert_eq!(
            std::fs::read_to_string(expected.join("skip.txt")).unwrap(),
            "already written\n"
        );
        assert_eq!(
            std::fs::read_to_string(expected.join("written.txt")).unwrap(),
            "existing transcript\n"
        );
        assert!(!expected.join("missing.txt").exists());
        assert!(pipeline.resolve(&Input::from("local.mp3")).await.is_err());
    }
}
