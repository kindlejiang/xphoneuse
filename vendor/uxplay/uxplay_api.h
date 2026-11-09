/*
 * uxplay_api.h - C ABI for the "XPhoneUse" fork of UxPlay.
 *
 * This fork builds uxplay as a shared library (uxplay.dll / libuxplay.so)
 * with GStreamer, audio, and local rendering removed. Mirror video is
 * delivered as decrypted Annex-B NAL data through the callbacks below.
 *
 * The library is built with MinGW-64 (MSYS2); the host application loads it
 * dynamically. All callbacks run on uxplay's internal (raop) threads and
 * must return quickly - copy any data you need to keep.
 *
 * License: GPLv3 (same as UxPlay; see LICENSE).
 */
#ifndef UXPLAY_API_H
#define UXPLAY_API_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32)
#  if defined(UXPLAY_BUILD_DLL)
#    define UXPLAY_API __declspec(dllexport)
#  else
#    define UXPLAY_API __declspec(dllimport)
#  endif
#else
#  define UXPLAY_API __attribute__((visibility("default")))
#endif

typedef enum uxplay_video_codec {
    UXPLAY_CODEC_H264 = 0,
    UXPLAY_CODEC_H265 = 1,
} uxplay_video_codec;

typedef struct uxplay_callbacks {
    /* Opaque pointer passed back as the first argument of every callback. */
    void *user_data;

    /*
     * Decrypted video access unit (one or more Annex-B NALs, each preceded
     * by a 00 00 00 01 start code; short SPS/PPS/SEI NALs may precede a VCL
     * NAL). ntp_time_ns is the stream clock (already offset-adjusted to the
     * local clock) at the time the frame was captured. The buffer is only
     * valid for the duration of the call.
     */
    void (*on_video_data)(void *user_data, const unsigned char *data,
                          int data_len, int nal_count, uint64_t ntp_time_ns);

    /* Video stream was flushed (new client / reset). Pending frames are stale. */
    void (*on_video_flush)(void *user_data);

    /* Source resolution reported by the client (before the first frame). */
    void (*on_video_size)(void *user_data, float width, float height);

    /* Codec selected for the mirror stream. */
    void (*on_video_codec)(void *user_data, uxplay_video_codec codec);

    /* A client connected / started mirroring. client_ip is the client's
     * source address ("1.2.3.4" or "[::1]%zone"), or "" if unknown. */
    void (*on_connected)(void *user_data, const char *device_id,
                         const char *client_name, const char *client_ip);

    /* All clients disconnected (mirroring ended). */
    void (*on_disconnected)(void *user_data);

    /* log: 0=error, 1=info, 2=debug */
    void (*on_log)(void *user_data, int level, const char *message);
} uxplay_callbacks;

typedef struct uxplay_config {
    const char *server_name;   /* AirPlay receiver name shown on the iPhone */
    int max_fps;               /* 0 = default (60) */
    int nohold;                /* nonzero: allow a new client to take over */
    const char *keyfile;       /* persistent pairing key file; NULL = default */
} uxplay_config;

/* Start the AirPlay mirroring server on a background thread.
 * Returns 0 on success, nonzero on failure. */
UXPLAY_API int uxplay_start(const uxplay_config *config,
                            const uxplay_callbacks *callbacks);

/* Stop the server and wait for the background thread to exit.
 * Safe to call from any thread; no-op if not running. */
UXPLAY_API void uxplay_stop(void);

/* 1 while the server is running (useful for exe-style builds). */
UXPLAY_API int uxplay_is_running(void);

#ifdef __cplusplus
}
#endif

#endif /* UXPLAY_API_H */
