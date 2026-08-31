//! What a transition actually puts on the monitor, in pixels.
//!
//! The unit tests each cover one link: the resolver overlaps the clips, the
//! plan produces a crop, the compositor obeys one. All of them passed while
//! the seam between the plan and the compositor was severed and every moving
//! transition rendered as a hard cut. A feature assembled from tested halves
//! is not a tested feature, so this walks the whole path the monitor walks -
//! real files, the real reader pool, `preview_frame` itself - and looks at
//! the colours that come back.
//!
//! Two solid-colour clips meet at a cut. Halfway through the transition the
//! frame must not be plain red and must not be plain blue: that is what "the
//! transition does nothing" looks like, and it is what shipped.
//!
//! Fixtures come from the system FFmpeg, like `proxy_substitution.rs`, and
//! the suite skips when there is none.

use std::path::{Path, PathBuf};
use std::process::Command;

use wolfcut_export::{ClipKind, ExportClip, PreviewFrameRequest, TransitionSpec};
use wolfcut_media::ReaderPool;

/// Source size; the preview asks for half of it.
const SOURCE: (u32, u32) = (640, 360);
const PREVIEW: (u32, u32) = (320, 180);
/// The cut, the transition's length, and the instant sampled - the midpoint,
/// where every motion is half-spent and none of them is a no-op.
const CUT: f64 = 2.0;
const SPAN: f64 = 1.0;
const MIDPOINT: f64 = CUT - SPAN / 2.0;

/// Every id `motion_from_id` knows. A new one that forgets to render is a
/// failing test rather than a silent hard cut.
const MOVING: [&str; 8] = [
    "wipe-left",
    "wipe-right",
    "wipe-up",
    "wipe-down",
    "push",
    "push-up",
    "zoom",
    "zoom-in",
];

fn system_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|out| out.status.success())
}

/// A clip of one flat colour, so what is on screen where is unambiguous.
fn fixture(directory: &Path, colour: &str) -> PathBuf {
    let path = directory.join(format!("{colour}.mp4"));
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=c={colour}:size={}x{}:rate=30:duration=6", SOURCE.0, SOURCE.1))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "could not build the {colour} fixture");
    path
}

fn clip(path: &Path, start: f64, source_start: f64, transition: Option<&str>) -> ExportClip {
    ExportClip {
        path: path.to_string_lossy().into_owned(),
        kind: ClipKind::Video,
        start,
        duration: 2.0,
        source_start,
        track: 0,
        hidden: false,
        muted: true,
        volume: 1.0,
        fade_in: 0.0,
        fade_out: 0.0,
        filter_chain: String::new(),
        speed: 1.0,
        preserve_pitch: true,
        scale: 1.0,
        offset_x: 0.0,
        offset_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        video_filter_chain: String::new(),
        transition: transition.map(|kind| TransitionSpec {
            kind: kind.to_owned(),
            duration: SPAN,
        }),
        video_fade_in: 0.0,
        video_motion_in: String::new(),
        video_motion_in_duration: 0.0,
        video_motion_out: String::new(),
        video_motion_out_duration: 0.0,
        media_width: Some(SOURCE.0),
        media_height: Some(SOURCE.1),
        has_audio: Some(false),
    }
}

/// The composited frame at `time`, as RGBA. Red runs first, blue arrives over
/// it with `kind` on the cut - the incoming clip keeps a second of handle
/// before its in-point, which is what the overlap plays.
fn composite(pool: &mut ReaderPool, red: &Path, blue: &Path, kind: &str, time: f64) -> Vec<u8> {
    let request = PreviewFrameRequest {
        time,
        width: PREVIEW.0,
        height: PREVIEW.1,
        rate_num: 30,
        rate_den: 1,
        clips: vec![clip(red, 0.0, 0.0, None), clip(blue, CUT, SPAN, Some(kind))],
    };
    wolfcut_export::preview_frame(pool, &request).expect("a composited frame")
}

/// Which of the two fixtures a pixel looks like, or `Mixed` for a blend of
/// them. Decoded flat colour is never exactly 255, so this reads the balance
/// between the channels rather than an exact value.
#[derive(PartialEq, Debug)]
enum Looks {
    Red,
    Blue,
    Mixed,
}

