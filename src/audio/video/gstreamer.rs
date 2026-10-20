//! `<video>` on Linux, with the system's GStreamer.
//!
//! The pipeline is `appsrc ! decodebin`, and each stream decodebin decodes gets
//! a branch to an appsink: video through `videoconvert` to BGRA, audio through
//! `audioconvert` to interleaved `f32`. A stream with no decoder installed
//! (H.264 needs gst-libav or openh264, which many systems leave out) goes to a
//! fakesink and is left out. The appsinks do not sync to the clock, so the
//! pipeline runs only as far ahead as the player pulls.
//!
//! appsrc reads the pushed bytes in random-access mode, so demuxers can seek in
//! the file; it needs the length for that, so opening waits until it is known.

use std::ffi::{c_int, c_uint, c_void, CStr};
use std::io::{Read, Seek, SeekFrom};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};

use servo_media::SupportsMediaType;

use super::gst::{self, Gst, GstAppSrcCallbacks, GstMapInfo};
use crate::audio::decode::split_media_type;
use crate::audio::media_decoder::{AudioChunk, MediaDecoder, VideoImage};
use crate::audio::player::GrowingReader;

/// How long a pull waits before looking again at the player and the bus.
const PULL_TIMEOUT_NS: u64 = 100_000_000;
/// What an image lasts when the stream does not say.
const DEFAULT_FRAME_SECONDS: f64 = 1.0 / 30.0;
/// What appsrc asks for when it does not say how much.
const DEFAULT_READ: usize = 64 * 1024;

const VIDEO_CAPS: &CStr = c"video/x-raw,format=BGRA";
const AUDIO_CAPS: &CStr = c"audio/x-raw,format=F32LE,layout=interleaved";

pub fn can_play_type(media_type: &str) -> SupportsMediaType {
    let Some(gst) = gst::get() else {
        return SupportsMediaType::No;
    };
    let (mime, codecs) = split_media_type(media_type);
    let container = match mime.as_str() {
        "video/mp4" => c"video/quicktime",
        "video/webm" => c"video/webm",
        "video/x-matroska" => c"video/x-matroska",
        _ => return SupportsMediaType::No,
    };
    // SAFETY: GStreamer is initialised, and the caps are valid strings.
    unsafe {
        if !has_factory(gst, gst::GST_ELEMENT_FACTORY_TYPE_DEMUXER, container) {
            return SupportsMediaType::No;
        }
        if codecs.is_empty() {
            return SupportsMediaType::Maybe;
        }
        let decodable = codecs.iter().all(|codec| {
            codec_caps(codec)
                .is_some_and(|caps| has_factory(gst, gst::GST_ELEMENT_FACTORY_TYPE_DECODER, caps))
        });
        if decodable {
            SupportsMediaType::Probably
        } else {
            SupportsMediaType::No
        }
    }
}

/// The caps a decoder for a `codecs=` entry takes.
fn codec_caps(codec: &str) -> Option<&'static CStr> {
    let family = codec.split('.').next().unwrap_or(codec);
    Some(match family {
        "avc1" | "avc3" => c"video/x-h264",
        "hev1" | "hvc1" => c"video/x-h265",
        "vp8" => c"video/x-vp8",
        "vp9" | "vp09" => c"video/x-vp9",
        "av01" => c"video/x-av1",
        "mp4a" => c"audio/mpeg, mpegversion=(int)4",
        "mp3" => c"audio/mpeg, mpegversion=(int)1, layer=(int)3",
        "opus" => c"audio/x-opus",
        "vorbis" => c"audio/x-vorbis",
        "flac" => c"audio/x-flac",
        _ => return None,
    })
}

