//! `decodeAudioData`.

use servo_media::audio::decoder::{
    AudioDecoder, AudioDecoderCallbacks, AudioDecoderError, AudioDecoderOptions,
};

use super::decode::decode_all;
use super::resample::resample_whole;

/// The most channels Servo can report: it identifies a channel by one bit of a
/// `u32`.
const MAX_CHANNELS: usize = 32;

pub struct SymphoniaAudioDecoder;

impl AudioDecoder for SymphoniaAudioDecoder {
    fn decode(
        &self,
        data: Vec<u8>,
        callbacks: AudioDecoderCallbacks,
        options: Option<AudioDecoderOptions>,
    ) {
        let rate = options.unwrap_or_default().sample_rate as u32;
        match decode_all(data) {
            Ok((source_rate, mut channels)) => {
                channels.truncate(MAX_CHANNELS);
                callbacks.ready(channels.len() as u32);
                for (index, channel) in channels.iter().enumerate() {
                    let samples = resample_whole(channel, source_rate, rate);
                    callbacks.progress(Box::new(samples), 1 << index);
                }
                callbacks.eos();
            }
            Err(error) => {
                log::warn!("godot-servo: decodeAudioData failed: {error}");
                callbacks.error(AudioDecoderError::InvalidMediaFormat);
            }
        }
    }
}
