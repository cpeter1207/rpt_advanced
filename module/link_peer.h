/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Joinable network peer with a bounded receive queue at its negotiated PCM rate.
 */
#ifndef RPT_ADVANCED_LINK_PEER_H
#define RPT_ADVANCED_LINK_PEER_H
#include <stdarg.h>

#include "link_audio.h"
#include <asterisk/lock.h>
#include <pthread.h>
#include <rate_adjusting_pcm_ring.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
struct ast_channel;
struct ast_format;
struct ast_trans_pvt;

/** @brief Maximum cached route-payload bytes accepted from an app_rpt IAX `L` message. */
#define RA_LINK_TOPOLOGY_TEXT_MAX 10000
/** @brief Largest outbound `L ` payload that remains within the legacy IAX text bound. */
#define RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX (RA_LINK_TOPOLOGY_TEXT_MAX - 2)
/** @brief Interdigit interval after which the peer reader emits one command terminator. */
#define RA_LINK_PEER_DTMF_TIMEOUT_MS 3000U
/** @brief Minimum incoming PCM retained during a brief IAX media shortage. */
#define RA_LINK_RECEIVE_RESERVE_MS 60U
/** @brief Total incoming PCM capacity retained for jitter and rate correction. */
#define RA_LINK_RECEIVE_CAPACITY_MS 300U
/** @brief Clock-recovery occupancy target, independent of the protected reserve. */
#define RA_LINK_RECEIVE_TARGET_MS 260U
/** @brief Maximum direct-peer or keyed-source identity length including the terminating null. */
#define RA_LINK_PEER_NAME_MAX 64
/** @brief Number of reader-owned legacy keyed-source control messages retained per peer. */
#define RA_LINK_PEER_KEY_QUEUE_CAPACITY 16U
/** @brief Maximum complete bounded legacy keyed-source IAX text message including its terminator.
 */
#define RA_LINK_PEER_KEY_TEXT_MAX (2U * RA_LINK_PEER_NAME_MAX + 32U)

/** @brief Deliver one validated remote IAX DTMF end event outside audio processing.
 * @param context Borrowed control-plane callback context.
 * @param digit Completed conventional DTMF character.
 */
typedef void (*ra_link_peer_digit_fn)(void *context, char digit);

/** @brief Classify one validated canonical legacy-compatible keyed-source IAX message. */
enum ra_link_peer_key_kind {
    RA_LINK_PEER_KEY_QUERY, /**< `K? * requester 0 0` broadcast source query. */
    RA_LINK_PEER_KEY_REPLY  /**< `K requester reporter keyed age` source response. */
};

/** @brief Deliver one validated canonical legacy-compatible keyed-source message outside audio
 * processing.
 * @param context Borrowed control-plane callback context.
 * @param kind Query or reply message class.
 * @param destination Query wildcard requester target or reply destination.
 * @param source Query requester or reply reporter identity.
 * @param keyed Reply carrier state; false for queries.
 * @param age_seconds Reply age in whole seconds; zero for queries.
 *
 * The reader invokes this only after strict parsing of the canonical protocol form. Every string
 * remains valid only for the duration of this synchronous reader-thread callback. The callback
 * must copy data before retaining it, queue any relay text for reader-owned delivery, and never
 * call an Asterisk channel API.
 */
typedef void (*ra_link_peer_key_fn)(void *context, enum ra_link_peer_key_kind kind,
                                    const char *destination, const char *source, bool keyed,
                                    uint64_t age_seconds);

