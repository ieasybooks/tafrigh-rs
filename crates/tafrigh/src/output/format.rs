use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;
use std::sync::Arc;

use crate::Segment;

/// A transcript file format.
pub trait Format: Send + Sync + fmt::Debug {
    /// File extension, without the dot.
    fn extension(&self) -> &str;

    /// Writes `segments` (in time order) to `out`.
    fn write(&self, segments: &[Segment], out: &mut dyn Write) -> io::Result<()>;

    /// Whether the format is only meaningful with word timings.
    fn needs_words(&self) -> bool {
        false
    }
}

/// Formats shipped with tafrigh.
///
/// The `Words*` formats list individual words with their timings (from recognizers that
/// provide them): one cue per word for srt/vtt, one row per word for csv/tsv/json. They are
/// written as `<id>.words.<ext>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinFormat {
    /// Plain text, one segment per line.
    Txt,
    /// SRT subtitles.
    Srt,
    /// VTT subtitles.
    Vtt,
    /// Comma-separated `start,end,text`.
    Csv,
    /// Tab-separated `start,end,text`.
    Tsv,
    /// JSON array of segments, with nested word timings when present.
    Json,
    /// SRT subtitles with one cue per word.
    WordsSrt,
    /// VTT subtitles with one cue per word.
    WordsVtt,
    /// Comma-separated `start,end,word,confidence,segment`.
    WordsCsv,
    /// Tab-separated `start,end,word,confidence,segment`.
    WordsTsv,
    /// JSON array of words with their segment number.
    WordsJson,
}

impl BuiltinFormat {
    /// Segment-level formats.
    pub const SEGMENT_LEVEL: [Self; 6] = [
        Self::Txt,
        Self::Srt,
        Self::Vtt,
        Self::Csv,
        Self::Tsv,
        Self::Json,
    ];

    /// Word-level formats.
    pub const WORD_LEVEL: [Self; 5] = [
        Self::WordsSrt,
        Self::WordsVtt,
        Self::WordsCsv,
        Self::WordsTsv,
        Self::WordsJson,
    ];

    /// Name used on the command line, e.g. `srt` or `words-srt`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::WordsSrt => "words-srt",
            Self::WordsVtt => "words-vtt",
            Self::WordsCsv => "words-csv",
            Self::WordsTsv => "words-tsv",
            Self::WordsJson => "words-json",
        }
    }

    /// The word-level counterpart of a segment format (none for txt).
    pub fn words(self) -> Option<Self> {
        match self {
            Self::Srt => Some(Self::WordsSrt),
            Self::Vtt => Some(Self::WordsVtt),
            Self::Csv => Some(Self::WordsCsv),
            Self::Tsv => Some(Self::WordsTsv),
            Self::Json => Some(Self::WordsJson),
            _ => None,
        }
    }

    /// True for the `Words*` formats.
    pub fn is_word_level(self) -> bool {
        Self::WORD_LEVEL.contains(&self)
    }
}

impl From<BuiltinFormat> for Arc<dyn Format> {
    fn from(format: BuiltinFormat) -> Self {
        Arc::new(format)
    }
}

impl fmt::Display for BuiltinFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for BuiltinFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut all = Self::SEGMENT_LEVEL.into_iter().chain(Self::WORD_LEVEL);
        all.find(|f| f.name().eq_ignore_ascii_case(value))
            .ok_or_else(|| {
                let names: Vec<&str> = Self::SEGMENT_LEVEL
                    .iter()
                    .chain(&Self::WORD_LEVEL)
                    .map(|f| f.name())
                    .collect();
                format!(
                    "unknown format `{value}` (expected one of {})",
                    names.join(", ")
                )
            })
    }
}

/// A word with the 1-based index of the segment it belongs to.
#[derive(serde::Serialize)]
struct WordRow<'a> {
    start: f64,
    end: f64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f32>,
    segment: usize,
}

fn word_rows(segments: &[Segment]) -> impl Iterator<Item = WordRow<'_>> {
    segments.iter().enumerate().flat_map(|(i, segment)| {
        segment.words.iter().map(move |word| WordRow {
            start: word.start,
            end: word.end,
            text: word.text.trim(),
            confidence: word.confidence,
            segment: i + 1,
        })
    })
}

