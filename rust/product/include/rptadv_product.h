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
#define RPTADV_PRODUCT_ABI_VERSION 4U
/** Product capability name stored in its fixed-width descriptor field. */
#define RPTADV_PRODUCT_CAPABILITY "rptadv.prod4"
/** Exact incompatible host-services ABI revision. */
#define RPTADV_HOST_ABI_VERSION 5U
/** Host-services capability name stored in its fixed-width descriptor field. */
#define RPTADV_HOST_CAPABILITY "rptadv.hst5"
/** Peer event carrying borrowed text bytes. */
#define RPTADV_PEER_EVENT_TEXT 1U
/** Peer event carrying one DTMF digit byte. */
#define RPTADV_PEER_EVENT_DIGIT 2U
/** Peer event carrying normalized native-rate F32 PCM. */
#define RPTADV_PEER_EVENT_AUDIO 3U
/** Peer event asserting remote receiver activity; no payload. */
#define RPTADV_PEER_EVENT_RADIO_KEY 4U
/** Peer event clearing remote receiver activity; no payload. */
#define RPTADV_PEER_EVENT_RADIO_UNKEY 5U

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
typedef int (*rptadv_radio_transmit_v3)(void *context, float *samples, uint32_t sample_count,
                                        uint32_t *keyed, uint32_t *ctcss_enabled);
/** Direct RadioPlusAdvanced option identifier, validated before ast_call. */
#define URP_AST_OPTION_DIRECT_CALLBACKS 0x52504144
/** Exact direct attachment version, independent of the host-services table. */
#define URP_AST_DIRECT_CALLBACKS_ABI_VERSION 3U
/** Mutable direct registration; the provider copies callbacks before returning.
 * Initialize accepted_abi_version to zero. Start only when setoption returns zero
 * and acknowledges this exact ABI. Callback code and contexts remain borrowed
 * until synchronous channel hangup, including a failed attachment/start. */
struct urp_ast_direct_callbacks {
    uint32_t struct_size;              /**< Exact readable/writable descriptor size. */
    uint32_t abi_version;              /**< Exact direct attachment revision. */
    void *receive_context;             /**< Stable input owner context. */
    rptadv_radio_receive_v2 receive;   /**< Input endpoint. */
    void *transmit_context;            /**< Stable output owner context. */
    rptadv_radio_transmit_v3 transmit; /**< Output endpoint: key and CTCSS enable. */
    uint32_t accepted_abi_version; /**< Provider writes the ABI only after retaining callbacks. */
};
/** Synchronous RadioPlusAdvanced link-graph attachment; use block=0. */
#define URP_AST_OPTION_LINK_ATTACH 0x52504C41
/** Exact link attachment option revision. */
#define URP_AST_LINK_ATTACH_ABI_VERSION 1U
/** Bind a peer to this radio's configured link profile before reading media.
 * The peer channel is borrowed for this call. Its datastore owns the installed
 * hook through hangup and USBRadioPlus reload. Require zero result and exact
 * acknowledgment; unknown-option success is not acceptance. */
struct urp_ast_link_attach {
    uint32_t struct_size;          /**< Exact readable/writable descriptor size. */
    uint32_t abi_version;          /**< Exact link attachment revision. */
    void *peer_channel;            /**< Borrowed public Asterisk channel pointer. */
    uint32_t accepted_abi_version; /**< Initialize to zero; require ABI 1 on success. */
};
/** Current-generation predicate used during one bounded outbound dial. */
typedef uint32_t (*rptadv_current_v1)(void *context);
/** One borrowed peer input event identified by the RPTADV_PEER_EVENT_* constants. */
typedef void (*rptadv_peer_event_v1)(void *context, uint32_t kind, const void *data, size_t count);
/** Borrowed status text sink. */
typedef void (*rptadv_text_sink_v1)(void *context, const char *text, size_t length);
struct UrpNativeStationConfig;
/** Resolved host requirements, borrowed only during the configuration visitor. */
struct rptadv_node_host_configuration {
    uint32_t struct_size;           /**< Complete readable record size. */
    const char *node;               /**< Node identity; not NUL terminated. */
    size_t node_length;             /**< Node identity byte count. */
    const char *channel;            /**< Radio channel; not NUL terminated. */
    size_t channel_length;          /**< Channel byte count. */
    uint32_t enabled;               /**< Zero for disabled nodes, which are still reported. */
    uint16_t iax_port;              /**< Effective UDP listener and registration port. */
    const char *registration_url;   /**< HTTPS endpoint; empty disables registration. */
    size_t registration_url_length; /**< Endpoint byte count. */
    uint64_t registration_interval_seconds;     /**< Effective registration interval. */
    const struct UrpNativeStationConfig *radio; /**< Borrowed native station request. */
};
/** Copy one node's requirements; return nonzero to abort inspection. */
typedef int (*rptadv_configuration_sink)(void *context,
                                         const struct rptadv_node_host_configuration *node);