/** @brief Peer ownership shared by its network reader and hardware-clocked consumer. */
struct ra_link_peer {
    struct ast_channel *channel;      /**< Owned after successful start. */
    struct ast_format *linear;        /**< Borrowed Asterisk PCM cache format. */
    unsigned int linear_rate;         /**< Immutable negotiated PCM rate for hardware callbacks. */
    pthread_t thread;                 /**< Joined before resources are freed. */
    atomic_bool stop;                 /**< Reader shutdown request. */
    atomic_bool ended;                /**< Reader observed hangup or a transport error. */
    struct rpcr_ring received;        /**< Reader-to-hardware elastic PCM ring. */
    struct ra_link_audio outgoing;    /**< Hardware-to-reader PCM ring. */
    int16_t *outgoing_storage;        /**< Owned outbound ring storage. */
    int16_t *send_buffer;             /**< Reader-owned outbound frame buffer. */
    struct ast_trans_pvt *decode;     /**< Asterisk codec-to-linear translator. */
    struct ast_format *decode_format; /**< Format currently served by decode. */
    atomic_uint_fast64_t receive_epoch;  /**< Reader increments this for each voice frame. */
    uint64_t seen_epoch;                 /**< Consumer's last observed inbound voice epoch. */
    size_t receive_age;                  /**< Consumer samples since the last voice frame. */
    bool receive_primed;                 /**< Consumer has reached its initial playout target. */
    char digits[64];                     /**< Control-thread-to-reader DTMF ring storage. */
    atomic_uint digit_head;              /**< Next DTMF slot written by the control executor. */
    atomic_uint digit_tail;              /**< Next DTMF slot read by the network reader. */
    ra_link_peer_digit_fn inbound_digit; /**< Borrowed reader-to-control DTMF callback. */
    void *inbound_digit_context;         /**< Borrowed context paired with inbound_digit. */
    ra_link_peer_key_fn inbound_key;     /**< Borrowed reader-to-hub keyed-source relay callback. */
    void *inbound_key_context;           /**< Borrowed context paired with inbound_key. */
    bool inbound_timeout_pending; /**< Reader schedules a terminator after the most recent digit. */
    uint64_t
        inbound_timeout_deadline_ms; /**< Monotonic deadline for that control-plane terminator. */
    char key_query_requester[RA_LINK_PEER_NAME_MAX]; /**< Immutable local node identity for `K?`. */
    char key_query_direct[RA_LINK_PEER_NAME_MAX];    /**< Immutable direct peer excluded from
                                                        downstream selection. */
    atomic_uint_fast64_t
        key_query_active_generation; /**< Query generation permitted during direct receive. */
    atomic_uint_fast64_t key_query_generation; /**< Audio-side request epoch for one `K?` query. */
    atomic_uint_fast64_t key_query_start_generation; /**< First query epoch for active receive. */
    uint_fast64_t key_query_attempted; /**< Reader-owned epoch already attempted on the channel. */
    uint_fast64_t key_query_sent;      /**< Reader-owned epoch already sent to the peer. */
    bool key_query_responded; /**< Reader already retained this query's first keyed responder. */
    atomic_uint_fast64_t
        key_source_sequence; /**< Lock-free coherent-copy sequence for keyed-source state. */
    atomic_uint_fast64_t key_source_generation; /**< Query epoch paired with the source snapshot. */
    atomic_char key_source[RA_LINK_PEER_NAME_MAX]; /**< Reader-published source identity bytes. */
    ast_mutex_t topology_lock;      /**< Protects topology and reader-owned keyed-source queues. */
    bool topology_lock_initialized; /**< Topology lock requires destruction after start. */
    size_t topology_length;         /**< Bytes currently stored in topology. */
    char topology[RA_LINK_TOPOLOGY_TEXT_MAX + 1]; /**< Latest validated peer `L` payload. */
    atomic_uint
        *topology_generation; /**< Borrowed hub change counter, or null for standalone use. */
    size_t advertised_length; /**< Bytes awaiting reader-owned IAX text delivery. */
    bool advertised_pending;  /**< A newer outbound topology replaces an unsent one. */
    char advertised[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1]; /**< Pending `L ` payload. */
    char key_messages[RA_LINK_PEER_KEY_QUEUE_CAPACITY]
                     [RA_LINK_PEER_KEY_TEXT_MAX]; /**< Ordered
                                                      reader-bound
                                                      keyed-source text. */
    unsigned int key_message_head; /**< Next keyed-source text slot written by control plane. */
    unsigned int key_message_tail; /**< Next keyed-source text slot sent by reader. */
};

/** @brief Configure Asterisk conversions and start reading a connected peer.
 * @param peer Zero-initialized destination.
 * @param channel Answered IAX channel; ownership transfers only on success.
 * @param linear Cached signed-linear format matching the negotiated peer wire rate.
 * @param inbound_digit Optional reader-thread DTMF-end callback.
 * @param inbound_digit_context Borrowed callback context.
 * @param inbound_key Optional reader-thread canonical keyed-source relay callback.
 * @param inbound_key_context Borrowed callback context.
 * @return Zero on success; minus one with caller retaining channel otherwise.
 */
int ra_link_peer_start(struct ra_link_peer *peer, struct ast_channel *channel,
                       struct ast_format *linear, ra_link_peer_digit_fn inbound_digit,
                       void *inbound_digit_context, ra_link_peer_key_fn inbound_key,
                       void *inbound_key_context);

/** @brief Receive one radio-paced block, without waiting for network audio.
 * @param peer Successfully started peer.
 * @param audio Output buffer.
 * @param samples Hardware block length. Requests exceeding the preallocated
 * converter workspace render one bounded block followed by silence.
 * @return Peer receive activity; false after hangup or unkey.
 */
bool ra_link_peer_receive(struct ra_link_peer *peer, int16_t *audio, size_t samples);

/** @brief Send a hardware-paced block and any required key transition.
 * @param peer Successfully started peer.
 * @param keyed Whether this peer should receive program audio.
 * @param audio PCM buffer, at peer.linear's sample rate.
 * @param samples Sample count, bounded by an Asterisk frame's integer fields.
 * @return Zero on success; minus one on signaling/audio failure.
 */
int ra_link_peer_send(struct ra_link_peer *peer, bool keyed, const int16_t *audio, size_t samples);

/** @brief Queue one remote-command digit for IAX delivery by the peer reader.
 * @param peer Started peer.
 * @param digit DTMF character accepted by Asterisk's IAX sender.
 * @return Zero when queued, minus one after hangup, for an invalid digit, or when full.
 * The control executor is the sole producer and the network reader is the sole consumer.
 */
