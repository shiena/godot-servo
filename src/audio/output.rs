//! The Godot end: an `AudioStreamPlayer` playing an `AudioStreamGenerator` that
//! the main thread fills from the mixer once per frame.
//!
//! Filling the generator to the brim would delay everything by its whole
//! length, so it is only topped up to a target that follows the frame time:
//! enough to last until the next frame or two, plus a margin that grows when the
//! generator runs dry and shrinks again after a while without that.

use std::time::Instant;

use godot::classes::{
    AudioServer, AudioStreamGenerator, AudioStreamGeneratorPlayback, AudioStreamPlayer, SceneTree,
};
use godot::prelude::*;

use super::mixer::{Frame, Mixer};

/// The generator's capacity: the most a frame can stall before the audio stops.
const BUFFER_SECONDS: f32 = 0.25;
/// How many frame times the generator holds beyond the current one.
const FRAMES_AHEAD: f64 = 2.0;
/// Floor under the target, for frame rates high enough that two frames are
/// shorter than the audio driver's own period.
const MIN_TARGET_SECONDS: f64 = 0.02;
/// Margin added each time the generator runs dry, and its ceiling.
const MARGIN_STEP_SECONDS: f64 = 0.01;
const MAX_MARGIN_SECONDS: f64 = 0.1;
/// How long the generator must go without running dry before the margin
/// shrinks by half a step.
const CALM_SECONDS: f64 = 5.0;
/// Frame times longer than this are a stall, such as a scene loading, not a
/// frame rate to plan for.
const MAX_FRAME_SECONDS: f64 = 0.1;
/// Per frame, how much of the longest recent frame time is still remembered.
const PEAK_DECAY: f64 = 0.98;
/// How many frame times each source keeps queued, and the floor under that.
const SOURCE_FRAMES_AHEAD: f64 = 2.0;
const MIN_HIGH_WATER: usize = 1024;
/// A few of the audio driver's periods, which are around 10 to 20 ms.
const RETIRE_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

pub struct AudioOutput {
    player: Gd<AudioStreamPlayer>,
    playback: Option<Gd<AudioStreamGeneratorPlayback>>,
    rate: f64,
    /// Frames the generator holds when full. Known once it is playing.
    capacity: i32,
    last_pump: Option<Instant>,
    peak_frame_seconds: f64,
    margin_seconds: f64,
    calm_seconds: f64,
    skips: i32,
    mixed: Vec<Frame>,
    paused: bool,
}

impl AudioOutput {
    /// Create the player and add it to `tree`'s root at the end of the frame.
    pub fn new(tree: &Gd<SceneTree>, bus: &StringName, mixer: &Mixer) -> Self {
        let rate = mixer.rate() as f32;
        let mut generator = AudioStreamGenerator::new_gd();
        generator.set_mix_rate(rate);
        generator.set_buffer_length(BUFFER_SECONDS);

        let mut player = AudioStreamPlayer::new_alloc();
        player.set_name("ServoAudio");
        player.set_stream(&generator);
        player.set_bus(bus);
        player.set_autoplay(true);
        // Paused along with the WebViews by `pump`, not by the tree.
        player.set_process_mode(godot::classes::node::ProcessMode::ALWAYS);

        // Servo starts from a node's `_ready()`, where the root can still be
        // busy adding its children.
        if let Some(mut root) = tree.get_root() {
            root.call_deferred("add_child", &[player.to_variant()]);
        }

        Self {
            player,
            playback: None,
            rate: rate as f64,
            capacity: 0,
            last_pump: None,
            peak_frame_seconds: 0.0,
            margin_seconds: 0.0,
            calm_seconds: 0.0,
            skips: 0,
            mixed: Vec::new(),
            paused: false,
        }
    }

    pub fn set_bus(&mut self, bus: &StringName) {
        if self.player.is_instance_valid() {
            self.player.set_bus(bus);
        }
    }

    /// Once per frame. While `paused`, the player is held where it is.
    pub fn pump(&mut self, mixer: &Mixer, paused: bool) {
        let now = Instant::now();
        let frame_seconds = self
            .last_pump
            .map_or(0.0, |last| (now - last).as_secs_f64())
            .min(MAX_FRAME_SECONDS);
        self.last_pump = Some(now);

        if !self.player.is_instance_valid() {
            return;
        }
        if paused != self.paused {
            self.paused = paused;
            self.player.set_stream_paused(paused);
        }
        if paused {
            return;
        }
        let Some(mut playback) = self.playback() else {
            return;
        };

        self.peak_frame_seconds = frame_seconds.max(self.peak_frame_seconds * PEAK_DECAY);
        let skips = playback.get_skips();
        if skips != self.skips {
            self.skips = skips;
            self.margin_seconds =
                (self.margin_seconds + MARGIN_STEP_SECONDS).min(MAX_MARGIN_SECONDS);
            self.calm_seconds = 0.0;
        } else {
            self.calm_seconds += frame_seconds;
            if self.calm_seconds >= CALM_SECONDS {
                self.margin_seconds = (self.margin_seconds - MARGIN_STEP_SECONDS / 2.0).max(0.0);
                self.calm_seconds = 0.0;
            }
        }

        // Each source keeps two frames' worth queued, so whatever this frame
        // takes is already waiting. Godot's audio thread drains the generator in
        // whole mix buffers, so one frame can take twice what its time suggests.
        mixer.set_high_water(
            ((self.peak_frame_seconds * SOURCE_FRAMES_AHEAD * self.rate) as usize)
                .max(MIN_HIGH_WATER),
        );

        let target_seconds =
            (self.peak_frame_seconds * FRAMES_AHEAD).max(MIN_TARGET_SECONDS) + self.margin_seconds;
        let target = ((target_seconds * self.rate) as i32).min(self.capacity);
        let queued = self.capacity - playback.get_frames_available();
        let wanted = target - queued;
        let output_latency = AudioServer::singleton().get_output_latency();
        mixer.set_latency(queued.max(target) as f64 / self.rate + output_latency);
        if wanted <= 0 {
            return;
        }

        mixer.pull(wanted as usize, &mut self.mixed);
        let frames: PackedVector2Array = self
            .mixed
            .iter()
            .map(|frame| Vector2::new(frame[0], frame[1]))
            .collect();
        playback.push_buffer(&frames);
    }

    /// The generator's playback, once the player has started.
    fn playback(&mut self) -> Option<Gd<AudioStreamGeneratorPlayback>> {
        if self.playback.is_none() && self.player.is_playing() {
            let playback = self
                .player
                .get_stream_playback()?
                .try_cast::<AudioStreamGeneratorPlayback>()
                .ok()?;
            self.capacity = playback.get_frames_available();
            self.skips = playback.get_skips();
            self.playback = Some(playback);
        }
        self.playback.clone()
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        // A player in the tree goes with it. One the deferred `add_child` never
        // reached is freed here.
        if self.player.is_instance_valid() && !self.player.is_inside_tree() {
            self.player.clone().free();
        }

        // Freeing the player only asks the AudioServer to fade the playback
        // out; the audio thread takes it off the list on its next mix. A
        // playback still on the list when the AudioServer is deleted is never
        // released, and Godot reports it as leaked at exit. The server is
        // deleted soon after this runs at exit, so give the audio thread time
        // for a mix or two. The driver is still running at this point.
        if self.playback.take().is_some() {
            std::thread::sleep(RETIRE_WAIT);
        }
    }
}