/// Whether an element of `kind` takes `caps` on its sink.
///
/// # Safety
///
/// GStreamer is initialised.
unsafe fn has_factory(gst: &Gst, kind: u64, caps: &CStr) -> bool {
    let factories = (gst.gst_element_factory_list_get_elements)(kind, gst::GST_RANK_MARGINAL);
    let caps = (gst.gst_caps_from_string)(caps.as_ptr());
    let found = if caps.is_null() {
        false
    } else {
        let matching = (gst.gst_element_factory_list_filter)(factories, caps, gst::GST_PAD_SINK, 0);
        (gst.gst_mini_object_unref)(caps);
        let found = !matching.is_null();
        (gst.gst_plugin_feature_list_free)(matching);
        found
    };
    (gst.gst_plugin_feature_list_free)(factories);
    found
}

/// What appsrc's callbacks read from.
struct Source {
    gst: &'static Gst,
    input: Mutex<GrowingReader>,
}

/// The appsinks decodebin's streams end up in, filled in from its streaming
/// threads as the streams appear.
struct Branches {
    gst: &'static Gst,
    pipeline: *mut c_void,
    video: AtomicPtr<c_void>,
    audio: AtomicPtr<c_void>,
}

pub struct GstreamerDecoder {
    gst: &'static Gst,
    pipeline: *mut c_void,
    bus: *mut c_void,
    branches: *mut Branches,
    video: *mut c_void,
    audio: *mut c_void,
    /// Makes a read that waits for bytes give up, so the pipeline can stop.
    abort: Arc<AtomicBool>,
    /// For seeing whether the player has gone.
    player: GrowingReader,
    duration: Option<f64>,
    video_size: Option<(u32, u32)>,
    planes: Vec<Vec<f32>>,
    last_video_end: f64,
}

impl GstreamerDecoder {
    pub fn open(input: GrowingReader) -> Result<Self, String> {
        let gst = gst::get().ok_or("GStreamer is not available")?;
        let length = input.wait_for_length().map_err(|e| e.to_string())?;
        let abort = Arc::new(AtomicBool::new(false));
        let player = input.duplicate();
        let input = input.with_abort(abort.clone());

        // SAFETY: every call is GStreamer's documented use. The objects made
        // here are owned by the decoder from the moment it exists, and its
        // Drop stops the pipeline before freeing what the callbacks use.
        unsafe {
            let pipeline = (gst.gst_pipeline_new)(ptr::null());
            if pipeline.is_null() {
                return Err("no pipeline".to_owned());
            }
            let branches = Box::into_raw(Box::new(Branches {
                gst,
                pipeline,
                video: AtomicPtr::new(ptr::null_mut()),
                audio: AtomicPtr::new(ptr::null_mut()),
            }));
            let mut decoder = Self {
                gst,
                pipeline,
                bus: (gst.gst_element_get_bus)(pipeline),
                branches,
                video: ptr::null_mut(),
                audio: ptr::null_mut(),
                abort,
                player,
                duration: None,
                video_size: None,
                planes: Vec::new(),
                last_video_end: 0.0,
            };

            let source = make(gst, c"appsrc")?;
            let decodebin = make(gst, c"decodebin")?;
            (gst.gst_bin_add)(pipeline, source);
            (gst.gst_bin_add)(pipeline, decodebin);
            if (gst.gst_element_link)(source, decodebin) == 0 {
                return Err("cannot link appsrc to decodebin".to_owned());
            }
            // decodebin's queues of compressed data are a few buffers by
            // default. The video branch stalls while its appsink is full, and
            // the demuxer with it, so the audio could only run as far ahead as
            // those few buffers let it. Let them hold seconds instead: the
            // audio has to stay ahead of the mix.
            set(gst, decodebin, c"max-size-time", c"5000000000");
            set(gst, decodebin, c"max-size-bytes", c"67108864");
            set(gst, decodebin, c"max-size-buffers", c"100000");
            set(gst, source, c"stream-type", c"random-access");
            set(gst, source, c"format", c"bytes");
            let length = std::ffi::CString::new(length.to_string()).unwrap_or_default();
            set(gst, source, c"size", &length);
            let mut callbacks = GstAppSrcCallbacks {
                need_data: Some(need_data),
                enough_data: None,
                seek_data: Some(seek_data),
                _gst_reserved: [ptr::null_mut(); 4],
            };
            let context = Box::into_raw(Box::new(Source {
                gst,
                input: Mutex::new(input),
            }));
            (gst.gst_app_src_set_callbacks)(
                source,
                &mut callbacks,
                context.cast(),
                Some(drop_source),
            );
            (gst.g_signal_connect_data)(
                decodebin,
                c"pad-added".as_ptr(),
                pad_added as *mut c_void,
                branches.cast(),
                ptr::null_mut(),
                0,
            );

            // Paused, the pipeline finds its streams and decodes up to the
            // first sample of each.
            if (gst.gst_element_set_state)(pipeline, gst::GST_STATE_PAUSED)
                == gst::GST_STATE_CHANGE_FAILURE
            {
                return Err(decoder.error_or("the pipeline would not start"));
            }
            loop {
                let result = (gst.gst_element_get_state)(
                    pipeline,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    PULL_TIMEOUT_NS,
                );
                if result == gst::GST_STATE_CHANGE_FAILURE {
                    return Err(decoder.error_or("unsupported media"));
                }
                if result != gst::GST_STATE_CHANGE_ASYNC {
                    break;
                }
                if decoder.player.is_closed() {
                    return Err("player closed".to_owned());
                }
            }

            decoder.video = (*branches).video.load(Ordering::Acquire);
            decoder.audio = (*branches).audio.load(Ordering::Acquire);
            if decoder.video.is_null() && decoder.audio.is_null() {
                return Err("no audio or video stream GStreamer can decode".to_owned());
            }
            if !decoder.video.is_null() {
                decoder.video_size = sink_size(gst, decoder.video);
            }
            let mut duration = 0i64;
            if (gst.gst_element_query_duration)(pipeline, gst::GST_FORMAT_TIME, &mut duration) != 0
                && duration > 0
            {
                decoder.duration = Some(duration as f64 / gst::GST_SECOND);
            }
            (gst.gst_element_set_state)(pipeline, gst::GST_STATE_PLAYING);
            Ok(decoder)
        }
    }

