//! The player behind `<audio>`.
//!
//! The media element fetches the file and pushes it here in pieces. Every byte
//! is kept: the decoder reads from that growing buffer and blocks when it gets
//! ahead of the download, and a seek moves within it. So the player never asks
//! the element to pause the download (`EnoughData`) or to fetch from another
//! offset (`SeekData`), which keeps the element's side simple at the cost of
//! holding the whole file in memory.
//!
//! One thread per player decodes into a mixer source, a little ahead of what is
//! heard, and reports back to the element: metadata once, the position a few
//! times a second, and the end.

use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use servo_base::generic_channel::GenericCallback;
use servo_media::player::metadata::Metadata;
use servo_media::player::{PlaybackState, Player, PlayerError, PlayerEvent};
use servo_media::streams::registry::MediaStreamId;
use servo_media::{MediaInstance, MediaInstanceError};
use symphonia::core::io::MediaSource;

use super::decode::StreamDecoder;
use super::mixer::{Mixer, SourceHandle};
use super::resample::StereoResampler;

/// How often the element hears where playback is.
const POSITION_INTERVAL: Duration = Duration::from_millis(250);
/// How far ahead of the mix the decoder runs, at the least.
const DECODE_AHEAD_SECONDS: f64 = 0.1;
/// Longest the decoding thread sleeps without looking at the clock.
const IDLE_WAIT: Duration = Duration::from_millis(50);

pub struct GodotPlayer {
    id: usize,
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    events: GenericCallback<PlayerEvent>,
}

#[derive(Default)]
struct State {
    data: Vec<u8>,
    input_size: Option<u64>,
    end_of_stream: bool,
    playing: bool,
    /// Paused by the page being hidden, apart from the element's own state.
    suspended: bool,
    seek_to: Option<f64>,
    volume: f64,
    muted: bool,
    /// Muted for the whole page through `Backend::mute`.
    page_muted: bool,
    duration: Option<f64>,
    quit: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.lock());
        self.changed.notify_all();
    }

    fn send(&self, event: PlayerEvent) {
        let _ = self.events.send(event);
    }
}

impl GodotPlayer {
    pub fn new(id: usize, mixer: &'static Mixer, events: GenericCallback<PlayerEvent>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                volume: 1.0,
                ..Default::default()
            }),
            changed: Condvar::new(),
            events,
        });

        let woken = shared.clone();
        let source = mixer.add_source(Box::new(move || woken.changed.notify_all()));
        let thread_shared = shared.clone();
        thread::Builder::new()
            .name("GodotMediaPlayer".to_owned())
            .spawn(move || Decoding::run(thread_shared, source))
            .expect("failed to start a media player thread");

        // The element holds the download back until the player asks for data.
        // Everything is taken from then on, so this is asked once.
        shared.send(PlayerEvent::NeedData);
        Self { id, shared }
    }
}

impl Drop for GodotPlayer {
    fn drop(&mut self) {
        self.shared.update(|state| state.quit = true);
    }
}

/// The decoding thread's own state.
struct Decoding {
    shared: Arc<Shared>,
    source: SourceHandle,
    rate: u32,
    resampler: Option<(u32, StereoResampler)>,
    /// Media time at `consumed_base`.
    base_time: f64,
    consumed_base: u64,
    /// Frames still to drop after a seek landed before the requested time.
    skip_frames: usize,
    decoded_all: bool,
    reported_end: bool,
    /// When the queue ran out after the last packet.
    drained_at: Option<Instant>,
    last_position: Instant,
}

impl Decoding {
    fn run(shared: Arc<Shared>, source: SourceHandle) {
        let reader = GrowingReader {
            shared: shared.clone(),
            position: 0,
        };
        let mut decoder = match StreamDecoder::open(Box::new(reader)) {
            Ok(decoder) => decoder,
            Err(error) => {
                if !shared.lock().quit {
                    shared.send(PlayerEvent::Error(error));
                }
                return;
            }
        };

        let duration = decoder.duration();
        shared.lock().duration = duration;
        shared.send(PlayerEvent::MetadataUpdated(Metadata {
            duration: duration.map(Duration::from_secs_f64),
            width: 0,
            height: 0,
            format: String::new(),
            is_seekable: true,
            video_tracks: Vec::new(),
            audio_tracks: vec!["audio".to_owned()],
            is_live: false,
            title: None,
        }));
        shared.send(PlayerEvent::DurationChanged(
            duration.map(Duration::from_secs_f64),
        ));
        // Enough is decodable to start: the element moves to HAVE_ENOUGH_DATA.
        shared.send(PlayerEvent::StateChanged(PlaybackState::Paused));

        let mut decoding = Decoding {
            rate: source.mixer().rate(),
            source,
            shared,
            resampler: None,
            base_time: 0.0,
            consumed_base: 0,
            skip_frames: 0,
            decoded_all: false,
            reported_end: false,
            drained_at: None,
            last_position: Instant::now(),
        };
        decoding.consumed_base = decoding.source.consumed();
        decoding.decode(&mut decoder);
        decoding.source.set_playing(false);
    }

