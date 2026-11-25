LOCAL_PATH := $(call my-dir)
GSTREAMER_NDK_BUILD_PATH := $(GSTREAMER_ROOT)/share/gst-android/ndk-build
GSTREAMER_PLUGINS := coreelements app audioconvert audiomixer audioresample audioparsers audiofx autodetect opensles gio id3demux isomp4 matroska ogg opus playback typefindfunctions videoconvertscale videofilter videoparsersbad volume vorbis wavparse flac mpg123 libav vpx theora deinterlace interleave opengl
GSTREAMER_EXTRA_DEPS := gstreamer-app-1.0 gstreamer-audio-1.0 gstreamer-base-1.0 gstreamer-gl-1.0 gstreamer-play-1.0 gstreamer-sdp-1.0 gstreamer-video-1.0 gstreamer-webrtc-1.0 gio-2.0 glib-2.0 gobject-2.0
include $(GSTREAMER_NDK_BUILD_PATH)/gstreamer-1.0.mk
