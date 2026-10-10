//! Library and playlist commands: likes, playlist CRUD, reorder, and the
//! YouTube-Music / JSON import & export flows.

use rift_types::{events, Library, Playlist, Track};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tracing::{info, warn};

use super::convert::{is_audio_track, MAX_FETCH_TRACKS};
use super::downloads::start_downloads;
use crate::util::LockExt;
use crate::AppState;

fn emit_library(app: &AppHandle, library: &Library) {
    let _ = app.emit(events::LIBRARY, library);
}

#[tauri::command(rename_all = "snake_case")]
pub fn toggle_like(track: Track, app: AppHandle, state: State<'_, AppState>) {
    let mut lib = state.library.lock_safe();
    lib.toggle_like(track);
    emit_library(&app, &lib.data);
}

#[tauri::command(rename_all = "snake_case")]
pub fn create_playlist(
    name: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Playlist, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("playlist name cannot be empty".into());
    }
    let mut lib = state.library.lock_safe();
    let playlist = lib.create_playlist(name.to_string());
    emit_library(&app, &lib.data);
    Ok(playlist)
}

/// Extract a YouTube/YouTube Music playlist ID from a pasted URL, or accept a
/// bare ID. Returns `None` if nothing usable is found.
fn parse_playlist_id(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    // URL with a `list=` query parameter (music.youtube.com / youtube.com).
    if let Some(rest) = input.split("list=").nth(1) {
        let id: String = rest
            .chars()
            .take_while(|c| *c != '&' && *c != '#')
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    // Otherwise treat the whole thing as a bare ID, but only if it looks like
    // one (no scheme/spaces) so a stray URL without `list=` doesn't slip through.
    if !input.contains("://") && !input.contains(char::is_whitespace) {
        return Some(input.to_string());
    }
    None
}

/// Import a YouTube Music (or YouTube) playlist by URL or ID into the library as
/// a new local playlist. Audio-only tracks are kept (videos/episodes dropped).
#[tauri::command(rename_all = "snake_case")]
pub async fn import_yt_playlist(
    url: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Playlist, String> {
    let id = parse_playlist_id(&url)
        .ok_or_else(|| "Couldn't find a playlist ID in that link.".to_string())?;
    info!("importing playlist {id}");

    let pl = state
        .player
        .yt
        .playlist(&id, MAX_FETCH_TRACKS)
        .await
        .map_err(|e| format!("could not load playlist: {e}"))?;

    let Playlist {
        name, mut tracks, ..
    } = pl;
    tracks.retain(is_audio_track);
    if tracks.is_empty() {
        return Err("That playlist has no playable songs.".into());
    }

    let name = if name.trim().is_empty() {
        "Imported playlist".to_string()
    } else {
        name
    };
    let playlist = {
        let mut lib = state.library.lock_safe();
        let playlist = lib.create_playlist_with(name, tracks);
        emit_library(&app, &lib.data);
        playlist
    };
    Ok(playlist)
}

/// Make a string safe to use as a filename: keep word chars, space, dash; drop
/// the rest. Avoids path separators and other awkward characters.
fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "playlist".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Export a playlist to a JSON file the user picks via a native Save dialog.
/// The file is Rift's own `Playlist` JSON, re-importable losslessly.
#[tauri::command(rename_all = "snake_case")]
pub fn export_playlist(id: String, app: AppHandle, state: State<'_, AppState>) {
    // Snapshot and serialize under the lock, before showing the dialog.
    let (file_name, json) = {
        let lib = state.library.lock_safe();
        let Some(p) = lib.data.playlists.iter().find(|p| p.id == id) else {
            return;
        };
        match serde_json::to_vec_pretty(p) {
            Ok(json) => (format!("{}.json", safe_filename(&p.name)), json),
            Err(e) => {
                warn!("could not serialize playlist {id}: {e}");
                return;
            }
        }
    };

    let app2 = app.clone();
    app.dialog()
        .file()
        .add_filter("Rift playlist", &["json"])
        .set_file_name(file_name)
        .save_file(move |path| {
            // `None` = user cancelled.
            let Some(path) = path.and_then(|p| p.into_path().ok()) else {
                return;
            };
            match std::fs::write(&path, &json) {
                Ok(()) => {
                    let _ = app2.emit(events::NOTICE, format!("Exported to {}", path.display()));
                }
                Err(e) => {
                    let _ = app2.emit(events::ERROR, format!("Could not export playlist: {e}"));
                }
            }
        });
}

/// Parse a Rift playlist document the user picked.
///
/// Split out of the dialog callback so the contract is behavioural rather than a
/// comment: this path is a **lossless** round trip of whatever
/// [`export_playlist`] wrote, and deliberately applies no audio/UGC filter. The
/// innertube import applies one because it is building a playlist from search
/// results, which mix in music videos and episodes; a filter here would silently
/// drop rows the file contained, and the file is the only thing the user has to
/// go on. The store's dedup-by-id is the sole transformation after this, and it
/// keys on identity rather than on anything about the title.
pub(crate) fn import_playlist_from(body: &[u8]) -> Result<Playlist, String> {
    serde_json::from_slice(body).map_err(|_| "That isn't a valid Rift playlist file.".to_owned())
}

/// Import a playlist from a Rift JSON file the user picks via a native Open
/// dialog. Always created as a new playlist (fresh id), never overwriting.
#[tauri::command(rename_all = "snake_case")]
pub fn import_playlist(app: AppHandle) {
    let app2 = app.clone();
    app.dialog()
        .file()
        .add_filter("Rift playlist", &["json"])
        .pick_file(move |path| {
            let Some(path) = path.and_then(|p| p.into_path().ok()) else {
                return; // cancelled
            };
            let parsed = match std::fs::read(&path)
                .map_err(|_| "That isn't a valid Rift playlist file.".to_owned())
                .and_then(|body| import_playlist_from(&body))
            {
                Ok(p) => p,
                Err(msg) => {
                    let _ = app2.emit(events::ERROR, msg);
                    return;
                }
            };
            let state = app2.state::<AppState>();
            let (data, id, name, count) = {
                let mut lib = state.library.lock_safe();
                let p = lib.create_playlist_with(parsed.name, parsed.tracks);
                let count = p.tracks.len();
                (lib.data.clone(), p.id, p.name, count)
            };
            emit_library(&app2, &data);
            // Jump the UI to the freshly imported playlist.
            let _ = app2.emit(events::OPEN_PLAYLIST, &id);
            let _ = app2.emit(
                events::NOTICE,
                format!(
                    "Imported \u{201c}{name}\u{201d} ({count} song{})",
                    if count == 1 { "" } else { "s" }
                ),
            );
        });
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_playlist(id: String, app: AppHandle, state: State<'_, AppState>) {
    let mut lib = state.library.lock_safe();
    lib.delete_playlist(&id);
    emit_library(&app, &lib.data);
}

#[tauri::command(rename_all = "snake_case")]
pub fn rename_playlist(id: String, name: String, app: AppHandle, state: State<'_, AppState>) {
    let mut lib = state.library.lock_safe();
    lib.rename_playlist(&id, name);
    emit_library(&app, &lib.data);
}

#[tauri::command(rename_all = "snake_case")]
pub fn move_playlist(id: String, to: usize, app: AppHandle, state: State<'_, AppState>) {
    let mut lib = state.library.lock_safe();
    if lib.move_playlist(&id, to) {
        emit_library(&app, &lib.data);
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn add_to_playlist(id: String, track: Track, app: AppHandle, state: State<'_, AppState>) {
    // A playlist whose every track is already downloaded is treated as "kept
    // offline": adding a new track should pull it down too, so the playlist
    // stays fully available offline. Evaluated before the add, and only for a
    // non-empty playlist that doesn't already contain this track.
    let keep_offline = {
        let lib = state.library.lock_safe();
        lib.data
            .playlists
            .iter()
            .find(|p| p.id == id)
            .is_some_and(|p| {
                !p.tracks.iter().any(|t| t.id == track.id)
                    && super::downloads::playlist_fully_downloaded(p, |tid| {
                        state.downloads.is_downloaded(tid)
                    })
            })
    };

    {
        let mut lib = state.library.lock_safe();
        lib.add_to_playlist(&id, track.clone());
        emit_library(&app, &lib.data);
    }

    if keep_offline {
        start_downloads(
            vec![track],
            app,
            state.downloads.clone(),
            state.player.clone(),
        );
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn remove_from_playlist(
    id: String,
    track_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) {
    let mut lib = state.library.lock_safe();
    lib.remove_from_playlist(&id, &track_id);
    emit_library(&app, &lib.data);
}

#[cfg(test)]
mod tests {
    use super::{import_playlist_from, parse_playlist_id};
    use rift_types::{Playlist, Track};

    fn track(id: &str, title: &str) -> Track {
        Track {
            id: id.into(),
            title: title.into(),
            artist: String::new(),
            album: None,
            duration: None,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    /// The JSON import is a lossless round trip, and that is a behavioural
    /// contract rather than a comment asking the next maintainer to be careful.
    /// A filter added here (the tempting "copy what the innertube import does")
    /// would silently drop rows the file contained: the export wrote them, the
    /// file is the user's only copy, and nothing would report the loss.
    #[test]
    fn importing_a_playlist_keeps_every_row_including_a_video_titled_one() {
        let exported = Playlist {
            id: "pl-1".into(),
            name: "Mix".into(),
            tracks: vec![
                track("a", "Ordinary Song"),
                track("b", "Some Song (Official Video)"),
            ],
        };
        // Exactly what `export_playlist` writes.
        let body = serde_json::to_vec(&exported).expect("serialize the export");

        let imported = import_playlist_from(&body).expect("a well-formed Rift playlist imports");

        assert_eq!(
            imported, exported,
            "the imported playlist must equal the exported one"
        );
        assert!(
            imported
                .tracks
                .iter()
                .any(|t| t.title.contains("Official Video")),
            "the video-titled row has to survive: {imported:?}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_playlist_reports_why() {
        assert!(import_playlist_from(b"{ not json").is_err());
        // Valid JSON, wrong shape — still refused rather than half-imported.
        assert!(import_playlist_from(b"{\"name\": \"Mix\"}").is_err());
    }

    #[test]
    fn parses_playlist_ids_from_urls_and_bare_ids() {
        assert_eq!(
            parse_playlist_id("https://music.youtube.com/playlist?list=PLabc123"),
            Some("PLabc123".into())
        );
        assert_eq!(
            parse_playlist_id("https://www.youtube.com/playlist?list=OLAK5uy_x&si=foo"),
            Some("OLAK5uy_x".into()),
            "stops at the next query param"
        );
        assert_eq!(parse_playlist_id("  PLbareId  "), Some("PLbareId".into()));
        assert_eq!(parse_playlist_id(""), None);
        assert_eq!(
            parse_playlist_id("https://youtube.com/watch?v=abc"),
            None,
            "a non-playlist URL yields nothing"
        );
    }
}
