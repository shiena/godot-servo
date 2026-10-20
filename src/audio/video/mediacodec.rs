//! `<video>` on Android, with the NDK's MediaExtractor and MediaCodec.
//!
//! Each track gets its own extractor and decoder, so audio and video are read
//! at their own pace. The extractors read the pushed bytes through an
//! `AMediaDataSource`, which needs the length, so opening waits until it is
//! known.
//!
//! Video is decoded into the decoder's own buffers, asked for in the flexible
//! YUV 4:2:0 format, and converted to BGRA here. The decoder says where each
//! plane sits through the output format's `image-data` (a `MediaImage2`), or
//! failing that through its colour format, stride and slice height. Decoding
//! onto an `AImageReader`'s surface instead does not work everywhere:
//! Qualcomm's decoders hand it bandwidth-compressed (UBWC) buffers with a
//! single plane, whatever usage the reader asks for.

use std::collections::HashMap;
use std::ffi::{c_void, CStr, CString};
use std::io::{Read, Seek, SeekFrom};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use servo_media::SupportsMediaType;

use super::ndk_media::{self, AMediaCodecBufferInfo, Media};
use crate::audio::decode::split_media_type;
use crate::audio::media_decoder::{AudioChunk, MediaDecoder, VideoImage};
use crate::audio::player::GrowingReader;

/// How long a decoder is waited on before looking again at the player.
const DEQUEUE_TIMEOUT_US: i64 = 10_000;
/// What an image lasts when the stream does not say.
const DEFAULT_FRAME_SECONDS: f64 = 1.0 / 30.0;

/// `AMEDIAFORMAT_KEY_*`, whose values are these strings.
const KEY_MIME: &CStr = c"mime";
const KEY_WIDTH: &CStr = c"width";
const KEY_HEIGHT: &CStr = c"height";
const KEY_DURATION: &CStr = c"durationUs";
const KEY_SAMPLE_RATE: &CStr = c"sample-rate";
const KEY_CHANNEL_COUNT: &CStr = c"channel-count";
const KEY_PCM_ENCODING: &CStr = c"pcm-encoding";
const KEY_COLOR_STANDARD: &CStr = c"color-standard";
const KEY_COLOR_RANGE: &CStr = c"color-range";
const KEY_FRAME_RATE: &CStr = c"frame-rate";
const KEY_COLOR_FORMAT: &CStr = c"color-format";
const KEY_STRIDE: &CStr = c"stride";
const KEY_SLICE_HEIGHT: &CStr = c"slice-height";
const KEY_CROP_LEFT: &CStr = c"crop-left";
const KEY_CROP_TOP: &CStr = c"crop-top";
const KEY_CROP_RIGHT: &CStr = c"crop-right";
const KEY_CROP_BOTTOM: &CStr = c"crop-bottom";
const KEY_IMAGE_DATA: &CStr = c"image-data";
/// `AMEDIAFORMAT_KEY_DISPLAY_CROP`, a rect.
const KEY_DISPLAY_CROP: &CStr = c"crop";

const COLOR_STANDARD_BT709: i32 = 1;
const COLOR_RANGE_FULL: i32 = 1;
const ENCODING_PCM_FLOAT: i32 = 4;

/// `MediaCodecInfo.CodecCapabilities` colour formats.
const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7f42_0888;
/// Qualcomm's linear NV12 with aligned strides (`YCbCr_420_SP_VENUS`).
const COLOR_FORMAT_QCOM_VENUS_NV12: i32 = 0x7fa3_0c04;
/// `MediaImage2::Type::MEDIA_IMAGE_TYPE_YUV`.
const MEDIA_IMAGE_TYPE_YUV: u32 = 1;

const AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED: isize = -2;

pub fn can_play_type(media_type: &str) -> SupportsMediaType {
    let Some(media) = ndk_media::get() else {
        return SupportsMediaType::No;
    };
    let (mime, codecs) = split_media_type(media_type);
    if !matches!(
        mime.as_str(),
        "video/mp4" | "video/webm" | "video/x-matroska"
    ) {
        return SupportsMediaType::No;
    }
    if codecs.is_empty() {
        return SupportsMediaType::Maybe;
    }
    let decodable = codecs
        .iter()
        .all(|codec| codec_mime(codec).is_some_and(|mime| has_decoder(media, mime)));
    if decodable {
        SupportsMediaType::Probably
    } else {
        SupportsMediaType::No
    }
}

