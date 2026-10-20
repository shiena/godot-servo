//! Decoding for `<video>`, with the platform's own decoders.
//!
//! Each platform's decoder demuxes the file and decodes both its tracks; the
//! player does the rest. Windows uses Media Foundation. The other platforms
//! have none yet, and a `<video>` there plays its audio alone, as far as
//! symphonia can.

#[cfg(windows)]
mod media_foundation;

use std::sync::Arc;

use servo_media::player::video::{Buffer, VideoFrameData};
use servo_media::SupportsMediaType;

use super::media_decoder::MediaDecoder;
use super::player::GrowingReader;

/// Open a file with the platform's decoder.
pub fn open(reader: GrowingReader) -> Result<Box<dyn MediaDecoder>, String> {
    #[cfg(windows)]
    {
        media_foundation::MediaFoundationDecoder::open(reader)
            .map(|decoder| Box::new(decoder) as Box<dyn MediaDecoder>)
    }
    #[cfg(not(windows))]
    {
        let _ = reader;
        Err("no video decoder on this platform".to_owned())
    }
}

/// How sure the platform's decoder is that it can play `media_type`.
pub fn can_play_type(media_type: &str) -> SupportsMediaType {
    #[cfg(windows)]
    {
        media_foundation::can_play_type(media_type)
    }
    #[cfg(not(windows))]
    {
        let _ = media_type;
        SupportsMediaType::No
    }
}

/// An image's BGRA pixels, as Servo takes them.
pub struct FrameBuffer(pub Arc<Vec<u8>>);

impl Buffer for FrameBuffer {
    fn to_vec(&self) -> Option<VideoFrameData> {
        Some(VideoFrameData::Raw(self.0.clone()))
    }
}
