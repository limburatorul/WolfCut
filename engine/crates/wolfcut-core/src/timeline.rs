//! The edit itself: a project, its tracks, and the clips on them.
//!
//! Tracks and clips live in arenas and are addressed by [`TrackId`] and
//! [`ClipId`]. Nothing here owns another editable object directly, which is
//! what makes undo, cross-references and multi-threaded rendering tractable.
//!
//! Track order is explicit and bottom-first: `track_ids()[0]` is composited
//! first and everything after it draws on top.

use std::path::{Path, PathBuf};

use crate::arena::{Arena, Id};
use crate::time::{FrameRate, Rational, TimeRange};

/// Handle to a [`Track`].
pub type TrackId = Id<Track>;
/// Handle to a [`Clip`].
pub type ClipId = Id<Clip>;

/// What kind of material a track carries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TrackKind {
    /// Picture.
    Video,
    /// Sound.
    Audio,
}

/// Where a clip's pixels come from.
///
/// A path today. When a media library lands this becomes a handle into it, and
/// this is the one type that has to change.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct MediaRef {
    /// Path to the media file on disk.
    pub path: PathBuf,
}

impl MediaRef {
    /// References a file on disk.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The referenced path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A span of one piece of media, placed on a track.
#[derive(Clone, Debug)]
pub struct Clip {
    /// Where the pixels come from.
    pub media: MediaRef,
    /// The in-point: how far into the media the clip starts.
    pub source_start: Rational,
    /// Where the clip sits on the timeline.
    pub start: Rational,
    /// How long the clip runs on the timeline.
    pub duration: Rational,
    /// Playback rate: source seconds consumed per timeline second. One is
    /// normal speed. Always positive - enforce at the boundary that sets it.
    ///
    /// This is the *only* definition of retiming. Everything downstream - the
    /// frame plan, the export decoder rate, the audio graph - derives from it,
    /// so picture and sound cannot disagree about what a sped-up clip means.
    pub speed: Rational,
    /// Blend factor in `0.0..=1.0`, applied over whatever is beneath.
    pub opacity: f32,
    /// Ramp the picture's opacity from zero over this many seconds from the
    /// clip's start. Zero means no ramp.
    ///
    /// This is what a cross-dissolve is made of: the incoming clip overlaps
    /// the outgoing one and ramps in on top. It multiplies `opacity`, so a
    /// half-transparent clip fades up to half, not to solid.
    pub video_fade_in: Rational,
    /// Ramp the opacity back to zero over this many seconds into the clip's
    /// end. Zero means no ramp.
    pub video_fade_out: Rational,
    /// How the picture sits in the frame. Identity is fitted and centred.
    pub transform: Transform,
    /// How the picture arrives, over `motion_in_duration` from the clip's
    /// start. [`Motion::None`] is the ordinary case: it appears.
    pub motion_in: Motion,
    /// Seconds the arrival takes. Zero means none, like the fades.
    pub motion_in_duration: Rational,
    /// How the picture leaves, over `motion_out_duration` into its end.
    ///
    /// Only a push needs this - the outgoing half of the cut slides away while
    /// the incoming half slides in. A wipe or a zoom leaves the picture
    /// underneath alone and simply covers it.
    pub motion_out: Motion,
    /// Seconds the departure takes.
    pub motion_out_duration: Rational,
}

/// The part of the output frame a layer may paint, as fractions of the frame.
///
/// In *output* space rather than source space, because that is what a wipe
/// actually is: a hard edge sweeping across the screen, with the picture
/// standing still behind it. Cropping the source would slide the picture
/// instead, which is a different transition.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Crop {
    /// Left edge, 0 at the frame's left.
    pub left: f32,
    /// Top edge, 0 at the frame's top.
    pub top: f32,
    /// Right edge, 1 at the frame's right.
    pub right: f32,
    /// Bottom edge, 1 at the frame's bottom.
    pub bottom: f32,
}

impl Crop {
    /// The whole frame: what every layer gets unless a wipe says otherwise.
    pub const FULL: Crop = Crop { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0 };

    /// True when this crop hides nothing, so a compositor can skip the test.
    pub fn is_full(&self) -> bool {
        *self == Self::FULL
    }
}

