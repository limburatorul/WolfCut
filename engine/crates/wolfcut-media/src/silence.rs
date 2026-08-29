//! Silence detection: the stretches of a file where the sound drops out.
//!
//! FFmpeg's `silencedetect` filter already decides this, on the decoded
//! samples, in one streaming pass. So this module is a parser and not a DSP:
//! it runs the filter and reads the `silence_start` / `silence_end` lines back
//! out of the log. Writing a detector here would mean re-deciding what "quiet
//! enough" means and then disagreeing with every other tool the user has.
//!
//! Ranges come back in the file's own seconds, not the timeline's, because a
//! clip may be trimmed, retimed, or used twice over. Mapping them onto a clip
//! is `wolfcut-project`'s job, and it belongs there: the document owns where
//! its clips sit, and the arithmetic exists once.

use std::path::Path;
use std::process::Stdio;

use crate::binaries::ffmpeg;
use crate::error::{Error, Result};
use crate::process::{command, summarize};

/// One silent stretch, in seconds from the start of the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Silence {
    /// Where the quiet begins.
    pub start: f64,
    /// Where it ends. FFmpeg closes an unfinished stretch at end-of-file, so
    /// this is always a real time - callers never have to invent one.
    pub end: f64,
}

impl Silence {
    /// How long the stretch lasts, in seconds.
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// Loudest level still counted as silence, in dBFS.
///
/// -50 keeps room tone and preamp hiss on the silent side of the line while
/// leaving a quiet voice on the other. Speech that has been noise-reduced
/// wants a lower number; a noisy handheld recording wants a higher one, which
/// is why this is a default and not a constant in the detector.
pub const DEFAULT_THRESHOLD_DB: f64 = -50.0;

/// Shortest gap worth cutting, in seconds.
///
/// Half a second is about where a pause stops being part of the delivery and
/// starts being dead air. Below roughly a tenth the cuts land inside words.
pub const DEFAULT_MIN_DURATION: f64 = 0.5;

/// Finds the silent stretches in a file's first audio stream.
///
/// `threshold_db` is the level at or below which sound counts as silence, and
/// `min_duration` the shortest stretch to report. Both are clamped into ranges
/// FFmpeg will accept, so a bad number from a caller costs a slightly
/// different result rather than a failed run.
///
/// A file with no audio stream fails the FFmpeg run and the error carries what
/// FFmpeg said, which is the same contract [`crate::peaks::extract`] has.
pub fn detect(path: &Path, threshold_db: f64, min_duration: f64) -> Result<Vec<Silence>> {
    // NaN would reach FFmpeg as the literal "NaN" and fail the run; the bounds
    // are wide enough that no meaningful setting is altered by passing here.
    let threshold_db = if threshold_db.is_nan() { DEFAULT_THRESHOLD_DB } else { threshold_db.clamp(-100.0, 0.0) };
    let min_duration = if min_duration.is_nan() { DEFAULT_MIN_DURATION } else { min_duration.clamp(0.01, 3600.0) };

    // silencedetect reports at info level, so this is the one place in the
    // crate that cannot use `base_command` - its `-loglevel error` would hide
    // the only output the function wants.
    let output = command(ffmpeg())
        .args(["-hide_banner", "-nostdin", "-loglevel", "info"])
        .arg("-i")
        .arg(path)
        // Only the first audio stream, and no pictures: decoding video for a
        // filter that never looks at it is the difference between seconds and
        // minutes on a long file.
        .args(["-map", "0:a:0", "-vn"])
        .args(["-af", &format!("silencedetect=noise={threshold_db}dB:d={min_duration}")])
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| Error::Spawn { program: "ffmpeg", source })?;

    if !output.status.success() {
        return Err(Error::Exited {
            program: "ffmpeg",
            path: path.to_path_buf(),
            status: output.status,
            stderr: summarize(&output.stderr),
        });
    }
    Ok(parse(&String::from_utf8_lossy(&output.stderr)))
}

/// Reads the filter's own lines out of an FFmpeg log.
///
/// The shape, unchanged for many releases:
///
/// ```text
/// [silencedetect @ 0x7f8] silence_start: 2
/// [silencedetect @ 0x7f8] silence_end: 5.000023 | silence_duration: 3.000023
/// ```
///
/// Everything else in the log is skipped. An end with no start is ignored, and
/// a start the log never closes is dropped rather than guessed at: a range
/// with no end cannot be cut safely, and inventing one would delete audio the
/// user still has.
fn parse(log: &str) -> Vec<Silence> {
    let mut found = Vec::new();
    let mut open: Option<f64> = None;
    for line in log.lines() {
        if let Some(start) = field(line, "silence_start:") {
            open = Some(start);
        } else if let Some(end) = field(line, "silence_end:") {
            if let Some(start) = open.take() {
                // A zero-length or backwards range says the log was garbled;
                // either way there is nothing to cut.
                if end > start {
                    found.push(Silence { start, end });
                }
            }
        }
    }
    found
}

/// The number following `label` on one log line.
///
/// The value runs to the next `|`, because `silence_end` carries the duration
/// after one.
fn field(line: &str, label: &str) -> Option<f64> {
    let rest = line.split(label).nth(1)?;
    rest.split('|').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real log, as FFmpeg 8 writes it, with the surrounding noise left in.
    const LOG: &str = "\
Input #0, wav, from 'take.wav':
  Duration: 00:00:10.00, bitrate: 705 kb/s
[silencedetect @ 000002a0b54d4500] silence_start: 2
[silencedetect @ 000002a0b54d4500] silence_end: 5.000023 | silence_duration: 3.000023
[silencedetect @ 000002a0b54d4500] silence_start: 7
[silencedetect @ 000002a0b54d4500] silence_end: 10 | silence_duration: 3
size=N/A time=00:00:10.00 bitrate=N/A speed=612x";

    #[test]
    fn reads_every_pair_and_ignores_the_rest_of_the_log() {
        assert_eq!(
            parse(LOG),
            vec![Silence { start: 2.0, end: 5.000023 }, Silence { start: 7.0, end: 10.0 }],
        );
    }

    #[test]
    fn a_start_the_log_never_closes_is_dropped() {
        // Truncated output - a killed child, a full pipe. Cutting from the
        // open start to a guessed end would delete sound that is still there.
        let log = "[silencedetect @ 0x1] silence_start: 2\n";
        assert!(parse(log).is_empty());
    }

    #[test]
    fn an_end_without_a_start_is_ignored() {
        let log = "[silencedetect @ 0x1] silence_end: 5 | silence_duration: 3\n";
        assert!(parse(log).is_empty());
    }

    #[test]
    fn a_backwards_or_empty_range_is_not_a_cut() {
        let log = "\
[silencedetect @ 0x1] silence_start: 5
[silencedetect @ 0x1] silence_end: 5 | silence_duration: 0
[silencedetect @ 0x1] silence_start: 9
[silencedetect @ 0x1] silence_end: 8 | silence_duration: -1";
        assert!(parse(log).is_empty());
    }

    #[test]
    fn a_log_with_no_filter_lines_finds_nothing() {
        assert!(parse("Input #0, wav\nsize=N/A time=00:00:10.00").is_empty());
    }

    #[test]
    fn duration_is_the_span() {
        assert_eq!(Silence { start: 2.0, end: 5.5 }.duration(), 3.5);
    }
}
