//! What a player decodes from: the audio and video of one file, each read on
//! its own.
//!
//! symphonia ([`StreamDecoder`](super::decode::StreamDecoder)) gives audio
//! only. The platform decoders in [`video`](super::video) give both.

pub struct AudioChunk<'a> {
    pub rate: u32,
    /// One slice per channel, all the same length.
    pub planes: &'a [Vec<f32>],
    /// Media time of the first frame, when the decoder knows it.
    pub time: Option<f64>,
}

pub struct VideoImage {
    /// Media time the image is shown at, and for how long.
    pub time: f64,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// BGRA, opaque, with rows packed: `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

pub trait MediaDecoder {
    /// Length in seconds, when the container says.
    fn duration(&self) -> Option<f64>;

    fn has_audio(&self) -> bool;

    /// The size images come in, when there is a video track.
    fn video_size(&self) -> Option<(u32, u32)> {
        None
    }

    /// The next packet's samples. `None` at the end of the audio.
    fn next_audio(&mut self) -> Result<Option<AudioChunk<'_>>, String>;

    /// The next image. `None` at the end of the video.
    fn next_video(&mut self) -> Result<Option<VideoImage>, String> {
        Ok(None)
    }

    /// Move to `seconds`. Returns where the audio resumes when the decoder
    /// knows, which may be a little before it; a decoder that stamps its
    /// chunks with their time returns `None`.
    fn seek(&mut self, seconds: f64) -> Result<Option<f64>, String>;
}
