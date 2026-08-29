//! The engine-owned editing session, exposed to the UI.
//!
//! One session at a time, held in managed state: open a project and the
//! engine holds the edit; every mutation arrives as a `wolfcut_project`
//! [`Command`], is applied with undo recorded, and the new state goes back
//! over the wire. This is the API `lib/editor.ts` mirrors; the provisional
//! TypeScript model it replaced is gone - see
//! `engine/docs/decisions/0007-engine-owns-the-project.md`.
//!
//! Saving reuses `projects::save`'s temp-file-and-rename, so the document on
//! disk is written by exactly one code path whichever side owns the model.

use std::sync::Mutex;

use wolfcut_project::{Command, DocumentSettings, Editor};
use serde::Serialize;

use crate::projects;

/// The one editing session, or None before a project is opened.
pub struct EditorState(pub Mutex<Option<Session>>);

pub struct Session {
    /// The project folder, for saving.
    path: String,
    settings: DocumentSettings,
    editor: Editor,
}

/// What every mutating call returns: the authoritative state plus history
/// availability, so the UI's undo/redo affordances are never guessing.
#[derive(Serialize)]
#[cfg_attr(feature = "types", derive(ts_rs::TS))]
#[cfg_attr(feature = "types", ts(export))]
#[serde(rename_all = "camelCase")]
pub struct EditorView {
    project: wolfcut_project::Project,
    can_undo: bool,
    can_redo: bool,
    /// The settings as the session holds them - the document's own output
    /// size wins over the manifest's on open, exactly as the old loader
    /// preferred it.
    settings: SettingsView,
    /// The id a creating command minted, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "types", ts(optional))]
    created_id: Option<String>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "types", derive(ts_rs::TS))]
#[cfg_attr(feature = "types", ts(export))]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    name: String,
    width: u32,
    height: u32,
    // i64 over the wire is a plain JSON number, not a bigint - serde_json
    // writes it bare and the UI reads it with JSON.parse.
    #[cfg_attr(feature = "types", ts(type = "number"))]
    rate_num: i64,
    #[cfg_attr(feature = "types", ts(type = "number"))]
    rate_den: i64,
}

fn view(session: &Session, created_id: Option<String>) -> EditorView {
    EditorView {
        project: session.editor.project().clone(),
        can_undo: session.editor.can_undo(),
        can_redo: session.editor.can_redo(),
        settings: SettingsView {
            name: session.settings.name.clone(),
            width: session.settings.width,
            height: session.settings.height,
            rate_num: session.settings.rate_num,
            rate_den: session.settings.rate_den,
        },
        created_id,
    }
}

/// The active timeline flattened for rendering, plus the session settings.
///
/// This is what export and preview consume. It used to be a clip list the
/// UI flattened and sent over the wire, which made the frontend's copy of
/// the model - not the model - the thing that rendered; see engine decision
/// 0009. Now the engine flattens its own session and the wire carries only
/// what the UI genuinely owns (the destination, the quality, its rasterised
/// titles).
pub fn flattened_clips(
    state: &EditorState,
) -> Result<(Vec<wolfcut_export::ExportClip>, DocumentSettings), String> {
    with_session(state, |session| {
        Ok((
            wolfcut_export::flatten::flatten_timeline(session.editor.project(), None),
            session.settings.clone(),
        ))
    })
}

/// The open session's document, project folder and settings, for features
/// that package the current edit (saving it as a template) rather than
/// editing it.
pub fn session_snapshot(
    state: &EditorState,
) -> Result<(serde_json::Value, String, DocumentSettings), String> {
    with_session(state, |session| {
        Ok((
            session.editor.to_document(&session.settings),
            session.path.clone(),
            session.settings.clone(),
        ))
    })
}

fn with_session<T>(
    state: &EditorState,
    operation: impl FnOnce(&mut Session) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = state.0.lock().map_err(|_| "editor state poisoned".to_owned())?;
    let session = guard.as_mut().ok_or("no project is open")?;
    operation(session)
}