    /// The pipeline's error, or `fallback` when it posted none.
    fn error_or(&self, fallback: &str) -> String {
        self.bus_error().unwrap_or_else(|| fallback.to_owned())
    }

    fn bus_error(&self) -> Option<String> {
        if self.bus.is_null() {
            return None;
        }
        // SAFETY: the bus is the pipeline's, held until Drop.
        unsafe {
            let message = (self.gst.gst_bus_pop_filtered)(self.bus, gst::GST_MESSAGE_ERROR);
            if message.is_null() {
                return None;
            }
            let mut error = ptr::null_mut();
            let mut debug = ptr::null_mut();
            (self.gst.gst_message_parse_error)(message, &mut error, &mut debug);
            let text = gst::take_error(self.gst, error);
            (self.gst.g_free)(debug.cast());
            (self.gst.gst_mini_object_unref)(message);
            Some(text)
        }
    }

    /// The next sample from an appsink, owned by the caller. `None` at the
    /// end of its stream.
    fn pull(&self, sink: *mut c_void) -> Result<Option<*mut c_void>, String> {
        loop {
            if self.player.is_closed() {
                return Err("player closed".to_owned());
            }
            if let Some(error) = self.bus_error() {
                return Err(error);
            }
            // SAFETY: the appsink belongs to the pipeline, held until Drop.
            unsafe {
                let sample = (self.gst.gst_app_sink_try_pull_sample)(sink, PULL_TIMEOUT_NS);
                if !sample.is_null() {
                    return Ok(Some(sample));
                }
                if (self.gst.gst_app_sink_is_eos)(sink) != 0 {
                    return Ok(None);
                }
            }
        }
    }
}

impl Drop for GstreamerDecoder {
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Release);
        // SAFETY: stopping joins GStreamer's threads, so nothing runs the
        // callbacks once it returns, and what they use can be freed.
        unsafe {
            (self.gst.gst_element_set_state)(self.pipeline, gst::GST_STATE_NULL);
            if !self.bus.is_null() {
                (self.gst.gst_object_unref)(self.bus);
            }
            (self.gst.gst_object_unref)(self.pipeline);
            drop(Box::from_raw(self.branches));
        }
    }
}

