//! Where a Web Audio `AudioContext` sends what it renders.
//!
//! Servo's `AudioRenderThread` keeps rendering 128-frame blocks while
//! `has_enough_data()` is false and otherwise waits on its message channel. So
//! the sink reports "enough" once its queue reaches the mixer's high-water mark,
//! and the mixer sends `SinkNeedData` down that channel when the queue runs low.

use std::sync::mpsc::Sender;
use std::sync::Mutex;

use servo_media::audio::block::{Chunk, FRAMES_PER_BLOCK_USIZE};
use servo_media::audio::render_thread::{AudioRenderThreadMsg, SinkEosCallback};
use servo_media::audio::sink::{AudioSink, AudioSinkError};
use servo_media::streams::MediaSocket;

use super::mixer::{Mixer, SourceHandle};
use super::resample::StereoResampler;

#[derive(Default)]
pub struct GodotAudioSink {
    state: Mutex<Option<SinkState>>,
}

struct SinkState {
    source: SourceHandle,
    resampler: StereoResampler,
}

impl AudioSink for GodotAudioSink {
    fn init(
        &self,
        sample_rate: f32,
        render_thread_channel: Sender<AudioRenderThreadMsg>,
    ) -> Result<(), AudioSinkError> {
        let mixer = Mixer::get()
            .ok_or_else(|| AudioSinkError::Backend("the audio mixer is not set up".to_owned()))?;
        let channel = Mutex::new(render_thread_channel);
        let source = mixer.add_source(Box::new(move || {
            let _ = channel
                .lock()
                .unwrap()
                .send(AudioRenderThreadMsg::SinkNeedData);
        }));
        *self.state.lock().unwrap() = Some(SinkState {
            source,
            resampler: StereoResampler::new(sample_rate as u32, mixer.rate()),
        });
        Ok(())
    }

    /// For `MediaStreamDestinationNode`, which is not supported. Servo panics if
    /// this fails, so it succeeds and the stream stays silent, as with the dummy.
    fn init_stream(&self, _: u8, _: f32, _: Box<dyn MediaSocket>) -> Result<(), AudioSinkError> {
        Ok(())
    }

    fn play(&self) -> Result<(), AudioSinkError> {
        if let Some(state) = &*self.state.lock().unwrap() {
            state.source.set_playing(true);
        }
        Ok(())
    }

    fn stop(&self) -> Result<(), AudioSinkError> {
        if let Some(state) = &*self.state.lock().unwrap() {
            state.source.set_playing(false);
            state.source.clear();
        }
        Ok(())
    }

    fn has_enough_data(&self) -> bool {
        match &*self.state.lock().unwrap() {
            Some(state) => state.source.queued() >= state.source.mixer().high_water(),
            None => true,
        }
    }

    fn push_data(&self, chunk: Chunk) -> Result<(), AudioSinkError> {
        let mut state = self.state.lock().unwrap();
        let Some(state) = &mut *state else {
            return Ok(());
        };

        const SILENCE: [f32; FRAMES_PER_BLOCK_USIZE] = [0.0; FRAMES_PER_BLOCK_USIZE];
        let (left, right) = match chunk.blocks.first() {
            Some(block) if !block.is_silence() => {
                // The destination renders two channels for a real-time context;
                // a mono block is played on both.
                let left = block.data_chan(0);
                let right = if block.chan_count() > 1 {
                    block.data_chan(1)
                } else {
                    left
                };
                (left, right)
            }
            _ => (&SILENCE[..], &SILENCE[..]),
        };

        let source = &state.source;
        state.resampler.process(left, right, |left, right| {
            source.push(left.iter().zip(right).map(|(l, r)| [*l, *r]));
        });
        Ok(())
    }

    /// Only an offline context ends, and that uses Servo's own sink.
    fn set_eos_callback(&self, _: SinkEosCallback) {}
}
