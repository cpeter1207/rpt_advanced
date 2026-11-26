/** @file
 * @brief ABI 3 for streaming file and speech PCM at decoded source rate.
 *
 * No function may be called by an audio worker. The host keeps the adapter and
 * callback code loaded until all calls and handles are finished; replacements
 * activate only at controlled restart or module reload. Contexts allow concurrent
 * requests, each with independent child ownership. Destroy follows quiescence.
 * Open and close may block on local I/O and process lifecycle. Step never
 * waits for data or child exit. Transfer a handle only with exclusive sequential
 * ownership; never call step and close concurrently for the same handle.
 * Cancellation observed before spawn leaves no handle. Once setup succeeds,
 * open transfers ownership even if cancellation races that success. The host
 * rechecks cancellation before using the stream and closes every transferred
 * handle exactly once. A cancelled step reports RPTADV_MEDIA_CANCELLED.
 * All input pointers are borrowed for the synchronous call. Strings are terminated;
 * paths use native bytes, text uses UTF-8. No pointer may refer to invalid memory.
 */
#ifndef RPTADV_MEDIA_TYPES_H
#define RPTADV_MEDIA_TYPES_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/** @brief Current incompatible-artifact discriminator. */
#define RPTADV_MEDIA_ABI_VERSION 3U

/** @brief Stable status values returned by preparation and creation. */
enum rptadv_media_status {
    RPTADV_MEDIA_OK = 0,               /**< Operation succeeded. */
    RPTADV_MEDIA_INVALID_REQUEST = -1, /**< Invalid pointers/settings. */
    RPTADV_MEDIA_UNAVAILABLE = -2,     /**< Local source or executable missing. */
    RPTADV_MEDIA_IO = -3,              /**< File/process I/O or contained internal failure. */
    RPTADV_MEDIA_PROCESS_FAILED = -4,  /**< Subprocess exited unsuccessfully. */
    RPTADV_MEDIA_TIMED_OUT = -5,       /**< Per-subprocess deadline expired. */
    RPTADV_MEDIA_CANCELLED = -6,       /**< Explicit request cancellation. */
    RPTADV_MEDIA_INVALID_OUTPUT = -7   /**< Malformed or empty decoded output. */
};

/** @brief Immutable local execution settings copied during create. */
struct rptadv_media_config {
    uint32_t struct_size;         /**< Complete structure size. */
    uint32_t abi_version;         /**< Required ABI version. */
    const char *executable;       /**< Selected executable name/path; never a shell command. */
    uint32_t timeout_ms;          /**< Nonzero budget for each child, normally 30000. */
    void (*reaper_acquire)(void); /**< Optional host exclusion before spawn. */
    void (*reaper_release)(void); /**< Paired restoration after reap/failure. */
};

/** @brief Borrowed request cancellation. Callback must not block or unwind. */
struct rptadv_media_cancellation {
    const void *context;                           /**< Caller-owned callback state. */
    uint32_t (*is_cancelled)(const void *context); /**< Required; nonzero cancels. */
};

/** @brief Literal speech request with no scheduling or fallback policy. */
struct rptadv_media_speech_request {
    const char *text;       /**< Nonempty UTF-8, sent literally on stdin. */
    const char *model;      /**< Existing local model path; never downloaded. */
    uint32_t speed_percent; /**< Inclusive range 1 through 1000. */
    int32_t level_db;       /**< Inclusive range -60 through 0; speech only. */
};

/** @brief Successful events from one nonblocking stream step. */
enum rptadv_media_step_kind {
    RPTADV_MEDIA_STEP_PENDING = 0,      /**< No samples or format ready on this visit. */
    RPTADV_MEDIA_STEP_FORMAT_READY = 1, /**< Once per stream, before any samples. */
    RPTADV_MEDIA_STEP_SAMPLES = 2,      /**< Nonzero count in the caller buffer. */
    RPTADV_MEDIA_STEP_EOF = 3           /**< All samples returned and child succeeded. */
};

/** @brief Tagged step result; fields unused by the event are zero. */
struct rptadv_media_step {
    uint32_t kind;           /**< One of enum rptadv_media_step_kind. */
    uint32_t sample_rate_hz; /**< Decoded source rate for FORMAT_READY only. */
    size_t samples;          /**< Output count for SAMPLES only, within capacity. */
};

#ifdef __cplusplus
}
#endif
#endif
