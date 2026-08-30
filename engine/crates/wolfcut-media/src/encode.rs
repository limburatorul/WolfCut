//! Writing RGBA frames back out to a file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};

use wolfcut_core::frame::Frame;
use wolfcut_core::time::FrameRate;

use crate::error::{Error, Result};
use crate::process::{StderrTail, base_command};

/// Name used in error messages. The binary actually run comes from
/// [`crate::binaries::ffmpeg`], which may be a bundled copy.
const FFMPEG: &str = "ffmpeg";

/// Anything that accepts finished frames.
///
/// The mirror of [`FrameSource`](crate::decode::FrameSource): render code
/// writes to this trait, so an export target, a preview window and a test spy
/// are interchangeable.
pub trait FrameSink {
    /// Accepts one frame. Frames must all be the size the sink was opened with.
    fn write_frame(&mut self, frame: &Frame) -> Result<()>;

    /// Flushes and closes. Always call this - a dropped sink produces a
    /// truncated file, because the encoder never got to write its trailer.
    fn finish(&mut self) -> Result<()>;
}

/// Encoder settings.
#[derive(Clone, Debug)]
pub struct EncodeOptions {
    /// Video codec, as FFmpeg names it. A name ending `_amf` is one of AMD's
    /// hardware encoders and is driven by a different set of flags - see
    /// [`quality_args`].
    pub codec: String,
    /// x264-style speed/size tradeoff.
    pub preset: String,
    /// Constant rate factor. Lower is better quality and a bigger file.
    pub crf: u8,
    /// Output pixel format. `yuv420p` is what players actually accept.
    pub pixel_format: String,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            codec: "libx264".to_owned(),
            preset: "medium".to_owned(),
            crf: 18,
            pixel_format: "yuv420p".to_owned(),
        }
    }
}

/// Encodes by piping raw RGBA into an `ffmpeg` child process.
pub struct FfmpegEncoder {
    path: PathBuf,
    child: Child,
    /// Taken in `finish` - closing the pipe is what tells FFmpeg to stop.
    stdin: Option<ChildStdin>,
    stderr: StderrTail,
    width: u32,
    height: u32,
    written: u64,
}

impl FfmpegEncoder {
    /// Opens `path` for writing, overwriting anything already there.
    pub fn create(
        path: impl AsRef<Path>,
        width: u32,
        height: u32,
        frame_rate: FrameRate,
        options: &EncodeOptions,
    ) -> Result<Self> {
        let path = path.as_ref();
        let fps = frame_rate.fps();

        let mut command = base_command(crate::binaries::ffmpeg());
        command
            .arg("-y")
            // Input: what is coming down the pipe.
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &format!("{}/{}", fps.numerator(), fps.denominator())])
            // `pipe:0`, not `-`: since FFmpeg 6 the bare dash resolves to the
            // `fd:` protocol, which a trimmed build (like the one we bundle)
            // may not include. The pipe protocol is what we actually mean.
            .args(["-i", "pipe:0"])
            // Output.
            .args(["-c:v", &options.codec])
            .args(quality_args(options))
            .args(["-pix_fmt", &options.pixel_format])
            .args(["-movflags", "+faststart"])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child =
            command.spawn().map_err(|source| Error::Spawn { program: FFMPEG, source })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stderr = StderrTail::drain(&mut child);

        Ok(Self {
            path: path.to_path_buf(),
            child,
            stdin: Some(stdin),
            stderr,
            width,
            height,
            written: 0,
        })
    }

    /// How many frames have been accepted so far.
    pub const fn written(&self) -> u64 {
        self.written
    }
}

impl FrameSink for FfmpegEncoder {
    fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        if frame.width() != self.width || frame.height() != self.height {
            // Raw video has no framing, so a wrong-sized frame would not fail
            // here - it would silently shear every frame after it.
            return Err(Error::FrameSizeMismatch {
                want_width: self.width,
                want_height: self.height,
                got_width: frame.width(),
                got_height: frame.height(),
            });
        }

