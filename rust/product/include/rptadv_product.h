/* SPDX-License-Identifier: GPL-2.0-only */
/** @file rptadv_product.h
 * @brief Portable product runtime and host-services ABI.
 */
#ifndef RPTADV_PRODUCT_H
#define RPTADV_PRODUCT_H

#include <rptadv_codec.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct rptadv_control_descriptor_v1;
struct rptadv_file_descriptor;
struct rptadv_speech_descriptor;
struct UrpAstSiteBinding;
struct UrpAstSiteRequest;
struct UrpAstSiteSnapshot;

/** Exact incompatible product ABI revision. */
#define RPTADV_PRODUCT_ABI_VERSION 7U
/** Product capability name stored in its fixed-width descriptor field. */
#define RPTADV_PRODUCT_CAPABILITY "rptadv.prod7"
/** Exact incompatible host-services ABI revision. */
#define RPTADV_HOST_ABI_VERSION 9U
/** Host-services capability name stored in its fixed-width descriptor field. */
#define RPTADV_HOST_CAPABILITY "rptadv.hst9"
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
/** Peer event for counted missing input-rate PCM samples; data is null and
 * count is positive. Only a standalone peer host emits this event. */
#define RPTADV_PEER_EVENT_GAP 6U
/** Compatible copied peer-observation record revision. */
#define RPTADV_PEER_OBSERVATION_ABI_VERSION 1U

/** Immutable protocol namespace, independent of a numeric node alias. */
#define RPTADV_PEER_ALLSTARLINK 1U
/** EchoLink's full real node number, without the operator's leading 3. */
#define RPTADV_PEER_ECHOLINK 2U
/** This peer supports the complete serial-owner group. */
#define RPTADV_PEER_ASYNC 1U
/** This peer supports complete source-timed prepared egress. */
#define RPTADV_PEER_PREPARED 2U
/** This peer can carry human text. */
#define RPTADV_PEER_TEXT 4U
/** This peer supports the existing AllStar remote-command envelopes. */
#define RPTADV_PEER_REMOTE_COMMANDS 8U
/** This peer uses accepted-prefix source reads and explicit finite drainage. */
#define RPTADV_PEER_SOURCE_MEDIA 16U

/** Borrowed immutable bytes. Null is valid only with zero length. */
struct rptadv_peer_bytes_v1 {
    const uint8_t *data; /**< Retained by its peer until destruction. */
    size_t length;       /**< Exact byte count, excluding a terminator. */
};

/** Copy before returning to other work. EchoLink metadata is freshly validated
 * against its owning account/QSO and directory during pending admission. */
struct rptadv_peer_info_v1 {
    uint32_t struct_size;        /**< Complete caller-provided size. */
    uint32_t abi_version;        /**< Exactly 1. */
    uint32_t protocol;           /**< RPTADV_PEER_ALLSTARLINK or ECHOLINK. */
    uint32_t capabilities;       /**< Operations supported by this handle, not the host table. */
    uint64_t account_generation; /**< Echo account generation; zero for channel-backed IAX. */
    uint64_t qso_generation;     /**< Echo QSO generation; zero for channel-backed IAX. */
    struct rptadv_peer_bytes_v1 remote;   /**< Canonical real decimal node ID. */
    struct rptadv_peer_bytes_v1 callsign; /**< Validated Echo identity; may be empty for IAX. */
    struct rptadv_peer_bytes_v1 local;    /**< Owning controller node. */
};

/** Incoming request bound to one protocol, local account and observed endpoint.
 * EchoLink requires a provider-owned pending peer, never a caller-supplied proof flag. */
struct rptadv_incoming_peer_v1 {
    uint32_t struct_size;               /**< Complete readable size. */
    uint32_t abi_version;               /**< Exactly 1. */
    uint32_t protocol;                  /**< RPTADV_PEER_ALLSTARLINK or ECHOLINK. */
    struct rptadv_peer_bytes_v1 local;  /**< Owning controller node. */
    struct rptadv_peer_bytes_v1 remote; /**< Complete real remote ID, not a dial alias. */
    struct rptadv_peer_bytes_v1 source; /**< Observed numeric network source. */
};

/** Accepted-prefix source PCM. Concealed is 1 only for codec-native PLC;
 * it must bypass in-band detection. Returned credit never exceeds count. */
typedef size_t (*rptadv_peer_source_audio_v1)(void *context, uint64_t source_start,
                                              const float *pcm, size_t count, uint32_t concealed);
