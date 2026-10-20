//! `<video>` on Windows, with Media Foundation's source reader.
//!
//! The source reader demuxes the file and decodes both tracks, and its video
//! processor brings images to RGB32, which is BGRA in memory. It reads the file
//! through an `IStream` over the bytes the element has pushed. Media Foundation
//! sizes the file up front, so opening waits until its length is known.
//!
//! Decoding is in software: the reader is given no Direct3D device.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

use servo_media::SupportsMediaType;
use windows::core::{implement, Interface, Ref, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    E_FAIL, E_NOTIMPL, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION, S_FALSE, S_OK,
};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSample, IMFSourceReader,
    MFAudioFormat_Float, MFCreateAttributes, MFCreateMFByteStreamOnStream, MFCreateMediaType,
    MFCreateSourceReaderFromByteStream, MFMediaType_Audio, MFMediaType_Video, MFShutdown,
    MFStartup, MFVideoArea, MFVideoFormat_RGB32, MFSTARTUP_NOSOCKET, MF_BYTESTREAM_CONTENT_TYPE,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_SIZE, MF_MT_GEOMETRIC_APERTURE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
    MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE, MF_VERSION,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, ISequentialStream_Impl, IStream, IStream_Impl,
    COINIT_MULTITHREADED, LOCKTYPE, STATFLAG, STATSTG, STGC, STGTY_STREAM, STREAM_SEEK,
    STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::Win32::System::Variant::{VT_I8, VT_UI8};

use crate::audio::decode::split_media_type;
use crate::audio::media_decoder::{AudioChunk, MediaDecoder, VideoImage};
use crate::audio::player::GrowingReader;

const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO_STREAM: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
/// Media Foundation's unit of time, in seconds.
const TICK: f64 = 1e-7;
/// What an image lasts when the file does not say.
const DEFAULT_FRAME_SECONDS: f64 = 1.0 / 30.0;

/// Codecs a stock Windows decodes, with or without the Store's free
/// extensions (HEVC, VP9 and AV1 are among those, and come preinstalled).
const KNOWN_CODECS: &[&str] = &[
    "avc1", "avc3", "hev1", "hvc1", "vp8", "vp9", "vp09", "av01", "mp4a", "mp3", "opus", "vorbis",
    "flac",
];
/// Codecs built into Windows itself, but for its N editions.
const BUILT_IN_MP4_CODECS: &[&str] = &["avc1", "avc3", "mp4a"];

pub fn can_play_type(media_type: &str) -> SupportsMediaType {
    let (mime, codecs) = split_media_type(media_type);
    let in_family = |codec: &String, families: &[&str]| {
        families.iter().any(|family| {
            codec
                .strip_prefix(family)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
    };
    match mime.as_str() {
        "video/mp4" | "video/webm" | "video/x-matroska" => {
            if codecs.is_empty() {
                SupportsMediaType::Maybe
            } else if mime == "video/mp4"
                && codecs
                    .iter()
                    .all(|codec| in_family(codec, BUILT_IN_MP4_CODECS))
            {
                SupportsMediaType::Probably
            } else if codecs.iter().all(|codec| in_family(codec, KNOWN_CODECS)) {
                SupportsMediaType::Maybe
            } else {
                SupportsMediaType::No
            }
        }
        _ => SupportsMediaType::No,
    }
}

fn error(context: &str, error: windows::core::Error) -> String {
    format!("{context}: {error}")
}

/// COM and Media Foundation, started for this thread for as long as it lives.
struct Runtime;

impl Runtime {
    fn start() -> Result<Self, String> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| error("CoInitializeEx", e))?;
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) {
                CoUninitialize();
                return Err(error("MFStartup", e));
            }
        }
        Ok(Self)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

struct VideoFormat {
    /// The decoded image, which can be taller than what is shown: H.264 codes
    /// 1080 lines as 1088.
    width: u32,
    height: u32,
    /// The part of it that is shown: left, top, width, height.
    shown: (u32, u32, u32, u32),
    /// Bytes from one row to the next, for buffers that are not 2D; negative
    /// when the rows are stored bottom up.
    stride: i32,
}

struct AudioFormat {
    rate: u32,
    channels: u32,
}

pub struct MediaFoundationDecoder {
    reader: IMFSourceReader,
    duration: Option<f64>,
    video: Option<VideoFormat>,
    audio: Option<AudioFormat>,
    planes: Vec<Vec<f32>>,
    // Last: Media Foundation's objects go before it is shut down.
    _runtime: Runtime,
}

