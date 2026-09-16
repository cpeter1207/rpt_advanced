/** @file
 * @brief ABI 1 for offline file and speech preparation at decoded source rate.
 *
 * No function may be called by an audio worker. The host keeps the adapter and
 * callback code loaded until all calls and handles are finished; replacements
 * activate only at controlled restart or module reload. Contexts allow concurrent
 * requests, each with independent child ownership. Destroy follows quiescence.
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
#define RPTADV_MEDIA_ABI_VERSION 1U

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
    uint32_t struct_size;            /**< Complete structure size. */
    uint32_t abi_version;            /**< Required ABI version. */
    const char *executable;          /**< Selected executable name/path; never a shell command. */
    const char *temporary_directory; /**< Existing service-owned directory. */
    uint32_t timeout_ms;             /**< Nonzero budget for each child, normally 30000. */
    void (*reaper_acquire)(void);    /**< Optional host exclusion before spawn. */
    void (*reaper_release)(void);    /**< Paired restoration after reap/failure. */
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

/** @brief Immutable mono source-rate F32 output, owned by this adapter. */
struct rptadv_media_audio {
    void *handle;            /**< Release exactly once through the originating descriptor. */
    const float *samples;    /**< View valid until handle release. */
    size_t sample_count;     /**< Nonzero count of mono samples. */
    uint32_t sample_rate_hz; /**< Source rate; telemetry ring owns conversion. */
};

#ifdef __cplusplus
}
#endif
#endif