/// The MIME type MediaCodec knows a `codecs=` entry by.
fn codec_mime(codec: &str) -> Option<&'static CStr> {
    Some(match codec.split('.').next().unwrap_or(codec) {
        "avc1" | "avc3" => c"video/avc",
        "hev1" | "hvc1" => c"video/hevc",
        "vp8" => c"video/x-vnd.on2.vp8",
        "vp9" | "vp09" => c"video/x-vnd.on2.vp9",
        "av01" => c"video/av01",
        "mp4a" => c"audio/mp4a-latm",
        "mp3" => c"audio/mpeg",
        "opus" => c"audio/opus",
        "vorbis" => c"audio/vorbis",
        "flac" => c"audio/flac",
        _ => return None,
    })
}

/// Whether the device has a decoder for `mime`. The NDK has no codec list
/// before API 29, so this makes one and throws it away, once per type.
fn has_decoder(media: &Media, mime: &'static CStr) -> bool {
    static KNOWN: OnceLock<Mutex<HashMap<&'static CStr, bool>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(found) = known.lock().unwrap().get(mime) {
        return *found;
    }
    // SAFETY: a codec made and deleted here, and nowhere else.
    let found = unsafe {
        let codec = (media.AMediaCodec_createDecoderByType)(mime.as_ptr());
        if !codec.is_null() {
            (media.AMediaCodec_delete)(codec);
        }
        !codec.is_null()
    };
    known.lock().unwrap().insert(mime, found);
    found
}

/// What an `AMediaDataSource` reads from.
struct Source {
    input: Mutex<GrowingReader>,
    length: u64,
}

unsafe extern "C" fn read_at(
    data: *mut c_void,
    offset: i64,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    let source = &*(data as *const Source);
    // The end of the file is 0, not -1: the extractor runs in another
    // process, and the proxy there reads in a loop until it has `size` bytes.
    // -1 fails the whole read, so the last bytes of a file would never arrive.
    if offset < 0 {
        return -1;
    }
    if size == 0 || offset as u64 >= source.length {
        return 0;
    }
    let buffer = std::slice::from_raw_parts_mut(buffer.cast::<u8>(), size);
    let mut input = source.input.lock().unwrap();
    if input.seek(SeekFrom::Start(offset as u64)).is_err() {
        return -1;
    }
    let mut filled = 0;
    while filled < size {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(_) => return -1,
        }
    }
    filled as isize
}

unsafe extern "C" fn get_size(data: *mut c_void) -> isize {
    (*(data as *const Source)).length as isize
}

/// The source's memory is the decoder's to free, after the data source.
unsafe extern "C" fn close(_: *mut c_void) {}

/// One track: its extractor, its decoder, and where their input comes from.
struct Track {
    media: &'static Media,
    source: *mut Source,
    data_source: *mut c_void,
    extractor: *mut c_void,
    codec: *mut c_void,
    /// The last sample has gone into the decoder.
    input_done: bool,
    /// The last output has come out.
    output_done: bool,
}

