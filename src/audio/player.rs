//! The player behind `<audio>` and `<video>`.
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
//!
//! A `<video>` is opened with the platform's decoder (see [`video`](super::video))
//! and falls back to symphonia, which plays the audio alone. Its images are
//! decoded a few ahead and handed to the element's renderer when the clock
//! reaches them. The clock is the audio that has been heard, or the wall clock
//! when there is no audio or it has ended.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use servo_base::generic_channel::GenericCallback;
use servo_media::player::metadata::Metadata;
use servo_media::player::video::{VideoFrame, VideoFrameRenderer};
use servo_media::player::{PlaybackState, Player, PlayerError, PlayerEvent};
use servo_media::streams::registry::MediaStreamId;
use servo_media::{MediaInstance, MediaInstanceError};
use symphonia::core::io::MediaSource;

use super::decode::StreamDecoder;
use super::media_decoder::{MediaDecoder, VideoImage};
use super::mixer::{Mixer, SourceHandle};
use super::resample::StereoResampler;
use super::video;

/// How often the element hears where playback is.
const POSITION_INTERVAL: Duration = Duration::from_millis(250);
/// How far ahead of the mix the decoder runs, at the least.
const DECODE_AHEAD_SECONDS: f64 = 0.1;
/// How many images are decoded ahead of the one on screen.
const VIDEO_AHEAD: usize = 4;
/// Longest the decoding thread sleeps without looking at the clock.
const IDLE_WAIT: Duration = Duration::from_millis(50);
/// Shortest sleep while an image waits for its time.
const MIN_WAIT: Duration = Duration::from_millis(1);
/// How often a reader with an abort flag looks at it while it waits for bytes.
const ABORT_POLL: Duration = Duration::from_millis(100);

