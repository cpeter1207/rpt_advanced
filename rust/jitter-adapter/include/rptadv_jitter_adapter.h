/** @file rptadv_jitter_adapter.h
 * @brief Versioned, single-owner adaptive packet jitter capability.
 *
 * Every handle is used serially; independent handles may run concurrently.
 * These are non-real-time operations. Stop callers before destroy and retain
 * this DSO until all handles are destroyed. No callback or borrowed payload is
 * retained. No function may unwind. Negative status rejects invalid arguments
 * or native allocation failure. now_ns must not move backwards within an epoch.
 * Packet timestamps are already unwrapped media-clock units; the caller owns
 * transport parsing, codec decoding, PLC and sample-rate conversion.
 */
#ifndef RPTADV_JITTER_ADAPTER_H
#define RPTADV_JITTER_ADAPTER_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

#define RPTADV_JITTER_ABI_VERSION 1U /**< Exact descriptor/configuration ABI. */
#define RPTADV_JITTER_PACKET 0U      /**< Returned opaque payload. */
#define RPTADV_JITTER_MISSING 1U     /**< Missing media-clock span; caller owns PLC. */
#define RPTADV_JITTER_INSERTION 2U   /**< Delay-insertion span; no media timestamp advance. */
#define RPTADV_JITTER_NOT_DUE 3U     /**< No playout is due yet. */
#define RPTADV_JITTER_DUPLICATE 1    /**< Duplicate put, counted without insertion. */
#define RPTADV_JITTER_LATE 2         /**< Expired put, counted without insertion. */
#define RPTADV_JITTER_FULL 3         /**< Full storage rejects the new packet. */

/** Immutable bounds, in media-clock units except explicit byte/count fields. */
typedef struct rptadv_jitter_config_v1 {
    uint32_t struct_size;           /**< Complete readable configuration size. */
    uint32_t abi_version;           /**< RPTADV_JITTER_ABI_VERSION. */
    uint32_t clock_rate_hz;         /**< Media-clock ticks per second, at most 48000. */
    uint32_t minimum_delay;         /**< Minimum packet-buffer delay. */
    uint32_t initial_delay;         /**< Startup packet-buffer delay. */
    uint32_t maximum_delay;         /**< Maximum packet-buffer delay. */
    uint32_t adjustment_step;       /**< Positive multiple of concealment_span. */
    uint32_t concealment_span;      /**< Smallest missing span. */
    uint32_t maximum_packets;       /**< Bounded opaque packet slots, 1 through 200. */
    uint32_t maximum_payload_bytes; /**< Largest opaque payload, 1 through 65527 bytes.
                                     * Only occupied packets allocate payload storage. */
} rptadv_jitter_config_v1;

/** One received packet, copied before put returns. */
typedef struct rptadv_jitter_packet_v1 {
    uint32_t struct_size;   /**< Complete readable packet size. */
    uint64_t timestamp;     /**< Unwrapped media timestamp. */
    uint32_t span;          /**< Positive media duration. */
    uint64_t arrival_ns;    /**< Captured monotonic receive time, before async scheduling. */
    const uint8_t *payload; /**< Borrowed opaque codec bytes. */
    size_t length;          /**< Positive payload byte count. */
} rptadv_jitter_packet_v1;

/** One due playout decision. */
typedef struct rptadv_jitter_result_v1 {
    uint32_t struct_size; /**< Caller-initialized complete writable size. */
    uint32_t kind;        /**< PACKET, MISSING, INSERTION or NOT_DUE. */
    uint64_t timestamp;   /**< Unwrapped media position of the packet/span. */
    uint32_t span;        /**< Actual duration, which can differ from requested duration. */
    int32_t start_offset; /**< Packet start relative to the requested media position. */
    size_t length;        /**< Copied packet bytes, zero for non-packet decisions. */
    uint64_t deadline_ns; /**< Earliest monotonic deadline for this decision. */
} rptadv_jitter_result_v1;