/// How a clip arrives over, or departs from, the cut it shares with another.
///
/// The opacity ramp (`video_fade_in`) is what a dissolve is made of; this is
/// what everything that *moves* is made of. It lives on the clip and is
/// evaluated per frame for exactly the reason the fade is: one definition that
/// every renderer has to agree with, and a compositor that never learns
/// transitions exist.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Motion {
    /// Nothing; the picture is simply there.
    #[default]
    None,
    /// Travels in from `dx` frame-widths and `dy` frame-heights away.
    Slide {
        /// Horizontal displacement at the start, in frame widths.
        dx: f64,
        /// Vertical displacement at the start, in frame heights.
        dy: f64,
    },
    /// Grows from `from` times its settled size. Above one, it shrinks in.
    Zoom {
        /// The scale multiplier the motion begins at.
        from: f64,
    },
    /// A hard edge sweeps across, uncovering the picture as it goes.
    Wipe {
        /// Sweeping left-to-right or right-to-left rather than vertically.
        horizontal: bool,
        /// Along increasing x (or y) rather than back towards the origin.
        forward: bool,
    },
}

impl Motion {
    /// The transform and crop partway through an arrival.
    ///
    /// `progress` is 0 the instant the clip appears and 1 once the motion is
    /// spent, so every arm reads as "where it starts" blended towards nothing.
    pub fn arriving(&self, base: Transform, progress: f64) -> (Transform, Crop) {
        let remaining = 1.0 - progress.clamp(0.0, 1.0);
        match *self {
            Motion::None => (base, Crop::FULL),
            Motion::Slide { dx, dy } => (
                Transform {
                    offset_x: base.offset_x + dx * remaining,
                    offset_y: base.offset_y + dy * remaining,
                    ..base
                },
                Crop::FULL,
            ),
            Motion::Zoom { from } => (
                Transform { scale: base.scale * (from + (1.0 - from) * progress), ..base },
                Crop::FULL,
            ),
            Motion::Wipe { horizontal, forward } => (base, wipe_crop(horizontal, forward, progress)),
        }
    }

    /// The transform and crop partway through a departure.
    ///
    /// `progress` runs the other way: 0 where the motion begins and 1 at the
    /// clip's last frame. Only [`Motion::Slide`] does anything here, because a
    /// wipe or a zoom covers what is beneath rather than moving it.
    pub fn leaving(&self, base: Transform, progress: f64) -> (Transform, Crop) {
        match *self {
            Motion::Slide { dx, dy } => {
                let gone = progress.clamp(0.0, 1.0);
                (
                    Transform {
                        offset_x: base.offset_x + dx * gone,
                        offset_y: base.offset_y + dy * gone,
                        ..base
                    },
                    Crop::FULL,
                )
            }
            _ => (base, Crop::FULL),
        }
    }
}

/// The uncovered rectangle partway through a wipe.
fn wipe_crop(horizontal: bool, forward: bool, progress: f64) -> Crop {
    let shown = progress.clamp(0.0, 1.0) as f32;
    match (horizontal, forward) {
        (true, true) => Crop { right: shown, ..Crop::FULL },
        (true, false) => Crop { left: 1.0 - shown, ..Crop::FULL },
        (false, true) => Crop { bottom: shown, ..Crop::FULL },
        (false, false) => Crop { top: 1.0 - shown, ..Crop::FULL },
    }
}

/// A clip's placement in the output frame.
///
/// Deliberately resolution-independent: `scale` is relative to the fitted
/// size and the offsets are fractions of the frame, so the same transform
/// means the same picture on a 720p export and a 4K one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transform {
    /// Multiplier over the fitted size. 1 fills the frame, preserving aspect.
    pub scale: f64,
    /// Offset of the picture's centre from frame centre, as a fraction of
    /// frame width.
    pub offset_x: f64,
    /// Offset as a fraction of frame height.
    pub offset_y: f64,
    /// Clockwise rotation about the picture's centre, in degrees.
    pub rotation: f64,
}

impl Transform {
    /// Fitted, centred, unrotated.
    pub const IDENTITY: Transform =
        Transform { scale: 1.0, offset_x: 0.0, offset_y: 0.0, rotation: 0.0 };

