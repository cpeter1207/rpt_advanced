/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Hardware-paced Asterisk frame exchange without an independent audio timer.
 */
#ifndef RPT_ADVANCED_RADIO_H
#define RPT_ADVANCED_RADIO_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

struct ast_channel;
struct ast_format;
struct ast_trans_pvt;

/** @brief Render one receive block in place, or handle a sample-free carrier event.
 * @param context Node-owned controller state.
 * @param receiving Qualified receiver state.
 * @param audio Borrowed signed-linear samples, null for a carrier event.
 * @param samples Number of samples; zero must not advance playback.
 * @return Requested transmitter key state.
 */
typedef bool (*ra_radio_render)(void *context, bool receiving, int16_t *audio, size_t samples);

/** @brief State owned by the node thread for one configured channel. */
struct ra_radio {
    struct ast_format *linear;    /**< Borrowed negotiated signed-linear format. */
    struct ast_format *codec;     /**< Borrowed compressed format when translation is needed. */
    struct ast_trans_pvt *decode; /**< Owned by the node; null for linear input. */
    struct ast_trans_pvt *encode; /**< Owned by the node; null for linear output. */
    bool receiving;               /**< Latest qualified carrier indication. */
    bool keyed;                   /**< Last successfully requested PTT state. */
    ra_radio_render render;       /**< Controller callback; performs no blocking preparation. */
    void *context;                /**< Callback state whose lifetime covers this channel. */
};

/** @brief Read and exchange one ready frame, preserving hardware pacing.
 * @param state Initialized node-owned state with non-null format and callback.
 * @param channel Channel configured for matching signed-linear read/write formats.
 * @return Zero on success, minus one on hangup, malformed audio, or output failure.
 * The owner waits on channel readiness before calling. Control events never emit
 * audio. Linear transport returns one block per received block, including silence.
 * Codec paths may buffer samples until a complete conversion block is available.
 */
int ra_radio_exchange(struct ra_radio *state, struct ast_channel *channel);
#endif
