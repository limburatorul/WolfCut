//! Nothing spawns a child process except through the constructor that hides
//! its window.
//!
//! On Windows a GUI-subsystem parent has no console, so every console-subsystem
//! child - ffmpeg, ffprobe, whisper-cli - is given a fresh one that pops up over
//! the editor. Importing a handful of clips flashes a window per probe, per
//! waveform and per filmstrip, and the long-lived decoder `pool.rs` keeps alive
//! leaves one sitting there for as long as the file is open.
//!
//! `wolfcut_media::command` sets `CREATE_NO_WINDOW` and every spawn goes
//! through it. That is a convention, though, and conventions are what this bug
//! already got through once: a single `Command::new(ffmpeg())` anywhere puts the
//! windows back, and nothing about it looks wrong in review. So the rule is
//! checked rather than remembered.
//!
//! This is a source scan, not a behavioural test. Whether a window actually
//! appears is not observable from inside a test process, but *where* children
//! are constructed is - and that is the thing that regresses.

use std::path::{Path, PathBuf};

/// The one file allowed to call `Command::new`: the constructor itself.
const SANCTIONED: &str = "wolfcut-media/src/process.rs";

/// Every `.rs` file under `root`, recursively.
fn rust_sources(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Build output is not source, and it is enormous.
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

fn repository() -> PathBuf {
    // The desktop crate consumes the engine as a path dependency, so it already
    // knows where the engine lives; walking up to the root is not new coupling.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[test]
fn every_child_process_is_built_by_the_windowless_constructor() {
    let root = repository();
    let mut sources = Vec::new();
    rust_sources(&root.join("desktop/src-tauri/src"), &mut sources);
    rust_sources(&root.join("engine/crates"), &mut sources);

    // A scan that silently found nothing would pass for the wrong reason.
    assert!(sources.len() > 20, "only {} source files scanned - the walk is broken", sources.len());

    let offenders: Vec<String> = sources
        .iter()
        .filter(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| text.contains("Command::new("))
        })
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .filter(|path| {
            // The constructor itself, and test files - those run from a console
            // and spawn the *system* ffmpeg to build their fixtures.
            !path.ends_with(SANCTIONED) && !path.contains("/tests/")
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these spawn a child directly instead of through wolfcut_media::command, \
         which puts a console window back on Windows: {offenders:#?}",
    );
}

#[test]
fn the_sanctioned_constructor_still_suppresses_the_window() {
    // The test above only proves everything routes through one function. If
    // that function stopped setting the flag, every call site would still look
    // correct and every window would be back.
    let source = std::fs::read_to_string(repository().join("engine/crates").join(SANCTIONED))
        .expect("the sanctioned constructor is where it says it is");

    for needle in ["CREATE_NO_WINDOW", "creation_flags", "0x0800_0000"] {
        assert!(source.contains(needle), "process.rs no longer mentions {needle}");
    }
}