/// Opens a project folder as the editing session and returns its state.
///
/// A folder whose document is missing or unreadable opens as an empty
/// project rather than failing - the same grace the TS loader extends -
/// but a *corrupt* document is an error, because silently replacing an
/// edit with emptiness is how projects get lost.
#[tauri::command]
pub async fn editor_open(
    app: tauri::AppHandle,
    state: tauri::State<'_, EditorState>,
    path: String,
    name: String,
    width: u32,
    height: u32,
    rate_num: i64,
    rate_den: i64,
) -> Result<EditorView, String> {
    // Reading and parsing the document is the slow part and needs no state,
    // so it runs on a blocking thread; the session lock is taken only for the
    // in-memory install below.
    let settings = DocumentSettings { name, width, height, rate_num, rate_den };
    let session = tauri::async_runtime::spawn_blocking(move || load_session(path, settings))
        .await
        .map_err(|error| format!("open task failed: {error}"))??;

    // Everything the document lists is media the user already imported, so
    // reopening a project restores exactly the asset scope importing built -
    // no re-probe, and nothing beyond the document's own list.
    for item in &session.editor.project().media {
        crate::grant_asset(&app, &item.path);
    }

    install(&state, session)
}

/// Reads a project folder into a session, touching no shared state.
///
/// Split out of [`editor_open`] so what a folder *means* can be tested without
/// standing up a Tauri app - this is where "#11 import before open" and "#12 a
/// fresh project reopens corrupt" both lived, and neither needed a window to
/// reproduce.
pub(crate) fn load_session(path: String, settings: DocumentSettings) -> Result<Session, String> {
    let mut settings = settings;
    let editor = match projects::read_document(&path) {
        Ok(document) => {
            // The document's frame wins over the manifest: it is where an
            // edited output size was saved.
            if let Some(video) = document.get("video") {
                if let (Some(width), Some(height)) = (
                    video.get("width").and_then(|value| value.as_u64()),
                    video.get("height").and_then(|value| value.as_u64()),
                ) {
                    if width > 0 && height > 0 {
                        settings.width = width as u32;
                        settings.height = height as u32;
                    }
                }
            }
            match Editor::from_document(&document) {
                Some(editor) => editor,
                // The settings-only manifest `create` writes: a project
                // closed before its first edit reopens empty, it is not
                // corrupt (#12).
                None if projects::is_settings_only(&document) => Editor::new(),
                None => {
                    return Err(format!("{path} holds a document this build cannot read"));
                }
            }
        }
        // No document yet - a project created moments ago.
        Err(_) => Editor::new(),
    };
    Ok(Session { path, settings, editor })
}

/// Makes `session` the open one, replacing whatever was there.
pub(crate) fn install(state: &EditorState, session: Session) -> Result<EditorView, String> {
    let mut guard = state.0.lock().map_err(|_| "editor state poisoned".to_owned())?;
    // `insert` hands back the reference it just stored, which is how this
    // reads the new session without an `expect("just set")` standing between
    // the write and the read.
    let installed = guard.insert(session);
    Ok(view(installed, None))
}

/// Applies one edit command and returns the new state.
#[tauri::command]
pub fn editor_apply(
    state: tauri::State<'_, EditorState>,
    command: Command,
) -> Result<EditorView, String> {
    apply_to(&state, command)
}

#[tauri::command]
pub fn editor_undo(state: tauri::State<'_, EditorState>) -> Result<EditorView, String> {
    undo_in(&state)
}

#[tauri::command]
pub fn editor_redo(state: tauri::State<'_, EditorState>) -> Result<EditorView, String> {
    redo_in(&state)
}

/// The current state without changing anything.
#[tauri::command]
pub fn editor_state(state: tauri::State<'_, EditorState>) -> Result<EditorView, String> {
    with_session(&state, |session| Ok(view(session, None)))
}

// The four above are one line each because their bodies take `&EditorState`
// rather than `tauri::State`: that is the difference between a session
// lifecycle a test can drive and one that needs a running app.

pub(crate) fn apply_to(state: &EditorState, command: Command) -> Result<EditorView, String> {
    with_session(state, |session| {
        let outcome = session.editor.apply(command).map_err(|error| error.to_string())?;
        Ok(view(session, outcome.created_id))
    })
}

