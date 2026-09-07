/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Bounded PCM buffering between independently clocked network and radio channels.
 */
#ifndef RPT_ADVANCED_LINK_AUDIO_H
#define RPT_ADVANCED_LINK_AUDIO_H
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>

/** @brief Lock-free single-producer/single-consumer PCM ring at a negotiated rate. */
struct ra_link_audio {
    int16_t *storage;               /**< Borrowed PCM storage. */
    size_t capacity;                /**< Nonzero sample capacity. */
    atomic_uint_fast64_t read;      /**< Consumer-owned monotonic sample position. */
    atomic_uint_fast64_t written;   /**< Producer-owned monotonic sample position. */
    atomic_uint_fast64_t discarded; /**< Samples rejected because the ring was full. */
    atomic_uint_fast64_t missing;   /**< Samples unavailable to the consumer. */
};

/** @brief Append samples without waiting for the consumer.
 * @param queue Initialized single-producer queue.
 * @param audio Input samples; null only for zero samples.
 * @param samples Input length.
 */
void ra_link_audio_write(struct ra_link_audio *queue, const int16_t *audio, size_t samples);

/** @brief Consume available samples and supply silence for unavailable samples.
 * @param queue Initialized single-consumer queue.
 * @param audio Output samples; null only for zero samples.
 * @param samples Required hardware-clocked length.
 */
void ra_link_audio_read(struct ra_link_audio *queue, int16_t *audio, size_t samples);

/** @brief Return currently published samples without reserving them.
 * @param queue Initialized lock-free ring.
 * @return Number of readable samples, bounded by capacity.
 */
size_t ra_link_audio_available(const struct ra_link_audio *queue);
#endif