    /// True when applying this transform would change nothing.
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Clip {
    /// A clip that starts at the beginning of its media and is fully opaque.
    pub fn new(media: MediaRef, start: Rational, duration: Rational) -> Self {
        Self {
            media,
            source_start: Rational::ZERO,
            start,
            duration,
            speed: Rational::ONE,
            opacity: 1.0,
            video_fade_in: Rational::ZERO,
            video_fade_out: Rational::ZERO,
            transform: Transform::IDENTITY,
            motion_in: Motion::None,
            motion_in_duration: Rational::ZERO,
            motion_out: Motion::None,
            motion_out_duration: Rational::ZERO,
        }
    }

    /// Where the picture sits, and how much of the frame it may paint, at
    /// `time`.
    ///
    /// The settled transform and the whole frame for a clip with no motion,
    /// which is nearly all of them - the common case costs two comparisons,
    /// the same bargain `video_fade_factor` makes.
    pub fn motion_at(&self, time: Rational) -> (Transform, Crop) {
        let local = time - self.start;

        if !self.motion_in_duration.is_zero() && local < self.motion_in_duration {
            let progress = (local / self.motion_in_duration).as_f64();
            return self.motion_in.arriving(self.transform, progress);
        }

        let remaining = self.duration - local;
        if !self.motion_out_duration.is_zero() && remaining < self.motion_out_duration {
            // Counted from the far end, so a departure reads as "how far gone"
            // rather than "how much is left".
            let progress = 1.0 - (remaining / self.motion_out_duration).as_f64();
            return self.motion_out.leaving(self.transform, progress);
        }

        (self.transform, Crop::FULL)
    }

    /// The opacity ramp factor at `time`, in `0.0..=1.0`.
    ///
    /// One at any instant outside the fade windows, and always one for a clip
    /// with no fades - the common case costs two comparisons. This lives on
    /// the clip, next to `source_time_at`, because it is the same kind of
    /// fact: the one definition every renderer has to agree with.
    pub fn video_fade_factor(&self, time: Rational) -> f32 {
        let mut factor = 1.0f32;
        let local = time - self.start;

        if !self.video_fade_in.is_zero() && local < self.video_fade_in {
            factor *= (local / self.video_fade_in).as_f64().clamp(0.0, 1.0) as f32;
        }
        let remaining = self.duration - local;
        if !self.video_fade_out.is_zero() && remaining < self.video_fade_out {
            factor *= (remaining / self.video_fade_out).as_f64().clamp(0.0, 1.0) as f32;
        }
        factor
    }

    /// The span of timeline this clip occupies.
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }

    /// True if the clip is on screen at `time`.
    pub fn contains(&self, time: Rational) -> bool {
        self.range().contains(time)
    }

    /// Converts a timeline timestamp into a timestamp within the source media,
    /// or `None` if the clip is not on screen then.
    ///
    /// An affine map: `source_start + (time - start) * speed`. This is the one
    /// piece of arithmetic that every trim, ripple, slip and retime operation
    /// ultimately has to agree with, so it lives in exactly one place.
    pub fn source_time_at(&self, time: Rational) -> Option<Rational> {
        self.contains(time)
            .then(|| self.source_start + (time - self.start) * self.speed)
    }

    /// How much of the source this clip consumes: `duration * speed`.
    pub fn source_duration(&self) -> Rational {
        self.duration * self.speed
    }
}

/// A horizontal lane of clips.
#[derive(Clone, Debug)]
pub struct Track {
    /// Display name.
    pub name: String,
    /// Picture or sound.
    pub kind: TrackKind,
    /// When false the track is skipped entirely while rendering.
    pub enabled: bool,
    clips: Vec<ClipId>,
}

impl Track {
    /// An empty, enabled track.
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self { name: name.into(), kind, enabled: true, clips: Vec::new() }
    }

    /// The clips on this track, in insertion order.
    pub fn clips(&self) -> &[ClipId] {
        &self.clips
    }
}

/// The edit: output format, tracks, and the clips on them.
#[derive(Debug)]
pub struct Timeline {
    /// Output frame rate.
    pub frame_rate: FrameRate,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    tracks: Arena<Track>,
    clips: Arena<Clip>,
    order: Vec<TrackId>,
}

impl Timeline {
    /// An empty timeline with the given output format.
    pub fn new(width: u32, height: u32, frame_rate: FrameRate) -> Self {
        Self {
            frame_rate,
            width,
            height,
            tracks: Arena::new(),
            clips: Arena::new(),
            order: Vec::new(),
        }
    }

    /// A 1920x1080, 30 fps timeline - the default for a new project.
    pub fn hd() -> Self {
        Self::new(1920, 1080, FrameRate::THIRTY)
    }

    /// Adds a track on top of the existing ones.
    pub fn add_track(&mut self, track: Track) -> TrackId {
        let id = self.tracks.insert(track);
        self.order.push(id);
        id
    }

    /// Track handles, bottom-most first. Composite in this order.
    pub fn track_ids(&self) -> &[TrackId] {
        &self.order
    }