impl Track {
    /// Open the first track whose MIME type starts with `kind`, and the
    /// format it has, which the caller deletes. `None` when there is none, or
    /// no decoder for it.
    ///
    /// # Safety
    ///
    /// `media` is the NDK's media library.
    unsafe fn open(
        media: &'static Media,
        input: GrowingReader,
        length: u64,
        kind: &str,
    ) -> Result<Option<(Self, *mut c_void, CString)>, String> {
        let source = Box::into_raw(Box::new(Source {
            input: Mutex::new(input),
            length,
        }));
        let mut track = Track {
            media,
            source,
            data_source: (media.AMediaDataSource_new)(),
            extractor: (media.AMediaExtractor_new)(),
            codec: ptr::null_mut(),
            input_done: false,
            output_done: false,
        };
        if track.data_source.is_null() || track.extractor.is_null() {
            return Err("no media extractor".to_owned());
        }
        (media.AMediaDataSource_setUserdata)(track.data_source, source.cast());
        (media.AMediaDataSource_setReadAt)(track.data_source, read_at);
        (media.AMediaDataSource_setGetSize)(track.data_source, get_size);
        (media.AMediaDataSource_setClose)(track.data_source, close);
        let status =
            (media.AMediaExtractor_setDataSourceCustom)(track.extractor, track.data_source);
        if status != ndk_media::AMEDIA_OK {
            return Err(format!("unsupported media ({status})"));
        }

        for index in 0..(media.AMediaExtractor_getTrackCount)(track.extractor) {
            let format = (media.AMediaExtractor_getTrackFormat)(track.extractor, index);
            if format.is_null() {
                continue;
            }
            let mut mime = ptr::null();
            if !(media.AMediaFormat_getString)(format, KEY_MIME.as_ptr(), &mut mime)
                || mime.is_null()
            {
                (media.AMediaFormat_delete)(format);
                continue;
            }
            let mime = CStr::from_ptr(mime).to_owned();
            if !mime.to_string_lossy().starts_with(kind) {
                (media.AMediaFormat_delete)(format);
                continue;
            }
            (media.AMediaExtractor_selectTrack)(track.extractor, index);
            track.codec = (media.AMediaCodec_createDecoderByType)(mime.as_ptr());
            if track.codec.is_null() {
                log::info!("godot-servo: no decoder for {}", mime.to_string_lossy());
                (media.AMediaFormat_delete)(format);
                return Ok(None);
            }
            return Ok(Some((track, format, mime)));
        }
        Ok(None)
    }

    /// Fill the decoder's free input buffers with samples.
    ///
    /// # Safety
    ///
    /// The codec has been started.
    unsafe fn feed(&mut self) {
        let media = self.media;
        while !self.input_done {
            let index = (media.AMediaCodec_dequeueInputBuffer)(self.codec, 0);
            if index < 0 {
                return;
            }
            let mut capacity = 0;
            let buffer =
                (media.AMediaCodec_getInputBuffer)(self.codec, index as usize, &mut capacity);
            let size = if buffer.is_null() {
                -1
            } else {
                (media.AMediaExtractor_readSampleData)(self.extractor, buffer, capacity)
            };
            if size < 0 {
                (media.AMediaCodec_queueInputBuffer)(
                    self.codec,
                    index as usize,
                    0,
                    0,
                    0,
                    ndk_media::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
                );
                self.input_done = true;
                return;
            }
            let time = (media.AMediaExtractor_getSampleTime)(self.extractor).max(0);
            (media.AMediaCodec_queueInputBuffer)(
                self.codec,
                index as usize,
                0,
                size as usize,
                time as u64,
                0,
            );
            (media.AMediaExtractor_advance)(self.extractor);
        }
    }

    /// The next output buffer, which the caller releases. `None` at the end.
    /// `on_format` hears of a change of output format.
    fn next_output(
        &mut self,
        player: &GrowingReader,
        mut on_format: impl FnMut(*mut c_void),
    ) -> Result<Option<(usize, AMediaCodecBufferInfo)>, String> {
        let media = self.media;
        loop {
            if player.is_closed() {
                return Err("player closed".to_owned());
            }
            if self.output_done {
                return Ok(None);
            }
            let mut info = AMediaCodecBufferInfo::default();
            // SAFETY: the codec is started, and owned by this track.
            let index = unsafe {
                self.feed();
                (media.AMediaCodec_dequeueOutputBuffer)(self.codec, &mut info, DEQUEUE_TIMEOUT_US)
            };
            if index >= 0 {
                if info.flags & ndk_media::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                    self.output_done = true;
                    if info.size <= 0 {
                        // SAFETY: the buffer was just dequeued.
                        unsafe {
                            (media.AMediaCodec_releaseOutputBuffer)(
                                self.codec,
                                index as usize,
                                false,
                            )
                        };
                        return Ok(None);
                    }
                }
                return Ok(Some((index as usize, info)));
            }
            if index == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED {
                // SAFETY: as above; the format is deleted after the callback.
                unsafe {
                    let format = (media.AMediaCodec_getOutputFormat)(self.codec);
                    if !format.is_null() {
                        on_format(format);
                        (media.AMediaFormat_delete)(format);
                    }
                }
            }
            // Otherwise nothing is ready yet, or the buffers changed, which the
            // NDK's per-index getters already account for.
        }
    }

    /// # Safety
    ///
    /// The codec is started.
    unsafe fn seek(&mut self, microseconds: i64) {
        (self.media.AMediaExtractor_seekTo)(
            self.extractor,
            microseconds,
            ndk_media::AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC,
        );
        (self.media.AMediaCodec_flush)(self.codec);
        self.input_done = false;
        self.output_done = false;
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        let media = self.media;
        // SAFETY: each object is this track's own, deleted once, decoder first
        // and the source the data source reads from last.
        unsafe {
            if !self.codec.is_null() {
                (media.AMediaCodec_stop)(self.codec);
                (media.AMediaCodec_delete)(self.codec);
            }
            if !self.extractor.is_null() {
                (media.AMediaExtractor_delete)(self.extractor);
            }
            if !self.data_source.is_null() {
                (media.AMediaDataSource_delete)(self.data_source);
            }
            drop(Box::from_raw(self.source));
        }
    }
}

