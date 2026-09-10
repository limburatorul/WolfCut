//! Building the stand-ins the monitor scrubs through.
//!
//! The engine decides what a proxy *is* and where it lives
//! (`wolfcut_media::proxy`); this decides *when* one gets built. That is a
//! queue and two workers, because the alternative - building on import, in
//! front of the user - trades the lag they complained about for a wait they
//! cannot skip.
//!
//! Nothing here is required for correctness. A proxy that has not been built
//! yet, or failed, simply is not there, and the pool falls back to the
//! original: the feature degrades to exactly the behaviour that existed
//! before it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use tauri::Emitter;

/// How many proxies build at once.
///
/// Two: enough to use a modern machine, few enough to leave room for the
/// preview being scrubbed right now - which is the thing this exists to make
/// fast, and would be a poor trade to starve.
const WORKERS: usize = 2;

struct Job {
    source: PathBuf,
    destination: PathBuf,
    height: u32,
    /// The source's length in seconds, from the probe the import already did.
    /// What turns FFmpeg's "encoded 41 seconds" into a fraction of a bar.
    duration: f64,
}

#[derive(Default)]
struct Pending {
    queue: VecDeque<Job>,
    /// Sources already queued or building. A reopened project asks for every
    /// clip's proxy again, and without this each ask would be another encode
    /// of a file already being encoded.
    claimed: HashSet<PathBuf>,
    /// How far each build in flight has got, in 0..=1. A surveillance hour
    /// takes minutes to transcode, so a bar counting whole files would sit at
    /// zero for all of it - which is not a progress bar, it is a spinner that
    /// lies about being one.
    active: HashMap<PathBuf, f64>,
    building: usize,
    built: usize,
    failed: usize,
}

struct Shared {
    pending: Mutex<Pending>,
    wake: Condvar,
}

/// What the UI is told after every change, so a settings panel can say how
/// far along the work is without polling.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    queued: usize,
    building: usize,
    built: usize,
    failed: usize,
    /// Mean progress of the builds in flight, in 0..=1; zero when none are.
    /// A mean rather than one file's share because two workers run at once
    /// and neither of them is "the" one.
    fraction: f64,
}

pub struct ProxyState(Arc<Shared>);

/// The lock, taking the data back rather than panicking on a poisoned one.
///
/// Same policy and same reasoning as `playback.rs`: a worker that dies on one
/// unreadable file must not stop the others, and everything behind this lock
/// is a work list that is rebuilt by the next import anyway.
fn held(shared: &Shared) -> MutexGuard<'_, Pending> {
    shared.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lets the webview read one proxy.
///
/// The asset scope starts empty and grows only where the user expresses
/// intent - importing media, opening a project that lists it. A proxy is this
/// app's own file, derived from an original that already passed that test, so
/// granting it adds no reach the window did not already have. It is needed
/// because the monitor's `<video>` reads through the asset protocol, and
/// leaving it on the original would keep dragging slow for the picture the
/// user sees first.
fn grant(app: &tauri::AppHandle, path: &std::path::Path) {
    crate::grant_asset(app, &path.to_string_lossy());
}

fn announce(app: &tauri::AppHandle, pending: &Pending) {
    let fraction = if pending.active.is_empty() {
        0.0
    } else {
        pending.active.values().sum::<f64>() / pending.active.len() as f64
    };
    let _ = app.emit(
        "proxies",
        Progress {
            queued: pending.queue.len(),
            building: pending.building,
            built: pending.built,
            failed: pending.failed,
            fraction,
        },
    );
}

impl ProxyState {
    /// Starts the workers. They live as long as the app does and spend all of
    /// it asleep on the condvar unless there is something to build.
    pub fn new(app: tauri::AppHandle) -> Self {
        let shared = Arc::new(Shared { pending: Mutex::new(Pending::default()), wake: Condvar::new() });
        for worker in 0..WORKERS {
            let shared = Arc::clone(&shared);
            let app = app.clone();
            std::thread::Builder::new()
                .name(format!("wolfcut-proxy-{worker}"))
                .spawn(move || run(&shared, &app))
                .expect("could not spawn a proxy worker");
        }
        Self(shared)
    }
}

/// One worker: take the oldest job, build it, report, repeat.
///
/// Oldest first, unlike the playback decoder's newest-first queue: proxies are
/// asked for in import order and there is no "what the user is looking at
/// right now" to prefer.
fn run(shared: &Shared, app: &tauri::AppHandle) {
    loop {
        let job = {
            let mut pending = held(shared);
            let job = loop {
                match pending.queue.pop_front() {
                    Some(job) => break job,
                    None => {
                        pending = shared
                            .wake
                            .wait(pending)
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                    }
                }
            };
            pending.building += 1;
            pending.active.insert(job.source.clone(), 0.0);
            announce(app, &pending);
            job
        };

        let source = job.source.clone();
        let duration = job.duration;
        let outcome = wolfcut_media::proxy::generate(
            &job.source,
            &job.destination,
            job.height,
            |seconds| {
                if duration <= 0.0 {
                    return;
                }
                let fraction = (seconds / duration).clamp(0.0, 1.0);
                let mut pending = held(shared);
                let Some(slot) = pending.active.get_mut(&source) else { return };
                // Whole percents only. FFmpeg reports several times a second
                // and every announcement crosses into the webview, where it
                // re-renders a bar that cannot show more than a percent.
                if (fraction * 100.0) as u32 == (*slot * 100.0) as u32 {
                    return;
                }
                *slot = fraction;
                announce(app, &pending);
            },
        );

        let mut pending = held(shared);
        pending.building -= 1;
        pending.active.remove(&job.source);
        pending.claimed.remove(&job.source);
        match outcome {
            Ok(()) => {
                pending.built += 1;
                grant(app, &job.destination);
            }
            Err(error) => {
                pending.failed += 1;
                // Counted, not toasted. A folder of unreadable files would
                // otherwise produce a notification each; the settings panel
                // shows the total, and the editor carries on with originals.
                eprintln!("WolfCut: no proxy for {}: {error}", job.source.display());
            }
        }
        announce(app, &pending);
    }
}