    /// Borrows a track.
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.get(id)
    }

    /// Mutably borrows a track.
    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.get_mut(id)
    }

    /// Iterates over tracks bottom-most first.
    pub fn tracks(&self) -> impl Iterator<Item = (TrackId, &Track)> {
        self.order.iter().filter_map(|&id| self.tracks.get(id).map(|track| (id, track)))
    }

    /// Removes a track and every clip on it.
    pub fn remove_track(&mut self, id: TrackId) -> Option<Track> {
        let track = self.tracks.remove(id)?;
        for &clip in &track.clips {
            self.clips.remove(clip);
        }
        self.order.retain(|&other| other != id);
        Some(track)
    }

    /// Places a clip on a track. Returns `None` if the track handle is stale.
    pub fn add_clip(&mut self, track: TrackId, clip: Clip) -> Option<ClipId> {
        // Insert into the clip arena only once we know the track is real,
        // otherwise a stale handle leaks an orphaned clip.
        self.tracks.get(track)?;
        let id = self.clips.insert(clip);
        self.tracks.get_mut(track).expect("checked above").clips.push(id);
        Some(id)
    }

    /// Borrows a clip.
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.get(id)
    }

    /// Mutably borrows a clip.
    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.get_mut(id)
    }

    /// Removes a clip from wherever it sits.
    pub fn remove_clip(&mut self, id: ClipId) -> Option<Clip> {
        let clip = self.clips.remove(id)?;
        for (_, track) in self.tracks.iter_mut() {
            track.clips.retain(|&other| other != id);
        }
        Some(clip)
    }

    /// How many clips the timeline holds, across all tracks.
    pub const fn clip_count(&self) -> usize {
        self.clips.len()
    }

    /// The clip visible on `track` at `time`, if any.
    ///
    /// Clips on one track are not supposed to overlap; if they do, the
    /// last-added one wins, which matches what you see after a paste.
    pub fn clip_on_track_at(&self, track: TrackId, time: Rational) -> Option<ClipId> {
        let track = self.tracks.get(track)?;
        track
            .clips
            .iter()
            .rev()
            .copied()
            .find(|&id| self.clips.get(id).is_some_and(|clip| clip.contains(time)))
    }

    /// Where the last clip ends. Zero for an empty timeline.
    pub fn duration(&self) -> Rational {
        self.clips.iter().map(|(_, clip)| clip.range().end()).max().unwrap_or(Rational::ZERO)
    }

    /// How many whole output frames the timeline runs for.
    pub fn frame_count(&self) -> i64 {
        self.frame_rate.frames_in(self.duration())
    }
}

/// A timeline plus the things that surround it on disk.
#[derive(Debug)]
pub struct Project {
    /// Display name.
    pub name: String,
    /// The edit.
    pub timeline: Timeline,
}