/** Counted unresolved source loss, retained by the provider until accepted. */
typedef uint32_t (*rptadv_peer_source_gap_v1)(void *context, uint64_t source_start, uint32_t count);
/** Borrowed human text, separate from protocol control and claimed sender authority. */
typedef void (*rptadv_peer_source_text_v1)(void *context, struct rptadv_peer_bytes_v1 sender,
                                           struct rptadv_peer_bytes_v1 body);
/** Synchronous source sink; callbacks cannot reenter the same peer. */
struct rptadv_peer_source_sink_v1 {
    uint32_t struct_size;              /**< Complete readable size. */
    uint32_t abi_version;              /**< Exactly 1. */
    void *context;                     /**< Borrowed during one source-read call. */
    rptadv_peer_source_audio_v1 audio; /**< Accept a leading PCM prefix. */
    rptadv_peer_source_gap_v1 gap;     /**< Accept a leading missing-sample prefix. */
};
/** Copied bounded source turn, not raw packet or heartbeat receive state. */
struct rptadv_peer_source_result_v1 {
    uint32_t struct_size;    /**< Complete caller-provided size. */
    uint32_t abi_version;    /**< Exactly 1. */
    uint32_t progress;       /**< 0 idle, 1 advanced, 2 blocked, 3 provider drained. */
    uint32_t activity;       /**< 0 unchanged, 1 selected key, 2 selected unkey. */
    uint32_t finish_pending; /**< Publish one finite token and await its exact result. */
    uint32_t state;          /**< 0 connecting, 1 connected, 2 closing, 3 closed. */
    /** Fresh protocol-scoped numeric identity for the one human-text callback
     * in this turn; zero is unresolved. Never authentication or command authority. */
    uint32_t represented_node_number;
};

/** Non-transactional peer ingress/jitter counters. Durations are negotiated
 * source-rate samples, never native 48 kHz ring samples. Buffered span is the
 * sum of queued packet spans and can include packet overlap. Drop fields count
 * packets; ingress overflow counts datagrams. Counters are connection-lifetime
 * totals unless named queued/delay/buffered/capacity. No payload or secret data. */
struct rptadv_peer_observation_v1 {
    uint32_t struct_size;                  /**< Set by caller; host must fill complete revision. */
    uint32_t abi_version;                  /**< RPTADV_PEER_OBSERVATION_ABI_VERSION. */
    uint32_t jitter_queued_packets;        /**< Packets retained for playout. */
    uint32_t jitter_delay_samples;         /**< Current packet-buffer delay. */
    uint64_t jitter_buffered_span_samples; /**< Sum of retained packet durations. */
    uint64_t jitter_duplicate_drops;       /**< Rejected duplicate packets. */
    uint64_t jitter_late_drops;            /**< Packets too late for playout. */
    uint64_t jitter_full_drops;            /**< Packets rejected at the slot bound. */
    uint64_t jitter_missing_samples;       /**< Released missing media duration. */
    uint64_t jitter_insertion_samples;     /**< Released delay-insertion duration. */
    uint64_t jitter_skipped_samples;       /**< Media skipped during delay reduction. */
    uint64_t jitter_limit_resets;          /**< Adaptive resets at the delay limit. */
    uint64_t jitter_overload_drops;        /**< Packets lost in overload resets. */
    uint64_t jitter_overload_span_samples; /**< Packet duration lost in overload resets. */
    uint64_t ingress_overflow_drops;       /**< Datagrams rejected by the ingress queue. */
    uint32_t ingress_queued_packets;       /**< Datagrams awaiting this peer's owner. */
    uint32_t ingress_capacity_packets;     /**< Bounded ingress queue capacity. */
    uint64_t egress_datagrams;             /**< Datagram sends accepted by the socket. */
    uint64_t egress_would_block;           /**< Socket backpressure events. */
    uint64_t egress_reliable_retries;      /**< Successful reliable retransmissions. */
    uint32_t egress_reliable_pending;      /**< Retained reliable frames. */
    uint32_t egress_reply_pending;         /**< Staged protocol replies. */
    uint32_t echo_valid;                   /**< Nonzero only for the copied Echo metadata below. */
    uint32_t echo_state;                   /**< Connecting0/connected1/closing2/closed3. */
    uint32_t echo_first_cause;       /**< None0/local1/BYE2/connect3/liveness4/transport5/owner6. */
    uint32_t echo_codec;             /**< Negotiated GSM0 or narrowband Speex1. */
    uint32_t echo_raw_receiving;     /**< Valid session-bound packet activity. */
    uint32_t echo_selected;          /**< Selected/draining RF activity. */
    uint32_t echo_transport;         /**< Direct UDP0 or standard TCP Proxy1. */
    uint64_t echo_last_valid_age_ms; /**< Age of valid voice/control. */
    uint64_t echo_heartbeat_due_ms;  /**< Remaining heartbeat deadline, UINT64_MAX if inactive. */
    uint64_t echo_liveness_due_ms;   /**< Remaining setup/liveness deadline, or inactive. */
    uint64_t echo_retained_age_ms;   /**< Retained original datagram age; zero if none. */
    uint64_t echo_stale_packets;     /**< Retired generation packets ignored. */
    uint64_t echo_malformed_packets; /**< Invalid wire/media framing. */
    uint64_t echo_out_of_order_packets; /**< Lower RTP sequences, excluding equal duplicates. */
    uint64_t echo_codec_plc_samples;    /**< Native codec PLC accepted by product,8kHz samples. */
    uint64_t echo_fallback_gap_samples; /**< Ordered spans accepted for shared-ring fallback PLC. */
    uint64_t echo_proxy_admitted;       /**< Proxy queue admissions, not actual TCP sends. */
    uint64_t echo_text_admitted;        /**< Human-text transport admissions. */
    uint64_t echo_text_rejected;        /**< Human-text send attempts not admitted. */
    uint8_t echo_cause_detail[32];      /**< NUL-padded content-free error category, max31bytes. */
    uint64_t echo_ingress_bytes;        /**< Queued and currently borrowed ingress payload bytes. */
};

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
/** One borrowed peer input event identified by the RPTADV_PEER_EVENT_* constants.
 * For GAP, data is null and count is the number of missing source-rate samples.
 * The product may defer reading another event until all of this event is
 * admitted to the ordered inbound ring. */
