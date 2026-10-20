//! The parts of GStreamer the Linux `<video>` decoder calls, opened at run
//! time.
//!
//! The extension does not link GStreamer: a system without it still loads the
//! extension and only goes without video. The libraries are opened once and
//! kept for the life of the process, since GStreamer's own threads run their
//! code.
//!
//! The structs here are GStreamer 1.x's public ones, whose layout is part of
//! its stable ABI.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CStr};
use std::sync::OnceLock;

use libloading::Library;

pub type Gboolean = c_int;

pub const GST_STATE_NULL: c_int = 1;
pub const GST_STATE_PAUSED: c_int = 3;
pub const GST_STATE_PLAYING: c_int = 4;
pub const GST_STATE_CHANGE_FAILURE: c_int = 0;
pub const GST_STATE_CHANGE_ASYNC: c_int = 2;
pub const GST_FORMAT_TIME: c_int = 3;
pub const GST_SEEK_FLAG_FLUSH: c_int = 1 << 0;
pub const GST_SEEK_FLAG_ACCURATE: c_int = 1 << 1;
pub const GST_MESSAGE_ERROR: c_uint = 1 << 1;
pub const GST_MAP_READ: c_int = 1 << 0;
pub const GST_PAD_SINK: c_int = 2;
pub const GST_RANK_MARGINAL: c_int = 64;
pub const GST_ELEMENT_FACTORY_TYPE_DECODER: u64 = 1 << 0;
pub const GST_ELEMENT_FACTORY_TYPE_DEMUXER: u64 = 1 << 5;
pub const GST_CLOCK_TIME_NONE: u64 = u64::MAX;
pub const GST_SECOND: f64 = 1e9;

#[repr(C)]
pub struct GstMiniObject {
    pub type_: usize,
    pub refcount: c_int,
    pub lockstate: c_int,
    pub flags: c_uint,
    pub copy: *mut c_void,
    pub dispose: *mut c_void,
    pub free: *mut c_void,
    pub priv_uint: c_uint,
    pub priv_pointer: *mut c_void,
}

#[repr(C)]
pub struct GstBuffer {
    pub mini_object: GstMiniObject,
    pub pool: *mut c_void,
    pub pts: u64,
    pub dts: u64,
    pub duration: u64,
    pub offset: u64,
    pub offset_end: u64,
}

#[repr(C)]
pub struct GstMapInfo {
    pub memory: *mut c_void,
    pub flags: c_int,
    pub data: *mut u8,
    pub size: usize,
    pub maxsize: usize,
    pub user_data: [*mut c_void; 4],
    pub _gst_reserved: [*mut c_void; 4],
}

#[repr(C)]
pub struct GstAppSrcCallbacks {
    pub need_data: Option<unsafe extern "C" fn(*mut c_void, c_uint, *mut c_void)>,
    pub enough_data: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub seek_data: Option<unsafe extern "C" fn(*mut c_void, u64, *mut c_void) -> Gboolean>,
    pub _gst_reserved: [*mut c_void; 4],
}

#[repr(C)]
pub struct GError {
    pub domain: u32,
    pub code: c_int,
    pub message: *mut c_char,
}

#[repr(C)]
pub struct GList {
    pub data: *mut c_void,
    pub next: *mut GList,
    pub prev: *mut GList,
}