fn write_delimited(
    out: &mut dyn Write,
    delimiter: u8,
    header: &[&str],
    rows: impl Iterator<Item = Vec<String>>,
) -> io::Result<()> {
    let mut writer = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(out);
    writer.write_record(header)?;
    for row in rows {
        writer.write_record(row)?;
    }
    writer.flush()?;
    Ok(())
}

impl Format for BuiltinFormat {
    fn needs_words(&self) -> bool {
        self.is_word_level()
    }

    fn extension(&self) -> &str {
        match self {
            Self::WordsSrt => "words.srt",
            Self::WordsVtt => "words.vtt",
            Self::WordsCsv => "words.csv",
            Self::WordsTsv => "words.tsv",
            Self::WordsJson => "words.json",
            other => other.name(),
        }
    }

    fn write(&self, segments: &[Segment], out: &mut dyn Write) -> io::Result<()> {
        let delimiter = match self {
            Self::Tsv | Self::WordsTsv => b'\t',
            _ => b',',
        };
        match self {
            Self::Txt => {
                for segment in segments {
                    writeln!(out, "{}", segment.text.trim())?;
                }
            }
            Self::Srt => {
                let cues = segments.iter().map(|s| (s.start, s.end, s.text.trim()));
                write_srt(out, cues)?;
            }
            Self::Vtt => {
                let cues = segments.iter().map(|s| (s.start, s.end, s.text.trim()));
                write_vtt(out, cues)?;
            }
            Self::Csv | Self::Tsv => write_delimited(
                out,
                delimiter,
                &["start", "end", "text"],
                segments.iter().map(|s| {
                    vec![
                        format!("{:.3}", s.start),
                        format!("{:.3}", s.end),
                        s.text.trim().to_owned(),
                    ]
                }),
            )?,
            Self::Json => {
                serde_json::to_writer_pretty(&mut *out, segments)?;
                writeln!(out)?;
            }
            Self::WordsSrt => {
                write_srt(out, word_rows(segments).map(|w| (w.start, w.end, w.text)))?;
            }
            Self::WordsVtt => {
                write_vtt(out, word_rows(segments).map(|w| (w.start, w.end, w.text)))?;
            }
            Self::WordsCsv | Self::WordsTsv => write_delimited(
                out,
                delimiter,
                &["start", "end", "word", "confidence", "segment"],
                word_rows(segments).map(|w| {
                    vec![
                        format!("{:.3}", w.start),
                        format!("{:.3}", w.end),
                        w.text.to_owned(),
                        w.confidence.map(|c| format!("{c:.3}")).unwrap_or_default(),
                        w.segment.to_string(),
                    ]
                }),
            )?,
            Self::WordsJson => {
                let rows: Vec<WordRow<'_>> = word_rows(segments).collect();
                serde_json::to_writer_pretty(&mut *out, &rows)?;
                writeln!(out)?;
            }
        }
        Ok(())
    }
}

fn write_srt<'a>(
    out: &mut dyn Write,
    cues: impl Iterator<Item = (f64, f64, &'a str)>,
) -> io::Result<()> {
    for (i, (start, end, text)) in cues.enumerate() {
        writeln!(
            out,
            "{}\n{} --> {}\n{}\n",
            i + 1,
            srt_timestamp(start),
            srt_timestamp(end),
            text
        )?;
    }
    Ok(())
}

fn write_vtt<'a>(
    out: &mut dyn Write,
    cues: impl Iterator<Item = (f64, f64, &'a str)>,
) -> io::Result<()> {
    writeln!(out, "WEBVTT\n")?;
    for (start, end, text) in cues {
        writeln!(
            out,
            "{} --> {}\n{}\n",
            vtt_timestamp(start),
            vtt_timestamp(end),
            text
        )?;
    }
    Ok(())
}

/// `HH:MM:SS,mmm`, as in SRT.
pub(super) fn srt_timestamp(seconds: f64) -> String {
    let (hours, minutes, secs, ms) = clock(seconds);
    format!("{hours:02}:{minutes:02}:{secs:02},{ms:03}")
}

