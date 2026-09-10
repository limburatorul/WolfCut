//! Small stand-ins that make scrubbing a big file bearable.
//!
//! Seeking through the subprocess backend costs a fresh FFmpeg per jump, and
//! most of that is decoding: measured on this machine, one preview frame from
//! 4K costs about 260 ms, of which roughly 70 ms is starting the process and
//! the rest is the decode. A 540p stand-in with dense keyframes brings the same
//! frame to about 100 ms - 4K then scrubs faster than 1080p originals do.
//!
//! Already-efficient sources are the ones to watch. Surveillance HEVC at a
//! megabit and a half can transcode into something *larger* than what it
//! stands in for, so the encoder settings below were measured against exactly
//! that footage rather than against camera originals.
//!
//! What a proxy is here: same pictures, same timing, no audio, small. Only the
//! preview path uses them - [`crate::pool::ReaderPool`] is what substitutes,
//! and the exporter opens its own decoders on the originals - so a proxy can
//! never reach the finished file. That separation is the whole safety
//! argument, and it is structural rather than a rule someone has to remember.
//!
//! Timing is the one invariant that matters. Frames are addressed by *time*,
//! and the pool probes whichever file it opens, so a proxy has to keep the
//! source's frame rate exactly: no `-r`, no frame dropping, only a scale.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::binaries::ffmpeg;
use crate::error::{Error, Result};
use crate::process::{command, StderrTail};

/// The heights offered as settings.
///
/// 540 is exactly what the monitor draws at the default half-resolution
/// preview, and it is the fastest of the three; 1080 is for previewing 4K at
/// full quality. 720 sits between them and is the default.
pub const HEIGHTS: [u32; 3] = [540, 720, 1080];

/// The default: 720p is already more than the monitor shows at the default
/// half-resolution preview, and it is the cheapest of the useful sizes.
pub const DEFAULT_HEIGHT: u32 = 720;

/// Whether a source is big enough for a stand-in to be worth building.
///
/// The height is the whole test: a proxy at or above the source's own size
/// would cost a transcode to decode no faster.
pub fn worth_proxying(source_height: u32, proxy_height: u32) -> bool {
    source_height > proxy_height
}

/// The proxy's file name: the source's path, hashed, plus the height.
///
/// FNV-1a 64 over the absolute path, the same choice - and for the same
/// reason - as the artwork and audio caches: these name files that outlive the
/// process, and `DefaultHasher` is free to change between Rust releases. The
/// height rides in the name so changing the setting builds a new one instead
/// of serving the old size.
pub fn file_name(original: &Path, height: u32) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in original.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}-{height}p.mp4")
}

/// Where `original`'s proxy lives inside `directory`.
pub fn path_in(directory: &Path, original: &Path, height: u32) -> PathBuf {
    directory.join(file_name(original, height))
}

