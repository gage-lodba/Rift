//! Library and playlist commands: likes, playlist CRUD, reorder, and the
//! YouTube-Music / JSON import & export flows.

use rift_types::{events, Library, Playlist, Track};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tracing::{info, warn};

use super::convert::{convert, is_audio_track, MAX_FETCH_TRACKS};
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

    let q = state.player.rp.query();
    let mut pl = q
        .music_playlist(&id)
        .await
        .map_err(|e| format!("could not load playlist: {e}"))?;
    // Pull beyond the first page so long playlists import in full (bounded).
    // A continuation failure still imports what loaded, but shouldn't be silent.
    if let Err(e) = pl.tracks.extend_limit(&q, MAX_FETCH_TRACKS).await {
        warn!("playlist continuation failed; importing a partial list: {e}");
    }

    let tracks: Vec<Track> = pl
        .tracks
        .items
        .into_iter()
        .filter(is_audio_track)
        .map(convert)
        .collect();
    if tracks.is_empty() {
        return Err("That playlist has no playable songs.".into());
    }

    let name = if pl.name.trim().is_empty() {
        "Imported playlist".to_string()
    } else {
        pl.name
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
            let parsed: Playlist = match std::fs::read(&path)
                .ok()
                .and_then(|d| serde_json::from_slice(&d).ok())
            {
                Some(p) => p,
                None => {
                    let _ = app2.emit(events::ERROR, "That isn't a valid Rift playlist file.");
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
    use super::parse_playlist_id;

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
