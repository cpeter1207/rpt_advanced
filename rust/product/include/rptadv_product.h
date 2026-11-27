/* SPDX-License-Identifier: GPL-2.0-only */
/** @file rptadv_product.h
 * @brief Portable product runtime and host-services ABI.
 */
#ifndef RPTADV_PRODUCT_H
#define RPTADV_PRODUCT_H

#include <rptadv_codec.h>
#include <stddef.h>
#include <stdint.h>
#include <usbradioplus_product.h>

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
#define RPTADV_PRODUCT_ABI_VERSION 8U
/** Product capability name stored in its fixed-width descriptor field. */
#define RPTADV_PRODUCT_CAPABILITY "rptadv.prod8"
/** Exact incompatible host-services ABI revision. */
#define RPTADV_HOST_ABI_VERSION 11U
/** Host-services capability name stored in its fixed-width descriptor field. */
#define RPTADV_HOST_CAPABILITY "rptadv.hst11"
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
/** Private opaque telephone call; never a public/dialable node identity. */
#define RPTADV_PEER_PHONE 3U
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
/** Call-addressed control/reply lifetime, independent of RF attachment.
 * This does not advertise remote call-origin or legacy-D isolation support. */
#define RPTADV_PEER_PRIVATE_CALL 32U
/** Private source has no negotiated digit events. Analyze original PCM for caller digits. */
#define RPTADV_PEER_PHONE_INBAND_DIGITS 64U
/** Private source receives negotiated digit events but cannot send them on this media clock. */
#define RPTADV_PEER_PHONE_RECEIVE_ONLY_DIGITS 512U

/** Borrowed immutable bytes. Null is valid only with zero length. */
struct rptadv_peer_bytes_v1 {
    const uint8_t *data; /**< Retained by its peer until destruction. */
    size_t length;       /**< Exact byte count, excluding a terminator. */
};

/** Copy before returning to other work. EchoLink metadata is freshly validated
 * against its owning account/QSO and directory during pending admission. */
struct rptadv_peer_info_v1 {
    uint32_t struct_size;        /**< Caller capacity on entry; bytes copied on return. */
    uint32_t abi_version;        /**< Exactly 1. */
    uint32_t protocol;           /**< ALLSTARLINK, ECHOLINK, or private PHONE. */
    uint32_t capabilities;       /**< Operations supported by this handle, not the host table. */
    uint64_t account_generation; /**< Echo/phone account lifetime; zero for channel-backed IAX. */
    uint64_t qso_generation; /**< Echo QSO or phone call lifetime; zero for channel-backed IAX. */
    struct rptadv_peer_bytes_v1
        remote; /**< Decimal node, or canonical 32-lowercase-hex phone token. */
    struct rptadv_peer_bytes_v1 callsign; /**< Validated Echo identity; may be empty for IAX. */
    struct rptadv_peer_bytes_v1
        local; /**< Owning controller binding: Phone NodeId <=63 bytes; node wire ID <=32. */
    /** Compatible optional tail. PHONE requires this complete span, nonzero generations,
     * and PRIVATE_CALL; other protocols retain the existing prefix contract.
     * This is a configured local account label, never provider credentials or a mailbox. */
    struct rptadv_peer_bytes_v1 provider_account;
};
/** Complete original node-peer prefix; callbacks copy at most the caller's capacity. */
#define RPTADV_PEER_INFO_PREFIX_SIZE offsetof(struct rptadv_peer_info_v1, provider_account)
/** Minimum complete metadata size required for a private telephone call. */
#define RPTADV_PEER_INFO_PHONE_SIZE sizeof(struct rptadv_peer_info_v1)

/** Incoming request bound to one protocol, local account and observed endpoint.
 * EchoLink requires a provider-owned pending peer, never a caller-supplied proof flag. */
