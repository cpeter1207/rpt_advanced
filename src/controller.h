/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief One node's identifier, audio, and transmit-ownership integration.
 */
#ifndef RPT_ADVANCED_CONTROLLER_H
#define RPT_ADVANCED_CONTROLLER_H
#include "duplex.h"
#include "identifier.h"
#include "playback.h"

/** @brief Prepared identifier media and its resolved configuration. */
struct ra_controller_id {
    struct ra_identifier_settings settings; /**< Borrowed immutable configuration strings. */
    const int16_t *audio;                   /**< Borrowed file/speech PCM, or null. */
    size_t samples;                         /**< Prepared samples at the controller rate. */
};

/** @brief Single-threaded node state; arrays remain owned by the node lifecycle. */
struct ra_controller {
    const struct ra_controller_id *ids; /**< Immutable prepared identifiers. */
    struct ra_id_rule *rules;           /**< Writable scheduling rules, parallel to ids. */
    struct ra_id_state *states;         /**< Writable scheduling state, parallel to ids. */
    size_t count;                       /**< Number of IDs; zero permits null arrays. */
    unsigned int rate;                  /**< Negotiated PCM sample rate. */
    bool full_duplex;                   /**< Whether local reception may transmit. */
    bool link_active;                   /**< Current link receive activity, set by the router. */
    const int16_t *link_audio;          /**< Borrowed link mix for the current block, or null. */
    uint64_t hang_ms;                   /**< Transmitter hang time. */
    struct ra_duplex_state duplex;      /**< Current PTT request and hang state. */
    struct ra_playback playback;        /**< Currently selected identifier playback. */
    size_t playing;                     /**< Active identifier index, or SIZE_MAX. */
    bool receiving;                     /**< Previous qualified receiver indication. */
    uint64_t last_activity_ms;          /**< Last receive activity, initially startup. */
    uint64_t key_idle_ms;               /**< Idle period preceding the current conversation. */
};

/** @brief Validate media and initialize a controller whose array bindings are set.
 * @param state Controller with ids, rules, states, count, rate, duplex and hang configured.
 * @param now_ms Monotonic startup time.
 * @return False for an unrenderable Morse configuration; no state changes on failure.
 */
bool ra_controller_start(struct ra_controller *state, uint64_t now_ms);

/** @brief Process one hardware-paced PCM block or a sample-free carrier event.
 * @param state Successfully started controller with unchanged configuration/media.
 * @param receiving Qualified receiver indication.
 * @param audio Receive PCM replaced by transmit PCM; null only when samples is zero.
 * @param samples Number of PCM samples. Zero never advances identifier playback.
 * @param now_ms Monotonic event time, never decreasing.
 * @return Requested PTT state. Half-duplex receive always overrides transmission.
 */
bool ra_controller_process(struct ra_controller *state, bool receiving, int16_t *audio,
                           size_t samples, uint64_t now_ms);
#endif