/** Owner-thread diagnostics; reset clears counters and buffered packets. */
typedef struct rptadv_jitter_observation_v1 {
    uint32_t struct_size;     /**< Caller-initialized complete writable size. */
    uint32_t queued_packets;  /**< Currently retained packet count. */
    uint32_t delay;           /**< Current packet-buffer delay in media-clock units. */
    uint64_t buffered_span;   /**< Sum of queued packet spans, including any overlaps. */
    uint64_t duplicate_drops; /**< Duplicate packets rejected. */
    uint64_t late_drops;      /**< Packets discarded after their useful media interval. */
    uint64_t full_drops;      /**< New packets rejected by the configured slot bound. */
    uint64_t missing_span;    /**< Missing media duration returned. */
    uint64_t insertion_span;  /**< Delay-insertion duration returned. */
    uint64_t skipped_span;    /**< Media duration skipped during delay reduction. */
    uint64_t limit_resets;    /**< Adaptive state resets at the maximum delay bound. */
    uint64_t overload_drops;  /**< Queued packets discarded by those overload resets. */
    uint64_t overload_span;   /**< Sum of discarded packet spans, including overlaps. */
} rptadv_jitter_observation_v1;

/** Stable ABI 1 table. Every operation is required. */
typedef struct rptadv_jitter_descriptor_v1 {
    uint32_t struct_size;   /**< Complete readable descriptor size. */
    uint32_t abi_version;   /**< RPTADV_JITTER_ABI_VERSION. */
    uint8_t capability[16]; /**< NUL-padded rptadv.jitter. */
    /** Create one exclusive context; failure clears the output handle. */
    int32_t (*create)(const rptadv_jitter_config_v1 *config, void **context);
    void (*destroy)(void *context);  /**< Destroy once; NULL is harmless. */
    int32_t (*reset)(void *context); /**< Begin a new timestamp/arrival epoch. */
    /** Copy one packet; zero accepts it, positive status counts a rejected packet.
     * Native resynchronization after >20 failed fetches starts a new arrival epoch;
     * discarded packets are counted as late, and cumulative counters are retained.
     */
    int32_t (*put)(void *context, const rptadv_jitter_packet_v1 *packet);
    /** Fetch due media into a buffer at least maximum_payload_bytes long.
     * Actual span can be shorter/longer than desired_span, including partial
     * packets and missing spans of concealment_span. Positive start_offset is
     * missing prefix; negative offset skips already released packet samples.
     * NOT_DUE requires no advance. A proposed delay above maximum_delay clears
     * queued packets and adaptive state, counts overload loss, and returns
     * NOT_DUE; the next packet starts a fresh arrival+initial_delay epoch.
     */
    int32_t (*get)(void *context, uint64_t now_ns, uint32_t desired_span, uint8_t *payload,
                   size_t capacity, rptadv_jitter_result_v1 *result);
    /** Commit exactly one non-NOT_DUE decision before another get or put.
     * Advance uses the actual returned span plus start_offset, not desired_span;
     * insertion consumes output time without advancing the media position.
     * downstream_span is decoded lookahead not yet published to the PCM ring,
     * never ring occupancy/reserve. Zero means all returned output was published.
     * Publish any retained tail before the next due get. Its duration is withheld
     * once here and included once when committing that next decision.
     */
    int32_t (*advance)(void *context, uint64_t now_ns, uint32_t downstream_span);
    /** Copy current owner-thread diagnostics. */
    int32_t (*observe)(const void *context, rptadv_jitter_observation_v1 *result);
} rptadv_jitter_descriptor_v1;

/** Return the immutable descriptor; retain the shared object while using it.
 * @return Immutable ABI1 table, never NULL; the caller must not free it.
 */
const rptadv_jitter_descriptor_v1 *rptadv_jitter_adapter_descriptor_v1(void);
#ifdef __cplusplus
}
#endif
#endif
