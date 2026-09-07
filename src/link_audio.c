/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Keep network scheduling delays from allocating or blocking on the audio path.
 */
#include "link_audio.h"

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
