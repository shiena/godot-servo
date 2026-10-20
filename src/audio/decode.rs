//! Audio decoding with symphonia, for `decodeAudioData` and `<audio>`.
//!
//! Formats: WAV, FLAC, Ogg Vorbis, WebM/Matroska Vorbis, MP3, and with the `aac`
//! feature AAC in MP4. Opus is not among them: symphonia has no Opus decoder.

use std::io::Cursor;

use servo_media::SupportsMediaType;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{self, Time, TimeBase, Timestamp};

use super::media_decoder::{AudioChunk, MediaDecoder};

/// One audio track, decoded a packet at a time into planar `f32`.
pub struct StreamDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    duration: Option<f64>,
    planes: Vec<Vec<f32>>,
    rate: u32,
}

impl StreamDecoder {
    pub fn open(source: Box<dyn MediaSource>) -> Result<Self, String> {
        let stream = MediaSourceStream::new(source, Default::default());
        let format = symphonia::default::get_probe()
            .probe(
                &Hint::new(),
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|error| format!("unrecognised media: {error}"))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or("no audio codec parameters")?;
        let rate = params.sample_rate.unwrap_or(0);
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|error| format!("unsupported codec: {error}"))?;

        let seconds = |base: Option<TimeBase>, duration: Option<units::Duration>| {
            base.zip(duration)
                .and_then(|(base, duration)| base.calc_time(Timestamp::new(duration.get() as i64)))
                .map(|time| time.as_secs_f64())
        };
        // Matroska gives the length of the whole file rather than of a track.
        let duration = match (track.num_frames, rate) {
            (Some(frames), rate) if rate > 0 => Some(frames as f64 / rate as f64),
            _ => seconds(track.time_base, track.duration).or_else(|| {
                let media = format.media_info();
                seconds(media.time_base, media.duration)
            }),
        };
        let track_id = track.id;

        Ok(Self {
            format,
            decoder,
            track_id,
            duration,
            planes: Vec::new(),
            rate,
        })
    }

    /// The next packet's samples. `None` at the end of the stream.
    pub fn next(&mut self) -> Result<Option<AudioChunk<'_>>, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                // A chained Ogg stream changing its tracks; treated as the end.
                Err(Error::ResetRequired) => return Ok(None),
                Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(error) => return Err(error.to_string()),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buffer) => {
                    if buffer.frames() == 0 {
                        continue;
                    }
                    self.rate = buffer.spec().rate();
                    buffer.copy_to_vecs_planar::<f32>(&mut self.planes);
                    return Ok(Some(AudioChunk {
                        rate: self.rate,
                        planes: &self.planes,
                        time: None,
                    }));
                }
                // A damaged packet is skipped, as players do.
                Err(Error::DecodeError(_)) | Err(Error::IoError(_)) => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    /// Move to `seconds`. Returns where decoding resumes, which may be a little
    /// before it.
    pub fn seek(&mut self, seconds: f64) -> Result<f64, String> {
        let time = Time::try_from_secs_f64(seconds.max(0.0)).ok_or("invalid seek time")?;
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|error| error.to_string())?;
        self.decoder.reset();
        let base = self
            .format
            .tracks()
            .iter()
            .find(|track| track.id == self.track_id)
            .and_then(|track| track.time_base);
        Ok(base
            .and_then(|base| base.calc_time(seeked.actual_ts))
            .map_or(seconds, |time| time.as_secs_f64()))
    }
}

impl MediaDecoder for StreamDecoder {
    fn duration(&self) -> Option<f64> {
        self.duration
    }

    fn has_audio(&self) -> bool {
        true
    }

    fn next_audio(&mut self) -> Result<Option<AudioChunk<'_>>, String> {
        self.next()
    }

    fn seek(&mut self, seconds: f64) -> Result<Option<f64>, String> {
        StreamDecoder::seek(self, seconds).map(Some)
    }
}

/// Decode a whole file held in memory. Returns the sample rate and one vector
/// per channel.
pub fn decode_all(data: Vec<u8>) -> Result<(u32, Vec<Vec<f32>>), String> {
    let mut decoder = StreamDecoder::open(Box::new(Cursor::new(data)))?;
    let mut rate = 0;
    let mut channels: Vec<Vec<f32>> = Vec::new();
    while let Some(decoded) = decoder.next()? {
        rate = decoded.rate;
        channels.resize(decoded.planes.len(), Vec::new());
        for (channel, plane) in channels.iter_mut().zip(decoded.planes) {
            channel.extend_from_slice(plane);
        }
    }
    if channels.is_empty() || rate == 0 {
        return Err("no audio decoded".to_owned());
    }
    Ok((rate, channels))
}

/// How sure the decoders here are that they can play `media_type`, a MIME type
/// with optional parameters such as `audio/ogg; codecs="vorbis"`.
pub fn can_play_type(media_type: &str) -> SupportsMediaType {
    let (mime, codecs) = split_media_type(media_type);

    // Sure when every codec named is one the decoders here play. With none
    // named, sure only for a type that always holds the same codec; a container
    // such as Ogg may hold one they cannot.
    let by_codecs = |playable: &[&str], single_codec: bool| {
        if codecs.is_empty() {
            if single_codec {
                SupportsMediaType::Probably
            } else {
                SupportsMediaType::Maybe
            }
        } else if codecs
            .iter()
            .all(|codec| playable.contains(&codec.as_str()))
        {
            SupportsMediaType::Probably
        } else {
            SupportsMediaType::No
        }
    };

    match mime.as_str() {
        "audio/wav" | "audio/wave" | "audio/x-wav" | "audio/vnd.wave" => {
            by_codecs(&["1", "3"], true)
        }
        "audio/mpeg" | "audio/mp3" => by_codecs(&["mp3"], true),
        "audio/flac" | "audio/x-flac" => by_codecs(&["flac"], true),
        "audio/ogg" | "application/ogg" => by_codecs(&["vorbis", "flac"], false),
        "audio/webm" => by_codecs(&["vorbis"], false),
        #[cfg(feature = "aac")]
        "audio/aac" => by_codecs(&["mp4a.40.2"], true),
        #[cfg(feature = "aac")]
        "audio/mp4" | "audio/x-m4a" => by_codecs(&["mp4a.40.2", "mp4a.40.5", "mp4a.40.29"], false),
        _ => SupportsMediaType::No,
    }
}

/// A MIME type and the codecs its `codecs` parameter names, both lowercased.
pub fn split_media_type(media_type: &str) -> (String, Vec<String>) {
    let mut parts = media_type.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let codecs = parts
        .filter_map(|part| part.trim().strip_prefix("codecs="))
        .flat_map(|list| list.trim_matches('"').split(','))
        .map(|codec| codec.trim().to_ascii_lowercase())
        .filter(|codec| !codec.is_empty())
        .collect();
    (mime, codecs)
}
