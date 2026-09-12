//! Text clip content: SRT/VTT subtitle import + plain text output.
//!
//! The parsing helpers are exercised by `#[cfg(test)]` unit tests; in the
//! release binary the editor only uses `text_to_subtitle` directly.

#![allow(dead_code)]

use crate::timeline::Timecode;

#[derive(Debug, Clone, PartialEq)]
pub struct Subtitle {
    pub start_us: i64,
    pub end_us: i64,
    pub text: String,
}

/// Parse SRT subtitle data.
pub fn parse_srt(input: &str) -> Vec<Subtitle> {
    let mut subs = Vec::new();
    for block in input.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut lines = block.lines();
        let _num = lines.next(); // cue index (optional)
        let Some(time) = lines.next() else { continue };
        let Some((start, end)) = parse_time_range(time) else { continue };
        let text = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        if text.is_empty() {
            continue;
        }
        subs.push(Subtitle {
            start_us: start,
            end_us: end,
            text,
        });
    }
    subs
}

/// Parse VTT subtitle data (also accepts SRT since the formats overlap).
pub fn parse_vtt(input: &str) -> Vec<Subtitle> {
    let mut subs = Vec::new();
    for block in input.split("\n\n") {
        let block = block.trim();
        if block.is_empty() || block.starts_with("WEBVTT") || block.contains("NOTE") {
            continue;
        }
        let time = block
            .lines()
            .find(|l| l.contains("-->"))
            .unwrap_or_default();
        let Some((start, end)) = parse_time_range(time) else { continue };
        let text = block
            .lines()
            .filter(|l| !l.contains("-->"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        subs.push(Subtitle { start_us: start, end_us: end, text });
    }
    subs
}

/// Parse either format by sniffing for WEBVTT.
pub fn parse_subtitles(input: &str) -> Vec<Subtitle> {
    if input.trim_start().starts_with("WEBVTT") {
        parse_vtt(input)
    } else {
        parse_srt(input)
    }
}

fn parse_time_range(s: &str) -> Option<(i64, i64)> {
    let mut parts = s.split("-->");
    let start = parse_timestamp(parts.next()?.trim())?;
    let end = parse_timestamp(parts.next()?.trim())?;
    Some((start, end))
}

fn parse_timestamp(s: &str) -> Option<i64> {
    // mm:ss,mmm | hh:mm:ss,mmm | ss.mmm  | hh:mm:ss.mmm
    let s = s.replace(',', ".");
    let (hms, frac) = match s.rsplit_once('.') {
        Some((h, f)) => (h.to_string(), f.chars().take(3).collect::<String>()),
        None => (s, "000".to_string()),
    };
    let millis: f64 = frac.parse().ok()?;
    let parts: Vec<i64> = hms
        .split(':')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut secs = *parts.last()?;
    if parts.len() >= 2 {
        secs += parts[parts.len() - 2] * 60;
    }
    if parts.len() >= 3 {
        secs += parts[parts.len() - 3] * 3600;
    }
    Some((secs as f64 * 1_000_000.0 + millis * 1000.0) as i64)
}

/// Turn a text clip into something displayable across time: for a plain text
/// block this is a single subtitle covering the whole clip.
pub fn text_to_subtitle(text: &str, start_us: Timecode, duration_us: i64) -> Subtitle {
    Subtitle {
        start_us,
        end_us: start_us + duration_us.max(0),
        text: text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_srt() {
        let srt = "1\n00:00:01,000 --> 00:00:03,500\nHello world\n\n2\n00:00:04,000 --> 00:00:05,000\nSecond line";
        let subs = parse_srt(srt);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].start_us, 1_000_000);
        assert_eq!(subs[0].end_us, 3_500_000);
        assert_eq!(subs[0].text, "Hello world");
    }

    #[test]
    fn parses_vtt() {
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.500\nHi there";
        let subs = parse_vtt(vtt);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].start_us, 1_000_000);
        assert_eq!(subs[0].text, "Hi there");
    }

    #[test]
    fn timecode_helpers() {
        assert_eq!(parse_timestamp("00:00:01,500").unwrap(), 1_500_000);
        assert_eq!(parse_timestamp("01:02.250").unwrap(), 62_250_000);
    }
}