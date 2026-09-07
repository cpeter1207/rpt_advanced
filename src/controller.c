/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Integrate identifier policy and playback with hardware-paced local repeat.
 */
#include "controller.h"
#include <limits.h>

bool ra_controller_start(struct ra_controller *state, uint64_t now_ms) {
    for (size_t i = 0; i < state->count; ++i) {
        struct ra_playback validated;
        const struct ra_controller_id *id = &state->ids[i];
        if (!ra_playback_init(&validated, id->audio, id->samples, &id->settings, state->rate,
                              false)) {
            return false;
        }
    }
    for (size_t i = 0; i < state->count; ++i) {
        const struct ra_identifier_settings *settings = &state->ids[i].settings;
        state->rules[i] =
            (struct ra_id_rule){settings->interval_ms, (int)settings->priority,
                                settings->first_key_only, settings->regardless_of_activity};
        state->states[i] = (struct ra_id_state){.satisfied_ms = now_ms};
    }
    state->duplex = (struct ra_duplex_state){0};
    state->playing = SIZE_MAX;
    state->receiving = false;
    state->last_activity_ms = now_ms;
    state->key_idle_ms = 0;
    return true;
}

bool ra_controller_process(struct ra_controller *state, bool receiving, int16_t *audio,
                           size_t samples, uint64_t now_ms) {
    uint64_t idle = now_ms - state->last_activity_ms;
    if (receiving || state->link_active) {
        if (!state->receiving) {
            state->key_idle_ms = idle;
        }
        state->last_activity_ms = now_ms;
        ra_id_activity(state->states, state->count);
    }
    state->receiving = receiving;
    if (receiving && state->playing != SIZE_MAX) {
        /* Carrier events interrupt prepared audio even without a sample tick. */
        (void)ra_playback_render(&state->playback, true, NULL, 0);
    }
    size_t selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                   state->full_duplex);
    bool may_transmit = state->full_duplex || !receiving;
    bool demand = state->link_active || (state->full_duplex && receiving) ||
                  state->playing != SIZE_MAX || (samples && selected != SIZE_MAX);
    if (may_transmit && demand && !state->duplex.keyed) {
        ra_id_first_key(state->rules, state->states, state->count,
                        state->key_idle_ms > idle ? state->key_idle_ms : idle);
        state->key_idle_ms = 0;
        selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                state->full_duplex);
    }
    if (samples && state->playing == SIZE_MAX && selected != SIZE_MAX) {
        const struct ra_controller_id *id = &state->ids[selected];
        /* Startup validated this immutable media and configuration at this rate. */
        (void)ra_playback_init(&state->playback, id->audio, id->samples, &id->settings, state->rate,
                               receiving);
        state->playing = selected;
    }
    bool identifier_audio = false;
    for (size_t offset = 0; offset < samples;) {
        int16_t identifier[256];
        size_t capacity = samples - offset;
        if (capacity > sizeof(identifier) / sizeof(*identifier)) {
            capacity = sizeof(identifier) / sizeof(*identifier);
        }
        size_t made = 0;
        if (may_transmit && state->playing != SIZE_MAX) {
            made = ra_playback_render(&state->playback, receiving, identifier, capacity);
            identifier_audio |= made != 0;
        }
        for (size_t i = 0; i < capacity; ++i) {
            int mixed = state->full_duplex && receiving ? audio[offset + i] : 0;
            if (may_transmit && state->link_active && state->link_audio) {
                mixed += state->link_audio[offset + i];
            }
            if (i < made) {
                mixed += identifier[i];
            }
            /* Saturation prevents integer wrap; all dynamics remain in USBRadioPlus. */
            audio[offset + i] = mixed > INT16_MAX   ? INT16_MAX
                                : mixed < INT16_MIN ? INT16_MIN
                                                    : (int16_t)mixed;
        }
        offset += capacity;
    }
    if (samples && state->playing != SIZE_MAX && state->playback.finished) {
        const struct ra_controller_id *id = &state->ids[state->playing];
        if (state->playback.prepared || id->settings.morse_text[0]) {
            ra_id_complete(state->rules, state->states, state->count, state->playing, now_ms);
        }
        state->playing = SIZE_MAX;
    }
    return ra_duplex_update(&state->duplex, state->full_duplex, receiving,
                            state->link_active || identifier_audio || (state->playing != SIZE_MAX),
                            now_ms, state->hang_ms);
}
