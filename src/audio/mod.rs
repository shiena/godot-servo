//! Servo's audio, played through Godot.
//!
//! Servo picks its media backend at compile time, and without the
//! `media-gstreamer` feature that is a dummy that drops every sample. This
//! module puts a backend of its own in first (see [`install`]), and that backend
//! hands every rendered sample to a [`Mixer`](mixer::Mixer).
//!
//! The mixer is drained once per frame from `ServoServer::pump`, on the main
//! thread, into an `AudioStreamGenerator` ([`AudioOutput`]). Godot's audio
//! thread then only ever runs Godot's own code: nothing here is called from it,
//! and the threads this module starts never touch Godot. That keeps clear of
//! godot-rust's `experimental-threads`, which a custom `AudioStreamPlayback`
//! would need, at the cost of tying the output to the frame rate. The output
//! keeps only a few frames' worth queued to make up for that.

mod backend;
mod decode;
mod decoder;
mod media_decoder;
mod mixer;
mod output;
mod player;
mod resample;
mod sink;
mod video;

use servo_media::ServoMedia;

pub use mixer::Mixer;
pub use output::AudioOutput;

/// Make the backend here the one Servo uses. Call before `ServoBuilder::build`.
///
/// `ServoMedia` keeps its backend in a `OnceLock`, and Servo fills it while it
/// is being built, with the dummy. Whoever fills it first wins, so this waits
/// until the backend here is in before returning, and Servo's later attempt
/// finds it taken. That relies on servo 0.5.0 internals; check it again when
/// Servo is updated.
pub fn install(output_rate: u32) {
    if !mixer::Mixer::init(output_rate) {
        return;
    }
    ServoMedia::init_with_backend(|| Box::new(backend::GodotBackend::new()));
    let _ = ServoMedia::get();
}
