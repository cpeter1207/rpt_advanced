/* SPDX-License-Identifier: GPL-2.0-only */
/** @file rptadv_product.h
 * @brief Portable product runtime and host-services ABI.
 */
#ifndef RPTADV_PRODUCT_H
#define RPTADV_PRODUCT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct rptadv_control_descriptor_v1;
struct rptadv_file_descriptor;
struct rptadv_speech_descriptor;

/** Exact incompatible product ABI revision. */
#define RPTADV_PRODUCT_ABI_VERSION 2U
/** Product capability name stored in its fixed-width descriptor field. */
#define RPTADV_PRODUCT_CAPABILITY "rptadv.prod2"
/** Exact incompatible host-services ABI revision. */
#define RPTADV_HOST_ABI_VERSION 2U
/** Host-services capability name stored in its fixed-width descriptor field. */
#define RPTADV_HOST_CAPABILITY "rptadv.hst2"

/** Copied local civil time. Valid is zero when conversion failed. */
struct rptadv_local_time_v1 {
    uint32_t struct_size; /**< Complete result size. */
    uint32_t valid;       /**< Nonzero when all calendar fields are valid. */
    uint16_t year;        /**< Gregorian year. */
    uint8_t month;        /**< Month from 1 through 12. */
    uint8_t day;          /**< Day of month. */
    uint8_t weekday;      /**< Sunday-based weekday from 0 through 6. */
    uint8_t hour;         /**< Local hour from 0 through 23. */
    uint8_t minute;       /**< Minute from 0 through 59. */
    uint8_t second;       /**< Second from 0 through 59. */
};

/** Serial input endpoint; inactive/gated generations succeed with silence. */
typedef int (*rptadv_radio_receive_v2)(void *context, uint32_t receiving, float *samples,
                                      uint32_t sample_count);
/** Serial output endpoint; inactive/gated generations succeed with silence/unkeyed.
 * Malformed arguments or processing failure return nonzero with silent output. */
typedef int (*rptadv_radio_transmit_v2)(void *context, float *samples, uint32_t sample_count,
                                       uint32_t *keyed);
/** Direct RadioPlusAdvanced option identifier, validated before ast_call. */
#define URP_AST_OPTION_DIRECT_CALLBACKS 0x52504144
/** Exact direct attachment version, independent of the host-services table. */
#define URP_AST_DIRECT_CALLBACKS_ABI_VERSION 1U
/** Copied callback registration retained until synchronous channel hangup. */
struct urp_ast_direct_callbacks {
    uint32_t struct_size; /**< Exact readable descriptor size. */
    uint32_t abi_version; /**< Exact direct attachment revision. */
    void *receive_context; /**< Stable input owner context. */
    rptadv_radio_receive_v2 receive; /**< Input endpoint. */
    void *transmit_context; /**< Stable output owner context. */
    rptadv_radio_transmit_v2 transmit; /**< Output endpoint. */
};
/** Current-generation predicate used during one bounded outbound dial. */
typedef uint32_t (*rptadv_current_v1)(void *context);
/** One borrowed peer input event: 1 text, 2 digit, 3 native F32 audio. */
typedef void (*rptadv_peer_event_v1)(void *context, uint32_t kind, const void *data, size_t count);
/** Borrowed status text sink. */
typedef void (*rptadv_text_sink_v1)(void *context, const char *text, size_t length);

/** Public-Asterisk primitives consumed by the portable product owner.
 * The immutable table, context and callback code remain valid through successful
 * product stop. The context supports concurrent calls. Each non-null radio/peer
 * handle is uniquely owned and used serially until its destroy callback. Open/dial
 * return zero and one new handle, or nonzero and no handle. Ready returns one when
 * readable, zero on timeout, and negative on failure. Other operations return zero
 * on success and nonzero on failure. Provider callbacks never unwind.
 *
 * Borrowed strings/slices and product callbacks are valid only for the synchronous
 * call, except radio endpoints retained from activate through destroy. PCM is aligned normalized F32 with exactly the stated
 * count. Radio transmit returns its key decision through the output pointer. Peer events are 1 text bytes, 2 one
 * DTMF byte, or 3 normalized F32 PCM. Directory methods are 0 both, 1 DNS, 2 file.
 */
struct rptadv_host_services_v2 {
    uint32_t struct_size;   /**< Complete readable table size. */
    uint32_t abi_version;   /**< Exact RPTADV_HOST_ABI_VERSION. */
    uint8_t capability[12]; /**< Exact NUL-padded RPTADV_HOST_CAPABILITY. */
    void *context;          /**< Shared provider context. */