    fn decode(&mut self, decoder: &mut StreamDecoder) {
        loop {
            let (playing, seek_to) = {
                let mut state = self.shared.lock();
                if state.quit {
                    return;
                }
                self.source.set_muted(state.muted || state.page_muted);
                self.source.set_gain(state.volume as f32);
                (state.playing && !state.suspended, state.seek_to.take())
            };

            if let Some(time) = seek_to {
                self.seek(decoder, time);
                continue;
            }
            self.source.set_playing(playing);

            if playing && self.last_position.elapsed() >= POSITION_INTERVAL {
                self.last_position = Instant::now();
                self.shared
                    .send(PlayerEvent::PositionChanged(self.position()));
            }

            // The end is reported once the last frame has been heard: the mix
            // has taken it, and the output's latency has passed since.
            if self.decoded_all && !self.reported_end && self.source.queued() == 0 && playing {
                let drained = *self.drained_at.get_or_insert_with(Instant::now);
                if drained.elapsed().as_secs_f64() >= self.source.mixer().latency() {
                    self.reported_end = true;
                    self.shared.update(|state| state.playing = false);
                    self.source.set_playing(false);
                    let end = self
                        .shared
                        .lock()
                        .duration
                        .unwrap_or_else(|| self.position());
                    self.shared.send(PlayerEvent::PositionChanged(end));
                    self.shared.send(PlayerEvent::EndOfStream);
                    continue;
                }
            }

            let ahead = ((DECODE_AHEAD_SECONDS * self.rate as f64) as usize)
                .max(self.source.mixer().high_water());
            if !self.decoded_all && self.source.queued() < ahead {
                match decoder.next() {
                    Ok(Some(decoded)) => self.push(decoded.rate, decoded.planes),
                    Ok(None) => {
                        self.decoded_all = true;
                        self.source.set_finished(true);
                    }
                    Err(error) => {
                        if self.shared.lock().quit {
                            return;
                        }
                        self.shared.send(PlayerEvent::Error(error));
                        self.decoded_all = true;
                        self.source.set_finished(true);
                    }
                }
                continue;
            }

            let state = self.shared.lock();
            if !state.quit && state.seek_to.is_none() {
                let _ = self.shared.changed.wait_timeout(state, IDLE_WAIT);
            }
        }
    }

    fn seek(&mut self, decoder: &mut StreamDecoder, time: f64) {
        self.source.clear();
        self.resampler = None;
        let landed = decoder.seek(time).unwrap_or(time);
        self.skip_frames = ((time - landed).max(0.0) * self.rate as f64) as usize;
        self.base_time = time;
        self.consumed_base = self.source.consumed();
        self.decoded_all = false;
        self.reported_end = false;
        self.drained_at = None;
        self.source.set_finished(false);
        self.shared.send(PlayerEvent::SeekDone(time));
        self.shared.send(PlayerEvent::PositionChanged(time));
    }

    /// Where playback is, in media time: what the mix has taken, less what has
    /// not been heard yet.
    fn position(&self) -> f64 {
        let consumed = self.source.consumed() - self.consumed_base;
        let heard = consumed as f64 / self.rate as f64 - self.source.mixer().latency();
        let position = self.base_time + heard.max(0.0);
        match self.shared.lock().duration {
            Some(duration) => position.min(duration),
            None => position,
        }
    }

    /// Bring one packet to stereo at the mixer's rate and queue it.
    fn push(&mut self, rate: u32, planes: &[Vec<f32>]) {
        let Some(left) = planes.first() else {
            return;
        };
        let right = planes.get(1).unwrap_or(left);
        let skip = self.skip_frames.min(left.len());
        self.skip_frames -= skip;
        if skip == left.len() {
            return;
        }

        if !matches!(&self.resampler, Some((current, _)) if *current == rate) {
            self.resampler = Some((rate, StereoResampler::new(rate, self.rate)));
        }
        let Some((_, resampler)) = &mut self.resampler else {
            return;
        };
        let source = &self.source;
        resampler.process(&left[skip..], &right[skip..], |left, right| {
            source.push(left.iter().zip(right).map(|(l, r)| [*l, *r]));
        });
    }
}

/// The bytes pushed so far, read as a file that is still downloading.
struct GrowingReader {
    shared: Arc<Shared>,
    position: u64,
}

