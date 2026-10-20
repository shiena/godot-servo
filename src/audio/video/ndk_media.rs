//! The parts of the NDK's media library the Android `<video>` decoder calls,
//! opened at run time.
//!
//! `AMediaDataSource`, which lets the extractor read the pushed bytes, arrived
//! in API 28, while Godot builds for API 24. Linked directly, an older device
//! would refuse to load the whole extension; opened here, it only goes
//! without video.

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

pub type MediaStatus = c_int;

pub const AMEDIA_OK: MediaStatus = 0;
pub const AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
pub const AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC: c_int = 0;

#[repr(C)]
#[derive(Default)]
pub struct AMediaCodecBufferInfo {
    pub offset: i32,
    pub size: i32,
    pub presentation_time_us: i64,
    pub flags: u32,
}

pub type ReadAt = unsafe extern "C" fn(*mut c_void, i64, *mut c_void, usize) -> isize;
pub type GetSize = unsafe extern "C" fn(*mut c_void) -> isize;
pub type Close = unsafe extern "C" fn(*mut c_void);

libraries! {
    struct Media;

    "libmediandk.so" {
        fn AMediaDataSource_new() -> *mut c_void;
        fn AMediaDataSource_delete(*mut c_void);
        fn AMediaDataSource_setUserdata(*mut c_void, *mut c_void);
        fn AMediaDataSource_setReadAt(*mut c_void, ReadAt);
        fn AMediaDataSource_setGetSize(*mut c_void, GetSize);
        fn AMediaDataSource_setClose(*mut c_void, Close);

        fn AMediaExtractor_new() -> *mut c_void;
        fn AMediaExtractor_delete(*mut c_void) -> MediaStatus;
        fn AMediaExtractor_setDataSourceCustom(*mut c_void, *mut c_void) -> MediaStatus;
        fn AMediaExtractor_getTrackCount(*mut c_void) -> usize;
        fn AMediaExtractor_getTrackFormat(*mut c_void, usize) -> *mut c_void;
        fn AMediaExtractor_selectTrack(*mut c_void, usize) -> MediaStatus;
        fn AMediaExtractor_readSampleData(*mut c_void, *mut u8, usize) -> isize;
        fn AMediaExtractor_getSampleTime(*mut c_void) -> i64;
        fn AMediaExtractor_advance(*mut c_void) -> bool;
        fn AMediaExtractor_seekTo(*mut c_void, i64, c_int) -> MediaStatus;

        fn AMediaFormat_delete(*mut c_void) -> MediaStatus;
        fn AMediaFormat_getString(*mut c_void, *const c_char, *mut *const c_char) -> bool;
        fn AMediaFormat_getInt32(*mut c_void, *const c_char, *mut i32) -> bool;
        fn AMediaFormat_getInt64(*mut c_void, *const c_char, *mut i64) -> bool;
        fn AMediaFormat_getBuffer(*mut c_void, *const c_char, *mut *mut c_void, *mut usize) -> bool;
        fn AMediaFormat_setInt32(*mut c_void, *const c_char, i32);
        fn AMediaFormat_getRect(*mut c_void, *const c_char, *mut i32, *mut i32, *mut i32, *mut i32)
            -> bool;

        fn AMediaCodec_createDecoderByType(*const c_char) -> *mut c_void;
        fn AMediaCodec_delete(*mut c_void) -> MediaStatus;
        fn AMediaCodec_configure(*mut c_void, *const c_void, *mut c_void, *mut c_void, u32)
            -> MediaStatus;
        fn AMediaCodec_start(*mut c_void) -> MediaStatus;
        fn AMediaCodec_stop(*mut c_void) -> MediaStatus;
        fn AMediaCodec_flush(*mut c_void) -> MediaStatus;
        fn AMediaCodec_dequeueInputBuffer(*mut c_void, i64) -> isize;
        fn AMediaCodec_getInputBuffer(*mut c_void, usize, *mut usize) -> *mut u8;
        // `off_t offset` is 64 bits on the 64-bit ABIs the extension ships for.
        fn AMediaCodec_queueInputBuffer(*mut c_void, usize, i64, usize, u64, u32) -> MediaStatus;
        fn AMediaCodec_dequeueOutputBuffer(*mut c_void, *mut AMediaCodecBufferInfo, i64) -> isize;
        fn AMediaCodec_getOutputBuffer(*mut c_void, usize, *mut usize) -> *mut u8;
        fn AMediaCodec_getOutputFormat(*mut c_void) -> *mut c_void;
        fn AMediaCodec_releaseOutputBuffer(*mut c_void, usize, bool) -> MediaStatus;

    }
}

#[cfg(not(target_pointer_width = "64"))]
compile_error!("AMediaCodec_queueInputBuffer is declared for a 64-bit off_t");

/// The NDK's media library, or `None` before API 28.
pub fn get() -> Option<&'static Media> {
    static MEDIA: OnceLock<Option<Media>> = OnceLock::new();
    MEDIA
        .get_or_init(|| match Media::load() {
            Ok(media) => Some(media),
            Err(error) => {
                log::info!("godot-servo: no NDK media decoding, so no video: {error}");
                None
            }
        })
        .as_ref()
}