impl MediaFoundationDecoder {
    pub fn open(mut input: GrowingReader) -> Result<Self, String> {
        let length = input.wait_for_length().map_err(|e| e.to_string())?;
        let content_type = sniff(&mut input);
        let runtime = Runtime::start()?;
        unsafe {
            let stream: IStream = PushedStream {
                input: Mutex::new(input),
                length,
            }
            .into();
            let byte_stream =
                MFCreateMFByteStreamOnStream(&stream).map_err(|e| error("byte stream", e))?;
            // Media Foundation picks a demuxer by content type before sniffing,
            // and has no name or type to go by otherwise.
            if let (Some(content_type), Ok(attributes)) =
                (content_type, byte_stream.cast::<IMFAttributes>())
            {
                let _ = attributes.SetString(&MF_BYTESTREAM_CONTENT_TYPE, content_type);
            }

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1).map_err(|e| error("attributes", e))?;
            let attributes = attributes.ok_or("no attributes")?;
            attributes
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .map_err(|e| error("attributes", e))?;
            let reader = MFCreateSourceReaderFromByteStream(&byte_stream, &attributes)
                .map_err(|e| error("unsupported media", e))?;

            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(|e| error("stream selection", e))?;
            let video = select_video(&reader).ok();
            let audio = select_audio(&reader).ok();
            if video.is_none() && audio.is_none() {
                return Err("no audio or video track Media Foundation can decode".to_owned());
            }

            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .filter(|value| value.Anonymous.Anonymous.vt == VT_UI8)
                .map(|value| value.Anonymous.Anonymous.Anonymous.uhVal as f64 * TICK)
                .filter(|duration| *duration > 0.0);

            Ok(Self {
                reader,
                duration,
                video,
                audio,
                planes: Vec::new(),
                _runtime: runtime,
            })
        }
    }

    /// The next sample of a stream, with its time. `None` at its end.
    fn read(&mut self, stream: u32) -> Result<Option<(IMFSample, f64)>, String> {
        loop {
            let mut flags = 0u32;
            let mut time = 0i64;
            let mut sample = None;
            unsafe {
                self.reader
                    .ReadSample(
                        stream,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut time),
                        Some(&mut sample),
                    )
                    .map_err(|e| error("ReadSample", e))?;
            }
            let has = |flag: i32| flags & flag as u32 != 0;
            if has(MF_SOURCE_READERF_ERROR.0) {
                return Err("the source reader failed".to_owned());
            }
            if has(MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0) {
                let current = unsafe { self.reader.GetCurrentMediaType(stream) }
                    .map_err(|e| error("media type", e))?;
                if stream == VIDEO_STREAM {
                    self.video = Some(video_format(&current)?);
                } else {
                    self.audio = Some(audio_format(&current)?);
                }
            }
            if let Some(sample) = sample {
                return Ok(Some((sample, time as f64 * TICK)));
            }
            if has(MF_SOURCE_READERF_ENDOFSTREAM.0) {
                return Ok(None);
            }
            // A gap in the stream: nothing to return, so read on.
        }
    }
}

impl MediaDecoder for MediaFoundationDecoder {
    fn duration(&self) -> Option<f64> {
        self.duration
    }

    fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    fn video_size(&self) -> Option<(u32, u32)> {
        self.video
            .as_ref()
            .map(|format| (format.shown.2, format.shown.3))
    }

    fn next_audio(&mut self) -> Result<Option<AudioChunk<'_>>, String> {
        if self.audio.is_none() {
            return Ok(None);
        }
        let Some((sample, time)) = self.read(AUDIO_STREAM)? else {
            return Ok(None);
        };
        let Some(format) = &self.audio else {
            return Ok(None);
        };
        let channels = format.channels.max(1) as usize;
        let rate = format.rate;
        // Stereo at most: the mixer takes the first two channels.
        self.planes.resize(channels.min(2), Vec::new());
        for plane in &mut self.planes {
            plane.clear();
        }
        unsafe {
            let buffer = sample
                .ConvertToContiguousBuffer()
                .map_err(|e| error("audio buffer", e))?;
            let mut data = std::ptr::null_mut();
            let mut length = 0u32;
            buffer
                .Lock(&mut data, None, Some(&mut length))
                .map_err(|e| error("audio buffer", e))?;
            let bytes = std::slice::from_raw_parts(data, length as usize);
            for frame in bytes.chunks_exact(4 * channels) {
                for (plane, sample) in self.planes.iter_mut().zip(frame.as_chunks::<4>().0) {
                    plane.push(f32::from_le_bytes(*sample));
                }
            }
            let _ = buffer.Unlock();
        }
        Ok(Some(AudioChunk {
            rate,
            planes: &self.planes,
            time: Some(time),
        }))
    }

    fn next_video(&mut self) -> Result<Option<VideoImage>, String> {
        if self.video.is_none() {
            return Ok(None);
        }
        let Some((sample, time)) = self.read(VIDEO_STREAM)? else {
            return Ok(None);
        };
        let format = self.video.as_ref().ok_or("no video format")?;
        let duration = unsafe { sample.GetSampleDuration() }
            .ok()
            .filter(|ticks| *ticks > 0)
            .map_or(DEFAULT_FRAME_SECONDS, |ticks| ticks as f64 * TICK);
        let pixels = unsafe { copy_image(&sample, format) }?;
        Ok(Some(VideoImage {
            time,
            duration,
            width: format.shown.2,
            height: format.shown.3,
            pixels,
        }))
    }

    fn seek(&mut self, seconds: f64) -> Result<Option<f64>, String> {
        let mut position = PROPVARIANT::default();
        unsafe {
            let value = &mut position.Anonymous.Anonymous;
            value.vt = VT_I8;
            value.Anonymous.hVal = (seconds.max(0.0) / TICK) as i64;
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &position)
                .map_err(|e| error("seek", e))?;
        }
        // Chunks carry their own time, so the player sees where it landed.
        Ok(None)
    }
}

