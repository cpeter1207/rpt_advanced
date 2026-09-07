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
    struct ra_link_audio queue = {.storage = storage, .capacity = 3};
    ra_link_audio_write(&queue, NULL, 0);
    ra_link_audio_read(&queue, NULL, 0);
    ra_link_audio_write(&queue, input, 2);
    ra_link_audio_read(&queue, output, 1);
    assert(output[0] == 10 && queue.count == 1);
    ra_link_audio_write(&queue, input + 2, 3);
    assert(queue.count == 3 && queue.discarded == 1);
    ra_link_audio_read(&queue, output, 5);
    assert(output[0] == 30 && output[1] == 40 && output[2] == 50);
    assert(output[3] == 0 && output[4] == 0 && queue.missing == 2);
    ra_link_audio_write(&queue, input, 5);
    ra_link_audio_read(&queue, output, 3);
    assert(output[0] == 30 && output[2] == 50 && queue.discarded == 3);
    puts("bounded network PCM queue tests passed");
    return 0;
}