struct rptadv_incoming_peer_v1 {
    uint32_t struct_size;               /**< Complete readable size. */
    uint32_t abi_version;               /**< Exactly 1. */
    uint32_t protocol;                  /**< ALLSTARLINK, ECHOLINK, or private PHONE. */
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
/** Original packet receipt in the supplied host clock. Retain unchanged through
 * rejected offers, partial credit, conversion and retries; never stamp at playout. */
struct rptadv_source_birth_v1 {
    uint32_t struct_size;      /**< At least this complete revision. */
    uint32_t abi_version;      /**< Exactly 1. */
    uint64_t host_clock_epoch; /**< Exact nonzero sink clock identity. */
    uint64_t born_ns;          /**< No later than sink reference_now_ns. */
};
/** Timed source PCM. A null birth is explicitly unqualified. */
typedef size_t (*rptadv_peer_source_audio_timed_v1)(void *context, uint64_t source_start,
                                                    const float *pcm, size_t count,
                                                    uint32_t concealed,
                                                    const struct rptadv_source_birth_v1 *birth);
/** Request actual media retirement. Zero completes, one means retry pending,
 * negative is terminal. Retain the reset and offer no new PCM until complete. */
typedef int32_t (*rptadv_peer_source_discontinuity_v1)(void *context);
/** A proved native adaptive source cut, never permission to transmit.
 * Nonzero count and nonoverflowing [source_start, source_start + count).
 * Zero completes retirement, one retries the identical cause/range, negative
 * is terminal. No later PCM or gap credit before completion. */
typedef int32_t (*rptadv_peer_source_adaptation_v1)(void *context, uint64_t source_start,
                                                    uint32_t count);
/** One private telephone digit. Exactly one of 0123456789ABCD*# is valid.
 * Zero accepts it once, one retains it for retry before later source events,
 * and negative is terminal. Provider retries preserve the same symbol. */
typedef int32_t (*rptadv_peer_source_digit_v1)(void *context, uint32_t symbol);
/** Synchronous source sink; callbacks cannot reenter the same peer. */
struct rptadv_peer_source_sink_v1 {
    uint32_t struct_size;              /**< Complete readable size. */
    uint32_t abi_version;              /**< Exactly 1. */
    void *context;                     /**< Borrowed during one source-read call. */
    rptadv_peer_source_audio_v1 audio; /**< Accept a leading PCM prefix. */
    rptadv_peer_source_gap_v1 gap;     /**< Accept a leading missing-sample prefix. */
    /** Optional tail; require struct_size through the field being accessed. */
    uint64_t host_clock_epoch; /**< Stable controller I/O lifecycle, zero unqualified. */
    uint64_t reference_now_ns; /**< Captured before entering the provider. */
    rptadv_peer_source_audio_timed_v1 audio_timed;            /**< Retains original age. */
    rptadv_peer_source_discontinuity_v1 source_discontinuity; /**< Actual reset receipt. */
    /** Optional private-phone-only tail. Check struct_size through this field
     * and a nonnull callback before use. Source-capable phones use this one
     * ordered stream, never a second legacy peer_read for digits. */
    rptadv_peer_source_digit_v1 digit;
    /** Optional exact native adaptation cause; absent/short uses generic reset. */
    rptadv_peer_source_adaptation_v1 source_adaptation;
};
/** Complete original sink prefix, supported by legacy unqualified providers. */
#define RPTADV_PEER_SOURCE_SINK_PREFIX_SIZE                                                        \
    offsetof(struct rptadv_peer_source_sink_v1, host_clock_epoch)
/** Complete old private-digit tail; unchanged by optional later extensions. */
#define RPTADV_PEER_SOURCE_SINK_DIGIT_SIZE                                                         \
    (offsetof(struct rptadv_peer_source_sink_v1, digit) + sizeof(rptadv_peer_source_digit_v1))
/** Minimum readable size for the optional native adaptation callback. */
#define RPTADV_PEER_SOURCE_SINK_ADAPTATION_SIZE                                                    \
    (offsetof(struct rptadv_peer_source_sink_v1, source_adaptation) +                              \
     sizeof(rptadv_peer_source_adaptation_v1))
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
typedef UrpNativeReceive rptadv_radio_receive_v3;
/** Serial output endpoint; inactive/gated generations succeed with silence/unkeyed.
 * Malformed arguments or processing failure return nonzero with silent output. */
typedef int (*rptadv_radio_transmit_v3)(void *context, float *samples, uint32_t sample_count,
                                        uint32_t *keyed, uint32_t *ctcss_enabled);
/** Serial TX owner: apply remote enabled/RX/TX/non-FM flags in bits 0..3.
 * Return zero/one pre-mute linked activity, including buffered tails and text.
 * No allocation, wait, or I/O; use the existing transmit context. */
typedef int (*rptadv_radio_metadata_v1)(void *transmit_context, uint32_t controls,
                                        uint32_t *linked_active);
/** Direct RadioPlusAdvanced option identifier, validated before ast_call. */
#define URP_AST_OPTION_DIRECT_CALLBACKS 0x52504144
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
/** Borrow one selected DNS SRV target and port for the synchronous backend call. */
typedef void (*rptadv_directory_srv_sink_v1)(void *context, const char *host, size_t length,
                                             uint16_t port);

/** Authorized physical-radio operation, executed only on serialized control.
 * STATUS=1, COMMAND=2, TRANSMIT=3, CLEAR_MANUAL=4,
 * PROCESSING_STATUS=5, ACTIVE_CONFIGURATION=6, RECEIVE_POLICY=7.
 * RECEIVE_POLICY works before activation and uses GET_RECEIVE_POLICY in the
 * nested command: value is effective VAD mode, flags is minimum native preroll.
 * COMMAND also uses the nested command. Text uses the synchronous sink. */
struct rptadv_radio_control_v1 {
    uint32_t struct_size;                /**< Exact complete size. */
    uint32_t abi_version;                /**< One. */
    uint32_t operation;                  /**< One operation listed above. */
    struct UrpAstChannelCommand command; /**< In/out shared maintenance command. */
    uint32_t keyed;                      /**< Temporary manual transmit request, zero or one. */
    uint32_t forced_ctcss_tenths_hz;     /**< Optional manual transmit tone. */
};

/** Optional stopped-only physical-feedback registration, with explicit acknowledgment. */
#define RPTADV_RADIO_CONTROL_BIND_FEEDBACK 8U
/** Optional native control-owner status copy. It never changes radio settings. */
#define RPTADV_RADIO_CONTROL_PHYSICAL_STATUS 9U

/** Additive radio_control request; the common prefix advertises this whole record's size.
 * The radio retains the binding only through successful synchronous retirement.
 * Acknowledgment accepts the binding; a missing optional hardware source reports no data. */
struct rptadv_radio_feedback_binding_v1 {
    struct rptadv_radio_control_v1 request; /**< Operation BIND_FEEDBACK, ABI one. */
    struct UrpRadioFeedbackPort port;       /**< Borrowed registration copied by the host. */
    uint32_t accepted_abi_version;          /**< Initialize zero, require one on acceptance. */
    uint32_t reserved;                      /**< Zero. */
};

/** Additive radio_control request; only the native station owns the process status. */
struct rptadv_radio_physical_status_v1 {
    struct rptadv_radio_control_v1 request; /**< Operation PHYSICAL_STATUS, ABI one. */
    struct UrpNativePhysicalStatus status;  /**< Caller-initialized writable SDK record. */
    uint32_t accepted_abi_version;          /**< Initialize zero, require one on success. */
    uint32_t reserved;                      /**< Zero. */
};

/** Borrowed UTF-8 result emitted synchronously by a remote operation ticket. */
typedef void (*rptadv_remote_sink_v1)(void *context, const uint8_t *text, uint32_t length);
/** One uniquely owned asynchronous operation. Functions and context stay live
 * until destroy. Source flags: bit0 active, bit1 valid; validity is terminal.
 * take returns zero with one copied JSON result, one while pending, or an error.
 * destroy cancels and releases without waiting for the physical owner.
 */
struct rptadv_remote_ticket_v1 {
    uint32_t struct_size; /**< Exact complete record size. */
    uint32_t abi_version; /**< One. */
    /** Unique ticket, never the radio or peer object. */
    void *context;
    int (*source)(void *ticket, uint32_t flags); /**< Publish current source state. */
    int (*take)(void *ticket, rptadv_remote_sink_v1 sink, void *context); /**< Copy completion. */
    void (*destroy)(void *ticket); /**< Cancel before releasing, including while pending. */
};
/** Prepared physical station passed synchronously from its owning product DSO. */
struct rptadv_station_request_v1 {
    uint32_t struct_size;        /**< Complete readable record size. */
    uint32_t abi_version;        /**< One. */
    uint8_t port_uuid[16];       /**< Registry UUID in network byte order. */
    uint64_t candidate_identity; /**< Nonzero, process-unique semantic staging attempt. */
    const struct UrpAstDescriptor *product; /**< Actual matched product1 descriptor. */
    const void *prepared; /**< Borrowed physical-preparation1 owner from this product. */
};
/** Immutable control-side physical station service, capability rptadv.station-binding1. */
struct rptadv_station_binding_v1 {
    uint32_t struct_size;        /**< Complete readable table size. */
    uint32_t abi_version;        /**< One. */
    const char *capability_name; /**< Process-lifetime terminated capability name. */
    /** Copy or retain the provider configuration without reserving hardware or changing live state.
     * Zero transfers a unique dormant plan. The operation may run on any serialized control owner.
     */
    int (*prepare)(void *context, const struct rptadv_station_request_v1 *request, void **plan);
    /** Open this exact plan after durable publication. Zero transfers the ordinary unique radio.
     * The live radio independently retains its provider, prepared data and operation context. */
    int (*open_prepared)(void *context, const void *plan, size_t maximum_frames, void **radio);
    /** Stage on the matching port. Zero staged, one untouched handoff, two pending (poll same
     * plan). Any other result requires actual rollback or confirmed retirement before releasing
     * ownership. */
    int (*stage_update)(void *context, void *radio, const void *plan);
    /** Destroy this plan exactly once after its last borrow. No creator-thread affinity. */
    void (*release_plan)(void *context, void *plan);
};
/** Native monotonic source clock shared by the producer and sole serial consumer. */
struct rptadv_media_clock_reader_v1 {
    uint32_t struct_size;      /**< Caller capacity; at least the complete known record. */
    uint32_t abi_version;      /**< Exact version 1. */
    uint32_t clock_kind;       /**< Output 1 for CLOCK_MONOTONIC, zero for unavailable. */
    uint32_t reserved;         /**< Output zero. */
    uint64_t (*read_ns)(void); /**< Current nanoseconds; zero means unavailable/failure. */
};

/** Borrowed explicit private-account row. Both UUIDs are in network byte order.
 * Read/copy only during prepare. No text is NUL-terminated or loggable.
 * An empty account list requests withdrawal of all private accounts. */
struct rptadv_phone_account_v1 {
    uint32_t struct_size;     /**< Exact ABI1 array stride. */
    uint32_t abi_version;     /**< One. */
    uint8_t port_uuid[16];    /**< Owning registry port. */
    uint8_t account_uuid[16]; /**< Explicit phone protocol resource. */
    uint64_t generation; /**< Nonzero identity of this authorized account and credential version. */
    uint32_t protocol;   /**< One for SIP, two for IAX2. */
    uint32_t port;       /**< Remote UDP port, 1 through 65535. */
    struct rptadv_peer_bytes_v1 server;   /**< Validated provider host. */
    struct rptadv_peer_bytes_v1 username; /**< Exact transport identity. */
    struct rptadv_peer_bytes_v1
        credential;             /**< Private bytes lent by final encrypted record owner. */
    uint32_t jitter_minimum_ms; /**< Lower bound on native packet playout. */
    uint32_t jitter_initial_ms; /**< Initial native target. */
    uint32_t jitter_maximum_ms; /**< Upper bound on native packet playout. */
    uint32_t reserved;          /**< Zero. */
};

/** Identity of a managed ordinary IAX authority. UUIDs are in network byte order. */
struct rptadv_asl_identity_v1 {
    uint8_t station_uuid[16], /**< Owning station UUID. */
        account_uuid[16];     /**< Selected ASL account UUID. */
    uint64_t generation,      /**< Nonzero local selection generation. */
        wire;                 /**< Numeric ASL wire node. */
    uint32_t udp_port,        /**< UDP port in host byte order. */
        reserved;             /**< Zero. */
};

/** Dormant account input. All spans are borrowed only during prepare. */
struct rptadv_asl_account_v2 {
    uint32_t struct_size,             /**< Exact account array stride in bytes. */
        abi_version;                  /**< ABI version 2. */
    struct rptadv_asl_identity_v1 id; /**< Selected local ASL authority. */
    uint32_t jitter_min_ms,           /**< Minimum playout delay in milliseconds. */
        jitter_initial_ms,            /**< Initial playout delay in milliseconds. */
        jitter_max_ms;                /**< Maximum playout delay in milliseconds. */
    uint32_t registration_interval_s; /**< Interval between registration requests in seconds. */
    struct rptadv_peer_bytes_v1 registration_url; /**< Registrar URL, empty when disabled. */
    uint8_t record_version[16];                   /**< Private. Zero if registration is disabled. */
    struct rptadv_peer_bytes_v1 registration_credential; /**< Private, never logged. */
    uint32_t admission_changed; /**< Zero or one. Includes directory and ACL changes. */
    uint32_t jitter_authored;   /**< One when any selected delay came from a registered profile. */
};

/** Product callback table retained until the host has drained every admitted poll. */
struct rptadv_asl_admission_v1 {
    uint32_t struct_size, /**< Size of this record in bytes. */
        abi_version;      /**< ABI version 1. */
    /** Success transfers one proof scoped to this single server poll. */
    int (*authorize)(const struct rptadv_asl_identity_v1 *id, struct rptadv_peer_bytes_v1 remote,
                     struct rptadv_peer_bytes_v1 source, void **proof);
    /** Always consumes proof. A null peer cancels. A positive return leaves a nonnull
     * wrapper with its caller. Zero or negative consumes the wrapper. */
    int (*complete)(void *proof, void *host_peer);
};

/** Optional ordinary IAX provider. No operation runs on an audio callback. */
struct rptadv_asl_provider_v2 {
    uint32_t struct_size, /**< Size of this record in bytes. */
        abi_version;      /**< ABI version 2. */
    uint32_t media_kind;  /**< 1: frontend asynchronous playout. 2: PBX channel PCM. */
    uint32_t reserved;    /**< Zero. */
    /** Prepare copies rows and credentials without network I/O. Zero basis denotes
     * an initially empty participant. Success transfers a unique dormant transaction. */
    int (*prepare)(void *host, uint64_t basis, uint64_t attempt,
                   const struct rptadv_asl_account_v2 *rows, size_t count,
                   const struct rptadv_asl_admission_v1 *product, void **transaction);
    /** Apply=1, publish=2, reverse=3, finish=4. Zero acknowledges, one is pending.
     * Only an acknowledged finish consumes the transaction. */
    int (*step)(void *host, void *transaction, uint32_t phase);
    /** Return the published registrar state for the exact live authority. */
    int (*observe)(void *host, const struct rptadv_asl_identity_v1 *id, uint32_t *registrar_state);
    /** Dial a verified numeric IP:port. The synchronous currency callback and input
     * spans expire on return. Success transfers a unique playout-prepared peer. */
    int (*originate)(void *host, const struct rptadv_asl_identity_v1 *id,
                     struct rptadv_peer_bytes_v1 remote,
                     struct rptadv_peer_bytes_v1 numeric_endpoint, size_t maximum_frames,
                     rptadv_current_v1 current, void *current_context, void **host_peer);
};

/** Observed participant state for exactly one selected generation. */
struct rptadv_phone_account_status_v1 {
    uint32_t struct_size; /**< Size of caller-provided output. */
    uint32_t abi_version; /**< One. */
    uint64_t generation;  /**< Confirmed account generation, or zero when inactive. */
    uint32_t state;       /**< 0 inactive, 1 starting, 2 registered, 3 retiring, 4 failed. */
    uint32_t reserved;    /**< Zero. */
};

/** Optional bounded mono progress read. The caller initializes struct_size, abi_version,
 * reserved (zero), pcm and capacity. A positive capacity requires a separate writable
 * buffer for that many floats. The output fields ringing, final_status, frames and
 * sample_rate may be uninitialized. A null record requests signaling completion only.
 * Original frames exclude concealment. Ringing is enabled by an actual SIP 180
 * until original early media or a final response. A nonzero final_status is a
 * final INVITE response code; the ordinary peer still requires confirmed readiness. */
struct rptadv_phone_progress_v1 {
    uint32_t struct_size, /**< Size of this record in bytes. */
        abi_version;      /**< ABI version 1. */
    uint32_t ringing,     /**< One while SIP ringing is active. */
        final_status;     /**< Final INVITE response code, or zero before a final response. */
    float *pcm;           /**< Caller-owned output buffer for original mono early media. */
    size_t capacity,      /**< Writable capacity of pcm in floats. */
        frames;           /**< Number of mono frames written by this poll. */
    uint32_t sample_rate, /**< Sample rate of returned PCM in hertz. */
        reserved;         /**< Zero. */
};

/** Private provider group, borrowed until both frontend and product have drained.
 * Every operation runs outside audio. A partial group cannot activate a phone
 * account. An unavailable group is valid for configurations without a phone.
 * No transition or dial callback borrows selectors or credential bytes past its call. */
struct rptadv_phone_provider_v2 {
    uint32_t struct_size; /**< Readable size, through the last required slot. */
    uint32_t abi_version; /**< Two. */
    /** Validate and copy the bounded final registry selection. No socket, registration,
     * listener change or active selection. Zero transfers a unique transaction,
     * negative clears output and leaves the old selection untouched. */
    int (*prepare)(void *context, const struct rptadv_phone_account_v1 *accounts, size_t count,
                   void **transaction);
    /** Apply the retained candidate. Repeated calls poll the same attempt.
     * Zero confirms native adoption, one is pending, negative requires restore. */
    int (*apply)(void *context, void *transaction);
    /** Restore and observe the exact prior participant. A prepared-only token
     * cancels without native I/O. Zero confirms, one is pending, negative is
     * indeterminate and retains every possibly live owner. */
    int (*restore)(void *context, void *transaction);
    /** Release only after an unused preparation or confirmed selected/reversed
     * outcome. Zero consumes exactly once. Any other result retains the token. */
    int (*release)(void *context, void *transaction);
    /** Read native state for the exact account after commit. Never dial or register. */
    int (*observe)(void *context, const uint8_t port_uuid[16], const uint8_t account_uuid[16],
                   uint64_t generation, struct rptadv_phone_account_status_v1 *status);
    /** Begin a dial under this exact registered generation. Copies all borrowed selectors.
     * Zero transfers one uniquely owned pending handle. No result waits on a worker. */
    int (*begin)(void *context, const uint8_t port_uuid[16], const uint8_t account_uuid[16],
                 uint64_t generation, struct rptadv_peer_bytes_v1 destination,
                 size_t maximum_frames, void **pending);
    /** Nonblocking poll on the same owner. One is pending, zero transfers a unique
     * answered peer, and a negative result ends the attempt. The caller disposes
     * the pending handle once after every terminal result or cancellation.
     * The optional progress destination is borrowed for this invocation only. */
    int (*poll)(void *context, void *pending, struct rptadv_phone_progress_v1 *progress,
                void **peer);
    /** Consume one pending handle. Unanswered native dialogs retain their own retirement. */
    void (*cancel)(void *context, void *pending);
    /** Send the complete previously prepared encoded unit on its actual provider
     * clock. Zero accepts, one is WouldBlock, two is Expired without acceptance,
     * negative is terminal. No partial byte credit. Retries retain the original
     * timestamp, duration and nonwrapping deadline. */
    int (*send_prepared_timed)(void *context, void *peer, const uint8_t *bytes, size_t count,
                               uint32_t frames, uint32_t timestamp_ms, uint32_t deadline_ms);
    /** Access the actual source receipt clock. A missing/failed clock gives no
     * timing qualification. Never manufacture birth time when servicing a retry. */
    int (*media_clock_reader)(void *context, struct rptadv_media_clock_reader_v1 *out);
};

/** Dormant direct EchoLink account input. All pointers are borrowed during row preparation. */
struct rptadv_managed_echo_row_v1 {
    uint32_t struct_size,       /**< Size of this record in bytes. */
        abi_version;            /**< ABI version 1. */
    uint8_t station_uuid[16],   /**< Owning station UUID. */
        account_uuid[16],       /**< Selected EchoLink account UUID. */
        credential_version[16]; /**< Version UUID of the directory credential. */
    uint64_t local_label;       /**< Private decimal correlation, independent of wire identity. */
    uint32_t minimum_ms,        /**< Minimum playout delay in milliseconds. */
        initial_ms,             /**< Initial playout delay in milliseconds. */
        maximum_ms;             /**< Maximum playout delay in milliseconds. */
    uint32_t role; /**< 1 add, 2 live exchange, 3 displaced incumbent backup, 4 replacement. */
    const struct rptadv_echolink_account_config_v1
        *settings; /**< Borrowed account configuration. */
    struct rptadv_peer_bytes_v1
        directory_password; /**< Borrowed private directory credential bytes. */
};

/** Public, copied fields for a selected direct account; no borrowed strings. */
struct rptadv_managed_echo_status_v1 {
    uint32_t struct_size, /**< Size of this record in bytes. */
        abi_version;      /**< ABI version 1. */
    uint64_t generation;  /**< Nonzero generation reported by the selected account. */
    uint32_t state,       /**< 1 registering, 2 online, 3 retrying. */
        registered,       /**< One when the account is registered. */
        fresh,            /**< One when the directory data is fresh. */
        available;        /**< One when directory data is available. */
};

/** Optional standalone direct-Echo participant. No operation retains borrowed row bytes.
 * begin reserves one basis/attempt, row only prepares dormant native owners. step polls
 * apply (1), publication (2), reversal (3), or release (4) on that same reservation.
 * shutdown closes admission and drains retained native and callback owners. */
struct rptadv_managed_echo_provider_v2 {
    uint32_t struct_size, /**< Size of this record in bytes. */
        abi_version;      /**< ABI version 2. */
    /** Reserve an attempt against the supplied basis. */
    int (*begin)(void *context, uint64_t basis, uint64_t attempt);
    /** Prepare one dormant account for the reserved attempt. */
    int (*row)(void *context, uint64_t attempt, const struct rptadv_managed_echo_row_v1 *);
    /** Poll application, publication, reversal or release of this attempt. */
    int (*step)(void *context, uint64_t attempt, uint32_t phase);
    /** Close admission and drain native owners and callbacks. */
    int (*shutdown)(void *context);
    /** Off-control read for the exact published port, account and selection label.
     * Caller initializes status extent and version; no output is valid on failure. */
    int (*observe)(void *context, const uint8_t port[16], const uint8_t account[16], uint64_t label,
                   struct rptadv_managed_echo_status_v1 *status);
};

/** Host I/O primitives consumed by the portable product owner.
 * On supported 64-bit platforms, ABI 10 retains its complete 360-byte prefix
 * through radio_update and its complete 400-byte prefix through radio_ready.
 * Omitted complete capability tails are copied as null; partial tails are rejected.
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
struct rptadv_host_services_v11 {
    uint32_t struct_size;   /**< Complete readable table size. */
    uint32_t abi_version;   /**< Exact RPTADV_HOST_ABI_VERSION. */
    uint8_t capability[16]; /**< Exact NUL-padded RPTADV_HOST_CAPABILITY. */
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

