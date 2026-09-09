/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Keep network scheduling delays from allocating or blocking on the audio path.
 */
#include "link_audio.h"

_Static_assert(RA_ATOMIC_UINT_FAST64_LOCK_FREE, "link audio counters must not call libatomic");

/** @brief Initialize a preallocated PCM ring and its atomic positions.
 * @param queue Caller-owned ring storage.
 * @param storage Preallocated PCM sample storage.
 * @param capacity Nonzero storage capacity in samples.
 *
 * This explicitly initializes every atomic cursor before either endpoint can access the queue.
 */
void ra_link_audio_init(struct ra_link_audio *queue, int16_t *storage, size_t capacity) {
    queue->storage = storage;
    queue->capacity = capacity;
    atomic_init(&queue->read, 0);
    atomic_init(&queue->written, 0);
    atomic_init(&queue->discarded, 0);
    atomic_init(&queue->missing, 0);
    atomic_init(&queue->consecutive_underruns, 0);
    atomic_init(&queue->underrun_average_milli, 0);
    atomic_init(&queue->reserve_samples, 0);
}

void ra_link_audio_write(struct ra_link_audio *queue, const int16_t *audio, size_t samples) {
    uint64_t written = atomic_load_explicit(&queue->written, memory_order_relaxed);
    uint64_t read = atomic_load_explicit(&queue->read, memory_order_acquire);
    size_t available =
        written - read < queue->capacity ? (size_t)(written - read) : queue->capacity;
    size_t accepted = samples < queue->capacity - available ? samples : queue->capacity - available;
    for (size_t i = 0; i < accepted; ++i) {
        queue->storage[(written + i) % queue->capacity] = audio[i];
    }
    atomic_store_explicit(&queue->written, written + accepted, memory_order_release);
    atomic_fetch_add_explicit(&queue->discarded, samples - accepted, memory_order_relaxed);
}

void ra_link_audio_read(struct ra_link_audio *queue, int16_t *audio, size_t samples) {
    uint64_t read = atomic_load_explicit(&queue->read, memory_order_relaxed);
    uint64_t written = atomic_load_explicit(&queue->written, memory_order_acquire);
    size_t available =
        written - read < queue->capacity ? (size_t)(written - read) : queue->capacity;
    size_t accepted = samples < available ? samples : available;
    for (size_t i = 0; i < accepted; ++i) {
        audio[i] = queue->storage[(read + i) % queue->capacity];
    }
    for (size_t i = accepted; i < samples; ++i)
        audio[i] = 0;
    atomic_store_explicit(&queue->read, read + accepted, memory_order_release);
    atomic_fetch_add_explicit(&queue->missing, samples - accepted, memory_order_relaxed);
}

/** @brief Update the observable exponentially weighted ten-second missing-sample average.
 * @param queue Consumer-owned queue whose current consecutive missing-sample count changed.
 * @param consecutive Current consecutive missing PCM samples.
 * @param samples Output samples in this callback.
 * @param rate PCM sample rate.
 */
static void update_underrun_average(struct ra_link_audio *queue, uint64_t consecutive,
                                    size_t samples, unsigned int rate) {
    uint64_t average = atomic_load_explicit(&queue->underrun_average_milli, memory_order_relaxed);
    uint64_t denominator = (uint64_t)rate * 10000U;
    uint64_t weight = denominator ? (uint64_t)samples * 1000U : 0;
    if (weight > denominator) {
        weight = denominator;
    }
    int64_t difference = (int64_t)(consecutive * 1000U) - (int64_t)average;
    int64_t adjustment = denominator ? difference * (int64_t)weight / (int64_t)denominator : 0;
    atomic_store_explicit(&queue->underrun_average_milli, (uint64_t)((int64_t)average + adjustment),
                          memory_order_relaxed);
}

void ra_link_audio_record_shortfall(struct ra_link_audio *queue, size_t missing, size_t samples,
                                    unsigned int rate) {
    atomic_fetch_add_explicit(&queue->missing, missing, memory_order_relaxed);
    uint64_t consecutive = missing ? atomic_fetch_add_explicit(&queue->consecutive_underruns,
                                                               missing, memory_order_relaxed) +
                                         missing
                                   : 0;
    if (!missing) {
        atomic_store_explicit(&queue->consecutive_underruns, 0, memory_order_relaxed);
    }
    update_underrun_average(queue, consecutive, samples, rate);
}

size_t ra_link_audio_available(const struct ra_link_audio *queue) {
    uint64_t written = atomic_load_explicit(&queue->written, memory_order_acquire);
    uint64_t read = atomic_load_explicit(&queue->read, memory_order_acquire);
    return written - read < queue->capacity ? (size_t)(written - read) : queue->capacity;
}