        let Some(stdin) = self.stdin.as_mut() else {
            return Err(Error::Io {
                program: FFMPEG,
                source: std::io::Error::other("encoder was already finished"),
            });
        };

        if let Err(source) = stdin.write_all(frame.pixels()) {
            // A broken pipe means the encoder died; what it printed on the way
            // out is the informative error, not the EPIPE it left behind.
            if source.kind() == std::io::ErrorKind::BrokenPipe {
                drop(self.stdin.take());
                if let Ok(status) = self.child.wait()
                    && !status.success()
                {
                    return Err(Error::Exited {
                        program: FFMPEG,
                        path: self.path.clone(),
                        status,
                        stderr: self.stderr.summary(),
                    });
                }
            }
            return Err(Error::Io { program: FFMPEG, source });
        }
        self.written += 1;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        // Closing stdin is the signal to flush and write the trailer.
        drop(self.stdin.take());

        let status = self.child.wait().map_err(|source| Error::Io { program: FFMPEG, source })?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::Exited {
                program: FFMPEG,
                path: self.path.clone(),
                status,
                stderr: self.stderr.summary(),
            })
        }
    }
}

impl Drop for FfmpegEncoder {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            // finish() was never called, so the output is garbage anyway.
            // Kill the child instead of blocking a drop on an encode flush.
            drop(self.stdin.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The speed and quality flags for one encoder, in the flags it takes.
///
/// x264 and x265 want `-preset` and `-crf`. AMD's AMF encoders want neither:
/// the speed knob is `-quality`, and constant quality is `-rc cqp` with an
/// explicit quantiser per frame type. This is a branch and not a set of
/// defaults because FFmpeg refuses the whole run when an encoder is handed a
/// flag it does not know - there is no degrading gracefully to fall back on.
///
/// AMF's quantiser scale runs 0-51 and means close enough to the same thing as
/// CRF that the quality picker's number carries across unchanged. Its files
/// come out larger at the same number; that is the encoder's character rather
/// than a mistake in the mapping, and it is the trade being made for speed.
fn quality_args(options: &EncodeOptions) -> Vec<String> {
    if options.codec.ends_with("_amf") {
        return vec![
            "-quality".to_owned(),
            amf_quality(&options.preset).to_owned(),
            "-rc".to_owned(),
            "cqp".to_owned(),
            "-qp_i".to_owned(),
            options.crf.to_string(),
            "-qp_p".to_owned(),
            options.crf.to_string(),
        ];
    }
    vec![
        "-preset".to_owned(),
        options.preset.clone(),
        "-crf".to_owned(),
        options.crf.to_string(),
    ]
}

/// x264's ten preset names onto the three AMF offers.
///
/// The picker speaks x264, because that is what the app has always shipped and
/// what every quality preset in the UI is written in; this is where that
/// vocabulary meets a hardware encoder that has its own.
fn amf_quality(preset: &str) -> &'static str {
    match preset {
        "ultrafast" | "superfast" | "veryfast" | "faster" | "fast" => "speed",
        "slow" | "slower" | "veryslow" | "placebo" => "quality",
        _ => "balanced",
    }
}

/// Encoders worth offering, best-known first.
///
/// Software x264 leads because it is on every machine and its output size is
/// predictable; the rest are hardware and are offered only where they run.
const CANDIDATES: [&str; 8] = [
    "libx264",
    "h264_amf",
    "hevc_amf",
    "av1_amf",
    "h264_nvenc",
    "hevc_nvenc",
    "h264_qsv",
    "hevc_qsv",
];

/// One black frame, encoded and thrown away.
///
/// 640x480, which is larger than it looks like it needs to be on purpose.
/// Hardware encoders each refuse below some minimum of their own - AMD's H.264
/// will not open at 64x64 and its HEVC will not open at 320x240, both with the
/// same unhelpful `encoder->Init() failed with error 5`. A probe below one of
/// those minimums reports no hardware on a machine that has it, which is worse
/// than not probing at all: it is silent, and it looks like an answer. The
/// margin costs nothing, since this is one frame either way.
fn encoder_runs(name: &str) -> bool {
    crate::process::command(crate::binaries::ffmpeg())
        .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "color=black:s=640x480:r=1:d=0.1"])
        .args(["-c:v", name])
        .args(quality_args(&EncodeOptions {
            codec: name.to_owned(),
            ..EncodeOptions::default()
        }))
        .args(["-frames:v", "1", "-f", "null", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Which of the encoders this app knows about the machine can actually run.
///
/// `ffmpeg -encoders` is not the answer to this question. A build lists
/// `h264_nvenc` whether or not an NVIDIA card is fitted, and `h264_amf` on a
/// machine with no AMD driver at all; the failure then arrives partway through
/// somebody's export. Encoding one black frame with each is the only claim
/// that means anything, and it costs a couple of seconds once.
///
/// Cached for the process. Hardware does not appear or vanish while an editor
/// is open, and the probe is not free enough to repeat per dialog.
pub fn working_encoders() -> &'static [String] {
    static FOUND: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        CANDIDATES
            .iter()
            .filter(|name| encoder_runs(name))
            .map(|name| (*name).to_owned())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_a_playable_h264_file() {
        let options = EncodeOptions::default();
        assert_eq!(options.codec, "libx264");
        assert_eq!(options.pixel_format, "yuv420p", "yuv444 will not play in browsers");
    }

    #[test]
    fn a_wrong_sized_frame_is_rejected() {
        let Ok(mut encoder) = FfmpegEncoder::create(
            std::env::temp_dir().join("wolfcut-encode-size-test.mp4"),
            64,
            64,
            FrameRate::THIRTY,
            &EncodeOptions::default(),
        ) else {
            // No FFmpeg on this machine; nothing to assert.
            return;
        };

        let wrong = Frame::black(32, 32);
        assert!(matches!(
            encoder.write_frame(&wrong),
            Err(Error::FrameSizeMismatch { got_width: 32, .. })
        ));
    }

    #[test]
    fn software_encoders_get_preset_and_crf() {
        let options = EncodeOptions { crf: 20, preset: "slow".to_owned(), ..Default::default() };
        assert_eq!(quality_args(&options), ["-preset", "slow", "-crf", "20"]);
    }

    #[test]
    fn amf_encoders_get_the_flags_they_actually_take() {
        // Handing an AMF encoder `-crf` does not degrade to a default - FFmpeg
        // refuses the run - so neither flag may survive this branch.
        let options = EncodeOptions {
            codec: "h264_amf".to_owned(),
            crf: 20,
            preset: "medium".to_owned(),
            ..Default::default()
        };
        let args = quality_args(&options);
        assert_eq!(args, ["-quality", "balanced", "-rc", "cqp", "-qp_i", "20", "-qp_p", "20"]);
        assert!(!args.iter().any(|arg| arg == "-crf" || arg == "-preset"));
    }

    #[test]
    fn every_x264_preset_lands_on_one_of_amfs_three() {
        for preset in ["ultrafast", "superfast", "veryfast", "faster", "fast"] {
            assert_eq!(amf_quality(preset), "speed", "{preset}");
        }
        for preset in ["slow", "slower", "veryslow", "placebo"] {
            assert_eq!(amf_quality(preset), "quality", "{preset}");
        }
        assert_eq!(amf_quality("medium"), "balanced");
        // Including one the picker has never sent: a name nobody planned for
        // has to produce a working encode, not an argument FFmpeg rejects.
        assert_eq!(amf_quality("something-new"), "balanced");
        assert_eq!(amf_quality(""), "balanced");
    }
}