macro_rules! libraries {
    ($($file:literal { $(fn $name:ident($($arg:ty),* $(,)?) $(-> $ret:ty)?;)* })*) => {
        pub struct Gst {
            $($(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*)*
            _libraries: Vec<Library>,
        }

        impl Gst {
            fn load() -> Result<Self, String> {
                let mut libraries = Vec::new();
                $(
                    // SAFETY: these are GStreamer's and GLib's own libraries,
                    // whose initialisers are safe to run in any process.
                    let library = unsafe { Library::new($file) }
                        .map_err(|error| format!("{}: {error}", $file))?;
                    $(
                        // SAFETY: the signature is the one in GStreamer 1.x's
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

libraries! {
    "libglib-2.0.so.0" {
        fn g_error_free(*mut GError);
        fn g_free(*mut c_void);
    }
    "libgobject-2.0.so.0" {
        fn g_signal_connect_data(
            *mut c_void,
            *const c_char,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            c_int,
        ) -> c_ulong;
    }
    "libgstreamer-1.0.so.0" {
        fn gst_init_check(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> Gboolean;
        fn gst_pipeline_new(*const c_char) -> *mut c_void;
        fn gst_element_factory_make(*const c_char, *const c_char) -> *mut c_void;
        fn gst_bin_add(*mut c_void, *mut c_void) -> Gboolean;
        fn gst_element_link(*mut c_void, *mut c_void) -> Gboolean;
        fn gst_element_set_state(*mut c_void, c_int) -> c_int;
        fn gst_element_get_state(*mut c_void, *mut c_int, *mut c_int, u64) -> c_int;
        fn gst_element_sync_state_with_parent(*mut c_void) -> Gboolean;
        fn gst_element_get_static_pad(*mut c_void, *const c_char) -> *mut c_void;
        fn gst_element_query_duration(*mut c_void, c_int, *mut i64) -> Gboolean;
        fn gst_element_seek_simple(*mut c_void, c_int, c_int, i64) -> Gboolean;
        fn gst_element_get_bus(*mut c_void) -> *mut c_void;
        fn gst_pad_link(*mut c_void, *mut c_void) -> c_int;
        fn gst_pad_get_current_caps(*mut c_void) -> *mut c_void;
        fn gst_caps_from_string(*const c_char) -> *mut c_void;
        fn gst_caps_get_structure(*const c_void, c_uint) -> *const c_void;
        fn gst_structure_get_name(*const c_void) -> *const c_char;
        fn gst_structure_get_int(*const c_void, *const c_char, *mut c_int) -> Gboolean;
        fn gst_mini_object_unref(*mut c_void);
        fn gst_object_unref(*mut c_void);
        fn gst_bus_pop_filtered(*mut c_void, c_uint) -> *mut c_void;
        fn gst_message_parse_error(*mut c_void, *mut *mut GError, *mut *mut c_char);
        fn gst_sample_get_buffer(*mut c_void) -> *mut GstBuffer;
        fn gst_sample_get_caps(*mut c_void) -> *mut c_void;
        fn gst_sample_get_segment(*mut c_void) -> *const c_void;
        fn gst_segment_to_stream_time(*const c_void, c_int, u64) -> u64;
        fn gst_buffer_new_allocate(*mut c_void, usize, *mut c_void) -> *mut GstBuffer;
        fn gst_buffer_fill(*mut GstBuffer, usize, *const c_void, usize) -> usize;
        fn gst_buffer_map(*mut GstBuffer, *mut GstMapInfo, c_int) -> Gboolean;
        fn gst_buffer_unmap(*mut GstBuffer, *mut GstMapInfo);
        fn gst_util_set_object_arg(*mut c_void, *const c_char, *const c_char);
        fn gst_element_factory_list_get_elements(u64, c_int) -> *mut GList;
        fn gst_element_factory_list_filter(*mut GList, *const c_void, c_int, Gboolean) -> *mut GList;
        fn gst_plugin_feature_list_free(*mut GList);
    }
    "libgstapp-1.0.so.0" {
        fn gst_app_src_set_callbacks(
            *mut c_void,
            *mut GstAppSrcCallbacks,
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void)>,
        );
        fn gst_app_src_push_buffer(*mut c_void, *mut GstBuffer) -> c_int;
        fn gst_app_src_end_of_stream(*mut c_void) -> c_int;
        fn gst_app_sink_try_pull_sample(*mut c_void, u64) -> *mut c_void;
        fn gst_app_sink_is_eos(*mut c_void) -> Gboolean;
    }
}

/// GStreamer, opened and initialised; `None` when the system has none.
pub fn get() -> Option<&'static Gst> {
    static GST: OnceLock<Option<Gst>> = OnceLock::new();
    GST.get_or_init(|| {
        let gst = match Gst::load() {
            Ok(gst) => gst,
            Err(error) => {
                log::info!("godot-servo: no GStreamer, so no video: {error}");
                return None;
            }
        };
        let mut error = std::ptr::null_mut();
        // SAFETY: GStreamer allows initialising from any thread, and with no
        // arguments to parse.
        let ok =
            unsafe { (gst.gst_init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) };
        if ok == 0 {
            // SAFETY: on failure `error` is a GError GStreamer handed over.
            let message = unsafe { take_error(&gst, error) };
            log::warn!("godot-servo: GStreamer failed to start: {message}");
            return None;
        }
        Some(gst)
    })
    .as_ref()
}

/// The message of a GError, which is freed.
///
/// # Safety
///
/// `error` is null or a GError the caller owns.
pub unsafe fn take_error(gst: &Gst, error: *mut GError) -> String {
    if error.is_null() {
        return "unknown error".to_owned();
    }
    let message = text((*error).message);
    (gst.g_error_free)(error);
    message
}

/// A C string as a Rust one; empty for null.
///
/// # Safety
///
/// `text` is null or a NUL-terminated string.
pub unsafe fn text(text: *const c_char) -> String {
    if text.is_null() {
        String::new()
    } else {
        CStr::from_ptr(text).to_string_lossy().into_owned()
    }
}