/// `MM:SS.mmm`, or `HH:MM:SS.mmm` from the first hour on, as in VTT.
fn vtt_timestamp(seconds: f64) -> String {
    let (hours, minutes, secs, ms) = clock(seconds);
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{secs:02}.{ms:03}")
    } else {
        format!("{minutes:02}:{secs:02}.{ms:03}")
    }
}

/// Hours, minutes, seconds and milliseconds in `seconds`, rounded to the millisecond.
fn clock(seconds: f64) -> (u64, u64, u64, u64) {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    (
        total_ms / 3_600_000,
        total_ms / 60_000 % 60,
        total_ms / 1000 % 60,
        total_ms % 1000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(format: BuiltinFormat, segments: &[Segment]) -> String {
        let mut out = Vec::new();
        format.write(segments, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn timestamps() {
        assert_eq!(srt_timestamp(3725.4567), "01:02:05,457");
        assert_eq!(srt_timestamp(0.0), "00:00:00,000");
        assert_eq!(vtt_timestamp(65.5), "01:05.500");
        assert_eq!(vtt_timestamp(3725.4567), "01:02:05.457");
    }

    #[test]
    fn renders_word_formats() {
        let mut first = Segment::new(0.5, 2.25, "مرحبا بكم");
        first.words = vec![
            crate::Word {
                start: 0.5,
                end: 1.0,
                text: "مرحبا".into(),
                confidence: Some(0.9),
            },
            crate::Word {
                start: 1.2,
                end: 2.25,
                text: "بكم".into(),
                confidence: None,
            },
        ];
        let second = Segment::new(3.0, 4.0, "no timings");
        let segments = [first, second];
        assert_eq!(
            render(BuiltinFormat::WordsSrt, &segments),
            "1\n00:00:00,500 --> 00:00:01,000\nمرحبا\n\n2\n00:00:01,200 --> 00:00:02,250\nبكم\n\n"
        );
        assert_eq!(
            render(BuiltinFormat::WordsVtt, &segments),
            "WEBVTT\n\n00:00.500 --> 00:01.000\nمرحبا\n\n00:01.200 --> 00:02.250\nبكم\n\n"
        );
        assert_eq!(
            render(BuiltinFormat::WordsTsv, &segments),
            "start\tend\tword\tconfidence\tsegment\n0.500\t1.000\tمرحبا\t0.900\t1\n1.200\t2.250\tبكم\t\t1\n"
        );
        let json: serde_json::Value =
            serde_json::from_str(&render(BuiltinFormat::WordsJson, &segments)).unwrap();
        assert_eq!(
            json[1],
            serde_json::json!({"start": 1.2, "end": 2.25, "text": "بكم", "segment": 1})
        );
        assert_eq!(BuiltinFormat::WordsCsv.extension(), "words.csv");
        assert_eq!(
            "words-vtt".parse::<BuiltinFormat>(),
            Ok(BuiltinFormat::WordsVtt)
        );
        assert_eq!(BuiltinFormat::Srt.words(), Some(BuiltinFormat::WordsSrt));
        assert_eq!(BuiltinFormat::Txt.words(), None);
    }

    #[test]
    fn renders_formats() {
        let segments = [
            Segment::new(0.5, 2.25, "مرحبا بكم"),
            Segment::new(3.0, 4.0, "hello, \"world\""),
        ];
        assert_eq!(
            render(BuiltinFormat::Srt, &segments),
            "1\n00:00:00,500 --> 00:00:02,250\nمرحبا بكم\n\n2\n00:00:03,000 --> 00:00:04,000\nhello, \"world\"\n\n"
        );
        assert_eq!(
            render(BuiltinFormat::Vtt, &segments),
            "WEBVTT\n\n00:00.500 --> 00:02.250\nمرحبا بكم\n\n00:03.000 --> 00:04.000\nhello, \"world\"\n\n"
        );
        assert_eq!(
            render(BuiltinFormat::Txt, &segments),
            "مرحبا بكم\nhello, \"world\"\n"
        );
        assert_eq!(
            render(BuiltinFormat::Csv, &segments),
            "start,end,text\n0.500,2.250,مرحبا بكم\n3.000,4.000,\"hello, \"\"world\"\"\"\n"
        );
        let json: Vec<Segment> =
            serde_json::from_str(&render(BuiltinFormat::Json, &segments)).unwrap();
        assert_eq!(json, segments);
    }
}