    /** Attach both endpoints before starting. Failure may retain a partial owner;
     * radio_quiesce must succeed before callback storage is released. A host without
     * that optional tail must detach/quiesce both endpoints before returning failure.
     * Endpoints are serial
     * individually and may run concurrently with each other, never with destroy. */
    int (*radio_activate)(void *context, void *radio, rptadv_radio_receive_v3 receive,
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
     * Copy no more than the caller's capacity, accepting at least INFO_PREFIX_SIZE.
     * PHONE requires INFO_PHONE_SIZE plus PRIVATE_CALL. Failure never transfers
     * ownership or retains the caller's result buffer. */
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
    /** Optional physical maintenance on this existing radio owner. No callback
     * or hardware handle escapes; old hosts may omit this sized tail. */
    int (*radio_control)(void *context, void *radio, struct rptadv_radio_control_v1 *request,
                         rptadv_text_sink_v1 sink, void *sink_context);
    /** Optional prepared same-device update: phase 0 stages and acknowledges,
     * phase 1 commits, phase 2 rolls back. Zero succeeds, one requires device
     * handoff. A negative stage result still requires phase 2 to restore any
     * partially adopted state. Phase 2 is idempotent with no staged update. */
    int (*radio_update)(void *context, void *radio, uint32_t phase);
    /** Optional complete tail: bind TX metadata before radio_activate. No hardware
     * is acquired. Callback lifetime ends with synchronous radio destruction.
     * Zero succeeds; enabled remote compositions require this operation. */
    int (*radio_metadata_bind)(void *context, void *radio, rptadv_radio_metadata_v1 metadata);
    /** Optional complete remote tail. Advance existing control/DSP adoption
     * without waiting for physical rig I/O. A disabled composition may omit it. */
    int (*radio_service)(void *context, void *radio);
    /** Begin/poll retirement. Zero proves callbacks stopped and physical outputs
     * safe; one means pending. Any nonzero result retains radio and callbacks.
     * radio_destroy follows successful retirement. Hosts without this operation
     * retain the synchronous radio_destroy contract above. */
    int (*radio_quiesce)(void *context, void *radio);
    /** Copy a command into the existing physical owner. Flags: bit0 local source,
     * bit1 source active. Success transfers one complete ticket to the caller. */
    int (*radio_remote_submit)(void *context, void *radio,
                               const struct UrpRemoteOperation *operation, uint32_t flags,
                               struct rptadv_remote_ticket_v1 *ticket);
    /** Observe activation without I/O: zero is ready, one is opening, negative
     * is the first failure. Remote compositions require this complete tail. */
    int (*radio_ready)(void *context, void *radio);
    /** Optional typed physical station operations. A host advertising this must also implement
     * radio_update phases one/two for commit/rollback; two means the same finish is pending. */
    const struct rptadv_station_binding_v1 *station_binding;
    /** Optional complete private phone participant and source-timed media group. */
    const struct rptadv_phone_provider_v2 *phone_provider;
    /** Optional retained ordinary-IAX operations. An account-free host may omit this tail. */
    const struct rptadv_asl_provider_v2 *asl_provider;
    /** Optional direct EchoLink account participant and status observer. */
    const struct rptadv_managed_echo_provider_v2 *managed_echo_provider;
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
    /** Reload the checked JSON working store: zero complete, one pending, negative failed. */
    int (*reload_working)(void);
    int (*stop)(void); /**< Stop and quiesce every product owner. */
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
    /** Apply site policy on the current serialized control generation.
     * Returns 0 when policy accepts the request (not proof of physical service),
     * -1 when unavailable/reloading/stopping, -2 for unknown node/target,
     * -3 when unsafe, or -4 for an invalid request. Requested/submitted/observed
     * state and downstream hardware faults remain visible in status. This entry
     * neither changes link-command semantics nor emits a spoken CLI reply. */
    int (*site_command)(const char *local, size_t local_length,
                        const struct rptadv_site_command_v1 *request);
    /** Advance retained activation/rollback: zero completed, one pending,
     * negative failed after safe rollback. Keep host candidate settings until
     * completion. Each call is nonblocking with respect to physical rig I/O. */
    int (*reload_poll)(void);
};

/** Checked JSON management startup, negotiated independently of the product table.
 * A successful call retains providers until product.stop succeeds. -1 means startup
 * was rejected and drained; -2 requires retaining all providers and retrying stop.
 * A missing or invalid table cannot be replaced by an alternate configuration startup path. */
struct rptadv_product_management_v1 {
    uint32_t struct_size;   /**< Complete readable table size. */
    uint32_t abi_version;   /**< Exactly 1. */
    uint8_t capability[16]; /**< Fixed-width management capability identity. */
    /** Start with the host and its control, file and speech providers. The
     * provisioning JSON is a borrowed span of provisioning_length bytes.
     * Return 0 on startup, -1 after rejection and drain, or -2 if stop must be retried.
     */
    int (*start)(const struct rptadv_host_services_v11 *host,
                 const struct rptadv_control_descriptor_v1 *control,
                 const struct rptadv_file_descriptor *file,
                 const struct rptadv_speech_descriptor *speech, const char *provisioning_json,
                 size_t provisioning_length);
};
/** Return the managed startup capability.
 * @return Immutable descriptor valid while the product library is loaded.
 */
const struct rptadv_product_management_v1 *rptadv_product_management_descriptor_v1(void);

/** Synchronous optional visitor for one registered port. The UUID points to 16 network-order
 * bytes, and the exact UTF-8 key is a bounded span. Return zero to continue. */
typedef int (*rptadv_managed_port_sink_v1)(void *context, const uint8_t *uuid, const char *key,
                                           size_t key_length, uint32_t enabled);

/** Read-only service-document validation and registered port enumeration.
 * Both sink modes validate the same service TLS credentials, store identity and selected
 * candidate before any records are emitted. No process writer lease or device is opened.
 * All callbacks and spans are borrowed for this call; nonzero from a port sink aborts. */
struct rptadv_product_inspection_v1 {
    uint32_t struct_size;   /**< Complete readable table size. */
    uint32_t abi_version;   /**< Exactly 1. */
    uint8_t capability[16]; /**< Fixed-width inspection capability identity. */
    /** Validate the provisioning JSON span of the given byte length. A null
     * port sink validates without visiting ports. An optional diagnostic sink receives
     * warnings or a safe error. Each sink is called with its supplied context.
     * Return 0 on success or -1 on validation failure or visitor rejection.
     */
    int (*inspect)(const char *provisioning_json, size_t length, rptadv_managed_port_sink_v1 port,
                   void *port_context, rptadv_text_sink_v1 diagnostic, void *diagnostic_context);
};
/** Return the read-only managed inspection capability.
 * @return Immutable descriptor valid while the product library is loaded.
 */
const struct rptadv_product_inspection_v1 *rptadv_product_inspection_descriptor_v1(void);

/** Return immutable process-lifetime product metadata.
 * @return Complete product descriptor retained through successful stop.
 */
const struct rptadv_product_descriptor_v1 *rptadv_product_descriptor_v1(void);

/** Exact local protected-store maintenance ABI, independent of runtime tables. */
#define RPTADV_STORE_MAINTENANCE_ABI_VERSION 1U
/** NUL-padded capability for the optional local-only maintenance descriptor. */
#define RPTADV_STORE_MAINTENANCE_CAPABILITY "rptadv.store1"
/** Verify selected graph and report safe hashes/counts. */
#define RPTADV_STORE_INSPECT 1U
/** Collect only verified registered unreachable artifacts. */
#define RPTADV_STORE_GC 2U
/** Write a verified exact manifest/object backup tree; keys stay separate. */
#define RPTADV_STORE_BACKUP 3U
/** Restore a verified backup using a fresh deliberate revision and journal. */
#define RPTADV_STORE_RESTORE 4U
/** Re-seal retained graphs using separate delivered old/new key IDs. */
#define RPTADV_STORE_ROTATE 5U
/** Explicitly replace bounded physical recovery roots. */
#define RPTADV_STORE_RETAIN 6U
/** Select the initial zero-port site or resume its interrupted first publication. */
#define RPTADV_STORE_INIT 7U

/** Local offline protected-store entry point; never an HTTP/radio API.
 * The bounded UTF-8 request contains paths, UUIDs, hashes and symbolic key IDs,
 * never key or plaintext credential bytes. The product owns all graph/crypto
 * algorithms and acquires the actual same-root writer lifetime for mutations.
 * Backup capture needs only its consistent edit snapshot. Roots must already
 * exist with the protected service-owner mode. Strings/sinks are borrowed only
 * during this synchronous call; callbacks must not unwind. Zero means durable
 * completion/publication, never current runtime adoption; negative is a
 * value-free failure also supplied to the optional sink. Keep the product DSO
 * loaded through the complete call. No installed-service stop is performed.
 */
struct rptadv_store_maintenance_v1 {
    uint32_t struct_size;   /**< Exact complete table size. */
    uint32_t abi_version;   /**< Exact RPTADV_STORE_MAINTENANCE_ABI_VERSION. */
    uint8_t capability[16]; /**< Exact padded RPTADV_STORE_MAINTENANCE_CAPABILITY. */
    /** Execute a bounded local metadata request using the product-owned algorithms. */
    int (*run)(uint32_t operation, const char *request, size_t request_length,
               rptadv_text_sink_v1 sink, void *sink_context);
};

/** Optional checked maintenance getter, independent of the old product table.
 * @return Immutable descriptor valid while the product library is loaded.
 */
const struct rptadv_store_maintenance_v1 *rptadv_product_store_maintenance_descriptor_v1(void);

#ifdef __cplusplus
}
#endif
#endif