int ra_link_peer_send_digit(struct ra_link_peer *peer, char digit);

/** @brief Begin one direct receive epoch and request its first legacy-compatible keyed-source
 * query.
 * @param peer Started direct peer.
 *
 * The hardware-paced caller advances lock-free epoch counters only. The first accepted downstream
 * reply to each successfully sent query is eligible for courtesy selection; a snapshot from an
 * earlier transmission is never reused.
 */
void ra_link_peer_begin_keyed_source(struct ra_link_peer *peer);

/** @brief End one direct receive epoch and cancel any unsent locally originated keyed-source query.
 * @param peer Started direct peer.
 *
 * The hardware-paced caller clears a lock-free receive-active gate before courtesy selection.
 * The reader then cannot begin a new local `K?` request or accept a reply as this transmission's
 * source. An already-started IAX write cannot be recalled, and transit relay traffic is separate.
 * A snapshot accepted before this edge remains available to that edge's courtesy selection.
 */
void ra_link_peer_end_keyed_source(struct ra_link_peer *peer);

/** @brief Request the next periodic legacy-compatible keyed-source query during active receive.
 * @param peer Started direct peer.
 *
 * The channel-owning reader emits canonical `K? * local-node 0 0`. A text-send failure does not
 * affect link media. A reply naming the direct peer is not downstream evidence. The first valid
 * remaining keyed reply delivered for each successfully sent query wins; later replies do not
 * replace it.
 */
void ra_link_peer_request_key_query(struct ra_link_peer *peer);

/** @brief Queue one canonical legacy-compatible keyed-source broadcast query for reader-owned IAX
 * delivery.
 * @param peer Started peer.
 * @param requester Valid origin node identity.
 * @return Zero when retained, minus one when unavailable, invalid, or queue-full.
 *
 * This is a control-plane relay API. It never writes an Asterisk channel from its caller.
 */
int ra_link_peer_queue_key_query(struct ra_link_peer *peer, const char *requester);

/** @brief Queue one canonical legacy-compatible keyed-source reply for reader-owned IAX delivery.
 * @param peer Started peer.
 * @param destination Valid requesting-node identity.
 * @param source Valid reporting-node identity.
 * @param keyed Current reporting receiver carrier state.
 * @param age_seconds Whole seconds since the reporting receiver's last key transition.
 * @return Zero when retained, minus one when unavailable, invalid, or queue-full.
 *
 * This is a control-plane relay API. It never writes an Asterisk channel from its caller.
 */
int ra_link_peer_queue_key_reply(struct ra_link_peer *peer, const char *destination,
                                 const char *source, bool keyed, uint64_t age_seconds);

/** @brief Copy the latest accepted first-per-query legacy-compatible keyed-source response.
 * @param peer Started direct peer.
 * @param output Destination for the complete source identity, or null when only testing.
 * @param capacity Bytes available in @p output, including its terminator.
 * @return True for the latest accepted first-per-query keyed downstream response since the current
 * receive epoch began.
 *
 * This lock-free snapshot never tags individual PCM frames. It is advisory control-plane evidence
 * for a linked transmission and is unavailable until a current receive epoch accepts a downstream
 * reply.
 */
bool ra_link_peer_keyed_source(const struct ra_link_peer *peer, char *output, size_t capacity);

/** @brief Copy the latest validated linked-node list advertised by one peer.
 * @param peer Started peer.
 * @param output Optional destination for a terminating-null string.
 * @param capacity Bytes available in output, including its terminator.
 * @return Full cached payload length; a return value at least capacity means truncation.
 * This control-plane snapshot never blocks the hardware-paced audio consumer.
 */
size_t ra_link_peer_topology(struct ra_link_peer *peer, char *output, size_t capacity);

/** @brief Queue a bounded linked-node list for IAX delivery by the channel-owning reader.
 * @param peer Started peer.
 * @param topology Valid comma-separated app_rpt route payload, without the leading `L `.
 * @return Zero when the latest payload is queued, minus one for invalid input or an ended peer.
 *
 * The reader emits `L ` followed by this payload on its next control iteration. Replacing an
 * unsent message deliberately coalesces topology changes without touching the radio callback.
 */
int ra_link_peer_queue_topology(struct ra_link_peer *peer, const char *topology);

/** @brief Join the reader, unkey, hang up, and release a successfully started peer.
 * @param[in,out] peer Started peer, called once and with no concurrent sender/consumer.
 */
void ra_link_peer_stop(struct ra_link_peer *peer);

/** @brief Stop and release a started peer while preserving caller channel ownership.
 * @param[in,out] peer Started peer, called once and with no concurrent sender/consumer.
 *
 * This is used only when a peer starts successfully but fails final hub publication. The caller
 * retains the answered channel under the attach API's failure contract and must hang it up once
 * after this helper returns.
 */
void ra_link_peer_stop_preserve_channel(struct ra_link_peer *peer);
#endif
