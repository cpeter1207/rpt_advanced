/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Joinable network peer with a bounded receive queue at the radio's PCM rate.
 */
#ifndef RPT_ADVANCED_LINK_PEER_H
#define RPT_ADVANCED_LINK_PEER_H
#include <stdarg.h>

#include "link_audio.h"
#include <asterisk/lock.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
struct ast_channel;
struct ast_format;

/** @brief Peer ownership shared by its network reader and hardware-clocked consumer. */
struct ra_link_peer {
    struct ast_channel *channel;   /**< Owned after successful start. */
    struct ast_format *linear;     /**< Borrowed Asterisk PCM cache format. */
    pthread_t thread;              /**< Joined before resources are freed. */
    ast_mutex_t lock;              /**< Protects receive PCM and radio-control state. */
    atomic_bool stop;              /**< Reader shutdown request. */
    atomic_bool ended;             /**< Reader observed hangup or a transport error. */
    struct ra_link_audio received; /**< Bounded decoded network samples. */
    bool receiving;                /**< Peer currently requests radio transmission. */
    bool voice_keying;             /**< Voice frames determine carrier after NEWKEY1. */
    bool handshake_replied;        /**< Respond to redundant-key negotiation only once. */
    size_t receive_age;            /**< Radio samples since the last receive activity refresh. */
    bool transmitting;             /**< Last successfully signaled outgoing key state. */
    size_t heartbeat_samples;      /**< Radio samples since the last redundant key indication. */
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

/** @brief Join the reader, unkey, hang up, and release a successfully started peer.
 * @param peer Started peer, called once and with no concurrent sender/consumer.
 */
void ra_link_peer_stop(struct ra_link_peer *peer);
#endif
