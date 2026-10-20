//! Decoding for `<video>`, with the platform's own decoders.
//!
//! Each platform's decoder demuxes the file and decodes both its tracks; the
//! player does the rest. Windows uses Media Foundation, Linux the system's
//! GStreamer when there is one, and Android MediaCodec from API 28. macOS has
//! none yet, and a `<video>` there plays its audio alone, as far as symphonia
//! can.

/// A struct of function pointers into C libraries opened at run time, rather
/// than linked: a system that lacks them, or has an older version without some
/// of the functions, still loads the extension and only goes without video.
/// The libraries are kept for as long as the struct, which callers keep for the
/// life of the process, since the libraries' own threads run their code.
#[cfg(any(target_os = "linux", target_os = "android"))]
macro_rules! libraries {
    (
        struct $struct:ident;
        $($file:literal { $(fn $name:ident($($arg:ty),* $(,)?) $(-> $ret:ty)?;)* })*
    ) => {
        #[allow(non_snake_case, reason = "the fields are named after the C functions")]
        pub struct $struct {
            $($(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*)*
            _libraries: Vec<libloading::Library>,
        }

        impl $struct {
            #[allow(non_snake_case, reason = "the locals are named after the C functions")]
            fn load() -> Result<Self, String> {
                let mut libraries = Vec::new();
                $(
                    // SAFETY: these are system libraries whose initialisers
                    // are safe to run in any process.
                    let library = unsafe { libloading::Library::new($file) }
                        .map_err(|error| format!("{}: {error}", $file))?;
                    $(
                        // SAFETY: the signature is the one in the library's C
                        // headers, and `library` is kept for as long as the
                        // pointer is.
                        let $name = unsafe {
                            *library
                                .get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                                    concat!(stringify!($name), "\0").as_bytes(),
                                )
                                .map_err(|error| format!("{}: {error}", stringify!($name)))?
                        };
                    )*
                    libraries.push(library);
                )*
                Ok(Self {
                    $($($name,)*)*
                    _libraries: libraries,
                })
            }
        }
    };
}

#[cfg(target_os = "linux")]
mod gst;
#[cfg(target_os = "linux")]
mod gstreamer;
#[cfg(windows)]
mod media_foundation;
#[cfg(target_os = "android")]
mod mediacodec;
#[cfg(target_os = "android")]
mod ndk_media;

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
    #[cfg(target_os = "linux")]
    {
        gstreamer::GstreamerDecoder::open(reader)
            .map(|decoder| Box::new(decoder) as Box<dyn MediaDecoder>)
    }
    #[cfg(target_os = "android")]
    {
        mediacodec::MediaCodecDecoder::open(reader)
            .map(|decoder| Box::new(decoder) as Box<dyn MediaDecoder>)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
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
    #[cfg(target_os = "linux")]
    {
        gstreamer::can_play_type(media_type)
    }
    #[cfg(target_os = "android")]
    {
        mediacodec::can_play_type(media_type)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
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
