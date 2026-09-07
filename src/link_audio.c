/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Keep network scheduling delays from allocating or blocking on the audio path.
 */
#include "link_audio.h"

void ra_link_audio_write(struct ra_link_audio *queue, const int16_t *audio, size_t samples) {
    for (size_t i = 0; i < samples; ++i) {
        if (queue->count == queue->capacity) {
            queue->head = (queue->head + 1) % queue->capacity;
            --queue->count;
            ++queue->discarded;
        }
        queue->storage[(queue->head + queue->count) % queue->capacity] = audio[i];
        ++queue->count;
    }
}

void ra_link_audio_read(struct ra_link_audio *queue, int16_t *audio, size_t samples) {
    for (size_t i = 0; i < samples; ++i) {
        if (queue->count) {
            audio[i] = queue->storage[queue->head];
            queue->head = (queue->head + 1) % queue->capacity;
            --queue->count;
        } else {
            audio[i] = 0;
            ++queue->missing;
        }
    }
}