pub(crate) fn undo_in(state: &EditorState) -> Result<EditorView, String> {
    with_session(state, |session| {
        session.editor.undo();
        Ok(view(session, None))
    })
}

pub(crate) fn redo_in(state: &EditorState) -> Result<EditorView, String> {
    with_session(state, |session| {
        session.editor.redo();
        Ok(view(session, None))
    })
}

/// Writes the session's document to its project folder. The output size can
/// have been edited in the preview footer and the name in the project
/// details dialog, so both ride along here.
#[tauri::command]
pub async fn editor_save(
    state: tauri::State<'_, EditorState>,
    name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(), String> {
    // Settings mutation and serialisation happen under the lock; the disk
    // write - the slow, blockable part - happens off the main thread with the
    // lock released.
    let (path, document) = with_session(&state, |session| {
        if let Some(name) = name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                session.settings.name = trimmed.to_owned();
            }
        }
        // A zero dimension is never a real output size, only a caller bug -
        // saving it would poison the document until the next open.
        if let Some(width) = width.filter(|width| *width > 0) {
            session.settings.width = width;
        }
        if let Some(height) = height.filter(|height| *height > 0) {
            session.settings.height = height;
        }
        Ok((session.path.clone(), session.editor.to_document(&session.settings)))
    })?;
    tauri::async_runtime::spawn_blocking(move || projects::save(&path, &document))
        .await
        .map_err(|error| format!("save task failed: {error}"))?
}

/// Closes the session, dropping its undo history.
///
/// A poisoned lock is closed anyway. Skipping it left the dead session
/// installed, and since every other entry point turns poison into an error,
/// the app then refused to open *any* project - one panic in one command
/// bricked the editor until it was restarted. Nothing here needs the state to
/// be consistent: it is being dropped. Clearing the poison afterwards is what
/// lets the next `editor_open` install a healthy session.
#[tauri::command]
pub fn editor_close(state: tauri::State<'_, EditorState>) {
    close_in(&state);
}

pub(crate) fn close_in(state: &EditorState) {
    let mut guard = state.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
    drop(guard);
    state.0.clear_poison();
}

#[cfg(test)]
mod tests {
    //! The session lifecycle, driven without a window.
    //!
    //! Everything here goes through the same functions the commands do; only
    //! the `tauri::State` wrapper is missing. Three of the bugs this file has
    //! produced - a fresh project reopening corrupt, edits arriving before the
    //! session, a panic bricking the editor - were all reproducible at this
    //! level, and none of them needed a running app to find.

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn settings() -> DocumentSettings {
        DocumentSettings {
            name: "Take".to_owned(),
            width: 1920,
            height: 1080,
            rate_num: 30,
            rate_den: 1,
        }
    }

    fn empty_state() -> EditorState {
        EditorState(Mutex::new(None))
    }

