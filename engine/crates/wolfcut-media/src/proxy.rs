//! Small stand-ins that make scrubbing a big file bearable.
//!
//! Seeking through the subprocess backend costs a fresh FFmpeg per jump, and
//! most of that is decoding: measured on this machine, one preview frame from
//! 4K costs about 260 ms, of which roughly 90 ms is starting the process and
//! the rest is the decode. A 540p stand-in with dense keyframes brings the same
//! frame to about 100 ms - 4K then scrubs faster than 1080p originals do.
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

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::binaries::ffmpeg;
use crate::error::{Error, Result};
use crate::process::{command, summarize};

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
pub fn generate(original: &Path, destination: &Path, height: u32) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io { program: "ffmpeg", source })?;
    }
    let partial = destination.with_extension("partial");

    let output = command(ffmpeg())
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(original)
        // Video only. Sound is never served from here - playback decodes the
        // original and the waveform is drawn from it - so carrying a copy
        // would be size and encode time spent on nothing.
        .args(["-map", "0:v:0", "-an", "-sn", "-dn"])
        // -2 rather than -1: the width has to stay even for yuv420p, and an
        // odd one fails the encode rather than rounding.
        .args(["-vf", &format!("scale=-2:{height}")])
        // Dense keyframes are the point. Measured, an all-intra proxy seeks no
        // faster than one keyed every 12 frames, and it is three times the
        // size - so 12 is where the curve flattens.
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-g", "12"])
        .args(["-pix_fmt", "yuv420p"])
        // Named, not inferred. The file being written is a `.partial`, and
        // leaving the muxer to the extension made FFmpeg refuse it outright.
        .args(["-f", "mp4"])
        .arg(&partial)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| Error::Spawn { program: "ffmpeg", source })?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::Exited {
            program: "ffmpeg",
            path: original.to_path_buf(),
            status: output.status,
            stderr: summarize(&output.stderr),
        });
    }

    std::fs::rename(&partial, destination).map_err(|source| {
        let _ = std::fs::remove_file(&partial);
        Error::Io { program: "ffmpeg", source }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