impl Read for GrowingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = self.shared.lock();
        loop {
            if state.quit {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "player closed",
                ));
            }
            let available = state.data.len() as u64;
            if self.position < available {
                let start = self.position as usize;
                let count = buf.len().min(state.data.len() - start);
                buf[..count].copy_from_slice(&state.data[start..start + count]);
                self.position += count as u64;
                return Ok(count);
            }
            if state.end_of_stream {
                return Ok(0);
            }
            state = self.shared.changed.wait(state).unwrap();
        }
    }
}

impl Seek for GrowingReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            SeekFrom::End(delta) => {
                let state = self.shared.lock();
                let length = state
                    .input_size
                    .or(state.end_of_stream.then_some(state.data.len() as u64));
                match length {
                    Some(length) => length.checked_add_signed(delta),
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "length not known yet",
                        ))
                    }
                }
            }
        };
        self.position = position
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.position)
    }
}

impl MediaSource for GrowingReader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        let state = self.shared.lock();
        state
            .input_size
            .or(state.end_of_stream.then_some(state.data.len() as u64))
    }
}

impl Player for GodotPlayer {
    fn play(&self) -> Result<(), PlayerError> {
        self.shared.update(|state| state.playing = true);
        self.shared
            .send(PlayerEvent::StateChanged(PlaybackState::Playing));
        Ok(())
    }

    fn pause(&self) -> Result<(), PlayerError> {
        self.shared.update(|state| state.playing = false);
        self.shared
            .send(PlayerEvent::StateChanged(PlaybackState::Paused));
        Ok(())
    }

    fn paused(&self) -> bool {
        !self.shared.lock().playing
    }

    fn can_resume(&self) -> bool {
        true
    }

    fn stop(&self) -> Result<(), PlayerError> {
        self.shared.update(|state| state.playing = false);
        Ok(())
    }

    fn seek(&self, time: f64) -> Result<(), PlayerError> {
        self.shared.update(|state| state.seek_to = Some(time));
        Ok(())
    }

    fn seekable(&self) -> Vec<Range<f64>> {
        self.shared
            .lock()
            .duration
            .map(|duration| vec![0.0..duration])
            .unwrap_or_default()
    }

    fn set_mute(&self, muted: bool) -> Result<(), PlayerError> {
        self.shared.update(|state| state.muted = muted);
        Ok(())
    }

    fn muted(&self) -> bool {
        self.shared.lock().muted
    }

    fn set_volume(&self, volume: f64) -> Result<(), PlayerError> {
        self.shared.update(|state| state.volume = volume);
        Ok(())
    }

    fn volume(&self) -> f64 {
        self.shared.lock().volume
    }

    fn set_input_size(&self, size: u64) -> Result<(), PlayerError> {
        self.shared.update(|state| state.input_size = Some(size));
        Ok(())
    }

    fn set_seekable(&self, _: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    /// Only normal speed is supported; other rates play at normal speed.
    fn set_playback_rate(&self, rate: f64) -> Result<(), PlayerError> {
        if rate != 1.0 {
            log::warn!("godot-servo: playbackRate {rate} is not supported; playing at 1");
        }
        Ok(())
    }

    fn playback_rate(&self) -> f64 {
        1.0
    }

    fn push_data(&self, data: Vec<u8>) -> Result<(), PlayerError> {
        self.shared
            .update(|state| state.data.extend_from_slice(&data));
        Ok(())
    }

    fn end_of_stream(&self) -> Result<(), PlayerError> {
        self.shared.update(|state| state.end_of_stream = true);
        Ok(())
    }

    fn buffered(&self) -> Vec<Range<f64>> {
        let state = self.shared.lock();
        let Some(duration) = state.duration else {
            return Vec::new();
        };
        let fraction = match (state.end_of_stream, state.input_size) {
            (true, _) => 1.0,
            (false, Some(size)) if size > 0 => (state.data.len() as f64 / size as f64).min(1.0),
            _ => 0.0,
        };
        vec![0.0..duration * fraction]
    }

    fn set_stream(&self, _: &MediaStreamId, _: bool) -> Result<(), PlayerError> {
        Err(PlayerError::SetStreamFailed)
    }

    fn render_use_gl(&self) -> bool {
        false
    }

    fn set_audio_track(&self, _: i32, _: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_video_track(&self, _: i32, _: bool) -> Result<(), PlayerError> {
        Err(PlayerError::SetTrackFailed)
    }
}

impl MediaInstance for GodotPlayer {
    fn get_id(&self) -> usize {
        self.id
    }

    fn mute(&self, muted: bool) -> Result<(), MediaInstanceError> {
        self.shared.update(|state| state.page_muted = muted);
        Ok(())
    }

    fn suspend(&self) -> Result<(), MediaInstanceError> {
        self.shared.update(|state| state.suspended = true);
        Ok(())
    }

    fn resume(&self) -> Result<(), MediaInstanceError> {
        self.shared.update(|state| state.suspended = false);
        Ok(())
    }
}
