/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Bounded PCM buffering between independently clocked network and radio channels.
 */
#ifndef RPT_ADVANCED_LINK_AUDIO_H
#define RPT_ADVANCED_LINK_AUDIO_H
#include <stddef.h>
#include <stdint.h>

/** @brief Caller-locked ring; storage and capacity are selected at the negotiated rate. */
struct ra_link_audio {
    int16_t *storage;   /**< Borrowed PCM storage. */
    size_t capacity;    /**< Nonzero sample capacity. */
    size_t head;        /**< Oldest unread sample. */
    size_t count;       /**< Number of unread samples. */
    uint64_t discarded; /**< Samples discarded when late consumption exhausts capacity. */
    uint64_t missing;   /**< Samples unavailable when the hardware requests audio. */
};

/** @brief Append samples, retaining the newest bounded window on overflow.
 * @param queue Initialized queue, locked by its owner.
 * @param audio Input samples; null only for zero samples.
 * @param samples Input length.
 */
void ra_link_audio_write(struct ra_link_audio *queue, const int16_t *audio, size_t samples);

/** @brief Consume available samples and supply silence for unavailable samples.
 * @param queue Initialized queue, locked by its owner.
 * @param audio Output samples; null only for zero samples.
 * @param samples Required hardware-clocked length.
 */
void ra_link_audio_read(struct ra_link_audio *queue, int16_t *audio, size_t samples);
#endif