pub type Renderer = Arc<Mutex<dyn VideoFrameRenderer>>;

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
    /// `renderer` is the element's, for a `<video>`.
    pub fn new(
        id: usize,
        mixer: &'static Mixer,
        events: GenericCallback<PlayerEvent>,
        renderer: Option<Renderer>,
    ) -> Self {
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
            .spawn(move || Decoding::run(thread_shared, source, renderer))
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

/// Open what has been pushed: with the platform's decoder for a `<video>`,
/// and otherwise, or when that fails, with symphonia.
fn open_decoder(shared: &Arc<Shared>, video: bool) -> Result<Box<dyn MediaDecoder>, String> {
    let platform_error = if video {
        match video::open(GrowingReader::new(shared.clone())) {
            Ok(decoder) => return Ok(decoder),
            Err(error) => Some(error),
        }
    } else {
        None
    };
    match StreamDecoder::open(Box::new(GrowingReader::new(shared.clone()))) {
        Ok(decoder) => Ok(Box::new(decoder)),
        Err(error) => Err(match platform_error {
            Some(platform_error) => format!("{platform_error}; {error}"),
            None => error,
        }),
    }
}

/// The decoding thread's own state.
struct Decoding {
    shared: Arc<Shared>,
    source: SourceHandle,
    rate: u32,
    resampler: Option<(u32, StereoResampler)>,
    has_audio: bool,
    /// Media time at `consumed_base`.
    base_time: f64,
    consumed_base: u64,
    /// Media time of the next audio frame out of the decoder.
    audio_time: f64,
    /// Audio before this media time is dropped: a seek can land a little early.
    audio_from: f64,
    audio_done: bool,
    /// When the queue ran out after the last packet.
    drained_at: Option<Instant>,
    /// The audio has been heard to its end, or there is none, so the clock
    /// runs on the wall.
    audio_drained: bool,
    /// The wall clock: media time at `wall_since`, or while it is stopped.
    wall_base: f64,
    wall_since: Option<Instant>,
    video: Option<VideoOutput>,
    reported_end: bool,
    last_position: Instant,
    /// Whether the duration is known, or has been looked for in the whole input.
    duration_settled: bool,
}

struct VideoOutput {
    renderer: Renderer,
    frames: VecDeque<VideoImage>,
    done: bool,
    /// Images that end before this media time are dropped: after a seek,
    /// decoding starts from the key frame before it.
    from: f64,
    /// Show the next image whatever the clock says: the first one after
    /// opening, or after a seek.
    preroll: bool,
    /// Media time the image on screen ends at.
    end: f64,
}

impl Decoding {
    fn run(shared: Arc<Shared>, source: SourceHandle, renderer: Option<Renderer>) {
        let mut decoder = match open_decoder(&shared, renderer.is_some()) {
            Ok(decoder) => decoder,
            Err(error) => {
                if !shared.lock().quit {
                    shared.send(PlayerEvent::Error(error));
                }
                return;
            }
        };

        let has_audio = decoder.has_audio();
        let video_size = renderer.as_ref().and(decoder.video_size());
        if !has_audio && video_size.is_none() {
            if !shared.lock().quit {
                shared.send(PlayerEvent::Error("no audio or video track".to_owned()));
            }
            return;
        }

        let duration = decoder.duration();
        shared.lock().duration = duration;
        let (width, height) = video_size.unwrap_or_default();
        shared.send(PlayerEvent::MetadataUpdated(Metadata {
            duration: duration.map(Duration::from_secs_f64),
            width,
            height,
            format: String::new(),
            is_seekable: true,
            video_tracks: video_size
                .map(|_| vec!["video".to_owned()])
                .unwrap_or_default(),
            audio_tracks: if has_audio {
                vec!["audio".to_owned()]
            } else {
                Vec::new()
            },
            is_live: false,
            title: None,
        }));
        shared.send(PlayerEvent::DurationChanged(
            duration.map(Duration::from_secs_f64),
        ));
        // Enough is decodable to start: the element moves to HAVE_ENOUGH_DATA.
        shared.send(PlayerEvent::StateChanged(PlaybackState::Paused));

        let video = renderer
            .filter(|_| video_size.is_some())
            .map(|renderer| VideoOutput {
                renderer,
                frames: VecDeque::new(),
                done: false,
                from: 0.0,
                preroll: true,
                end: 0.0,
            });
        let mut decoding = Decoding {
            rate: source.mixer().rate(),
            source,
            shared,
            resampler: None,
            has_audio,
            base_time: 0.0,
            consumed_base: 0,
            audio_time: 0.0,
            audio_from: 0.0,
            audio_done: !has_audio,
            drained_at: None,
            audio_drained: !has_audio,
            wall_base: 0.0,
            wall_since: None,
            video,
            reported_end: false,
            last_position: Instant::now(),
            duration_settled: duration.is_some(),
        };
        decoding.consumed_base = decoding.source.consumed();
        decoding.source.set_finished(!has_audio);
        decoding.decode(&mut *decoder);
        decoding.source.set_playing(false);
    }

    fn decode(&mut self, decoder: &mut dyn MediaDecoder) {
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
            self.source.set_playing(playing && !self.audio_drained);
            self.run_wall_clock(playing);
            if !self.duration_settled && self.shared.lock().end_of_stream {
                self.duration_settled = true;
                self.find_duration();
            }

            if playing && self.last_position.elapsed() >= POSITION_INTERVAL {
                self.last_position = Instant::now();
                self.shared
                    .send(PlayerEvent::PositionChanged(self.position()));
            }

            self.present(playing);

            // The audio has been heard to its end once the mix has taken the
            // last frame and the output's latency has passed since. The clock
            // carries on from there on the wall, for any video still to come.
            if self.audio_done && !self.audio_drained && self.source.queued() == 0 && playing {
                let drained = *self.drained_at.get_or_insert_with(Instant::now);
                if drained.elapsed().as_secs_f64() >= self.source.mixer().latency() {
                    self.wall_base = self.position();
                    self.wall_since = Some(Instant::now());
                    self.audio_drained = true;
                    self.source.set_playing(false);
                }
            }

            if self.audio_drained && self.video_ended() && !self.reported_end && playing {
                self.reported_end = true;
                self.shared.update(|state| state.playing = false);
                self.run_wall_clock(false);
                let end = self
                    .shared
                    .lock()
                    .duration
                    .unwrap_or_else(|| self.position());
                self.shared.send(PlayerEvent::PositionChanged(end));
                self.shared.send(PlayerEvent::EndOfStream);
                continue;
            }

            let ahead = ((DECODE_AHEAD_SECONDS * self.rate as f64) as usize)
                .max(self.source.mixer().high_water());
            if !self.audio_done && self.source.queued() < ahead {
                match decoder.next_audio() {
                    Ok(Some(chunk)) => self.push(chunk.rate, chunk.planes, chunk.time),
                    Ok(None) => {
                        self.audio_done = true;
                        self.source.set_finished(true);
                    }
                    Err(error) => {
                        if self.shared.lock().quit {
                            return;
                        }
                        self.shared.send(PlayerEvent::Error(error));
                        self.audio_done = true;
                        self.source.set_finished(true);
                    }
                }
                continue;
            }

            if self.decode_video(decoder) {
                continue;
            }

            let wait = self.wait_time(playing);
            let state = self.shared.lock();
            if !state.quit && state.seek_to.is_none() {
                let _ = self.shared.changed.wait_timeout(state, wait);
            }
        }
    }

    /// Decode one more image while fewer than [`VIDEO_AHEAD`] are waiting.
    /// Returns whether it tried.
    fn decode_video(&mut self, decoder: &mut dyn MediaDecoder) -> bool {
        let Some(video) = &mut self.video else {
            return false;
        };
        if video.done || video.frames.len() >= VIDEO_AHEAD {
            return false;
        }
        match decoder.next_video() {
            Ok(Some(image)) => {
                if image.time + image.duration > video.from {
                    video.frames.push_back(image);
                }
            }
            Ok(None) => video.done = true,
            Err(error) => {
                if !self.shared.lock().quit {
                    log::warn!("godot-servo: video decoding failed: {error}");
                }
                video.done = true;
            }
        }
        true
    }

    /// Hand the element the image the clock has reached, when that has changed.
    fn present(&mut self, playing: bool) {
        let position = self.position();
        // The clock stops at the duration. An image stamped past it (some
        // files' timestamps run a frame or two long) is due then all the same.
        let at_end = self
            .shared
            .lock()
            .duration
            .is_some_and(|duration| position >= duration);
        let Some(video) = &mut self.video else {
            return;
        };
        let mut due = None;
        if video.preroll {
            due = video.frames.pop_front();
            video.preroll = due.is_none();
        } else if playing {
            // Images the clock has passed are skipped; only the latest is shown.
            while video
                .frames
                .front()
                .is_some_and(|image| at_end || image.time <= position)
            {
                due = video.frames.pop_front();
            }
        }
        let Some(image) = due else {
            return;
        };
        video.end = image.time + image.duration;
        let buffer = Arc::new(video::FrameBuffer(Arc::new(image.pixels)));
        if let Some(frame) = VideoFrame::new(image.width as i32, image.height as i32, buffer) {
            video.renderer.lock().unwrap().render(frame);
            self.shared.send(PlayerEvent::VideoFrameUpdated);
        }
        // currentTime moves only on these, so it moves with every new image;
        // otherwise it lags the picture by up to POSITION_INTERVAL. Servo still
        // fires timeupdate at most every 250 ms.
        if playing {
            self.last_position = Instant::now();
            self.shared.send(PlayerEvent::PositionChanged(position));
        }
    }

    /// Every image has been shown, and the last one's time is up. The clock
    /// stops at the duration, which the last image can run past.
    fn video_ended(&self) -> bool {
        self.video.as_ref().is_none_or(|video| {
            let end = match self.shared.lock().duration {
                Some(duration) => video.end.min(duration),
                None => video.end,
            };
            video.done && video.frames.is_empty() && self.position() >= end
        })
    }

    /// How long to sleep: until the next image is due, within bounds.
    fn wait_time(&self, playing: bool) -> Duration {
        let next = self
            .video
            .as_ref()
            .filter(|_| playing)
            .and_then(|video| video.frames.front());
        match next {
            Some(image) => Duration::from_secs_f64(
                (image.time - self.position())
                    .clamp(MIN_WAIT.as_secs_f64(), IDLE_WAIT.as_secs_f64()),
            ),
            None => IDLE_WAIT,
        }
    }

    fn run_wall_clock(&mut self, running: bool) {
        match (running, self.wall_since) {
            (true, None) => self.wall_since = Some(Instant::now()),
            (false, Some(since)) => {
                self.wall_base += since.elapsed().as_secs_f64();
                self.wall_since = None;
            }
            _ => {}
        }
    }

    /// Some formats (Ogg) learn their duration from the end of the input, and
    /// without a Content-Length (a data: URL) the length is only known once the
    /// last byte has arrived. Probe again then, over input that cannot block.
    fn find_duration(&self) {
        let Some(duration) = open_decoder(&self.shared, self.video.is_some())
            .ok()
            .and_then(|decoder| decoder.duration())
        else {
            return;
        };
        self.shared.lock().duration = Some(duration);
        self.shared
            .send(PlayerEvent::DurationChanged(Some(Duration::from_secs_f64(
                duration,
            ))));
    }

    fn seek(&mut self, decoder: &mut dyn MediaDecoder, time: f64) {
        self.source.clear();
        self.resampler = None;
        let landed = decoder.seek(time).unwrap_or(None);
        self.audio_time = landed.unwrap_or(time);
        self.audio_from = time;
        self.base_time = time;
        self.consumed_base = self.source.consumed();
        self.audio_done = !self.has_audio;
        self.audio_drained = !self.has_audio;
        self.drained_at = None;
        self.wall_base = time;
        self.wall_since = self.wall_since.map(|_| Instant::now());
        self.reported_end = false;
        self.source.set_finished(!self.has_audio);
        if let Some(video) = &mut self.video {
            video.frames.clear();
            video.done = false;
            video.from = time;
            video.preroll = true;
        }
        self.shared.send(PlayerEvent::SeekDone(time));
        self.shared.send(PlayerEvent::PositionChanged(time));
    }

    /// Where playback is, in media time: what the mix has taken, less what has
    /// not been heard yet; or the wall clock once there is no audio to go by.
    fn position(&self) -> f64 {
        let position = if self.audio_drained {
            self.wall_base
                + self
                    .wall_since
                    .map_or(0.0, |since| since.elapsed().as_secs_f64())
        } else {
            let consumed = self.source.consumed() - self.consumed_base;
            let heard = consumed as f64 / self.rate as f64 - self.source.mixer().latency();
            self.base_time + heard.max(0.0)
        };
        match self.shared.lock().duration {
            Some(duration) => position.min(duration),
            None => position,
        }
    }

    /// Bring one packet to stereo at the mixer's rate and queue it.
    fn push(&mut self, rate: u32, planes: &[Vec<f32>], time: Option<f64>) {
        let Some(left) = planes.first() else {
            return;
        };
        if rate == 0 {
            return;
        }
        let right = planes.get(1).unwrap_or(left);
        if let Some(time) = time {
            self.audio_time = time;
        }
        let skip = (((self.audio_from - self.audio_time) * rate as f64)
            .round()
            .max(0.0) as usize)
            .min(left.len());
        self.audio_time += left.len() as f64 / rate as f64;
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
pub struct GrowingReader {
    shared: Arc<Shared>,
    position: u64,
    /// When set, a read waiting for bytes gives up once this turns true, for a
    /// decoder that is being closed while the player is not.
    abort: Option<Arc<AtomicBool>>,
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "android")),
    allow(dead_code, reason = "each platform decoder uses its own part of this")
)]
impl GrowingReader {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            position: 0,
            abort: None,
        }
    }

    /// Another reader over the same bytes, from the start and without an
    /// abort flag.
    pub fn duplicate(&self) -> Self {
        Self::new(self.shared.clone())
    }

    pub fn with_abort(mut self, abort: Arc<AtomicBool>) -> Self {
        self.abort = Some(abort);
        self
    }

    /// The player has been dropped.
    pub fn is_closed(&self) -> bool {
        self.shared.lock().quit
    }

    /// The length of the whole file, once it is known: from the response's
    /// Content-Length, or when the last byte has arrived.
    pub fn wait_for_length(&self) -> io::Result<u64> {
        let mut state = self.shared.lock();
        loop {
            if state.quit || self.aborted() {
                return Err(closed());
            }
            if let Some(length) = state
                .input_size
                .or(state.end_of_stream.then_some(state.data.len() as u64))
            {
                return Ok(length);
            }
            state = self.wait(state);
        }
    }

    fn aborted(&self) -> bool {
        self.abort
            .as_ref()
            .is_some_and(|abort| abort.load(Ordering::Acquire))
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        match self.abort {
            Some(_) => {
                self.shared
                    .changed
                    .wait_timeout(state, ABORT_POLL)
                    .unwrap()
                    .0
            }
            None => self.shared.changed.wait(state).unwrap(),
        }
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "player closed")
}

impl Read for GrowingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = self.shared.lock();
        loop {
            if state.quit || self.aborted() {
                return Err(closed());
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
            state = self.wait(state);
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
        Ok(())
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