    /// A project folder in the temp directory, holding `document` or nothing.
    ///
    /// The counter is what keeps two tests in the same run from sharing a
    /// folder; the process id keeps two runs apart.
    fn temp_project(document: Option<serde_json::Value>) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root: PathBuf = std::env::temp_dir()
            .join(format!("wolfcut-session-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&root).expect("temp project folder");
        let manifest = root.join("wolfcut.json");
        match document {
            Some(document) => std::fs::write(&manifest, serde_json::to_vec(&document).expect("json"))
                .expect("write manifest"),
            None => {
                let _ = std::fs::remove_file(&manifest);
            }
        }
        root.to_string_lossy().into_owned()
    }

    #[test]
    fn a_folder_with_no_document_yet_opens_empty() {
        // A project created moments ago, before anything was written.
        let path = temp_project(None);
        let session = load_session(path.clone(), settings()).expect("a fresh folder is not corrupt");
        assert!(session.editor.project().media.is_empty());
        assert_eq!(session.path, path);
        assert_eq!(session.settings.width, 1920);
    }

    #[test]
    fn a_settings_only_manifest_opens_empty_rather_than_corrupt() {
        // #12. The manifest `create` writes carries settings and no edit
        // state; a project closed before its first edit is empty, not broken.
        let path = temp_project(Some(serde_json::json!({
            "name": "Take",
            "video": { "width": 1920, "height": 1080 },
        })));
        let session = load_session(path, settings()).expect("settings-only is an empty project");
        assert!(session.editor.project().active().clips.is_empty());
    }

    #[test]
    fn a_document_this_build_cannot_read_is_an_error() {
        // The other half of the same decision: a document that claims edit
        // state and will not load must fail loudly. Opening it as empty is
        // how an edit gets replaced by nothing and then autosaved over.
        let path = temp_project(Some(serde_json::json!({ "timelines": 5 })));
        assert!(load_session(path, settings()).is_err());
    }

    #[test]
    fn the_documents_own_frame_wins_over_the_manifests() {
        // Where an edited output size was saved.
        let mut document = Editor::new().to_document(&settings());
        document["video"] = serde_json::json!({ "width": 1080, "height": 1920 });
        let session = load_session(temp_project(Some(document)), settings()).expect("loads");
        assert_eq!((session.settings.width, session.settings.height), (1080, 1920));
    }

    #[test]
    fn a_zero_frame_in_the_document_is_ignored() {
        // Zero is never a real output size, and honouring it would leave the
        // session unable to render anything at all.
        let mut document = Editor::new().to_document(&settings());
        document["video"] = serde_json::json!({ "width": 0, "height": 0 });
        let session = load_session(temp_project(Some(document)), settings()).expect("loads");
        assert_eq!((session.settings.width, session.settings.height), (1920, 1080));
    }

    #[test]
    fn every_command_is_refused_until_a_project_is_open() {
        // #11's engine-side half: the window seeds its queue with the open so
        // an early edit waits, but if one arrives anyway it must be refused
        // with a sentence, not applied to nothing.
        let state = empty_state();
        // `err()` rather than `unwrap_err()`: the latter wants Debug on the
        // success type, and EditorView is a wire struct with no reason to
        // grow one for a test's benefit.
        let refusal = Some("no project is open");
        assert_eq!(apply_to(&state, Command::AddTrack).err().as_deref(), refusal);
        assert_eq!(undo_in(&state).err().as_deref(), refusal);
        assert_eq!(redo_in(&state).err().as_deref(), refusal);
    }

    #[test]
    fn a_session_opens_edits_undoes_redoes_and_closes() {
        let state = empty_state();
        let opened = install(&state, load_session(temp_project(None), settings()).expect("loads"))
            .expect("installs");
        assert!(!opened.can_undo, "a freshly opened project has no history");

        let edited = apply_to(&state, Command::AddTrack).expect("adds a track");
        assert!(edited.can_undo);
        assert!(!edited.can_redo);

        let undone = undo_in(&state).expect("undoes");
        assert!(!undone.can_undo);
        assert!(undone.can_redo, "and the edit is still ahead of us");

        let redone = redo_in(&state).expect("redoes");
        assert!(redone.can_undo);
        assert!(!redone.can_redo);

        close_in(&state);
        assert_eq!(
            apply_to(&state, Command::AddTrack).err().as_deref(),
            Some("no project is open"),
            "a closed session is gone, not merely idle",
        );
    }

    #[test]
    fn a_poisoned_session_closes_and_lets_the_next_project_open() {
        // A panic inside any command poisons this lock. Skipping the close on
        // poison left the dead session installed, and since every other entry
        // point turns poison into an error, one panic then refused *every*
        // project until the app was restarted.
        let state = empty_state();
        install(&state, load_session(temp_project(None), settings()).expect("loads"))
            .expect("installs");

        // The hook is silenced so an expected panic does not print a stack
        // trace into an otherwise passing run.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.0.lock().expect("not poisoned yet");
            panic!("a command died holding the session");
        }));
        std::panic::set_hook(previous);

        assert!(state.0.is_poisoned(), "the panic poisoned the lock");
        assert!(apply_to(&state, Command::AddTrack).is_err(), "and every command with it");

        close_in(&state);
        assert!(!state.0.is_poisoned(), "closing a poisoned session clears it");
        install(&state, load_session(temp_project(None), settings()).expect("loads"))
            .expect("and the next project opens");
        apply_to(&state, Command::AddTrack).expect("editing works again");
    }
}