/// Builds the stand-in for `original` at `destination`.
///
/// Written to a neighbouring `.partial` and renamed at the end, so a run that
/// is killed - a crash, a closed lid - leaves nothing that a later
/// `is_file()` would mistake for a finished proxy. That check is how the pool
/// decides whether a proxy exists, and it has no way to tell a truncated file
/// from a whole one.
///
/// `on_progress` is called with the seconds of source encoded so far, as
/// often as FFmpeg reports them. A surveillance hour takes minutes to
/// transcode, and without this the only honest thing the UI could say about
/// it was "working".
pub fn generate(
    original: &Path,
    destination: &Path,
    height: u32,
    mut on_progress: impl FnMut(f64),
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io { program: "ffmpeg", source })?;
    }
    let partial = destination.with_extension("partial");

    let mut child = command(ffmpeg())
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        // Decoding is the whole cost of a proxy - 18 of the 21 seconds it
        // took to transcode five minutes of 1440p HEVC - so this is the only
        // place worth accelerating. It buys about a tenth, not the multiple
        // one might hope for: a hardware decode still copies every frame back
        // to system memory for the scaler, and that copy eats most of what
        // the decode saved. `auto` falls back to software on its own, which
        // is what makes it safe to ask for unconditionally.
        .args(["-hwaccel", "auto"])
        .arg("-i")
        .arg(original)
        // Video only. Sound is never served from here - playback decodes the
        // original and the waveform is drawn from it - so carrying a copy
        // would be size and encode time spent on nothing.
        .args(["-map", "0:v:0", "-an", "-sn", "-dn"])
        // -2 rather than -1: the width has to stay even for yuv420p, and an
        // odd one fails the encode rather than rounding.
        .args(["-vf", &format!("scale=-2:{height}")])
        // Keyframe density turns out not to matter at all, which was a
        // surprise: seeking a proxy keyed every 60 frames measures the same
        // 174 ms as one keyed every 12, because the FFmpeg spawn dominates so
        // completely that where the decode starts is lost in it. Dense keys
        // were therefore costing 4.4x the size for nothing - a 44-second
        // surveillance clip made an 18.5 MB proxy out of a 3.5 MB original.
        //
        // CRF 28 rather than 23 for the same reason it is a preview and not a
        // deliverable: at 720p it is indistinguishable at the size the monitor
        // draws, and it takes that same clip to 2.5 MB.
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "28", "-g", "60"])
        .args(["-pix_fmt", "yuv420p"])
        // Named, not inferred. The file being written is a `.partial`, and
        // leaving the muxer to the extension made FFmpeg refuse it outright.
        .args(["-f", "mp4"])
        // Machine-readable progress on stdout. stdout was already free here -
        // the encode writes to a file, not a pipe.
        .args(["-progress", "pipe:1"])
        .arg(&partial)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::Spawn { program: "ffmpeg", source })?;

    // Drained on a thread, as everywhere else: a full stderr pipe would stall
    // the child, and this one runs for minutes.
    let mut stderr = StderrTail::drain(&mut child);
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            if let Some(seconds) = encoded_seconds(&line) {
                on_progress(seconds);
            }
        }
    }

    let status = child.wait().map_err(|source| Error::Io { program: "ffmpeg", source })?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::Exited {
            program: "ffmpeg",
            path: original.to_path_buf(),
            status,
            stderr: stderr.summary(),
        });
    }

    std::fs::rename(&partial, destination).map_err(|source| {
        let _ = std::fs::remove_file(&partial);
        Error::Io { program: "ffmpeg", source }
    })
}

/// Seconds of source encoded, from one `-progress` line, or `None` for the
/// lines that say something else.
///
/// `out_time` rather than `out_time_ms`: that field has reported
/// *microseconds* for its whole life despite the name, so reading it means
/// betting that FFmpeg never fixes its own bug. `out_time=HH:MM:SS.ffffff`
/// says what it means. Before the first frame lands the value is `N/A`,
/// which simply fails to parse - which is the right answer for it.
fn encoded_seconds(line: &str) -> Option<f64> {
    let mut parts = line.strip_prefix("out_time=")?.trim().split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_read_as_seconds() {
        assert_eq!(encoded_seconds("out_time=00:00:12.500000"), Some(12.5));
        assert_eq!(encoded_seconds("out_time=01:02:03.000000"), Some(3723.0));
        // Emitted before the first frame; not a number, and not a zero either.
        assert_eq!(encoded_seconds("out_time=N/A"), None);
        // Every other field of the report, and the one that lies about units.
        assert_eq!(encoded_seconds("out_time_ms=12500000"), None);
        assert_eq!(encoded_seconds("frame=42"), None);
        assert_eq!(encoded_seconds("progress=continue"), None);
    }

    #[test]
    fn only_sources_taller_than_the_proxy_are_worth_building() {
        assert!(worth_proxying(2160, 720));
        assert!(worth_proxying(1080, 720));
        assert!(!worth_proxying(720, 720), "the same size decodes no faster");
        assert!(!worth_proxying(480, 720), "and a bigger stand-in is worse than the source");
    }

    #[test]
    fn the_name_follows_the_path_and_the_height() {
        let one = Path::new("/takes/a.mp4");
        let other = Path::new("/takes/b.mp4");
        assert_ne!(file_name(one, 720), file_name(other, 720), "different files, different names");
        assert_ne!(
            file_name(one, 720),
            file_name(one, 1080),
            "changing the setting must not serve the old size",
        );
        assert!(file_name(one, 720).ends_with("-720p.mp4"));
        assert_eq!(file_name(one, 720), file_name(one, 720), "and it is stable");
    }

    #[test]
    fn the_proxy_sits_directly_in_the_chosen_folder() {
        // Flat on purpose: the folder is the user's, and a tree of hashed
        // subdirectories inside it would be harder to inspect or empty by hand.
        let path = path_in(Path::new("/proxies"), Path::new("/takes/a.mp4"), 720);
        assert_eq!(path.parent(), Some(Path::new("/proxies")));
    }
}