    /** Convert one Unix second to local civil time. */
    int (*local_time)(void *context, int64_t unix_seconds, struct rptadv_local_time_v1 *result);
    /** Publish command completion through the selected host logger. */
    void (*command_notice)(void *context, const char *local, size_t local_length,
                           uint32_t completed);
    /** Suspend the host child reaper. */
    void (*reaper_acquire)(void);
    /** Restore the host child reaper. */
    void (*reaper_release)(void);

    /** Resolve one peer destination into caller storage. */
    int (*directory_lookup)(void *context, uint32_t method, const char *static_file,
                            size_t static_length, const char *external_file, size_t external_length,
                            const char *remote, size_t remote_length, const char *source,
                            size_t source_length, char *output, size_t capacity, size_t *written);

    /** Reserve one uniquely owned radio without starting it. */
    int (*radio_open)(void *context, const char *name, size_t name_length, size_t maximum_frames,
                      void **radio);
    /** Attach both endpoints before starting. On failure detach/quiesce both endpoints
     * before return, retaining a safely destroyable reservation. Endpoints are serial
     * individually and may run concurrently with each other, never with destroy. */
    int (*radio_activate)(void *context, void *radio, rptadv_radio_receive_v2 receive,
                          void *receive_context, rptadv_radio_transmit_v2 transmit,
                          void *transmit_context);
    /** Synchronously stop/hang up and detach both callbacks before returning. */
    void (*radio_destroy)(void *context, void *radio);

    /** Dial and return one uniquely owned peer. */
    int (*peer_dial)(void *context, const char *destination, size_t destination_length,
                     const char *local, size_t local_length, size_t maximum_frames,
                     rptadv_current_v1 current, void *current_context, void **peer);
    /** Return the peer's negotiated linear PCM rate. */
    uint32_t (*peer_rate)(void *context, const void *peer);
    /** Wait briefly for peer input. */
    int (*peer_ready)(void *context, void *peer);
    /** Read and synchronously dispatch one peer event. */
    int (*peer_read)(void *context, void *peer, rptadv_peer_event_v1 event, void *event_context);
    /** Send one text payload. */
    int (*peer_send_text)(void *context, void *peer, const char *text, size_t length);
    /** Send one completed DTMF digit. */
    int (*peer_send_digit)(void *context, void *peer, uint8_t digit);
    /** Send one normalized F32 PCM block, or one end-of-burst marker when empty. */
    int (*peer_write)(void *context, void *peer, const float *samples, size_t sample_count);
    /** Destroy one uniquely owned peer. */
    void (*peer_destroy)(void *context, void *peer);
};

/** Portable runtime entry points.
 * Strings and sinks are borrowed only for each synchronous call. Authorization
 * returns zero when current policy permits handoff. Incoming returns
 * zero after accepting/consuming the peer, minus one after rejecting/consuming it,
 * or one when rejecting without consuming it. Link actions are 1 transceive,
 * 2 monitor, 3 local-monitor, and 4 disconnect. Digit completion is zero or one.
 * No callback unwinds, and descriptor code remains loaded through successful stop.
 */
struct rptadv_product_descriptor_v1 {
    uint32_t struct_size;   /**< Complete readable table size. */
    uint32_t abi_version;   /**< Exact RPTADV_PRODUCT_ABI_VERSION. */
    uint8_t capability[16]; /**< Exact NUL-padded RPTADV_PRODUCT_CAPABILITY. */
    /** Start one product owner from copied configuration. */
    int (*start)(const struct rptadv_host_services_v2 *host,
                 const struct rptadv_control_descriptor_v1 *control,
                 const struct rptadv_file_descriptor *file,
                 const struct rptadv_speech_descriptor *speech, const char *configuration,
                 size_t configuration_length);
    int (*reload)(const char *configuration,
                  size_t configuration_length); /**< Replace configuration. */
    int (*stop)(void);                          /**< Stop and quiesce every product owner. */
    /** Check current incoming policy before answering a channel. */
    int (*authorize_incoming)(const char *local, size_t local_length, const char *remote,
                              size_t remote_length, const char *source, size_t source_length);
    /** Admit one transferred peer. */
    int (*incoming)(const char *local, size_t local_length, const char *remote,
                    size_t remote_length, const char *source, size_t source_length, void *peer);
    /** Apply one stable link action. */
    int (*link_command)(const char *local, size_t local_length, const char *remote,
                        size_t remote_length, uint32_t action);
    /** Emit one copied link-status snapshot. */
    int (*link_status)(const char *local, size_t local_length, rptadv_text_sink_v1 sink,
                       void *sink_context);
    /** Feed one validated DTMF digit. */
    int (*digit)(const char *local, size_t local_length, uint8_t digit, uint32_t *completed);
};

/** Return immutable process-lifetime product metadata.
 * @return Complete product descriptor retained through successful stop.
 */
const struct rptadv_product_descriptor_v1 *rptadv_product_descriptor_v1(void);

#ifdef __cplusplus
}
#endif
#endif