/** Copy one validated secret; node is "general" for the shared default.
 * No secret or input text is included in diagnostics. Return nonzero to abort. */
typedef int (*rptadv_secret_sink)(void *context, const char *node, size_t node_length,
                                  const char *secret, size_t secret_length);
/** Borrow one selected DNS SRV target and port for the synchronous backend call. */
typedef void (*rptadv_directory_srv_sink_v1)(void *context, const char *host, size_t length,
                                             uint16_t port);

/** Host I/O primitives consumed by the portable product owner.
 * The immutable table, context and callback code remain valid through successful
 * product stop. The context supports concurrent calls. Each non-null radio/peer
 * handle is uniquely owned and used serially until its destroy callback. Open/dial
 * return zero and one new handle, or nonzero and no handle. Ready returns one when
 * readable, zero on timeout, and negative on failure. Other operations return zero
 * on success and nonzero on failure. Provider callbacks never unwind.
 *
 * Borrowed strings/slices and product callbacks are valid only for the synchronous
 * call, except radio endpoints retained from activate through destroy. PCM is aligned normalized
 * F32 with exactly the stated count. Radio transmit returns key and CTCSS-enable
 * decisions through separate output pointers. Peer text/audio data and DTMF byte
 * are borrowed for the callback; radio key/unkey events have null data and zero
 * count. Directory backends return zero after delivering borrowed results, negative
 * on authoritative rejection, or positive on recoverable DNS failure. Record and
 * SRV callbacks emit at most one result; no result means absent. Address callbacks
 * emit numeric IPs in resolver order. A standalone empty DNS answer is recoverable;
 * Asterisk's empty answer succeeds with no results, preserving its existing diagnosis.
 */
struct rptadv_host_services_v5 {
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

    /** Read a raw extnodes record. Unavailable files emit no result and succeed. */
    int (*directory_record)(void *context, const char *path, size_t path_length, const char *node,
                            size_t node_length, rptadv_text_sink_v1 sink, void *sink_context);
    /** Read one backend-selected SRV target; an absent record uses the ordinary hostname. */
    int (*directory_srv)(void *context, const char *service, size_t service_length,
                         rptadv_directory_srv_sink_v1 sink, void *sink_context);
    /** Resolve numeric addresses for the selected host and port. */
    int (*directory_addresses)(void *context, const char *host, size_t host_length, uint16_t port,
                               rptadv_text_sink_v1 sink, void *sink_context);
    /** Preserve host diagnostics: 1 invalid record, 2 source mismatch, 3 not found, 4 DNS failed.
     */
    void (*directory_notice)(void *context, uint32_t reason);

    /** Reserve one uniquely owned radio without starting it. The length-delimited name contains
     * local-node, NUL, then adapter-selected channel; older single-name test providers may use
     * the same value for both fields. */
    int (*radio_open)(void *context, const char *name, size_t name_length, size_t maximum_frames,
                      void **radio);
    /** Attach both endpoints before starting. On failure detach/quiesce both endpoints
     * before return, retaining a safely destroyable reservation. Endpoints are serial
     * individually and may run concurrently with each other, never with destroy. */
    int (*radio_activate)(void *context, void *radio, rptadv_radio_receive_v2 receive,
                          void *receive_context, rptadv_radio_transmit_v3 transmit,
                          void *transmit_context);
    /** Synchronously stop/hang up and detach both callbacks before returning. */
    void (*radio_destroy)(void *context, void *radio);

    /** Dial and return one uniquely owned peer. */
    int (*peer_dial)(void *context, const char *destination, size_t destination_length,
                     const char *local, size_t local_length, size_t maximum_frames,
                     rptadv_current_v1 current, void *current_context, void **peer);
    /** Bind one exclusively owned peer to its live radio's link processing before
     * any peer media operation. Control-only; neither handle transfers ownership.
     * Nonzero rejects admission, and the caller destroys the peer. */
    int (*peer_bind_radio)(void *context, void *peer, void *radio);
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
    int (*start)(const struct rptadv_host_services_v5 *host,
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
    /** Resolve host requirements without starting workers or opening hardware.
     * Validate the whole document before emitting records. Diagnostics are warnings
     * on success and a safe error on failure. Null node sink validates only. */
    int (*inspect_configuration)(const char *configuration, size_t configuration_length,
                                 rptadv_configuration_sink node, void *node_context,
                                 rptadv_text_sink_v1 diagnostic, void *diagnostic_context);
    /** Parse secrets without file I/O; the host still enforces ownership/mode 0600.
     * Validate the whole document before emitting records. No callback may unwind. */
    int (*inspect_secrets)(const char *configuration, size_t configuration_length,
                           rptadv_secret_sink secret, void *secret_context,
                           rptadv_text_sink_v1 diagnostic, void *diagnostic_context);
};

/** Return immutable process-lifetime product metadata.
 * @return Complete product descriptor retained through successful stop.
 */
const struct rptadv_product_descriptor_v1 *rptadv_product_descriptor_v1(void);

#ifdef __cplusplus
}
#endif
#endif