typedef void (*rptadv_peer_event_v1)(void *context, uint32_t kind, const void *data, size_t count);
/** One serialized peer-owner turn. Return zero to wait for readiness/deadline
 * or a negative value to retire the peer.
 * The product may call the peer's synchronous I/O operations during this callback;
 * the host must not hold a peer borrow or lock while invoking it. */
typedef int32_t (*rptadv_peer_step_v1)(void *step_context);
/** Borrowed status text sink. */
typedef void (*rptadv_text_sink_v1)(void *context, const char *text, size_t length);
struct UrpNativeStationConfig;
struct rptadv_echolink_account_config_v1;
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
    /** Resolved public EchoLink settings, including disabled accounts. No passwords.
     * The provider ABI record and its nested spans/lists live only during this visit. */
    const struct rptadv_echolink_account_config_v1 *echolink;
    const uint8_t
        *echolink_secrets_file; /**< Configured host-relative path, or empty for default. */
    size_t echolink_secrets_file_length; /**< Path byte count; not NUL terminated. */
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
 * call, except radio endpoints retained from activate through destroy and the
 * optional peer-step callback retained from successful peer_step_attach through
 * peer_step_detach. PCM is aligned normalized
 * F32 with exactly the stated count. Radio transmit returns key and CTCSS-enable
 * decisions through separate output pointers. Peer text/audio data and DTMF byte
 * are borrowed for the callback; radio key/unkey events have null data and zero
 * count. Directory backends return zero after delivering borrowed results, negative
 * on authoritative rejection, or positive on recoverable DNS failure. Record and
 * SRV callbacks emit at most one result; no result means absent. Address callbacks
 * emit numeric IPs in resolver order. A standalone empty DNS answer is recoverable;
 * Asterisk's empty answer succeeds with no results, preserving its existing diagnosis.
 */
struct rptadv_host_services_v9 {
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

