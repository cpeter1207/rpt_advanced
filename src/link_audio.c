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

size_t ra_link_audio_available(const struct ra_link_audio *queue) {
    uint64_t written = atomic_load_explicit(&queue->written, memory_order_acquire);
    uint64_t read = atomic_load_explicit(&queue->read, memory_order_acquire);
    return written - read < queue->capacity ? (size_t)(written - read) : queue->capacity;
}