/// YUV to RGB, in 1/1024ths.
#[derive(Clone, Copy)]
struct Matrix {
    y: i32,
    y_offset: i32,
    r_v: i32,
    g_u: i32,
    g_v: i32,
    b_u: i32,
}

impl Matrix {
    fn new(bt709: bool, full_range: bool) -> Self {
        let (kr, kb) = if bt709 {
            (0.2126, 0.0722)
        } else {
            (0.299, 0.114)
        };
        let kg = 1.0 - kr - kb;
        let (y, chroma, y_offset) = if full_range {
            (1.0, 1.0, 0)
        } else {
            (255.0 / 219.0, 255.0 / 224.0, 16)
        };
        let scale = |value: f64| (value * 1024.0).round() as i32;
        Self {
            y: scale(y),
            y_offset,
            r_v: scale(2.0 * (1.0 - kr) * chroma),
            g_u: scale(2.0 * (1.0 - kb) * kb / kg * chroma),
            g_v: scale(2.0 * (1.0 - kr) * kr / kg * chroma),
            b_u: scale(2.0 * (1.0 - kb) * chroma),
        }
    }
}

/// One of Y, U and V in a decoded buffer: where it starts, and the steps to
/// the next pixel and the next row. U and V are at half resolution both ways.
#[derive(Clone, Copy, Default)]
struct Plane {
    offset: usize,
    pixel: usize,
    row: usize,
}

#[derive(Clone, Copy)]
struct Layout {
    planes: [Plane; 3],
    /// The shown part: left, top, width, height.
    crop: (usize, usize, usize, usize),
}

struct VideoTrack {
    track: Track,
    size: (u32, u32),
    /// Known from the decoder's first output format on.
    layout: Result<Layout, String>,
    matrix: Matrix,
    frame_seconds: f64,
}

struct AudioTrack {
    track: Track,
    rate: u32,
    channels: usize,
    float: bool,
}

pub struct MediaCodecDecoder {
    media: &'static Media,
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
    duration: Option<f64>,
    /// Makes a read that waits for bytes give up, so the tracks can close.
    abort: Arc<AtomicBool>,
    /// For seeing whether the player has gone.
    player: GrowingReader,
    planes: Vec<Vec<f32>>,
    /// The first image, decoded while opening to learn the size it is shown
    /// at: the track's own size is the coded one on some devices (1088 for
    /// 1080, 192 for 180).
    first_image: Option<VideoImage>,
}

