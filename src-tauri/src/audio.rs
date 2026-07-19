//! Dedicated audio thread owning the rodio output device.
//!
//! rodio's output stream is not `Send`, so it lives on its own OS thread and
//! is driven through a channel. Position/end-of-track notifications flow back
//! through a tokio channel to the player manager.

use std::io::Cursor;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use rodio::stream::DeviceSinkBuilder;
use rodio::{Decoder, Player, Source};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};

pub enum AudioCmd {
    /// Decode and play a full in-memory audio file, replacing whatever plays.
    Play(Vec<u8>),
    /// Decode and play a new track, fading it in over the given duration while
    /// the current track fades out over the same window (a crossfade).
    Crossfade(Vec<u8>, Duration),
    Pause,
    Resume,
    Stop,
    /// Seek to an absolute position in seconds.
    Seek(f64),
    /// Volume in 0.0..=1.0.
    Volume(f32),
}

/// A track on its way out during a crossfade: its player's volume is ramped
/// from the master volume down to zero, then dropped (which stops it).
struct FadeOut {
    player: Player,
    start: Instant,
    dur: Duration,
}

/// Decode a full in-memory audio file. The explicit byte length marks the
/// source as seekable; without it symphonia treats the stream as forward-only,
/// and a backward seek in a fragmented MP4 (YouTube m4a) leaves the demuxer
/// unable to rewind — the source drains and the track appears to have ended.
fn decode(data: Vec<u8>) -> Result<Decoder<Cursor<Vec<u8>>>, rodio::decoder::DecoderError> {
    let len = data.len() as u64;
    Decoder::builder()
        .with_data(Cursor::new(data))
        .with_byte_len(len)
        .with_seekable(true)
        .build()
}

pub enum AudioEvent {
    /// Authoritative track length in seconds, read from the decoder.
    Duration(f64),
    /// Current playback position in seconds.
    Position(f64),
    /// The current track finished playing.
    Ended,
    /// The current track's bytes wouldn't decode (e.g. a corrupt offline
    /// file). The track is unplayable, but the queue can still advance.
    DecodeFailed(String),
    /// Device-level failure; playback can't continue at all.
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
    let mut player = Player::connect_new(device.mixer());
    info!("audio thread started");

    // True while a track is loaded and we should watch for it ending.
    let mut active = false;
    // Master volume (0.0..=1.0), applied to every player including fade-outs.
    let mut volume = 1.0f32;
    // Tracks currently fading out behind the current one.
    let mut fading: Vec<FadeOut> = Vec::new();
    // Paces position ticks at ~4 Hz: a crossfade tightens the loop to 30 ms
    // for smooth volume ramps, but the UI doesn't need 33 progress events/s.
    let mut last_position = Instant::now() - Duration::from_secs(1);

    loop {
        // Tick fast enough for smooth fades while one is in progress; idle
        // slower the rest of the time.
        let timeout = if fading.is_empty() {
            Duration::from_millis(250)
        } else {
            Duration::from_millis(30)
        };
        match rx.recv_timeout(timeout) {
            Ok(cmd) => match cmd {
                AudioCmd::Play(data) => {
                    player.clear();
                    // A hard cut discards any in-progress crossfade.
                    fading.clear();
                    match decode(data) {
                        Ok(source) => {
                            // Report the real length so the seek bar is
                            // accurate even when metadata duration is missing.
                            let dur = source.total_duration().map(|d| d.as_secs_f64());
                            player.append(source);
                            player.set_volume(volume);
                            player.play();
                            active = true;
                            if let Some(d) = dur.filter(|d| *d > 0.0) {
                                let _ = events.send(AudioEvent::Duration(d));
                            }
                        }
                        Err(e) => {
                            error!("failed to decode audio: {e}");
                            active = false;
                            let _ =
                                events.send(AudioEvent::DecodeFailed(format!("decode error: {e}")));
                        }
                    }
                }
                AudioCmd::Crossfade(data, fade) => match decode(data) {
                    Ok(source) => {
                        let dur = source.total_duration().map(|d| d.as_secs_f64());
                        // Retire the current track onto its own fade-out and
                        // bring the new one up on a fresh player so the two
                        // overlap.
                        let outgoing =
                            std::mem::replace(&mut player, Player::connect_new(device.mixer()));
                        if active {
                            fading.push(FadeOut {
                                player: outgoing,
                                start: Instant::now(),
                                dur: fade,
                            });
                        }
                        player.append(source.fade_in(fade));
                        player.set_volume(volume);
                        player.play();
                        active = true;
                        if let Some(d) = dur.filter(|d| *d > 0.0) {
                            let _ = events.send(AudioEvent::Duration(d));
                        }
                    }
                    Err(e) => {
                        // Leave the current track playing on decode failure; the
                        // queue will advance normally when it ends.
                        error!("failed to decode crossfade audio: {e}");
                    }
                },
                AudioCmd::Pause => {
                    player.pause();
                    for f in &fading {
                        f.player.pause();
                    }
                }
                AudioCmd::Resume => {
                    player.play();
                    for f in &fading {
                        f.player.play();
                    }
                }
                AudioCmd::Stop => {
                    player.clear();
                    fading.clear();
                    active = false;
                }
                AudioCmd::Seek(secs) => {
                    if let Err(e) = player.try_seek(Duration::from_secs_f64(secs.max(0.0))) {
                        error!("seek failed: {e}");
                    }
                }
                AudioCmd::Volume(v) => {
                    volume = v.clamp(0.0, 1.0);
                    player.set_volume(volume);
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Advance any fade-outs, dropping (and so silencing) the ones that have
        // run their course.
        fading.retain(|f| {
            let t = f.start.elapsed().as_secs_f32() / f.dur.as_secs_f32().max(0.001);
            if t >= 1.0 || f.player.empty() {
                false
            } else {
                f.player.set_volume(volume * (1.0 - t));
                true
            }
        });

        if active {
            if player.empty() {
                active = false;
                let _ = events.send(AudioEvent::Ended);
            } else if !player.is_paused() && last_position.elapsed() >= Duration::from_millis(200) {
                last_position = Instant::now();
                let _ = events.send(AudioEvent::Position(player.get_pos().as_secs_f64()));
            }
        }
    }
    info!("audio thread stopped");
}
