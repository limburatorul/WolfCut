# Changelog

One entry per release, newest first. Plain lists of what changed for the
person using the app; internal refactors appear only when they change
behaviour.

## Unreleased

- The transitions that moved are no longer promises. Wipe, push and zoom
  work, and there are four more alongside them: wipe up, wipe down, push up
  and zoom in. Eleven in total, all of them rendered by the engine, so what
  the monitor shows when the playhead settles is what the export writes.

- Every slider has a reset button. Resetting one was already possible by
  double-clicking its name, which is a thing you have to be told; the button
  is the same action where it can be seen.

- Scale and speed sliders are usable. Both run over multiplying ranges - a
  sixteenth of speed to sixteen times it - and both were laid out evenly, so
  normal speed sat six per cent along the track and every setting anyone
  actually wants was crammed into a few pixels. They are spaced by proportion
  now: 1x is the middle, and each doubling is the same distance in either
  direction.
- The rotation slider stays where it is put. Dragging it to the far left
  landed on -180, which the model stores as +180, and the knob jumped the
  whole width of the track.
- Settings can repaint the sliders and switches in the side panels. The
  track's tint follows whatever colour is picked.

- Export can hand the encoding to a graphics card. The picker in the export
  sheet lists only what the machine can actually do - probed by encoding a
  frame with each, not by asking FFmpeg what it was built with - and offers
  AMD, NVIDIA and Intel encoders where they run. On a Radeon RX 9070 that is
  about 2.4 times the throughput of the software default. It writes a larger
  file at the same quality setting, which is the trade; software x264 remains
  the default and the predictable one.

- Playback no longer goes black for recordings the window cannot decode.
  A camera writes what it writes, and an NVR export is routinely HEVC in an
  MPEG program stream that Chromium has no reader for; the paused frame
  looked right and pressing play emptied the monitor. The engine now takes
  over the moment the window admits it cannot open a file.
- Filmstrips for long recordings arrive in seconds rather than a minute and
  a half. Building one used to walk the whole file to find its two dozen
  frames - a hundred seconds for a forty-three-minute recording, which just
  looks broken. Past two minutes of material it now seeks to each frame:
  six seconds, whatever the length.
- Proxies are a great deal smaller. They were being keyed every twelve
  frames on the assumption that dense keyframes seek faster, which measured
  against a real recording turns out to be worth nothing at all - it was
  costing four times the size for no speed. A surveillance clip's proxy goes
  from 18.5 MB to 2.5 MB.

- The timeline has a horizontal scrollbar. Panning time used to be a gesture
  only - shift-wheel, or a trackpad sideways - which left a mouse no way to
  reach the far end of a long edit, and nothing on screen saying how much of
  it was off to the right.

- Proxy media: Settings → Performance takes a folder, and imports bigger than
  the proxy size get a small stand-in built there in the background. The
  editor scrubs through those instead of the originals, which is the
  difference between a playhead that drags and one that keeps up — a drag
  across 4K went from about 1.3 seconds to 0.7 here. Exports always read the
  original files; a proxy cannot reach a finished video.

- Remove silence: with one clip selected, Edit → Remove silence finds the
  pauses in it and cuts them out, closing the gaps behind them. The level
  and the shortest pause worth cutting are yours to set, because they
  describe the recording rather than the app. It is one undo.
- Artwork for media removed from the bin is released instead of being held
  for the rest of the session. Filmstrips hold GPU memory, so a long day of
  importing and deleting used to only ever grow.
- An undo or redo that fails now says so. It used to do nothing, silently.
- A project can be opened again after a command panics mid-edit. Closing
  the broken session used to be skipped, which left it installed and
  refused every project until the app was restarted.

- Text to speech: File → Text to speech turns typed narration into an audio
  clip at the playhead, spoken by one of 36 Kokoro voices (American and
  British English, Chinese) at a chosen pace. Generation runs entirely on
  this machine; the voice model downloads once (about 130 MB) from the sheet
  itself or Settings → Speech.
- The entire interface is translatable: the app follows the system language
  when a translation exists, with an override in Settings → General.
  Translations are plain JSON files contributors can add — see TRANSLATING.md.
- Simplified Chinese ships as the first translation (machine-drafted,
  pending native review).

## v0.2.0-alpha.6 — 2026-08-29

- Effect tiles preview the real render: each thumbnail comes from the
  effect's actual FFmpeg chain, not an approximation.
- Editor icons come from Lucide.

## v0.2.0-alpha.5 — 2026-08-29

- Timeline tabs reorder by dragging, following the pointer live.
- Toasts rise in, hold long enough to be read, and sink out.
- Tab drops land where the preview caret shows.
- Transcriber settings select models by card; the internal engine row is gone.
- Color fixes: panel resizers, the idle play button and dark sunken surfaces
  now sit correctly in the palette.

## v0.2.0-alpha.4 — 2026-08-29

- Nix flake for Linux.
- Portrait phone video imports as portrait: probing reports displayed
  dimensions.
- A project closed before its first edit reopens empty instead of corrupt.
- Edits dispatched before the session opens wait for it.
- Packagers that guarantee PATH tools can skip the bundle guard.

## v0.2.0-alpha.3 — 2026-08-29

- Preview quality picker replaces the footer fps readout.
- Fixed same-tick edits reaching the engine empty.

## v0.2.0-alpha.2 — 2026-08-29

- Clock-paced playback stream with engine decode-ahead.
- Waveform peaks decode in the engine.

## v0.2.0-alpha.1 — 2026-08-29

- The engine owns the whole render path: export and preview render the
  engine's session, and the FFmpeg chains are built engine-side.
- Stacked glow/mirror effects no longer break the export.
- A lost GPU device degrades to the CPU compositor instead of failing.
- Playback no longer re-renders the whole interface at 60fps.
- Timecode, trim and autosave fixes in the timeline.
- File access starts empty and grows only by user intent.
- Every green main build publishes the next alpha automatically.

## v0.1.0-alpha.1 — 2026-08-27

- First public alpha: timeline editing, media bin, effects and filters,
  text and captions, export via FFmpeg, on-device transcription.
