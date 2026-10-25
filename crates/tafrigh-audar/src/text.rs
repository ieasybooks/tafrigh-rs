//! Audar output normalization, retaining the evaluated qwen-asr repetition repair.
//
// Repetition repair adapted from qwen-asr 0.0.6, inference/utils.py.
// Copyright 2026 The Alibaba Qwen team. SPDX-License-Identifier: Apache-2.0.
// Modified: ported to Rust; added Audar output metadata and prompt-echo handling.
// See licenses/LICENSE-qwen-asr.

const PROMPT_ECHO: &str = "فرّغ الكلام العربي التالي.";

/// The shortest trailing pattern of up to 16 tokens repeated at least 10 times.
fn repeated_period(ids: &[i32]) -> Option<usize> {
    const REPEATS: usize = 10;
    (1..=16).find(|&period| {
        let span = period * REPEATS;
        ids.len() >= span && {
            let tail = &ids[ids.len() - span..];
            tail.iter().zip(&tail[period..]).all(|(a, b)| a == b)
        }
    })
}

/// Generated token pieces, including byte boundaries for trimming a repeating tail.
#[derive(Default)]
pub(crate) struct GeneratedText {
    ids: Vec<i32>,
    byte_ends: Vec<usize>,
    bytes: Vec<u8>,
}

impl GeneratedText {
    pub(crate) fn push(&mut self, id: i32, piece: &[u8]) {
        self.ids.push(id);
        self.bytes.extend_from_slice(piece);
        self.byte_ends.push(self.bytes.len());
    }

    /// Keep one copy of a repeating tail and return its before/after byte lengths.
    pub(crate) fn trim_repetition(&mut self) -> Option<(usize, usize)> {
        let period = repeated_period(&self.ids)?;
        let before = self.bytes.len();
        while self.ids.len() >= 2 * period {
            let end = self.ids.len();
            if self.ids[end - period..] != self.ids[end - 2 * period..end - period] {
                break;
            }
            self.ids.truncate(end - period);
        }
        self.byte_ends.truncate(self.ids.len());
        self.bytes
            .truncate(self.byte_ends.last().copied().unwrap_or(0));
        Some((before, self.bytes.len()))
    }

    pub(crate) fn finish(self) -> String {
        clean(&String::from_utf8_lossy(&self.bytes))
    }
}

/// qwen-asr's `detect_and_fix_repetitions`: runs of one character over `threshold` become one
/// character, and the first pattern of up to 20 characters repeated `threshold`+ times is kept once.
fn fix_repetitions(text: &str, threshold: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut runs = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let mut count = 1;
        while i + count < chars.len() && chars[i + count] == chars[i] {
            count += 1;
        }
        if count > threshold {
            runs.push(chars[i]);
        } else {
            runs.extend_from_slice(&chars[i..i + count]);
        }
        i += count;
    }
    fix_patterns(&runs, threshold).into_iter().collect()
}

fn fix_patterns(s: &[char], threshold: usize) -> Vec<char> {
    let n = s.len();
    if n < threshold * 2 {
        return s.to_vec();
    }
    let mut result = Vec::new();
    let mut i = 0;
    while i + threshold * 2 <= n {
        for k in 1..=20 {
            if i + k * threshold > n {
                break;
            }
            let pattern = &s[i..i + k];
            if (1..threshold).all(|rep| &s[i + rep * k..i + rep * k + k] == pattern) {
                let mut end = i + threshold * k;
                while end + k <= n && &s[end..end + k] == pattern {
                    end += k;
                }
                result.extend_from_slice(pattern);
                result.extend(fix_patterns(&s[end..], threshold));
                return result;
            }
        }
        result.push(s[i]);
        i += 1;
    }
    result.extend_from_slice(&s[i..]);
    result
}