impl MediaCodecDecoder {
    pub fn open(input: GrowingReader) -> Result<Self, String> {
        let media = ndk_media::get().ok_or("the NDK's media decoding needs API 28")?;
        let length = input.wait_for_length().map_err(|e| e.to_string())?;
        let abort = Arc::new(AtomicBool::new(false));
        let reader = || input.duplicate().with_abort(abort.clone());
        let mut duration: Option<f64> = None;
        let mut note_duration = |format: *mut c_void| {
            // SAFETY: a live format from the extractor.
            if let Some(us) = unsafe { get_i64(media, format, KEY_DURATION) } {
                let seconds = us as f64 / 1e6;
                duration = Some(duration.map_or(seconds, |known| known.max(seconds)));
            }
        };

        // SAFETY: the NDK's documented use; each object is owned by its track
        // from the moment it is made.
        let video = unsafe {
            match Track::open(media, reader(), length, "video/")? {
                Some((track, format, _)) => {
                    note_duration(format);
                    let video = open_video(media, track, format);
                    (media.AMediaFormat_delete)(format);
                    Some(video?)
                }
                None => None,
            }
        };
        let audio = unsafe {
            match Track::open(media, reader(), length, "audio/")? {
                Some((track, format, _)) => {
                    note_duration(format);
                    let audio = open_audio(media, track, format);
                    (media.AMediaFormat_delete)(format);
                    Some(audio?)
                }
                None => None,
            }
        };
        if video.is_none() && audio.is_none() {
            return Err("no audio or video track MediaCodec can decode".to_owned());
        }
        let mut decoder = Self {
            media,
            video,
            audio,
            duration,
            abort,
            player: input,
            planes: Vec::new(),
            first_image: None,
        };
        if decoder.video.is_some() {
            decoder.first_image = decoder.decode_image()?;
            if let (Some(video), Some(image)) = (&mut decoder.video, &decoder.first_image) {
                video.size = (image.width, image.height);
            }
        }
        Ok(decoder)
    }

    /// Decode the next image. `None` at the end of the video.
    fn decode_image(&mut self) -> Result<Option<VideoImage>, String> {
        let media = self.media;
        let Some(video) = &mut self.video else {
            return Ok(None);
        };
        let VideoTrack {
            track,
            layout,
            matrix,
            frame_seconds,
            ..
        } = video;
        loop {
            let output = track.next_output(&self.player, |format| {
                // SAFETY: the callback is given a live format.
                unsafe {
                    *layout = layout_for(media, format);
                    if let Some(height) = get_i32(media, format, KEY_HEIGHT) {
                        *matrix = matrix_for(media, format, height);
                    }
                }
            })?;
            let Some((index, info)) = output else {
                return Ok(None);
            };
            // SAFETY: the buffer was just dequeued and is released below; the
            // NDK reports its size, and `info` the part of it in use.
            let converted = unsafe {
                let mut capacity = 0;
                let buffer = (media.AMediaCodec_getOutputBuffer)(track.codec, index, &mut capacity);
                let start = info.offset.max(0) as usize;
                let converted = if info.size <= 0 {
                    None
                } else if buffer.is_null() || start > capacity {
                    Some(Err("no output buffer".to_owned()))
                } else {
                    let bytes = std::slice::from_raw_parts(buffer.add(start), capacity - start);
                    Some(
                        layout
                            .clone()
                            .and_then(|layout| convert(bytes, &layout, *matrix)),
                    )
                };
                (media.AMediaCodec_releaseOutputBuffer)(track.codec, index, false);
                converted
            };
            let Some(converted) = converted else {
                continue;
            };
            let (width, height, pixels) = converted?;
            return Ok(Some(VideoImage {
                time: info.presentation_time_us as f64 / 1e6,
                duration: *frame_seconds,
                width,
                height,
                pixels,
            }));
        }
    }
}

/// # Safety
///
/// `format` is the track's live format.
unsafe fn open_video(
    media: &'static Media,
    track: Track,
    format: *mut c_void,
) -> Result<VideoTrack, String> {
    let width = get_i32(media, format, KEY_WIDTH).unwrap_or(0);
    let height = get_i32(media, format, KEY_HEIGHT).unwrap_or(0);
    if width <= 0 || height <= 0 {
        return Err("video without a size".to_owned());
    }
    (media.AMediaFormat_setInt32)(
        format,
        KEY_COLOR_FORMAT.as_ptr(),
        COLOR_FORMAT_YUV420_FLEXIBLE,
    );
    let configured =
        (media.AMediaCodec_configure)(track.codec, format, ptr::null_mut(), ptr::null_mut(), 0);
    if configured != ndk_media::AMEDIA_OK
        || (media.AMediaCodec_start)(track.codec) != ndk_media::AMEDIA_OK
    {
        return Err(format!("the video decoder would not start ({configured})"));
    }
    let frame_rate = get_i32(media, format, KEY_FRAME_RATE).filter(|rate| *rate > 0);
    Ok(VideoTrack {
        track,
        size: (width as u32, height as u32),
        layout: Err("no output format yet".to_owned()),
        matrix: matrix_for(media, format, height),
        frame_seconds: frame_rate.map_or(DEFAULT_FRAME_SECONDS, |rate| 1.0 / rate as f64),
    })
}