impl Project {
    /// A new, empty project at the given output format.
    pub fn new(name: impl Into<String>, timeline: Timeline) -> Self {
        Self { name: name.into(), timeline }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(value: i64) -> Rational {
        Rational::from_int(value)
    }

    fn fixture() -> (Timeline, TrackId, ClipId) {
        let mut timeline = Timeline::hd();
        let track = timeline.add_track(Track::new("V1", TrackKind::Video));
        let clip = timeline
            .add_clip(track, Clip::new(MediaRef::new("a.mp4"), seconds(2), seconds(3)))
            .expect("track exists");
        (timeline, track, clip)
    }

    #[test]
    fn a_clip_maps_timeline_time_to_source_time() {
        let (timeline, _, clip) = fixture();
        let clip = timeline.clip(clip).expect("clip exists");

        assert_eq!(clip.source_time_at(seconds(2)), Some(Rational::ZERO));
        assert_eq!(clip.source_time_at(seconds(4)), Some(seconds(2)));
        assert_eq!(clip.source_time_at(seconds(1)), None, "before the clip");
        assert_eq!(clip.source_time_at(seconds(5)), None, "the end is exclusive");
    }

    #[test]
    fn speed_scales_the_source_time_map() {
        let (mut timeline, _, id) = fixture();
        {
            let clip = timeline.clip_mut(id).expect("clip exists");
            clip.speed = Rational::from_int(2);
            clip.source_start = seconds(10);
        }
        let clip = timeline.clip(id).expect("clip exists");

        // The clip covers timeline [2, 5) at 2x, so it consumes source [10, 16).
        assert_eq!(clip.source_time_at(seconds(2)), Some(seconds(10)));
        assert_eq!(clip.source_time_at(seconds(4)), Some(seconds(14)));
        assert_eq!(clip.source_duration(), seconds(6));

        // Half speed consumes half the source.
        timeline.clip_mut(id).expect("clip exists").speed = Rational::new(1, 2);
        let clip = timeline.clip(id).expect("clip exists");
        assert_eq!(clip.source_time_at(seconds(4)), Some(seconds(11)));
        assert_eq!(clip.source_duration(), Rational::new(3, 2));
    }

    #[test]
    fn an_in_point_offsets_the_source_time() {
        let (mut timeline, _, id) = fixture();
        timeline.clip_mut(id).expect("clip exists").source_start = seconds(10);
        let clip = timeline.clip(id).expect("clip exists");
        assert_eq!(clip.source_time_at(seconds(2)), Some(seconds(10)));
        assert_eq!(clip.source_time_at(seconds(3)), Some(seconds(11)));
    }

    #[test]
    fn video_fades_ramp_and_hold() {
        let (mut timeline, _, id) = fixture(); // covers timeline [2, 5)
        {
            let clip = timeline.clip_mut(id).expect("clip exists");
            clip.video_fade_in = Rational::ONE;
            clip.video_fade_out = Rational::ONE;
        }
        let clip = timeline.clip(id).expect("clip exists");

        assert_eq!(clip.video_fade_factor(seconds(2)), 0.0, "starts at zero");
        assert_eq!(clip.video_fade_factor(Rational::new(5, 2)), 0.5, "halfway up");
        assert_eq!(clip.video_fade_factor(seconds(3)), 1.0, "holds at one between fades");
        assert_eq!(clip.video_fade_factor(Rational::new(9, 2)), 0.5, "halfway down");
    }

    #[test]
    fn no_fade_means_no_attenuation() {
        let (timeline, _, id) = fixture();
        let clip = timeline.clip(id).expect("clip exists");
        assert_eq!(clip.video_fade_factor(seconds(2)), 1.0);
        assert_eq!(clip.video_fade_factor(seconds(4)), 1.0);
    }

    #[test]
    fn finds_the_clip_under_the_playhead() {
        let (timeline, track, clip) = fixture();
        assert_eq!(timeline.clip_on_track_at(track, seconds(3)), Some(clip));
        assert_eq!(timeline.clip_on_track_at(track, seconds(9)), None);
    }

    #[test]
    fn duration_is_the_end_of_the_last_clip() {
        let (mut timeline, track, _) = fixture();
        assert_eq!(timeline.duration(), seconds(5));

        timeline
            .add_clip(track, Clip::new(MediaRef::new("b.mp4"), seconds(5), seconds(4)))
            .expect("track exists");
        assert_eq!(timeline.duration(), seconds(9));
        assert_eq!(timeline.frame_count(), 270); // 9s at 30fps
    }

    #[test]
    fn tracks_come_back_bottom_most_first() {
        let mut timeline = Timeline::hd();
        let lower = timeline.add_track(Track::new("V1", TrackKind::Video));
        let upper = timeline.add_track(Track::new("V2", TrackKind::Video));
        assert_eq!(timeline.track_ids(), &[lower, upper]);
    }

    #[test]
    fn removing_a_track_takes_its_clips_with_it() {
        let (mut timeline, track, clip) = fixture();
        timeline.remove_track(track);

        assert!(timeline.clip(clip).is_none());
        assert_eq!(timeline.clip_count(), 0);
        assert!(timeline.track_ids().is_empty());
        assert_eq!(timeline.duration(), Rational::ZERO);
    }

    #[test]
    fn removing_a_clip_unlinks_it_from_its_track() {
        let (mut timeline, track, clip) = fixture();
        assert!(timeline.remove_clip(clip).is_some());

        assert!(timeline.track(track).expect("track exists").clips().is_empty());
        assert_eq!(timeline.clip_on_track_at(track, seconds(3)), None);
        assert!(timeline.remove_clip(clip).is_none(), "removing twice is harmless");
    }

    #[test]
    fn a_stale_track_handle_does_not_leak_a_clip() {
        let (mut timeline, track, _) = fixture();
        timeline.remove_track(track);

        let orphan = timeline.add_clip(track, Clip::new(MediaRef::new("c.mp4"), seconds(0), seconds(1)));
        assert_eq!(orphan, None);
        assert_eq!(timeline.clip_count(), 0);
    }

    fn moving(motion_in: Motion, seconds: i64) -> Clip {
        let mut clip = Clip::new(
            MediaRef::new("a.mp4"),
            Rational::ZERO,
            Rational::from_int(10),
        );
        clip.motion_in = motion_in;
        clip.motion_in_duration = Rational::from_int(seconds);
        clip
    }

    #[test]
    fn a_clip_with_no_motion_is_left_exactly_alone() {
        // The common case, and the one that must cost nothing: every clip in
        // a project that uses no motion transitions goes through here.
        let clip = Clip::new(MediaRef::new("a.mp4"), Rational::ZERO, Rational::from_int(10));
        let (transform, crop) = clip.motion_at(Rational::from_int(3));
        assert_eq!(transform, Transform::IDENTITY);
        assert!(crop.is_full());
    }

    #[test]
    fn a_slide_travels_from_its_offset_to_none() {
        let clip = moving(Motion::Slide { dx: 1.0, dy: 0.0 }, 2);
        let at = |seconds: i64| clip.motion_at(Rational::from_int(seconds)).0.offset_x;

        assert_eq!(at(0), 1.0, "a whole frame width away at the first instant");
        assert_eq!(clip.motion_at(Rational::new(1, 1)).0.offset_x, 0.5, "halfway, halfway");
        assert_eq!(at(2), 0.0, "settled the moment the window closes");
        assert_eq!(at(6), 0.0, "and stays settled");
    }

    #[test]
    fn a_zoom_settles_at_the_clips_own_scale() {
        // From larger, not smaller: growing from small would show black around
        // the incoming picture for the whole transition.
        let clip = moving(Motion::Zoom { from: 2.0 }, 2);
        assert_eq!(clip.motion_at(Rational::ZERO).0.scale, 2.0);
        assert_eq!(clip.motion_at(Rational::from_int(1)).0.scale, 1.5);
        assert_eq!(clip.motion_at(Rational::from_int(2)).0.scale, 1.0);
    }

    #[test]
    fn a_zoom_multiplies_the_clips_scale_rather_than_replacing_it() {
        // A clip the user already scaled must end up where they put it.
        let mut clip = moving(Motion::Zoom { from: 2.0 }, 2);
        clip.transform.scale = 0.5;
        assert_eq!(clip.motion_at(Rational::ZERO).0.scale, 1.0);
        assert_eq!(clip.motion_at(Rational::from_int(2)).0.scale, 0.5);
    }

    #[test]
    fn a_wipe_uncovers_from_the_side_it_says() {
        let forward = moving(Motion::Wipe { horizontal: true, forward: true }, 2);
        assert_eq!(forward.motion_at(Rational::ZERO).1.right, 0.0, "nothing at the start");
        assert_eq!(forward.motion_at(Rational::from_int(1)).1.right, 0.5);
        assert!(forward.motion_at(Rational::from_int(2)).1.is_full(), "all of it at the end");

        let back = moving(Motion::Wipe { horizontal: true, forward: false }, 2);
        assert_eq!(back.motion_at(Rational::ZERO).1.left, 1.0);
        assert_eq!(back.motion_at(Rational::from_int(1)).1.left, 0.5);
        assert!(back.motion_at(Rational::from_int(2)).1.is_full());
    }

    #[test]
    fn a_departure_runs_the_other_way() {
        // The outgoing half of a push: still where it was when the motion
        // starts, a whole frame away by the clip's last instant.
        let mut clip = Clip::new(MediaRef::new("a.mp4"), Rational::ZERO, Rational::from_int(10));
        clip.motion_out = Motion::Slide { dx: -1.0, dy: 0.0 };
        clip.motion_out_duration = Rational::from_int(2);

        assert_eq!(clip.motion_at(Rational::from_int(5)).0.offset_x, 0.0, "before it begins");
        assert_eq!(clip.motion_at(Rational::from_int(8)).0.offset_x, 0.0, "as it begins");
        assert_eq!(clip.motion_at(Rational::from_int(9)).0.offset_x, -0.5);
    }

    #[test]
    fn a_wipe_and_a_zoom_leave_what_is_beneath_alone() {
        // Only a push moves the picture it is replacing; the others cover it.
        let base = Transform::IDENTITY;
        assert_eq!(Motion::Wipe { horizontal: true, forward: true }.leaving(base, 0.5).1, Crop::FULL);
        assert_eq!(Motion::Zoom { from: 2.0 }.leaving(base, 0.5).0, base);
    }

}
