/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Verify Morse timing, PCM frequency, validation, and block independence.
 */
#include "morse.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Render one identifier and check exact duration.
 * @param text Identifier fixture.
 * @param rate Output sample rate.
 * @param speed PARIS words per minute.
 * @param expected Expected sample count.
 */
static void duration(const char *text, unsigned int rate, unsigned int speed, size_t expected) {
    struct ra_morse state;
    int16_t output[4096];
    assert(ra_morse_init(&state, text, rate, speed, 1));
    size_t total = 0;
    size_t count;
    while ((count = ra_morse_render(&state, output, 4096))) {
        total += count;
    }
    assert(total == expected);
}

/** @brief Exercise sample-accurate durations and PCM output at multiple rates.
 * @return Zero after all checks pass.
 */
int main(void) {
    struct ra_morse state = {.rate = 123};
    assert(!ra_morse_init(&state, "E", 0, 20, 1));
    assert(!ra_morse_init(&state, "E", 8000, 0, 1));
    assert(!ra_morse_init(&state, "E", 8000, 101, 1));
    assert(!ra_morse_init(&state, "E", 8000, 20, 0));
    assert(!ra_morse_init(&state, "E", 8000, 20, 4000));
    assert(!ra_morse_init(&state, "E*", 8000, 20, 1000));
    assert(!ra_morse_init(&state, "{", 8000, 20, 1000));
    assert(state.rate == 123);
    duration("", 8000, 20, 0);
    duration(" \t\r\n", 8000, 20, 0);
    duration("e", 8000, 20, 480);
    duration("T", 8000, 20, 1440);
    duration("ET", 8000, 20, 3360);
    duration(" E \tT\r\n", 8000, 20, 5280);
    duration("I", 8000, 20, 1440);
    duration("E", 48000, 20, 2880);
    duration("E", 96000, 20, 5760);
    duration("EE", 44100, 17, (uint64_t)5 * 44100 * 6 / 85);
    duration("E", 3, 100, 0);
    int16_t complete[32768], blocks[32768];
    char changed[] = "E";
    assert(ra_morse_init(&state, changed, 8000, 20, 1000));
    changed[0] = '*';
    assert(ra_morse_render(&state, complete, 32768) == 0);
    assert(ra_morse_render(&state, complete, 32768) == 0);
    assert(ra_morse_init(&state, "E T I", 8000, 20, 1000));
    assert(ra_morse_render(&state, NULL, 0) == 0);
    size_t length = ra_morse_render(&state, complete, 32768);
    assert(length == 10080);
    /* A 1 kHz tone at 8 kHz repeats every eight samples; gaps are exact zero. */
    assert(complete[2] > 16380 && complete[6] < -16380);
    for (size_t i = 0; i < 472; ++i) {
        assert(complete[i] == complete[i + 8]);
    }
    for (size_t i = 480; i < 3840; ++i) {
        assert(complete[i] == 0);
    }
    assert(ra_morse_init(&state, "E T I", 8000, 20, 1000));
    size_t total = 0, count;
    while ((count = ra_morse_render(&state, blocks + total, 37))) {
        total += count;
    }
    assert(total == length && memcmp(complete, blocks, length * sizeof(*blocks)) == 0);
    assert(ra_morse_init(&state, "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/.,?-=+@()'!\":;_$&", 8000,
                         100, 1000));
    while (ra_morse_render(&state, blocks, 32768)) {
    }
    puts("streaming Morse timing and PCM tests passed");
    return 0;
}