    /** Control-only site I/O on the existing radio owner; never open another device.
     * Bank 0 is CM119 and bank 1 is parallel-port I/O. Records use the installed
     * USBRadioPlus site ABI. Zero means accepted, not physically read back.
     * Binding/replacement generations and radio-pin exclusions are enforced by
     * the owner. Snapshot acquisition times and submission/service generations
     * are preserved. Nonzero provider errors pass through unchanged.
     * Calls are serialized with other control operations and radio destruction,
     * never made by an audio callback. Records are borrowed only during the call. */
    int (*radio_site_bind)(void *context, void *radio, uint32_t bank,
                           const struct UrpAstSiteBinding *binding);
    /** Submit logical site output levels through the current binding. */
    int (*radio_site_publish)(void *context, void *radio, uint32_t bank,
                              const struct UrpAstSiteRequest *request);
    /** Copy the current owner observation without synthesizing physical readback. */
    int (*radio_site_snapshot)(void *context, void *radio, uint32_t bank,
                               struct UrpAstSiteSnapshot *snapshot);

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
    /** Send one normalized F32 PCM block, or one end-of-burst marker when empty.
     * Required for PCM-only hosts; absent when the complete prepared group is supplied. */
    int (*peer_write)(void *context, void *peer, const float *samples, size_t sample_count);
    /** Destroy one uniquely owned peer. */
    void (*peer_destroy)(void *context, void *peer);

    /** Optional async peer-owner group: all three entries are null or all present.
     * Attach retains step_context but does not transfer the uniquely owned peer.
     * On failure it neither invokes nor retains the callback/context and sets
     * registration to null. On success the host serializes every step for this
     * peer, never holding a peer borrow or lock across the call. It services
     * queued ingress and protocol/media deadlines at most 10 ms apart. */
    int (*peer_step_attach)(void *context, void *peer, rptadv_peer_step_v1 step, void *step_context,
                            void **registration);
    /** Nonblocking and safe from within step; prevent further callbacks. */
    void (*peer_step_stop)(void *context, void *registration);
    /** Off-callback-only quiescence barrier. After return no callback can run;
     * the product may then release step_context and destroy the peer. */
    void (*peer_step_detach)(void *context, void *registration);
    /** Optional serialized peer-owner observation. The product calls this only
     * from peer_step, after ordinary peer I/O and before returning to the host.
     * It never calls this from control or audio. Zero fills the complete record;
     * nonzero leaves the previous copied observation unchanged. */
    int (*peer_observe)(void *context, void *peer, struct rptadv_peer_observation_v1 *out);

    /** Optional prepared-egress group: all six entries absent or present, with
     * the async owner group. Retain incomplete source units on the peer owner;
     * return common codec status and explicit counts. OutputFull consumes no
     * new PCM. Negative means invalid arguments or a host failure. */
    int (*peer_prepare_audio)(void *context, void *peer, const float *samples, size_t sample_count,
                              uint8_t *encoded, size_t capacity, uint32_t flush,
                              struct rptadv_codec_encode_result_v1 *result);
    /** Abandon expired/discontinuous PCM and encoder history, on the peer owner. */
    int (*peer_reset_encoder)(void *context, void *peer);
    /** Submit encoded media at its original call-relative source timestamp.
     * Zero accepts ownership; one is uncommitted socket backpressure; negative
     * is terminal. The caller retains a would-block item without re-encoding.
     * A zero count submits the source-timed end-of-burst marker. */
    int (*peer_send_prepared)(void *context, void *peer, const uint8_t *encoded, size_t count,
                              uint32_t packet_frames, uint32_t timestamp_ms);
    /** Current call-relative monotonic milliseconds, saturating at UINT32_MAX. */
    uint32_t (*peer_elapsed_ms)(void *context, void *peer);
    /** Thread-safe signal-only wake. The caller must quiesce its notifications
     * before detaching registration; this never borrows mutable peer state. */
    void (*peer_step_wake)(void *context, void *registration);
    /** Owner-only deadline: relative nanoseconds, UINT64_MAX clears it. */
    int (*peer_set_deadline)(void *context, void *peer, uint64_t wake_after_ns);

    /** Required control-only scheduler health warning through the host logger.
     * Text is borrowed UTF-8 for this synchronous call only. Never retain it
     * or unwind across the callback boundary. Never called by audio workers. */
    void (*scheduler_notice)(void *context, const char *text, size_t length);

