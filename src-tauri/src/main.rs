// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod commands;
mod discord;
mod downloads;
mod library;
mod media;
mod player;
mod settings;
mod util;

use std::sync::{Arc, Mutex};

use downloads::Downloads;
use library::LibraryStore;
use player::{PlayerCore, PlayerShared};
use rustypipe::client::RustyPipe;
use settings::SettingsStore;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

use crate::audio::AudioCmd;

pub struct AppState {
    pub player: Arc<PlayerShared>,
    pub library: Arc<Mutex<LibraryStore>>,
    pub downloads: Arc<Downloads>,
    pub settings: Arc<Mutex<SettingsStore>>,
    pub media: media::MediaHandle,
    pub discord: discord::DiscordHandle,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("rift=debug,rustypipe=info")),
        )
        .init();

    tauri::Builder::default()
        // Must be the first plugin: on a second launch it focuses the running
        // window instead of starting a rival instance (and a second audio
        // thread fighting over the output device).
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // Remember window size/position across restarts.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            tracing::info!("data dir: {}", data_dir.display());

            let rp = RustyPipe::builder().storage_dir(&data_dir).build()?;
            let library = Arc::new(Mutex::new(LibraryStore::load(&data_dir)));
            let downloads = Arc::new(Downloads::load(data_dir.join("downloads")));
            let settings = SettingsStore::load(&data_dir);
            let volume = settings.data.volume;
            let discord_rpc = settings.data.discord_rpc;
            tracing::info!("restored volume {volume}");
            let settings = Arc::new(Mutex::new(settings));

            let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
            let audio_tx = audio::spawn(event_tx);
            // Apply the persisted volume to the fresh audio thread.
            let _ = audio_tx.send(AudioCmd::Volume(volume));

            // Restore the previous session's queue (stopped, not auto-playing).
            let playback_path = data_dir.join("playback.json");
            let mut core = PlayerCore {
                volume,
                ..PlayerCore::default()
            };
            if let Some(snap) = player::load_snapshot(&playback_path) {
                tracing::info!("restored queue of {} tracks", snap.tracks.len());
                core.shuffle = snap.shuffle;
                core.repeat = snap.repeat;
                core.source = snap.source;
                core.current = snap.current.filter(|&i| i < snap.tracks.len());
                core.queue = snap.tracks;
                if let Some(t) = core.current.and_then(|i| core.queue.get(i)) {
                    core.duration = t.duration.unwrap_or(0) as f64;
                }
                if core.shuffle {
                    core.shuffle_history = core.current.into_iter().collect();
                }
            }

            let player = Arc::new(PlayerShared {
                core: Mutex::new(core),
                audio: audio_tx,
                rp,
                http: reqwest::Client::new(),
                playback_path,
                persist: util::Persister::spawn(),
            });

            let media = media::spawn(app.handle().clone());
            let discord = discord::spawn(discord_rpc);

            app.manage(AppState {
                player,
                library,
                downloads,
                settings,
                media,
                discord,
            });
            tauri::async_runtime::spawn(player::event_loop(app.handle().clone(), event_rx));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::search_artists,
            commands::search_albums,
            commands::get_artist,
            commands::get_album,
            commands::play_tracks,
            commands::play_track,
            commands::toggle_play,
            commands::next_track,
            commands::prev_track,
            commands::seek,
            commands::set_volume,
            commands::save_volume,
            commands::set_discord_rpc,
            commands::toggle_shuffle,
            commands::cycle_repeat,
            commands::queue_add,
            commands::queue_remove,
            commands::queue_jump,
            commands::queue_clear,
            commands::toggle_like,
            commands::create_playlist,
            commands::delete_playlist,
            commands::rename_playlist,
            commands::add_to_playlist,
            commands::remove_from_playlist,
            commands::bootstrap,
            commands::download_tracks,
            commands::remove_downloads,
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Rift")
}
