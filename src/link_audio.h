/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Bounded PCM buffering between independently clocked network and radio channels.
 */
#ifndef RPT_ADVANCED_LINK_AUDIO_H
#define RPT_ADVANCED_LINK_AUDIO_H
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/** @brief Compile-time proof that the selected native fast-64 atomic has no library fallback. */
#define RA_ATOMIC_UINT_FAST64_LOCK_FREE                                                            \
    _Generic((uint_fast64_t){0},                                                                   \
        unsigned char: ATOMIC_CHAR_LOCK_FREE == 2,                                                 \
        unsigned short: ATOMIC_SHORT_LOCK_FREE == 2,                                               \
        unsigned int: ATOMIC_INT_LOCK_FREE == 2,                                                   \
        unsigned long: ATOMIC_LONG_LOCK_FREE == 2,                                                 \
        unsigned long long: ATOMIC_LLONG_LOCK_FREE == 2,                                           \
        default: 0)

/** @brief Lock-free single-producer/single-consumer PCM ring at a negotiated rate. */
struct ra_link_audio {
    int16_t *storage;                            /**< Borrowed PCM storage. */
    size_t capacity;                             /**< Nonzero sample capacity. */
    atomic_uint_fast64_t read;                   /**< Consumer-owned monotonic sample position. */
    atomic_uint_fast64_t written;                /**< Producer-owned monotonic sample position. */
    atomic_uint_fast64_t discarded;              /**< Samples rejected because the ring was full. */
    atomic_uint_fast64_t missing;                /**< Samples unavailable to the consumer. */
    atomic_uint_fast64_t consecutive_underruns;  /**< Current consecutive missing PCM samples. */
    atomic_uint_fast64_t underrun_average_milli; /**< Ten-second EWMA of missing samples. */
    atomic_uint_fast64_t reserve_samples;        /**< Current elastic reserve in PCM samples. */
};

/** @brief Initialize a preallocated single-producer/single-consumer PCM ring. */
void ra_link_audio_init(struct ra_link_audio *queue, int16_t *storage, size_t capacity);

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

/** @brief Record one output shortfall without modifying queue positions.
 * @param queue Initialized single-consumer queue.
 * @param missing PCM output samples that could not be rendered.
 * @param samples Required hardware-clocked output length.
 * @param rate PCM sample rate used for the ten-second statistics window.
 */
void ra_link_audio_record_shortfall(struct ra_link_audio *queue, size_t missing, size_t samples,
                                    unsigned int rate);

/** @brief Return currently published samples without reserving them.
 * @param queue Initialized lock-free ring.
 * @return Number of readable samples, bounded by capacity.
 */
size_t ra_link_audio_available(const struct ra_link_audio *queue);
#endif
