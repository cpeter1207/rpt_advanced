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

/** @brief Deliver one validated remote IAX DTMF end event outside audio processing.
 * @param context Borrowed control-plane callback context.
 * @param digit Completed conventional DTMF character.
 */
typedef void (*ra_link_peer_digit_fn)(void *context, char digit);

/** @brief Peer ownership shared by its network reader and hardware-clocked consumer. */
struct ra_link_peer {
    struct ast_channel *channel;      /**< Owned after successful start. */
    struct ast_format *linear;        /**< Borrowed Asterisk PCM cache format. */
    unsigned int linear_rate;         /**< Immutable negotiated PCM rate for hardware callbacks. */
    pthread_t thread;                 /**< Joined before resources are freed. */
    atomic_bool stop;                 /**< Reader shutdown request. */
    atomic_bool ended;                /**< Reader observed hangup or a transport error. */
    struct ra_link_audio received;    /**< Reader-to-hardware PCM ring. */
    struct ra_link_audio outgoing;    /**< Hardware-to-reader PCM ring. */
    int16_t *outgoing_storage;        /**< Owned outbound ring storage. */
    int16_t *send_buffer;             /**< Reader-owned outbound frame buffer. */
    struct ast_trans_pvt *decode;     /**< Asterisk codec-to-linear translator. */
    struct ast_format *decode_format; /**< Format currently served by decode. */
    atomic_bool receiving;            /**< Control-frame receive state. */
    atomic_bool voice_keying;         /**< Voice frames determine carrier after NEWKEY1. */
    atomic_uint_fast64_t receive_epoch;  /**< Reader increments this for each voice frame. */
    atomic_bool desired_key;             /**< Hardware worker's outbound key state. */
    atomic_uint_fast64_t sent_samples;   /**< Hardware worker's outbound sample count. */
    uint64_t seen_epoch;                 /**< Consumer's last observed inbound voice epoch. */
    size_t receive_age;                  /**< Consumer samples since the last voice frame. */
    bool transmitting;                   /**< Reader's last signaled outgoing key state. */
    uint64_t heartbeat_samples;          /**< Reader's last outbound heartbeat position. */
    char digits[64];                     /**< Control-thread-to-reader DTMF ring storage. */
    atomic_uint digit_head;              /**< Next DTMF slot written by the control executor. */
    atomic_uint digit_tail;              /**< Next DTMF slot read by the network reader. */
    ra_link_peer_digit_fn inbound_digit; /**< Borrowed reader-to-control DTMF callback. */
    void *inbound_digit_context;         /**< Borrowed context paired with inbound_digit. */
    bool inbound_timeout_pending; /**< Reader schedules a terminator after the most recent digit. */
    uint64_t
        inbound_timeout_deadline_ms; /**< Monotonic deadline for that control-plane terminator. */
    ast_mutex_t topology_lock;       /**< Protects topology cache outside the audio path. */
    bool topology_lock_initialized;  /**< Topology lock requires destruction after start. */
    size_t topology_length;          /**< Bytes currently stored in topology. */
    char topology[RA_LINK_TOPOLOGY_TEXT_MAX + 1]; /**< Latest validated peer `L` payload. */
    atomic_uint
        *topology_generation; /**< Borrowed hub change counter, or null for standalone use. */
    size_t advertised_length; /**< Bytes awaiting reader-owned IAX text delivery. */
    bool advertised_pending;  /**< A newer outbound topology replaces an unsent one. */
    char advertised[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1]; /**< Pending `L ` payload. */
};

/** @brief Configure Asterisk conversions and start reading a connected peer.
 * @param peer Zero-initialized destination.
 * @param channel Answered IAX channel; ownership transfers only on success.
 * @param linear Cached signed-linear format matching the negotiated peer wire rate.
 * @param inbound_digit Optional reader-thread DTMF-end callback.
 * @param inbound_digit_context Borrowed callback context.
 * @return Zero on success; minus one with caller retaining channel otherwise.
 */
int ra_link_peer_start(struct ra_link_peer *peer, struct ast_channel *channel,
                       struct ast_format *linear, ra_link_peer_digit_fn inbound_digit,
                       void *inbound_digit_context);

/** @brief Receive one radio-paced block, without waiting for network audio.
 * @param peer Successfully started peer.
 * @param audio Output buffer.
 * @param samples Hardware block length.
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
 * @param peer Started peer, called once and with no concurrent sender/consumer.
 */
void ra_link_peer_stop(struct ra_link_peer *peer);
#endif