/// Points the preview pool at a proxy folder, or back at the originals.
///
/// Called when the setting changes and once at startup. The pool checks for
/// each proxy as it serves frames, so a folder set here takes effect on the
/// next frame drawn, with no reopening of anything.
#[tauri::command]
pub fn proxy_configure(
    pool: tauri::State<'_, crate::PoolState>,
    directory: Option<String>,
    height: u32,
) -> Result<(), String> {
    let mut pool = pool.0.lock().map_err(|_| "reader pool poisoned".to_owned())?;
    pool.use_proxies(directory.map(PathBuf::from), height);
    Ok(())
}

/// What `ensure_proxy` decided about one file.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyStatus {
    /// Already built; the preview is already using it.
    Ready,
    /// Queued, or being built now. Worth asking again once the workers go
    /// quiet - that is when the path arrives.
    Building,
    /// Not worth one: the source is no taller than the proxy would be. Never
    /// worth asking about again.
    Skipped,
}

/// What the window gets back: the decision, and the file when there is one.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyAnswer {
    status: ProxyStatus,
    /// Where the stand-in is, once it exists. The window points its own
    /// `<video>` at this, which is the half of the preview the engine's
    /// substitution cannot reach.
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

/// Makes sure a stand-in exists for one file, building it if it does not.
///
/// Fire and forget, like artwork: the answer says what will happen, not what
/// has happened, and the preview picks the proxy up on its own once it lands.
/// `source_height` comes from the probe the importer already did rather than
/// from a second one here.
#[tauri::command]
pub fn ensure_proxy(
    state: tauri::State<'_, ProxyState>,
    app: tauri::AppHandle,
    path: String,
    directory: String,
    height: u32,
    source_height: u32,
    duration: f64,
) -> ProxyAnswer {
    if !wolfcut_media::proxy::worth_proxying(source_height, height) {
        return ProxyAnswer { status: ProxyStatus::Skipped, path: None };
    }
    let source = PathBuf::from(&path);
    let destination = wolfcut_media::proxy::path_in(std::path::Path::new(&directory), &source, height);
    if destination.is_file() {
        grant(&app, &destination);
        return ProxyAnswer {
            status: ProxyStatus::Ready,
            path: Some(destination.to_string_lossy().into_owned()),
        };
    }

    let mut pending = held(&state.0);
    if pending.claimed.insert(source.clone()) {
        pending.queue.push_back(Job { source, destination, height, duration });
        announce(&app, &pending);
        drop(pending);
        state.0.wake.notify_one();
    }
    ProxyAnswer { status: ProxyStatus::Building, path: None }
}

/// How many bytes of proxies are sitting in `directory`.
///
/// Only the files this app names, so pointing the setting at a folder that
/// holds other things reports - and later deletes - none of them.
#[tauri::command]
pub fn proxy_usage(directory: String) -> u64 {
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| ours(&entry.file_name().to_string_lossy()))
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Deletes every proxy in `directory`, returning how many bytes came back.
#[tauri::command]
pub fn clear_proxies(directory: String) -> u64 {
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return 0;
    };
    let mut freed = 0;
    for entry in entries.flatten() {
        if !ours(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if std::fs::remove_file(entry.path()).is_ok() {
            freed += size;
        }
    }
    freed
}

/// Whether a filename is one this app wrote.
///
/// The proxy folder is the user's own directory and may hold anything;
/// deleting a file because it happened to be in there would be unforgivable,
/// so both the usage total and the clear are limited to the shape
/// `wolfcut_media::proxy::file_name` produces - sixteen hex digits, a height,
/// and the extension.
fn ours(name: &str) -> bool {
    let Some((hash, tail)) = name.split_once('-') else {
        return false;
    };
    hash.len() == 16
        && hash.chars().all(|character| character.is_ascii_hexdigit())
        && (tail.ends_with("p.mp4") || tail.ends_with("p.partial"))
        && tail.trim_end_matches("p.mp4").trim_end_matches("p.partial").parse::<u32>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_this_app_wrote_are_counted_or_deleted() {
        // The folder is the user's. Anything that is not exactly the shape the
        // engine writes has to survive a clear.
        assert!(ours(&wolfcut_media::proxy::file_name(std::path::Path::new("/a.mp4"), 720)));
        assert!(ours("0123456789abcdef-1080p.mp4"));
        assert!(ours("0123456789abcdef-540p.partial"), "an abandoned build is ours to sweep");

        assert!(!ours("holiday.mp4"), "a real file that happens to live here");
        assert!(!ours("0123456789abcdef-720p.mov"), "not an extension we write");
        assert!(!ours("0123456789abcde-720p.mp4"), "hash too short");
        assert!(!ours("0123456789abcdefg-720p.mp4"), "hash too long");
        assert!(!ours("zzzzzzzzzzzzzzzz-720p.mp4"), "not hex");
        assert!(!ours("0123456789abcdef-bigp.mp4"), "height is not a number");
        assert!(!ours("notes.txt"));
        assert!(!ours("-720p.mp4"));
    }
}
