/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Per-node peer ownership and hardware-paced mix-minus routing.
 */
#ifndef RPT_ADVANCED_LINK_HUB_H
#define RPT_ADVANCED_LINK_HUB_H
#include "controller.h"
#include <pthread.h>
#include <stdatomic.h>
struct ast_channel;
struct ast_format;
struct ra_link_port;
/** @brief Redial callback invoked by the hub reaper for a permanent peer.
 * @param context Runtime-owned callback context.
 * @param remote Decimal remote node identity.
 * @param transmit Preserve outbound audio mode.
 * @param forward Preserve forwarding mode.
 * @return Zero when a replacement peer was attached.
 */
typedef int (*ra_link_reconnect_fn)(void *context, const char *remote, bool transmit, bool forward);

/** @brief Node-owned routing state; zero initialization is sufficient. */
struct ra_link_hub {
    struct ra_link_port *ports;     /**< Owned peer list. */
    int16_t *local;                 /**< Original local receive block. */
    int16_t *remote;                /**< Summed incoming audio for the transmitter. */
    int16_t *outgoing;              /**< Per-peer mix-minus scratch block. */
    size_t capacity;                /**< Allocated samples per scratch block. */
    pthread_t manager;              /**< Reaps disconnected peers outside the audio thread. */
    atomic_bool stop;               /**< Requests manager shutdown. */
    bool manager_started;           /**< Manager must be joined before releasing the hub. */
    ra_link_reconnect_fn reconnect; /**< Runtime callback for permanent peers. */
    void *reconnect_context;        /**< Borrowed runtime callback context. */
};

/** @brief Attach an authenticated answered peer without changing a live list on failure.
 * @param hub Node-owned hub.
 * @param name Verified remote node name.
 * @param channel Answered channel, ownership transfers on success only.
 * @param linear Cached local PCM format.
 * @param transmit Send audio to this peer; false selects monitor mode.
 * @param forward Forward received audio to other peers; false selects local-monitor mode.
 * @param permanent Redial after an unexpected transport failure.
 * @return Zero on success, minus one on allocation, duplicate, or transport failure.
 */
int ra_link_hub_attach(struct ra_link_hub *hub, const char *name, struct ast_channel *channel,
                       struct ast_format *linear, bool transmit, bool forward, bool permanent);

/** @brief Set the callback used to restore permanent peers after transport failure.
 * @param hub Node-owned hub.
 * @param callback Borrowed callback, or null to disable recovery.
 * @param context Borrowed callback context.
 */
void ra_link_hub_set_reconnector(struct ra_link_hub *hub, ra_link_reconnect_fn callback,
                                 void *context);

/** @brief Disconnect one peer, after removing it from the hardware-visible list.
 * @param hub Node-owned hub.
 * @param name Exact remote node name.
 * @return True if the peer existed and was released.
 */
bool ra_link_hub_disconnect(struct ra_link_hub *hub, const char *name);

/** @brief Disconnect all peers and release routing buffers after the radio worker stops.
 * @param hub Owned hub, safe when empty.
 */
void ra_link_hub_close(struct ra_link_hub *hub);

/** @brief Route one hardware block and render the local transmitter.
 * @param hub Node-owned hub.
 * @param controller Started radio controller.
 * @param receiving Qualified local reception.
 * @param audio Local receive PCM replaced by transmitter PCM.
 * @param samples Hardware block length; zero handles control events only.
 * @param now_ms Monotonic controller time.
 * @return Requested transmitter key state.
 */
bool ra_link_hub_process(struct ra_link_hub *hub, struct ra_controller *controller, bool receiving,
                         int16_t *audio, size_t samples, uint64_t now_ms);
#endif
