/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Verify prepared ID playback, interruption, and terminal completion.
 */
#include "playback.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Exercise file/speech-equivalent PCM and Morse at hardware-sized boundaries.
 * @return Zero after all assertions.
 */
int main(void) {
    struct ra_identifier_settings settings = {
        .morse_text = "E", .morse_speed_wpm = 20, .morse_frequency_hz = 1000};
    const int16_t prepared[] = {1, -2, 3, -4};
    int16_t output[960], reference[960];
    struct ra_playback state = {.offset = 123};
    assert(!ra_playback_init(&state, prepared, 4, &settings, 1000, false));
    assert(state.offset == 123);
    assert(ra_playback_init(&state, prepared, 4, &settings, 8000, false));
    assert(ra_playback_render(&state, false, NULL, 0) == 0 && state.offset == 0);
    assert(ra_playback_render(&state, false, output, 2) == 2);
    assert(memcmp(output, prepared, 2 * sizeof(*output)) == 0 && !state.finished);
    assert(ra_playback_render(&state, false, output, 960) == 2);
    assert(output[0] == 3 && output[1] == -4 && state.finished);
    assert(ra_playback_render(&state, true, output, 960) == 0);
    assert(ra_playback_init(&state, prepared, 4, &settings, 8000, true));
    assert(ra_playback_render(&state, true, reference, 960) == 480);
    assert(state.finished && state.offset == 0);
    assert(ra_playback_init(&state, prepared, 4, &settings, 8000, false));
    assert(ra_playback_render(&state, false, output, 2) == 2);
    assert(ra_playback_render(&state, true, output, 160) == 160);
    assert(memcmp(output, reference, 160 * sizeof(*output)) == 0);
    /* Releasing receive must not resume the interrupted file/speech ID. */
    assert(ra_playback_render(&state, false, output, 960) == 320);
    assert(memcmp(output, reference + 160, 320 * sizeof(*output)) == 0 && state.finished);
    assert(ra_playback_init(&state, NULL, 0, &settings, 8000, false));
    assert(ra_playback_render(&state, false, output, 960) == 480);
    assert(ra_playback_init(&state, prepared, 4, &settings, 8000, false));
    assert(ra_playback_render(&state, true, NULL, 0) == 0);
    assert(ra_playback_render(&state, false, output, 960) == 480);
    settings.morse_text = "";
    assert(ra_playback_init(&state, NULL, 0, &settings, 48000, false));
    assert(ra_playback_render(&state, false, output, 960) == 0 && state.finished);
    assert(ra_playback_init(&state, prepared, 4, &settings, 48000, false));
    assert(ra_playback_render(&state, false, output, 4) == 4 && state.finished);
    puts("hardware-paced identifier playback tests passed");
    return 0;
}
