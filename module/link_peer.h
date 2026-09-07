/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Joinable network peer with a bounded receive queue at the radio's PCM rate.
 */
#ifndef RPT_ADVANCED_LINK_PEER_H
#define RPT_ADVANCED_LINK_PEER_H
#include <stdarg.h>

#include "link_audio.h"
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
struct ast_channel;
struct ast_format;
struct ast_trans_pvt;

/** @brief Peer ownership shared by its network reader and hardware-clocked consumer. */
struct ra_link_peer {
    struct ast_channel *channel;        /**< Owned after successful start. */
    struct ast_format *linear;          /**< Borrowed Asterisk PCM cache format. */
    pthread_t thread;                   /**< Joined before resources are freed. */
    atomic_bool stop;                   /**< Reader shutdown request. */
    atomic_bool ended;                  /**< Reader observed hangup or a transport error. */
    struct ra_link_audio received;      /**< Reader-to-hardware PCM ring. */
    struct ra_link_audio outgoing;      /**< Hardware-to-reader PCM ring. */
    int16_t *outgoing_storage;          /**< Owned outbound ring storage. */
    int16_t *send_buffer;               /**< Reader-owned outbound frame buffer. */
    struct ast_trans_pvt *decode;       /**< Asterisk codec-to-linear translator. */
    struct ast_format *decode_format;   /**< Format currently served by decode. */
    atomic_bool receiving;              /**< Control-frame receive state. */
    atomic_bool voice_keying;           /**< Voice frames determine carrier after NEWKEY1. */
    atomic_uint_fast64_t receive_epoch; /**< Reader increments this for each voice frame. */
    atomic_bool desired_key;            /**< Hardware worker's outbound key state. */
    atomic_uint_fast64_t sent_samples;  /**< Hardware worker's outbound sample count. */
    bool handshake_replied;             /**< Respond to redundant-key negotiation only once. */
    uint64_t seen_epoch;                /**< Consumer's last observed inbound voice epoch. */
    size_t receive_age;                 /**< Consumer samples since the last voice frame. */
    bool transmitting;                  /**< Reader's last signaled outgoing key state. */
    uint64_t heartbeat_samples;         /**< Reader's last outbound heartbeat position. */
    char digits[64];                    /**< Control-thread-to-reader DTMF ring storage. */
    atomic_uint digit_head;             /**< Next DTMF slot written by the control executor. */
    atomic_uint digit_tail;             /**< Next DTMF slot read by the network reader. */
};

/** @brief Configure Asterisk conversions and start reading a connected peer.
 * @param peer Zero-initialized destination.
 * @param channel Answered IAX channel; ownership transfers only on success.
 * @param linear Cached signed-linear format at the local radio rate.
 * @return Zero on success; minus one with caller retaining channel otherwise.
 */
int ra_link_peer_start(struct ra_link_peer *peer, struct ast_channel *channel,
                       struct ast_format *linear);

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

/** @brief Join the reader, unkey, hang up, and release a successfully started peer.
 * @param peer Started peer, called once and with no concurrent sender/consumer.
 */
void ra_link_peer_stop(struct ra_link_peer *peer);
#endif