impl MediaDecoder for GstreamerDecoder {
    fn duration(&self) -> Option<f64> {
        self.duration
    }

    fn has_audio(&self) -> bool {
        !self.audio.is_null()
    }

    fn video_size(&self) -> Option<(u32, u32)> {
        self.video_size
    }

    fn next_audio(&mut self) -> Result<Option<AudioChunk<'_>>, String> {
        if self.audio.is_null() {
            return Ok(None);
        }
        let Some(sample) = self.pull(self.audio)? else {
            return Ok(None);
        };
        // SAFETY: the sample is ours until it is unreferenced below, and its
        // buffer and caps live as long as it does.
        unsafe {
            let gst = self.gst;
            let caps = (gst.gst_sample_get_caps)(sample);
            let rate = caps_int(gst, caps, c"rate").unwrap_or(0).max(0) as u32;
            let channels = caps_int(gst, caps, c"channels").unwrap_or(1).max(1) as usize;
            let buffer = (gst.gst_sample_get_buffer)(sample);
            let time = stream_time(gst, sample, (*buffer).pts);

            // Stereo at most: the mixer takes the first two channels.
            self.planes.resize(channels.min(2), Vec::new());
            for plane in &mut self.planes {
                plane.clear();
            }
            let mut map: GstMapInfo = std::mem::zeroed();
            if (gst.gst_buffer_map)(buffer, &mut map, gst::GST_MAP_READ) != 0 {
                let bytes = std::slice::from_raw_parts(map.data, map.size);
                for frame in bytes.chunks_exact(4 * channels) {
                    for (plane, sample) in self.planes.iter_mut().zip(frame.as_chunks::<4>().0) {
                        plane.push(f32::from_le_bytes(*sample));
                    }
                }
                (gst.gst_buffer_unmap)(buffer, &mut map);
            }
            (gst.gst_mini_object_unref)(sample);
            Ok(Some(AudioChunk {
                rate,
                planes: &self.planes,
                time,
            }))
        }
    }

    fn next_video(&mut self) -> Result<Option<VideoImage>, String> {
        if self.video.is_null() {
            return Ok(None);
        }
        let Some(sample) = self.pull(self.video)? else {
            return Ok(None);
        };
        // SAFETY: as in `next_audio`.
        let image = unsafe {
            let gst = self.gst;
            let caps = (gst.gst_sample_get_caps)(sample);
            let width = caps_int(gst, caps, c"width").unwrap_or(0).max(0) as usize;
            let height = caps_int(gst, caps, c"height").unwrap_or(0).max(0) as usize;
            let buffer = (gst.gst_sample_get_buffer)(sample);
            let time = stream_time(gst, sample, (*buffer).pts).unwrap_or(self.last_video_end);
            let duration = timestamp((*buffer).duration).unwrap_or(DEFAULT_FRAME_SECONDS);

            let mut pixels = Vec::new();
            let mut map: GstMapInfo = std::mem::zeroed();
            if width > 0
                && height > 0
                && (gst.gst_buffer_map)(buffer, &mut map, gst::GST_MAP_READ) != 0
            {
                let bytes = std::slice::from_raw_parts(map.data, map.size);
                // videoconvert packs BGRA rows, but a stride wider than the
                // row is allowed for.
                let row = width * 4;
                let stride = if bytes.len() / height >= row {
                    bytes.len() / height
                } else {
                    0
                };
                if stride > 0 {
                    pixels.reserve(row * height);
                    for y in 0..height {
                        pixels.extend_from_slice(&bytes[y * stride..y * stride + row]);
                    }
                }
                (gst.gst_buffer_unmap)(buffer, &mut map);
            }
            (gst.gst_mini_object_unref)(sample);
            if pixels.is_empty() {
                return Err("an empty video frame".to_owned());
            }
            VideoImage {
                time,
                duration,
                width: width as u32,
                height: height as u32,
                pixels,
            }
        };
        self.last_video_end = image.time + image.duration;
        Ok(Some(image))
    }

    fn seek(&mut self, seconds: f64) -> Result<Option<f64>, String> {
        // SAFETY: the pipeline is held until Drop.
        let ok = unsafe {
            (self.gst.gst_element_seek_simple)(
                self.pipeline,
                gst::GST_FORMAT_TIME,
                gst::GST_SEEK_FLAG_FLUSH | gst::GST_SEEK_FLAG_ACCURATE,
                (seconds.max(0.0) * gst::GST_SECOND) as i64,
            )
        };
        if ok == 0 {
            return Err("seek failed".to_owned());
        }
        self.last_video_end = seconds;
        // Samples carry their own time, so the player sees where it landed.
        Ok(None)
    }
}