/// Where the planes of the decoder's buffers are, from its output format.
///
/// # Safety
///
/// `format` is a live format.
unsafe fn layout_for(media: &Media, format: *mut c_void) -> Result<Layout, String> {
    let width = get_i32(media, format, KEY_WIDTH).unwrap_or(0).max(0) as usize;
    let height = get_i32(media, format, KEY_HEIGHT).unwrap_or(0).max(0) as usize;
    if width == 0 || height == 0 {
        return Err("output without a size".to_owned());
    }
    let mut rect = [0i32; 4];
    let display_crop = (media.AMediaFormat_getRect)(
        format,
        KEY_DISPLAY_CROP.as_ptr(),
        &mut rect[0],
        &mut rect[1],
        &mut rect[2],
        &mut rect[3],
    )
    .then_some(rect);
    let crop = match display_crop.map(|rect| rect.map(Some)).unwrap_or([
        get_i32(media, format, KEY_CROP_LEFT),
        get_i32(media, format, KEY_CROP_TOP),
        get_i32(media, format, KEY_CROP_RIGHT),
        get_i32(media, format, KEY_CROP_BOTTOM),
    ]) {
        // Right and bottom are inclusive.
        [Some(left), Some(top), Some(right), Some(bottom)]
            if left >= 0 && top >= 0 && right >= left && bottom >= top =>
        {
            let (left, top) = (left as usize, top as usize);
            (
                left,
                top,
                right as usize - left + 1,
                bottom as usize - top + 1,
            )
        }
        _ => (0, 0, width, height),
    };

    let planes = match media_image(media, format) {
        Some(planes) => planes,
        None => {
            let stride = get_i32(media, format, KEY_STRIDE)
                .filter(|stride| *stride > 0)
                .map_or(width, |stride| stride as usize);
            let slice = get_i32(media, format, KEY_SLICE_HEIGHT)
                .filter(|slice| *slice > 0)
                .map_or(height, |slice| slice as usize);
            let chroma = stride * slice;
            match get_i32(media, format, KEY_COLOR_FORMAT) {
                Some(COLOR_FORMAT_YUV420_PLANAR) => [
                    Plane {
                        offset: 0,
                        pixel: 1,
                        row: stride,
                    },
                    Plane {
                        offset: chroma,
                        pixel: 1,
                        row: stride / 2,
                    },
                    Plane {
                        offset: chroma + stride / 2 * (slice / 2),
                        pixel: 1,
                        row: stride / 2,
                    },
                ],
                Some(COLOR_FORMAT_YUV420_SEMI_PLANAR | COLOR_FORMAT_QCOM_VENUS_NV12) => [
                    Plane {
                        offset: 0,
                        pixel: 1,
                        row: stride,
                    },
                    Plane {
                        offset: chroma,
                        pixel: 2,
                        row: stride,
                    },
                    Plane {
                        offset: chroma + 1,
                        pixel: 2,
                        row: stride,
                    },
                ],
                other => {
                    return Err(format!(
                        "an output colour format with no layout: {:#x}",
                        other.unwrap_or(0)
                    ))
                }
            }
        }
    };
    Ok(Layout { planes, crop })
}

