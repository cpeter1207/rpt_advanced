/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Sample-driven scheduled-media playback and irreversible receive interruption.
 */
#include "playback.h"

bool ra_playback_init(struct ra_playback *state, const int16_t *audio, size_t samples,
                      const struct ra_identifier_settings *settings, unsigned int rate,
                      bool receiving) {
    struct ra_playback replacement = {
        .audio = audio, .samples = samples, .prepared = samples > 0 && !receiving};
    if (!ra_morse_init(&replacement.morse, settings->morse_text, rate,
                       (unsigned int)settings->morse_speed_wpm,
                       (unsigned int)settings->morse_frequency_hz, (int)settings->morse_level_db)) {
        return false;
    }
    *state = replacement;
    return true;
}

size_t ra_playback_render(struct ra_playback *state, bool receiving, int16_t *output,
                          size_t capacity) {
    if (state->finished) {
        return 0;
    }
    if (receiving) {
        state->prepared = false;
    }
    if (!capacity) {
        return 0;
    }
    size_t count;
    if (state->prepared) {
        size_t remaining = state->samples - state->offset;
        count = remaining < capacity ? remaining : capacity;
        for (size_t sample = 0; sample < count; ++sample) {
            output[sample] = state->audio[state->offset + sample];
        }
        state->offset += count;
        state->finished = state->offset == state->samples;
    } else {
        count = ra_morse_render(&state->morse, output, capacity);
        state->finished = count < capacity;
    }
    return count;
}
