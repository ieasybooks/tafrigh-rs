use crate::Segment;

/// Merges segments with fewer than `min_words` words into the segments that follow them,
/// until each has at least `min_words` (the last one may stay short). `0` returns a copy.
pub(super) fn compact(segments: &[Segment], min_words: usize) -> Vec<Segment> {
    if min_words == 0 {
        return segments.to_vec();
    }
    let mut out = Vec::with_capacity(segments.len());
    let mut pending: Option<Segment> = None;
    for segment in segments {
        match pending.as_mut() {
            Some(acc) => {
                let text = segment.text.trim();
                if !text.is_empty() {
                    if !acc.text.is_empty() {
                        acc.text.push(' ');
                    }
                    acc.text.push_str(text);
                }
                acc.end = segment.end;
                acc.words.extend(segment.words.iter().cloned());
                if acc.word_count() >= min_words {
                    out.extend(pending.take());
                }
            }
            None if segment.word_count() < min_words => pending = Some(segment.clone()),
            None => out.push(segment.clone()),
        }
    }
    out.extend(pending);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment::new(start, end, text)
    }

    #[test]
    fn merges_short_segments_forward() {
        let input = [
            seg(0.0, 1.0, "a"),
            seg(1.0, 2.0, "b c"),
            seg(2.0, 3.0, "d e f"),
            seg(3.0, 4.0, "g"),
        ];
        let out = compact(&input, 3);
        assert_eq!(
            out,
            vec![
                seg(0.0, 2.0, "a b c"),
                seg(2.0, 3.0, "d e f"),
                seg(3.0, 4.0, "g")
            ]
        );
        assert_eq!(compact(&input, 0), input.to_vec());
        assert_eq!(compact(&input, 1), input.to_vec());
    }
}
