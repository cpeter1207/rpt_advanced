/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Radio transmit ownership and hang-time policy.
 */
#ifndef RPT_ADVANCED_DUPLEX_H
#define RPT_ADVANCED_DUPLEX_H

#include <stdbool.h>
#include <stdint.h>

/** @brief State owned by one node's control thread. */
struct ra_duplex_state {
    bool keyed;               /**< Whether the controller currently requests PTT. */
    uint64_t last_audio_ms;   /**< Last monotonic time at which transmit audio was requested. */
    uint64_t release_hang_ms; /**< Tail selected by the most recent transmit-audio source. */
};

/** @brief Apply local receiver and program-audio requests to the transmitter.
 * @param state Node-owned state, initially zero.
 * @param full_duplex True permits receive audio to repeat and concurrent reception.
 * @param receiver_active Qualified receiver indication.
 * @param transmit_active Program audio is currently available for transmission.
 * @param now_ms Monotonic time, no earlier than last_audio_ms.
 * @param hang_ms Milliseconds to hold PTT after this transmit audio ends; zero releases
 * immediately.
 * @return Whether PTT should be asserted. Half-duplex reception overrides hang time.
 */
bool ra_duplex_update(struct ra_duplex_state *state, bool full_duplex, bool receiver_active,
                      bool transmit_active, uint64_t now_ms, uint64_t hang_ms);

#endif