/// The planes a `MediaImage2` describes, when the format carries an 8-bit
/// YUV 4:2:0 one.
///
/// # Safety
///
/// `format` is a live format.
unsafe fn media_image(media: &Media, format: *mut c_void) -> Option<[Plane; 3]> {
    let (mut data, mut size) = (ptr::null_mut(), 0);
    if !(media.AMediaFormat_getBuffer)(format, KEY_IMAGE_DATA.as_ptr(), &mut data, &mut size)
        || data.is_null()
    {
        return None;
    }
    // Six u32 fields, then four planes of five 32-bit fields.
    let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size);
    let field = |index: usize| {
        let word = bytes.get(index * 4..index * 4 + 4)?;
        Some(u32::from_ne_bytes([word[0], word[1], word[2], word[3]]))
    };
    let (kind, planes, bit_depth) = (field(0)?, field(1)?, field(4)?);
    if kind != MEDIA_IMAGE_TYPE_YUV || planes < 3 || bit_depth != 8 {
        return None;
    }
    let mut result = [Plane::default(); 3];
    for (index, plane) in result.iter_mut().enumerate() {
        let base = 6 + index * 5;
        let (offset, pixel, row) = (
            field(base)?,
            field(base + 1)? as i32,
            field(base + 2)? as i32,
        );
        let subsampling = (field(base + 3)?, field(base + 4)?);
        let expected = if index == 0 { (1, 1) } else { (2, 2) };
        if pixel <= 0 || row <= 0 || subsampling != expected {
            return None;
        }
        *plane = Plane {
            offset: offset as usize,
            pixel: pixel as usize,
            row: row as usize,
        };
    }
    Some(result)
}

/// # Safety
///
/// `format` is the track's live format.
unsafe fn open_audio(
    media: &'static Media,
    track: Track,
    format: *mut c_void,
) -> Result<AudioTrack, String> {
    let configured =
        (media.AMediaCodec_configure)(track.codec, format, ptr::null_mut(), ptr::null_mut(), 0);
    if configured != ndk_media::AMEDIA_OK
        || (media.AMediaCodec_start)(track.codec) != ndk_media::AMEDIA_OK
    {
        return Err(format!("the audio decoder would not start ({configured})"));
    }
    Ok(AudioTrack {
        rate: get_i32(media, format, KEY_SAMPLE_RATE).unwrap_or(0).max(0) as u32,
        channels: get_i32(media, format, KEY_CHANNEL_COUNT)
            .unwrap_or(1)
            .max(1) as usize,
        float: false,
        track,
    })
}

/// The colours the stream says it uses, or what is usual for its size.
///
/// # Safety
///
/// `format` is a live format.
unsafe fn matrix_for(media: &Media, format: *mut c_void, height: i32) -> Matrix {
    let bt709 = get_i32(media, format, KEY_COLOR_STANDARD)
        .map_or(height >= 720, |standard| standard == COLOR_STANDARD_BT709);
    let full_range = get_i32(media, format, KEY_COLOR_RANGE) == Some(COLOR_RANGE_FULL);
    Matrix::new(bt709, full_range)
}

/// # Safety
///
/// `format` is a live format.
unsafe fn get_i32(media: &Media, format: *mut c_void, key: &CStr) -> Option<i32> {
    let mut value = 0;
    (media.AMediaFormat_getInt32)(format, key.as_ptr(), &mut value).then_some(value)
}

/// # Safety
///
/// `format` is a live format.
unsafe fn get_i64(media: &Media, format: *mut c_void, key: &CStr) -> Option<i64> {
    let mut value = 0;
    (media.AMediaFormat_getInt64)(format, key.as_ptr(), &mut value).then_some(value)
}

impl Drop for MediaCodecDecoder {
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Release);
        self.video = None;
        self.audio = None;
    }
}

impl MediaDecoder for MediaCodecDecoder {
    fn duration(&self) -> Option<f64> {
        self.duration
    }

    fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    fn video_size(&self) -> Option<(u32, u32)> {
        self.video.as_ref().map(|video| video.size)
    }

