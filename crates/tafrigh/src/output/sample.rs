use std::path::{Path, PathBuf};

use rand::seq::SliceRandom;

use crate::{Error, Result, Segment};

/// One segment of a run, with where it came from.
#[derive(Debug, Clone)]
pub struct SampleRow {
    /// The (compacted) segment.
    pub segment: Segment,
    /// Item id it belongs to.
    pub item: String,
    /// Link to the original, timestamped in the output when it is a web URL.
    pub link: Option<String>,
    /// The media file transcribed.
    pub file: Option<PathBuf>,
}

/// Keeps a fixed-size random sample of rows using reservoir sampling.
#[derive(Debug, Default)]
pub struct SampleReservoir {
    capacity: usize,
    seen: usize,
    rows: Vec<SampleRow>,
}

impl SampleReservoir {
    /// Creates an empty reservoir that keeps at most `capacity` rows.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            seen: 0,
            rows: Vec::new(),
        }
    }

    /// Adds one row, replacing an existing row with Algorithm R's probability when full.
    pub fn push(&mut self, row: SampleRow) {
        if self.capacity == 0 {
            return;
        }
        self.seen = self.seen.saturating_add(1);
        if self.rows.len() < self.capacity {
            self.rows.push(row);
        } else {
            let index = rand::random_range(0..self.seen);
            if index < self.capacity {
                self.rows[index] = row;
            }
        }
    }

    /// Rows currently retained by the reservoir.
    pub fn rows(&self) -> &[SampleRow] {
        &self.rows
    }

    /// Takes ownership of the rows currently retained by the reservoir.
    pub fn into_rows(self) -> Vec<SampleRow> {
        self.rows
    }
}

/// Writes `count` randomly chosen rows to a CSV file for spot-checking transcripts.
pub fn write_sample(path: &Path, rows: &[SampleRow], count: usize) -> Result<()> {
    let mut picked: Vec<&SampleRow> = rows.iter().collect();
    picked.shuffle(&mut rand::rng());
    picked.truncate(count);
    picked.sort_by(|a, b| {
        a.item
            .cmp(&b.item)
            .then(a.segment.start.total_cmp(&b.segment.start))
    });

    let io_err = |err: std::io::Error| Error::io(format!("writing {}", path.display()), err);
    let mut writer = csv::Writer::from_path(path).map_err(|err| io_err(err.into()))?;
    let write = || -> csv::Result<()> {
        writer.write_record(["item", "start", "end", "text", "link", "file"])?;
        for row in picked {
            writer.write_record([
                row.item.as_str(),
                &super::format_timestamp(row.segment.start, true, ','),
                &super::format_timestamp(row.segment.end, true, ','),
                row.segment.text.trim(),
                &row.link
                    .as_deref()
                    .map(|link| timestamped_link(link, row.segment.start))
                    .unwrap_or_default(),
                &row.file
                    .as_deref()
                    .map(|file| file.display().to_string())
                    .unwrap_or_default(),
            ])?;
        }
        writer.flush()?;
        Ok(())
    };
    write().map_err(|err| io_err(err.into()))
}

/// Adds a `t=<seconds>` parameter to web links.
fn timestamped_link(link: &str, start: f64) -> String {
    if !(link.starts_with("http://") || link.starts_with("https://")) {
        return link.to_owned();
    }
    let separator = if link.contains('?') { '&' } else { '?' };
    format!("{link}{separator}t={}", start.floor() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: usize) -> SampleRow {
        SampleRow {
            segment: Segment::new(index as f64, index as f64 + 1.0, format!("row {index}")),
            item: "item".into(),
            link: None,
            file: None,
        }
    }

    #[test]
    fn links() {
        assert_eq!(
            timestamped_link("https://www.youtube.com/watch?v=x", 12.7),
            "https://www.youtube.com/watch?v=x&t=12"
        );
        assert_eq!(
            timestamped_link("https://youtu.be/x", 3.0),
            "https://youtu.be/x?t=3"
        );
        assert_eq!(timestamped_link("/a/b.mp3", 3.0), "/a/b.mp3");
    }

    #[test]
    fn reservoir_respects_capacity_and_preserves_word_timings() {
        let mut reservoir = SampleReservoir::new(3);
        let mut timed = row(0);
        timed.segment.words.push(crate::Word {
            start: 0.0,
            end: 0.5,
            text: "word".into(),
            confidence: Some(0.9),
        });
        reservoir.push(timed);
        reservoir.push(row(1));
        reservoir.push(row(2));
        assert_eq!(reservoir.rows().len(), 3);
        assert!(
            reservoir
                .rows()
                .iter()
                .filter_map(|sample| sample.segment.words.first())
                .any(|word| word.text == "word")
        );
        reservoir.push(row(3));
        assert_eq!(reservoir.rows().len(), 3);
    }

    #[test]
    fn reservoir_keeps_all_rows_below_capacity() {
        let mut reservoir = SampleReservoir::new(4);
        reservoir.push(row(0));
        reservoir.push(row(1));
        let rows = reservoir.into_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .map(|row| row.segment.text.as_str())
                .collect::<Vec<_>>(),
            ["row 0", "row 1"]
        );
    }

    #[test]
    fn reservoir_eventually_selects_every_index() {
        let mut picked = [false; 10];
        for _ in 0..10_000 {
            let mut reservoir = SampleReservoir::new(1);
            for index in 0..picked.len() {
                reservoir.push(row(index));
            }
            let index = reservoir.rows()[0].segment.start as usize;
            picked[index] = true;
        }
        assert!(picked.into_iter().all(|was_picked| was_picked));
    }
}