    /** Copy this handle's immutable identity and capabilities before media starts.
     * Failure does not transfer ownership or retain the caller's result buffer. */
    int (*peer_info)(void *context, const void *peer, struct rptadv_peer_info_v1 *out);
    /** Optional accepted-prefix source group, selected by this peer capability.
     * Unaccepted PCM/gap suffixes remain with the provider. Completion is 0 while
     * pending, 1 only after the exact finite token plays, or 2 on explicit
     * generation discard. A completing turn must reject new PCM/gap credit.
     * A null sink queries state without consuming input or advancing media. */
    int (*peer_read_source)(void *context, void *peer, uint32_t product_completion,
                            const struct rptadv_peer_source_sink_v1 *sink,
                            rptadv_peer_source_text_v1 text, void *text_context,
                            struct rptadv_peer_source_result_v1 *out);
    /** Structured human text for a capable protocol peer. Borrowed literal spans;
     * never reinterpret as commands or send synthesized speech to the peer.
     * Zero admits one message, nonzero rejects only that message. Serial owner only. */
    int (*peer_send_human_text)(void *context, void *peer, struct rptadv_peer_bytes_v1 sender,
                                struct rptadv_peer_bytes_v1 body);
    /** Optional EchoLink account diagnostics, including when there are no QSOs.
     * Control only, off provider/audio executors. Emit borrowed metadata text;
     * zero with no text means no account, negative means unavailable, never offline. */
    int (*echo_account_status)(void *context, const char *local, size_t local_length,
                               rptadv_text_sink_v1 sink, void *sink_context);
};

/** Exact revision of a typed site control request. */
#define RPTADV_SITE_COMMAND_ABI_VERSION 1U
/** Request a configured logical output on. */
#define RPTADV_SITE_OUTPUT_ON 1U
/** Request a configured logical output off. */
#define RPTADV_SITE_OUTPUT_OFF 2U
/** Pulse using the configured duration, restoring the prior logical state. */
#define RPTADV_SITE_OUTPUT_PULSE 3U
/** Silence alarm repeats without clearing protection. */
#define RPTADV_SITE_ALARM_ACKNOWLEDGE 4U
/** Reset only a recovered alarm. */
#define RPTADV_SITE_ALARM_RESET 5U
/** Reset retained sensor extrema. */
#define RPTADV_SITE_RESET_EXTREMA 6U
/** Restore automatic fan demand. */
#define RPTADV_SITE_FAN_AUTOMATIC 7U
/** Add normalized manual fan demand; never reduce protective demand. */
#define RPTADV_SITE_FAN_DUTY 8U
/** Assert the manual RF inhibit. */
#define RPTADV_SITE_INHIBIT_ON 9U
/** Release only the manual RF inhibit, never alarms or hardware interlocks. */
#define RPTADV_SITE_INHIBIT_OFF 10U

/** An already-authorized control request, never a physical I/O call.
 * Names are borrowed UTF-8 configured identities. Inhibit operations require
 * an empty target; all other operations require a nonempty target. Only FAN_DUTY
 * uses value, which must be finite in [0,1]; other operations require zero.
 * The host must authenticate administrative callers before invoking this API.
 */
struct rptadv_site_command_v1 {
    uint32_t struct_size; /**< Exact readable record size. */
    uint32_t abi_version; /**< Exact RPTADV_SITE_COMMAND_ABI_VERSION. */
    uint32_t operation;   /**< One RPTADV_SITE_* operation. */
    const char *target;   /**< Borrowed target name, not NUL terminated. */
    size_t target_length; /**< Name byte count; null is valid only for zero length. */
    double value;         /**< Normalized manual fan demand, otherwise zero. */
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
    int (*start)(const struct rptadv_host_services_v9 *host,
                 const struct rptadv_control_descriptor_v1 *control,
                 const struct rptadv_file_descriptor *file,
                 const struct rptadv_speech_descriptor *speech, const char *configuration,
                 size_t configuration_length);
    int (*reload)(const char *configuration,
                  size_t configuration_length); /**< Replace configuration. */
    int (*stop)(void);                          /**< Stop and quiesce every product owner. */
    /** Check current incoming policy before answering a channel. */
    int (*authorize_incoming)(const struct rptadv_incoming_peer_v1 *request, void *peer);
    /** Admit one transferred peer. */
    int (*incoming)(const struct rptadv_incoming_peer_v1 *request, void *peer);
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
    /** Apply site policy on the current serialized control generation.
     * Returns 0 when policy accepts the request (not proof of physical service),
     * -1 when unavailable/reloading/stopping, -2 for unknown node/target,
     * -3 when unsafe, or -4 for an invalid request. Requested/submitted/observed
     * state and downstream hardware faults remain visible in status. This entry
     * neither changes link-command semantics nor emits a spoken CLI reply. */
    int (*site_command)(const char *local, size_t local_length,
                        const struct rptadv_site_command_v1 *request);
};

/** Return immutable process-lifetime product metadata.
 * @return Complete product descriptor retained through successful stop.
 */
const struct rptadv_product_descriptor_v1 *rptadv_product_descriptor_v1(void);

#ifdef __cplusplus
}
#endif
#endif
