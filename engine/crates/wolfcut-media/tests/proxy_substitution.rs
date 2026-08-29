//! The preview path served from a stand-in instead of the original.
//!
//! Two things have to hold for a proxy to be safe, and neither is provable
//! from unit tests: that the substituted file decodes faster, and that it shows
//! *the same moment*. The second is the one that would ruin an edit quietly -
//! a cut placed against a preview that was a few frames out of step is wrong
//! everywhere except on screen - so this decodes both files at the same
//! timestamps and compares the pixels.
//!
//! Fixtures come from the system FFmpeg, like `export_integration.rs`, and the
//! suite skips when there is none.

use std::path::{Path, PathBuf};
use std::process::Command;

use wolfcut_core::frame::Frame;
use wolfcut_core::time::Rational;
use wolfcut_media::{proxy, ReaderPool};

/// Source and proxy heights. The source has to be the taller of the two, or
/// there would be nothing to substitute.
const SOURCE_HEIGHT: u32 = 720;
const PROXY_HEIGHT: u32 = 360;
/// What the monitor asks the pool for.
const PREVIEW: (u32, u32) = (480, 270);

fn system_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|out| out.status.success())
}

/// A fixture whose picture changes every frame, so a frame out of step shows
/// up as a large pixel difference rather than a subtle one.
fn fixture(directory: &Path) -> PathBuf {
    let path = directory.join("source.mp4");
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size=1280x{SOURCE_HEIGHT}:rate=30:duration=6"))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "60", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "could not build the fixture");
    path
}

fn frame_at(pool: &mut ReaderPool, clip: &Path, seconds: f64) -> std::sync::Arc<Frame> {
    let time = Rational::new((seconds * 1000.0) as i64, 1000);
    pool.frame_at(clip, time, PREVIEW.0, PREVIEW.1, false, None).expect("a frame")
}

/// Mean absolute channel difference over a sampled grid. 0 is identical.
fn difference(left: &Frame, right: &Frame) -> f64 {
    let (mut total, mut count) = (0u64, 0u64);
    for y in (0..PREVIEW.1).step_by(11) {
        for x in (0..PREVIEW.0).step_by(13) {
            let (Some(a), Some(b)) = (left.pixel(x, y), right.pixel(x, y)) else { continue };
            for channel in 0..3 {
                total += u64::from((i32::from(a[channel]) - i32::from(b[channel])).unsigned_abs());
                count += 1;
            }
        }
    }
    total as f64 / count.max(1) as f64
}

#[test]
fn a_substituted_proxy_shows_the_same_moment_as_the_original() {
    if !system_ffmpeg() {
        eprintln!("no system ffmpeg; skipping");
        return;
    }
    let directory = std::env::temp_dir().join(format!("wolfcut-proxy-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let original = fixture(&directory);

    let store = directory.join("proxies");
    let destination = proxy::path_in(&store, &original, PROXY_HEIGHT);
    proxy::generate(&original, &destination, PROXY_HEIGHT).expect("builds a proxy");
    assert!(destination.is_file(), "the finished proxy lands under its own name");
    assert!(
        std::fs::metadata(&destination).unwrap().len()
            < std::fs::metadata(&original).unwrap().len(),
        "a stand-in that is not smaller has no reason to exist",
    );

    let mut originals = ReaderPool::with_defaults();
    let mut proxied = ReaderPool::with_defaults();
    proxied.use_proxies(Some(store.clone()), PROXY_HEIGHT);

    // Scattered on purpose: every one of these is a seek, which is the case
    // the substitution exists for and the case where a timing mistake shows.
    let mut worst = 0.0_f64;
    for seconds in [0.0, 4.5, 1.25, 3.0, 0.5, 5.5, 2.0] {
        let want = frame_at(&mut originals, &original, seconds);
        let got = frame_at(&mut proxied, &original, seconds);
        worst = worst.max(difference(&want, &got));
    }
    // Scaling down and back up moves pixels a little; a frame out of step in
    // this fixture moves them enormously. The gap between those two is wide,
    // so this threshold is not delicately placed.
    assert!(worst < 30.0, "the proxy is showing a different moment ({worst:.1}/255)");

    // With no proxy directory set, the same pool must go back to the original.
    proxied.use_proxies(None, PROXY_HEIGHT);
    let direct = frame_at(&mut proxied, &original, 2.0);
    let reference = frame_at(&mut originals, &original, 2.0);
    assert!(difference(&direct, &reference) < 1.0, "unset must mean originals");

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_missing_proxy_falls_back_to_the_original_rather_than_failing() {
    if !system_ffmpeg() {
        eprintln!("no system ffmpeg; skipping");
        return;
    }
    let directory = std::env::temp_dir().join(format!("wolfcut-proxy-miss-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let original = fixture(&directory);

    // A directory that exists but holds nothing: the common case for the whole
    // first minute of a project, while proxies are still being built.
    let mut pool = ReaderPool::with_defaults();
    pool.use_proxies(Some(directory.join("empty")), PROXY_HEIGHT);
    let frame = frame_at(&mut pool, &original, 1.0);
    assert_eq!((frame.width(), frame.height()), PREVIEW);

    let _ = std::fs::remove_dir_all(&directory);
}