fn timestamp(nanoseconds: u64) -> Option<f64> {
    (nanoseconds != gst::GST_CLOCK_TIME_NONE).then(|| nanoseconds as f64 / gst::GST_SECOND)
}

/// Where a buffer sits in the media's own time. Its timestamp alone is not
/// that: qtdemux applies an MP4 edit list through the segment instead, so a
/// file whose video starts two frames into its track stamps the first frame
/// two frames late.
///
/// # Safety
///
/// `sample` is a live sample.
unsafe fn stream_time(gst: &Gst, sample: *mut c_void, pts: u64) -> Option<f64> {
    let segment = (gst.gst_sample_get_segment)(sample);
    if segment.is_null() || pts == gst::GST_CLOCK_TIME_NONE {
        return timestamp(pts);
    }
    timestamp((gst.gst_segment_to_stream_time)(
        segment,
        gst::GST_FORMAT_TIME,
        pts,
    ))
}

/// # Safety
///
/// GStreamer is initialised.
unsafe fn make(gst: &Gst, factory: &CStr) -> Result<*mut c_void, String> {
    let element = (gst.gst_element_factory_make)(factory.as_ptr(), ptr::null());
    if element.is_null() {
        Err(format!("no {} element", factory.to_string_lossy()))
    } else {
        Ok(element)
    }
}

/// # Safety
///
/// `object` is a live GObject with a property `name`.
unsafe fn set(gst: &Gst, object: *mut c_void, name: &CStr, value: &CStr) {
    (gst.gst_util_set_object_arg)(object, name.as_ptr(), value.as_ptr());
}

/// # Safety
///
/// `caps` is null or live caps.
unsafe fn caps_int(gst: &Gst, caps: *mut c_void, field: &CStr) -> Option<c_int> {
    if caps.is_null() {
        return None;
    }
    let structure = (gst.gst_caps_get_structure)(caps, 0);
    if structure.is_null() {
        return None;
    }
    let mut value = 0;
    ((gst.gst_structure_get_int)(structure, field.as_ptr(), &mut value) != 0).then_some(value)
}

/// The width and height an appsink has negotiated.
///
/// # Safety
///
/// `sink` is a live appsink.
unsafe fn sink_size(gst: &Gst, sink: *mut c_void) -> Option<(u32, u32)> {
    let pad = (gst.gst_element_get_static_pad)(sink, c"sink".as_ptr());
    if pad.is_null() {
        return None;
    }
    let caps = (gst.gst_pad_get_current_caps)(pad);
    (gst.gst_object_unref)(pad);
    let size = caps_int(gst, caps, c"width").zip(caps_int(gst, caps, c"height"));
    if !caps.is_null() {
        (gst.gst_mini_object_unref)(caps);
    }
    size.filter(|(width, height)| *width > 0 && *height > 0)
        .map(|(width, height)| (width as u32, height as u32))
}

