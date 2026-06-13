// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod commands;
mod downloads;
mod library;
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
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("rift=debug,rustypipe=info")),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            tracing::info!("data dir: {}", data_dir.display());

            let rp = RustyPipe::builder().storage_dir(&data_dir).build()?;
            let library = Arc::new(Mutex::new(LibraryStore::load(&data_dir)));
            let downloads = Arc::new(Downloads::load(data_dir.join("downloads")));
            let settings = SettingsStore::load(&data_dir);
            let volume = settings.data.volume;
            tracing::info!("restored volume {volume}");
            let settings = Arc::new(Mutex::new(settings));

            let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
            let audio_tx = audio::spawn(event_tx);
            // Apply the persisted volume to the fresh audio thread.
            let _ = audio_tx.send(AudioCmd::Volume(volume));

            let player = Arc::new(PlayerShared {
                core: Mutex::new(PlayerCore {
                    volume,
                    ..PlayerCore::default()
                }),
                audio: audio_tx,
                rp,
                http: reqwest::Client::new(),
            });

            app.manage(AppState {
                player,
                library,
                downloads,
                settings,
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
