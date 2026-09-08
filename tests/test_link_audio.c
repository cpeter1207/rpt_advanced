/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Ring wrap, overflow, silence, and differently sized network/radio blocks.
 */
#include "link_audio.h"
#include <assert.h>
#include <stdio.h>

/** @brief Test exact PCM retention and bounded failure behavior.
 * @return Zero after assertions.
 */
int main(void) {
    int16_t storage[3], output[5];
    const int16_t input[] = {10, 20, 30, 40, 50};
    struct ra_link_audio queue;
    ra_link_audio_init(&queue, storage, sizeof(storage) / sizeof(*storage));
    assert(queue.storage == storage && queue.capacity == sizeof(storage) / sizeof(*storage));
    assert(!atomic_load(&queue.read) && !atomic_load(&queue.written) &&
           !atomic_load(&queue.discarded) && !atomic_load(&queue.missing));
    ra_link_audio_write(&queue, NULL, 0);
    ra_link_audio_read(&queue, NULL, 0);
    ra_link_audio_write(&queue, input, 2);
    ra_link_audio_read(&queue, output, 1);
    assert(output[0] == 10 && ra_link_audio_available(&queue) == 1);
    ra_link_audio_write(&queue, input + 2, 3);
    assert(ra_link_audio_available(&queue) == 3 && atomic_load(&queue.discarded) == 1);
    ra_link_audio_read(&queue, output, 5);
    assert(output[0] == 20 && output[1] == 30 && output[2] == 40);
    assert(output[3] == 0 && output[4] == 0 && atomic_load(&queue.missing) == 2);
    ra_link_audio_write(&queue, input, 5);
    ra_link_audio_read(&queue, output, 3);
    assert(output[0] == 10 && output[2] == 30 && atomic_load(&queue.discarded) == 3);
    ra_link_audio_record_shortfall(&queue, 3, 10, 1000);
    assert(atomic_load(&queue.missing) == 5 && atomic_load(&queue.consecutive_underruns) == 3 &&
           atomic_load(&queue.underrun_average_milli));
    ra_link_audio_record_shortfall(&queue, 0, 10, 1000);
    assert(!atomic_load(&queue.consecutive_underruns));
    ra_link_audio_record_shortfall(&queue, 1, 20000, 1000);
    assert(atomic_load(&queue.underrun_average_milli));
    ra_link_audio_record_shortfall(&queue, 0, 1, 0);
    puts("bounded network PCM queue tests passed");
    return 0;
}
