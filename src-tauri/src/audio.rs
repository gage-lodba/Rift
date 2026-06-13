//! Dedicated audio thread owning the rodio output device.
//!
//! rodio's output stream is not `Send`, so it lives on its own OS thread and
//! is driven through a channel. Position/end-of-track notifications flow back
//! through a tokio channel to the player manager.

use std::io::Cursor;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use rodio::stream::DeviceSinkBuilder;
use rodio::{Decoder, Player, Source};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};

pub enum AudioCmd {
    /// Decode and play a full in-memory audio file, replacing whatever plays.
    Play(Vec<u8>),
    Pause,
    Resume,
    Stop,
    /// Seek to an absolute position in seconds.
    Seek(f64),
    /// Volume in 0.0..=1.0.
    Volume(f32),
}

pub enum AudioEvent {
    /// Authoritative track length in seconds, read from the decoder.
    Duration(f64),
    /// Current playback position in seconds.
    Position(f64),
    /// The current track finished playing.
    Ended,
    /// Decoding or device failure for the current track.
    Failed(String),
}

pub fn spawn(events: UnboundedSender<AudioEvent>) -> Sender<AudioCmd> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rift-audio".into())
        .spawn(move || run(rx, events))
        .expect("failed to spawn audio thread");
    tx
}

fn run(rx: Receiver<AudioCmd>, events: UnboundedSender<AudioEvent>) {
    let device = match DeviceSinkBuilder::open_default_sink() {
        Ok(d) => d,
        Err(e) => {
            error!("failed to open audio output device: {e}");
            let _ = events.send(AudioEvent::Failed(format!("no audio output device: {e}")));
            return;
        }
    };
    let player = Player::connect_new(device.mixer());
    info!("audio thread started");

    // True while a track is loaded and we should watch for it ending.
    let mut active = false;

    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(cmd) => match cmd {
                AudioCmd::Play(data) => {
                    player.clear();
                    match Decoder::new(Cursor::new(data)) {
                        Ok(source) => {
                            // Report the real length so the seek bar is
                            // accurate even when metadata duration is missing.
                            let dur = source.total_duration().map(|d| d.as_secs_f64());
                            player.append(source);
                            player.play();
                            active = true;
                            if let Some(d) = dur.filter(|d| *d > 0.0) {
                                let _ = events.send(AudioEvent::Duration(d));
                            }
                        }
                        Err(e) => {
                            error!("failed to decode audio: {e}");
                            active = false;
                            let _ = events.send(AudioEvent::Failed(format!("decode error: {e}")));
                        }
                    }
                }
                AudioCmd::Pause => player.pause(),
                AudioCmd::Resume => player.play(),
                AudioCmd::Stop => {
                    player.clear();
                    active = false;
                }
                AudioCmd::Seek(secs) => {
                    if let Err(e) = player.try_seek(Duration::from_secs_f64(secs.max(0.0))) {
                        error!("seek failed: {e}");
                    }
                }
                AudioCmd::Volume(v) => player.set_volume(v.clamp(0.0, 1.0)),
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if active {
            if player.empty() {
                active = false;
                let _ = events.send(AudioEvent::Ended);
            } else if !player.is_paused() {
                let _ = events.send(AudioEvent::Position(player.get_pos().as_secs_f64()));
            }
        }
    }
    info!("audio thread stopped");
}