/// Strips Qwen3-ASR's `language <Lang><asr_text>` wrapper; `language None` means no speech.
/// qwen-asr repair collapses character runs longer than 20 and patterns of up to 20
/// characters repeated at least 20 times. Token loops are trimmed by `GeneratedText`
/// before this cleanup. An echo of the system prompt counts as no speech.
pub(crate) fn clean(raw: &str) -> String {
    let fixed = fix_repetitions(raw.trim(), 20);
    let s = fixed.trim();
    if s.eq_ignore_ascii_case("language None") || s == "language Arabic" {
        return String::new();
    }
    let text = match s.split_once("<asr_text>") {
        Some((meta, text)) => {
            if meta.to_lowercase().contains("language none") {
                return String::new();
            }
            text
        }
        None => s,
    };
    let text = text.replace("</asr_text>", "").trim().to_owned();
    if text.trim_end_matches(['.', ' ']) == PROMPT_ECHO.trim_end_matches(['.', ' ']) {
        return String::new();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_metadata_and_treats_no_speech_and_prompt_echoes_as_empty() {
        for raw in [
            "",
            "  ",
            "language None",
            "language None<asr_text>ignored",
            "language Arabic<asr_text>",
            PROMPT_ECHO,
        ] {
            assert_eq!(clean(raw), "", "{raw}");
        }
        assert_eq!(
            clean("language Arabic<asr_text> مرحبا. </asr_text>"),
            "مرحبا."
        );
        assert_eq!(clean("  السلام عليكم  "), "السلام عليكم");
        assert_eq!(
            clean(&format!("language Arabic<asr_text>{PROMPT_ECHO}")),
            ""
        );
    }

    #[test]
    fn repairs_repeated_characters_and_phrases_without_changing_normal_speech() {
        assert_eq!(clean(&"ب".repeat(21)), "ب");
        assert_eq!(clean(&"السلام".repeat(20)), "السلام");
        assert_eq!(clean("لا لا لا إله إلا الله"), "لا لا لا إله إلا الله");
    }

    #[test]
    fn stops_only_after_ten_repetitions_of_a_short_token_pattern() {
        assert_eq!(repeated_period(&[7; 9]), None);
        assert_eq!(repeated_period(&[7; 10]), Some(1));
        assert_eq!(repeated_period(&[1, 2, 3].repeat(10)), Some(3));
        assert_eq!(repeated_period(&(0..30).collect::<Vec<_>>()), None);
        assert_eq!(
            repeated_period(&(0..16).collect::<Vec<_>>().repeat(10)),
            Some(16)
        );
        assert_eq!(
            repeated_period(&(0..17).collect::<Vec<_>>().repeat(10)),
            None
        );
    }

    #[test]
    fn repetition_stop_keeps_prefix_and_one_copy_before_cleanup() {
        for pieces in [
            vec!["السلام", " عليكم", " "],
            vec!["مرحبا "],
            vec!["السلام عليكم ", "ورحمة الله ", "وبركاته "],
        ] {
            let mut output = GeneratedText::default();
            let prefix = "language Arabic<asr_text>قال الشيخ ";
            output.push(100, prefix.as_bytes());
            for repeat in 0..10 {
                for (id, piece) in pieces.iter().enumerate() {
                    output.push(id as i32, piece.as_bytes());
                    let trimmed = output.trim_repetition();
                    if repeat == 9 && id == pieces.len() - 1 {
                        let block = pieces.concat();
                        assert_eq!(
                            trimmed,
                            Some((prefix.len() + block.len() * 10, prefix.len() + block.len()))
                        );
                    } else {
                        assert_eq!(trimmed, None);
                    }
                }
            }
            assert_eq!(
                output.finish(),
                format!("قال الشيخ {}", pieces.concat().trim())
            );
        }
    }

    #[test]
    fn trims_all_copies_and_preserves_split_utf8_pieces() {
        let mut output = GeneratedText::default();
        output.push(100, "قال ".as_bytes());
        let block = "ب ".as_bytes();
        for _ in 0..12 {
            output.push(1, &block[..1]);
            output.push(2, &block[1..]);
            output.push(3, b"");
        }
        assert_eq!(
            output.trim_repetition(),
            Some(("قال ".len() + 12 * block.len(), "قال ".len() + block.len()))
        );
        assert_eq!(output.finish(), "قال ب");
    }

    #[test]
    fn leaves_short_repetitions_and_normal_speech_unchanged() {
        let mut output = GeneratedText::default();
        for _ in 0..9 {
            output.push(1, "مرحبا ".as_bytes());
            assert_eq!(output.trim_repetition(), None);
        }
        output.push(2, "بكم".as_bytes());
        assert_eq!(output.trim_repetition(), None);
        assert_eq!(output.finish(), format!("{}بكم", "مرحبا ".repeat(9)));
    }
}