fn at(pixels: &[u8], x: u32, y: u32) -> Looks {
    let index = ((y * PREVIEW.0 + x) * 4) as usize;
    let (red, blue) = (i32::from(pixels[index]), i32::from(pixels[index + 2]));
    match () {
        _ if red > 160 && blue < 60 => Looks::Red,
        _ if blue > 160 && red < 60 => Looks::Blue,
        _ => Looks::Mixed,
    }
}

/// Mean absolute channel difference between two frames, 0 being identical.
///
/// Measured against the plain cut rather than by looking for two colours in
/// one frame: a zoom covers the whole frame at half opacity, so it is uniform
/// and still nothing like a cut. What every transition has in common is only
/// that it does not leave the frame as the cut left it.
fn difference(left: &[u8], right: &[u8]) -> f64 {
    let (mut total, mut count) = (0u64, 0u64);
    for y in (0..PREVIEW.1).step_by(7) {
        for x in (0..PREVIEW.0).step_by(5) {
            let index = ((y * PREVIEW.0 + x) * 4) as usize;
            for channel in 0..3 {
                let a = i32::from(left[index + channel]);
                let b = i32::from(right[index + channel]);
                total += u64::from((a - b).unsigned_abs());
                count += 1;
            }
        }
    }
    total as f64 / count.max(1) as f64
}

/// A scratch directory and the two fixtures in it, built once per test.
fn colours() -> (PathBuf, PathBuf) {
    let directory = std::env::temp_dir().join("wolfcut-transition-pixels");
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    (fixture(&directory, "red"), fixture(&directory, "blue"))
}

#[test]
fn every_moving_transition_changes_the_picture_at_its_midpoint() {
    if !system_ffmpeg() {
        eprintln!("skipping: no system ffmpeg");
        return;
    }
    let (red, blue) = colours();
    let mut pool = ReaderPool::with_defaults();

    // The control: an unknown kind renders as a hard cut, so halfway through
    // the frame is still entirely the outgoing picture. Every other frame is
    // measured against this one.
    let cut = composite(&mut pool, &red, &blue, "none-at-all", MIDPOINT);
    assert_eq!(at(&cut, PREVIEW.0 / 2, PREVIEW.1 / 2), Looks::Red);
    assert_eq!(at(&cut, 8, 8), Looks::Red, "no transition, so the frame is all outgoing");

    for kind in MOVING {
        let frame = composite(&mut pool, &red, &blue, kind, MIDPOINT);
        let moved = difference(&frame, &cut);
        assert!(moved > 20.0, "{kind} renders as a hard cut (difference {moved:.1})");
    }
}

#[test]
fn a_wipe_uncovers_one_side_and_leaves_the_other() {
    if !system_ffmpeg() {
        eprintln!("skipping: no system ffmpeg");
        return;
    }
    let (red, blue) = colours();
    let mut pool = ReaderPool::with_defaults();

    // Named for the direction the edge travels: a wipe left sweeps in from
    // the right, so halfway through the incoming picture holds the right half
    // and the outgoing one is untouched on the left.
    let frame = composite(&mut pool, &red, &blue, "wipe-left", MIDPOINT);
    let middle = PREVIEW.1 / 2;
    assert_eq!(at(&frame, 16, middle), Looks::Red, "the outgoing picture still stands");
    assert_eq!(at(&frame, PREVIEW.0 - 16, middle), Looks::Blue, "the incoming picture arrived");

    // And down the other axis, so a transposed crop cannot pass.
    let frame = composite(&mut pool, &red, &blue, "wipe-up", MIDPOINT);
    let centre = PREVIEW.0 / 2;
    assert_eq!(at(&frame, centre, 16), Looks::Red);
    assert_eq!(at(&frame, centre, PREVIEW.1 - 16), Looks::Blue);
}

#[test]
fn a_push_carries_both_pictures_across() {
    if !system_ffmpeg() {
        eprintln!("skipping: no system ffmpeg");
        return;
    }
    let (red, blue) = colours();
    let mut pool = ReaderPool::with_defaults();

    // A push is the one transition that moves the picture it replaces: half
    // a frame in, the outgoing half has slid left and the incoming half has
    // taken the right, tiling without a seam of black between them.
    let frame = composite(&mut pool, &red, &blue, "push", MIDPOINT);
    let middle = PREVIEW.1 / 2;
    assert_eq!(at(&frame, 16, middle), Looks::Red, "the outgoing picture is on its way out");
    assert_eq!(at(&frame, PREVIEW.0 - 16, middle), Looks::Blue, "the incoming picture pushed in");
}