/// The content type Media Foundation needs to pick a demuxer, from the first
/// bytes of the file.
fn sniff(input: &mut GrowingReader) -> Option<PCWSTR> {
    let mut head = [0u8; 12];
    let read = input.read_exact(&mut head);
    let _ = input.seek(SeekFrom::Start(0));
    read.ok()?;
    if &head[4..8] == b"ftyp" {
        Some(windows::core::w!("video/mp4"))
    } else if head[..4] == [0x1a, 0x45, 0xdf, 0xa3] {
        Some(windows::core::w!("video/webm"))
    } else {
        None
    }
}

unsafe fn select_video(reader: &IMFSourceReader) -> Result<VideoFormat, String> {
    let wanted = media_type(&MFMediaType_Video, &MFVideoFormat_RGB32)?;
    select(reader, VIDEO_STREAM, &wanted)?;
    video_format(
        &reader
            .GetCurrentMediaType(VIDEO_STREAM)
            .map_err(|e| error("video type", e))?,
    )
}

unsafe fn select_audio(reader: &IMFSourceReader) -> Result<AudioFormat, String> {
    let wanted = media_type(&MFMediaType_Audio, &MFAudioFormat_Float)?;
    select(reader, AUDIO_STREAM, &wanted)?;
    audio_format(
        &reader
            .GetCurrentMediaType(AUDIO_STREAM)
            .map_err(|e| error("audio type", e))?,
    )
}

unsafe fn media_type(major: &GUID, subtype: &GUID) -> Result<IMFMediaType, String> {
    let media_type = MFCreateMediaType().map_err(|e| error("media type", e))?;
    media_type
        .SetGUID(&MF_MT_MAJOR_TYPE, major)
        .and_then(|_| media_type.SetGUID(&MF_MT_SUBTYPE, subtype))
        .map_err(|e| error("media type", e))?;
    Ok(media_type)
}

/// Turn a stream on and ask for `wanted` out of it; off again if that fails.
unsafe fn select(
    reader: &IMFSourceReader,
    stream: u32,
    wanted: &IMFMediaType,
) -> Result<(), String> {
    reader
        .SetStreamSelection(stream, true)
        .map_err(|e| error("no such stream", e))?;
    if let Err(e) = reader.SetCurrentMediaType(stream, None, wanted) {
        let _ = reader.SetStreamSelection(stream, false);
        return Err(error("no decoder", e));
    }
    Ok(())
}

fn video_format(media_type: &IMFMediaType) -> Result<VideoFormat, String> {
    unsafe {
        let size = media_type
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| error("frame size", e))?;
        let (width, height) = ((size >> 32) as u32, size as u32);
        if width == 0 || height == 0 {
            return Err("empty frame size".to_owned());
        }
        let shown = [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE]
            .iter()
            .find_map(|key| {
                let mut area = MFVideoArea::default();
                let bytes = std::slice::from_raw_parts_mut(
                    (&mut area as *mut MFVideoArea).cast::<u8>(),
                    size_of::<MFVideoArea>(),
                );
                media_type.GetBlob(key, bytes, None).ok()?;
                let (left, top) = (area.OffsetX.value, area.OffsetY.value);
                let (w, h) = (area.Area.cx, area.Area.cy);
                (left >= 0 && top >= 0 && w > 0 && h > 0)
                    .then_some((left as u32, top as u32, w as u32, h as u32))
                    .filter(|(left, top, w, h)| left + w <= width && top + h <= height)
            })
            .unwrap_or((0, 0, width, height));
        // RGB rows are bottom up unless the type says otherwise.
        let stride = media_type
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map_or(-((width * 4) as i32), |stride| stride as i32);
        Ok(VideoFormat {
            width,
            height,
            shown,
            stride,
        })
    }
}