/// decodebin found a stream: send it down a branch to an appsink when it is
/// decoded audio or video, the first of its kind, and to a fakesink otherwise.
unsafe extern "C" fn pad_added(_decodebin: *mut c_void, pad: *mut c_void, data: *mut c_void) {
    let branches = &*(data as *const Branches);
    let gst = branches.gst;
    let caps = (gst.gst_pad_get_current_caps)(pad);
    let kind = if caps.is_null() {
        String::new()
    } else {
        let structure = (gst.gst_caps_get_structure)(caps, 0);
        let name = gst::text((gst.gst_structure_get_name)(structure));
        (gst.gst_mini_object_unref)(caps);
        name
    };
    let branch = match kind.as_str() {
        "video/x-raw" => Some((&branches.video, c"videoconvert", VIDEO_CAPS, c"4")),
        "audio/x-raw" => Some((&branches.audio, c"audioconvert", AUDIO_CAPS, c"0")),
        _ => None,
    }
    .filter(|(slot, ..)| slot.load(Ordering::Acquire).is_null());

    let Some((slot, convert, caps, max_buffers)) = branch else {
        if let Ok(sink) = make(gst, c"fakesink") {
            attach(gst, branches.pipeline, pad, &[sink]);
        }
        return;
    };
    let (Ok(convert), Ok(sink)) = (make(gst, convert), make(gst, c"appsink")) else {
        return;
    };
    set(gst, sink, c"caps", caps);
    set(gst, sink, c"sync", c"false");
    set(gst, sink, c"max-buffers", max_buffers);
    set(gst, sink, c"enable-last-sample", c"false");
    if attach(gst, branches.pipeline, pad, &[convert, sink]) {
        slot.store(sink, Ordering::Release);
    }
}

/// Add `chain` to the pipeline, link it in order, start it, and feed `pad`
/// into its head.
///
/// # Safety
///
/// The elements are new and floating; the pipeline takes them.
unsafe fn attach(
    gst: &Gst,
    pipeline: *mut c_void,
    pad: *mut c_void,
    chain: &[*mut c_void],
) -> bool {
    for element in chain {
        (gst.gst_bin_add)(pipeline, *element);
    }
    for pair in chain.windows(2) {
        if (gst.gst_element_link)(pair[0], pair[1]) == 0 {
            return false;
        }
    }
    for element in chain {
        (gst.gst_element_sync_state_with_parent)(*element);
    }
    let sink_pad = (gst.gst_element_get_static_pad)(chain[0], c"sink".as_ptr());
    if sink_pad.is_null() {
        return false;
    }
    let linked = (gst.gst_pad_link)(pad, sink_pad) == 0;
    (gst.gst_object_unref)(sink_pad);
    linked
}

unsafe extern "C" fn need_data(source: *mut c_void, length: c_uint, data: *mut c_void) {
    let context = &*(data as *const Source);
    let gst = context.gst;
    let size = match length {
        0 | c_uint::MAX => DEFAULT_READ,
        length => (length as usize).min(16 * DEFAULT_READ),
    };
    let mut bytes = vec![0u8; size];
    let mut filled = 0;
    {
        let mut input = context.input.lock().unwrap();
        while filled < size {
            match input.read(&mut bytes[filled..]) {
                Ok(0) | Err(_) => break,
                Ok(count) => filled += count,
            }
        }
    }
    if filled == 0 {
        (gst.gst_app_src_end_of_stream)(source);
        return;
    }
    let buffer = (gst.gst_buffer_new_allocate)(ptr::null_mut(), filled, ptr::null_mut());
    if buffer.is_null() {
        (gst.gst_app_src_end_of_stream)(source);
        return;
    }
    (gst.gst_buffer_fill)(buffer, 0, bytes.as_ptr().cast(), filled);
    (gst.gst_app_src_push_buffer)(source, buffer);
}

unsafe extern "C" fn seek_data(
    _source: *mut c_void,
    offset: u64,
    data: *mut c_void,
) -> gst::Gboolean {
    let context = &*(data as *const Source);
    context
        .input
        .lock()
        .unwrap()
        .seek(SeekFrom::Start(offset))
        .is_ok() as gst::Gboolean
}

unsafe extern "C" fn drop_source(data: *mut c_void) {
    drop(Box::from_raw(data as *mut Source));
}