    fn next_audio(&mut self) -> Result<Option<AudioChunk<'_>>, String> {
        let media = self.media;
        let Some(audio) = &mut self.audio else {
            return Ok(None);
        };
        let (rate, channels, float) = (&mut audio.rate, &mut audio.channels, &mut audio.float);
        let output = audio.track.next_output(&self.player, |format| {
            // SAFETY: the callback is given a live format.
            unsafe {
                if let Some(value) = get_i32(media, format, KEY_SAMPLE_RATE) {
                    *rate = value.max(0) as u32;
                }
                if let Some(value) = get_i32(media, format, KEY_CHANNEL_COUNT) {
                    *channels = value.max(1) as usize;
                }
                *float = get_i32(media, format, KEY_PCM_ENCODING) == Some(ENCODING_PCM_FLOAT);
            }
        })?;
        let Some((index, info)) = output else {
            return Ok(None);
        };
        let (channels, float) = (audio.channels, audio.float);
        self.planes.resize(channels.min(2), Vec::new());
        for plane in &mut self.planes {
            plane.clear();
        }
        // SAFETY: the buffer was just dequeued and is released below; the
        // NDK reports its size, and `info` the part of it in use.
        unsafe {
            let codec = audio.track.codec;
            let mut capacity = 0;
            let buffer = (media.AMediaCodec_getOutputBuffer)(codec, index, &mut capacity);
            let (start, length) = (info.offset.max(0) as usize, info.size.max(0) as usize);
            if !buffer.is_null() && start + length <= capacity {
                let bytes = std::slice::from_raw_parts(buffer.add(start), length);
                if float {
                    for frame in bytes.chunks_exact(4 * channels) {
                        for (plane, sample) in self.planes.iter_mut().zip(frame.as_chunks::<4>().0)
                        {
                            plane.push(f32::from_le_bytes(*sample));
                        }
                    }
                } else {
                    for frame in bytes.chunks_exact(2 * channels) {
                        for (plane, sample) in self.planes.iter_mut().zip(frame.as_chunks::<2>().0)
                        {
                            plane.push(i16::from_le_bytes(*sample) as f32 / 32768.0);
                        }
                    }
                }
            }
            (media.AMediaCodec_releaseOutputBuffer)(codec, index, false);
        }
        Ok(Some(AudioChunk {
            rate: audio.rate,
            planes: &self.planes,
            time: Some(info.presentation_time_us as f64 / 1e6),
        }))
    }

    fn next_video(&mut self) -> Result<Option<VideoImage>, String> {
        if let Some(image) = self.first_image.take() {
            return Ok(Some(image));
        }
        self.decode_image()
    }

    fn seek(&mut self, seconds: f64) -> Result<Option<f64>, String> {
        let microseconds = (seconds.max(0.0) * 1e6) as i64;
        // SAFETY: the tracks' codecs are started.
        unsafe {
            self.first_image = None;
            if let Some(video) = &mut self.video {
                video.track.seek(microseconds);
            }
            if let Some(audio) = &mut self.audio {
                audio.track.seek(microseconds);
            }
        }
        // Samples carry their own time, so the player sees where it landed.
        Ok(None)
    }
}

/// The shown part of a decoded YUV 4:2:0 buffer as packed, opaque BGRA.
fn convert(bytes: &[u8], layout: &Layout, matrix: Matrix) -> Result<(u32, u32, Vec<u8>), String> {
    let (left, top, width, height) = layout.crop;
    if width == 0 || height == 0 {
        return Err("an empty video frame".to_owned());
    }
    let [y_plane, u_plane, v_plane] = layout.planes;
    // Every index the loop reads is checked here once, so that the loop
    // itself cannot fail.
    let last = |plane: Plane, x: usize, y: usize| plane.offset + y * plane.row + x * plane.pixel;
    let (right, bottom) = (left + width - 1, top + height - 1);
    if last(y_plane, right, bottom) >= bytes.len()
        || last(u_plane, right / 2, bottom / 2) >= bytes.len()
        || last(v_plane, right / 2, bottom / 2) >= bytes.len()
    {
        return Err("a video buffer smaller than its image".to_owned());
    }

    let clamp = |value: i32| (value >> 10).clamp(0, 255) as u8;
    let mut pixels = vec![0u8; width * height * 4];
    for (row, out) in pixels.chunks_exact_mut(width * 4).enumerate() {
        let y_start = y_plane.offset + (top + row) * y_plane.row;
        let u_start = u_plane.offset + (top + row) / 2 * u_plane.row;
        let v_start = v_plane.offset + (top + row) / 2 * v_plane.row;
        for (column, pixel) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = left + column;
            let y = (bytes[y_start + x * y_plane.pixel] as i32 - matrix.y_offset) * matrix.y + 512;
            let u = bytes[u_start + x / 2 * u_plane.pixel] as i32 - 128;
            let v = bytes[v_start + x / 2 * v_plane.pixel] as i32 - 128;
            *pixel = [
                clamp(y + matrix.b_u * u),
                clamp(y - matrix.g_u * u - matrix.g_v * v),
                clamp(y + matrix.r_v * v),
                0xff,
            ];
        }
    }
    Ok((width as u32, height as u32, pixels))
}