fn audio_format(media_type: &IMFMediaType) -> Result<AudioFormat, String> {
    unsafe {
        Ok(AudioFormat {
            rate: media_type
                .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
                .map_err(|e| error("sample rate", e))?,
            channels: media_type
                .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
                .map_err(|e| error("channels", e))?,
        })
    }
}

/// Copy the shown part of an image out, opaque, with its rows packed.
unsafe fn copy_image(sample: &IMFSample, format: &VideoFormat) -> Result<Vec<u8>, String> {
    let buffer: IMFMediaBuffer = sample
        .ConvertToContiguousBuffer()
        .map_err(|e| error("video buffer", e))?;
    let (left, top, width, height) = format.shown;
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let copy = |first_row: *const u8, pitch: isize, pixels: &mut [u8]| {
        let row_bytes = width as usize * 4;
        for (row, out) in pixels.chunks_exact_mut(row_bytes).enumerate() {
            let start = first_row.offset((top as isize + row as isize) * pitch + left as isize * 4);
            out.copy_from_slice(std::slice::from_raw_parts(start, row_bytes));
            // RGB32's fourth byte is padding, not alpha.
            for pixel in out.as_chunks_mut::<4>().0 {
                pixel[3] = 0xff;
            }
        }
    };

    if let Ok(buffer_2d) = buffer.cast::<IMF2DBuffer>() {
        let mut first_row = std::ptr::null_mut();
        let mut pitch = 0i32;
        buffer_2d
            .Lock2D(&mut first_row, &mut pitch)
            .map_err(|e| error("video buffer", e))?;
        if pitch.unsigned_abs() < format.width * 4 {
            let _ = buffer_2d.Unlock2D();
            return Err("video rows shorter than the image".to_owned());
        }
        copy(first_row, pitch as isize, &mut pixels);
        let _ = buffer_2d.Unlock2D();
    } else {
        let mut data = std::ptr::null_mut();
        let mut length = 0u32;
        buffer
            .Lock(&mut data, None, Some(&mut length))
            .map_err(|e| error("video buffer", e))?;
        let row = format.stride.unsigned_abs() as usize;
        if row < format.width as usize * 4 || (length as usize) < row * format.height as usize {
            let _ = buffer.Unlock();
            return Err("video buffer smaller than the image".to_owned());
        }
        let first_row = if format.stride < 0 {
            data.add(row * (format.height as usize - 1))
        } else {
            data
        };
        copy(first_row, format.stride as isize, &mut pixels);
        let _ = buffer.Unlock();
    }
    Ok(pixels)
}

/// The pushed bytes as a read-only `IStream`, which Media Foundation wraps in
/// a byte stream. Reads block until the bytes have arrived.
#[implement(IStream)]
struct PushedStream {
    input: Mutex<GrowingReader>,
    length: u64,
}

impl ISequentialStream_Impl for PushedStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        let mut filled = 0usize;
        if cb > 0 {
            if pv.is_null() {
                return E_FAIL;
            }
            let buf = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
            let mut input = self.input.lock().unwrap();
            while filled < buf.len() {
                match input.read(&mut buf[filled..]) {
                    Ok(0) => break,
                    Ok(count) => filled += count,
                    Err(_) => return E_FAIL,
                }
            }
        }
        if !pcbread.is_null() {
            unsafe { *pcbread = filled as u32 };
        }
        if filled == cb as usize {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Write(&self, _: *const core::ffi::c_void, _: u32, _: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for PushedStream_Impl {
    fn Seek(
        &self,
        offset: i64,
        origin: STREAM_SEEK,
        new_position: *mut u64,
    ) -> windows::core::Result<()> {
        let from = match origin {
            STREAM_SEEK_SET => SeekFrom::Start(offset.max(0) as u64),
            STREAM_SEEK_CUR => SeekFrom::Current(offset),
            STREAM_SEEK_END => SeekFrom::Start(self.length.saturating_add_signed(offset)),
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        let position = self
            .input
            .lock()
            .unwrap()
            .seek(from)
            .map_err(|_| windows::core::Error::from(STG_E_INVALIDFUNCTION))?;
        if !new_position.is_null() {
            unsafe { *new_position = position };
        }
        Ok(())
    }

    fn SetSize(&self, _: u64) -> windows::core::Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn CopyTo(
        &self,
        _: Ref<IStream>,
        _: u64,
        _: *mut u64,
        _: *mut u64,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _: u64, _: u64, _: &LOCKTYPE) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _: u64, _: u64, _: u32) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn Stat(&self, stat: *mut STATSTG, _: &STATFLAG) -> windows::core::Result<()> {
        if stat.is_null() {
            return Err(E_FAIL.into());
        }
        unsafe {
            *stat = STATSTG {
                r#type: STGTY_STREAM.0 as u32,
                cbSize: self.length,
                ..Default::default()
            };
        }
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}
