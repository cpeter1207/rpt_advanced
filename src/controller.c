/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Integrate identifier policy and playback with hardware-paced local repeat.
 */
#include "controller.h"
#include <limits.h>

/** @brief Require atomics that never fall back to a library mutex in the radio worker. */
_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "status queue requires lock-free unsigned atomics");

/** @brief Check whether a control-prepared RF status awaits radio playback.
 * @param state Started controller shared by one control producer and one radio consumer.
 * @return True when the queue retains an active or pending status slot.
 */
static bool status_pending(const struct ra_controller *state) {
    return atomic_load_explicit(&state->status_read, memory_order_relaxed) !=
           atomic_load_explicit(&state->status_write, memory_order_acquire);
}

/** @brief Begin the oldest queued status without releasing its backing text slot.
 * @param state Started controller, called only by the radio worker.
 */
static void status_start(struct ra_controller *state) {
    unsigned int read = atomic_load_explicit(&state->status_read, memory_order_relaxed);
    state->status_morse = state->status_queue[read % RA_CONTROLLER_STATUS_QUEUE_DEPTH].morse;
    state->status_playing = true;
}

/** @brief Release the active status slot after its Morse renderer reaches completion.
 * @param state Started controller, called only by the radio worker.
 */
static void status_finish(struct ra_controller *state) {
    unsigned int read = atomic_load_explicit(&state->status_read, memory_order_relaxed);
    atomic_store_explicit(&state->status_read, read + 1, memory_order_release);
    state->status_playing = false;
}

bool ra_controller_start(struct ra_controller *state, uint64_t now_ms) {
    unsigned int status_speed = state->status_speed_wpm;
    unsigned int status_frequency = state->status_frequency_hz;
    int status_level = state->status_level_db;
    if (!status_speed && !status_frequency && !status_level) {
        status_speed = 20;
        status_frequency = 800;
        status_level = -6;
    }
    struct ra_morse status_validation;
    if (!ra_morse_init(&status_validation, "", state->rate, status_speed, status_frequency,
                       status_level)) {
        return false;
    }
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
    state->status_speed_wpm = status_speed;
    state->status_frequency_hz = status_frequency;
    state->status_level_db = status_level;
    atomic_store_explicit(&state->status_write, 0, memory_order_relaxed);
    atomic_store_explicit(&state->status_read, 0, memory_order_relaxed);
    state->status_morse = (struct ra_morse){0};
    state->status_playing = false;
    return true;
}

bool ra_controller_queue_status(struct ra_controller *state, const char *text) {
    if (!text || !*text) {
        return false;
    }
    size_t length = 0;
    while (length < RA_CONTROLLER_STATUS_TEXT_MAX && text[length]) {
        ++length;
    }
    if (length == RA_CONTROLLER_STATUS_TEXT_MAX) {
        return false;
    }
    unsigned int write = atomic_load_explicit(&state->status_write, memory_order_relaxed);
    unsigned int read = atomic_load_explicit(&state->status_read, memory_order_acquire);
    if (write - read >= RA_CONTROLLER_STATUS_QUEUE_DEPTH) {
        return false;
    }
    struct ra_controller_status *slot =
        &state->status_queue[write % RA_CONTROLLER_STATUS_QUEUE_DEPTH];
    for (size_t index = 0; index <= length; ++index) {
        slot->text[index] = text[index];
    }
    if (!ra_morse_init(&slot->morse, slot->text, state->rate, state->status_speed_wpm,
                       state->status_frequency_hz, state->status_level_db)) {
        return false;
    }
    atomic_store_explicit(&state->status_write, write + 1, memory_order_release);
    return true;
}

bool ra_controller_process(struct ra_controller *state, bool receiving, int16_t *audio,
                           size_t samples, uint64_t now_ms) {
    /* A linked transmission interrupts an ID just like qualified local carrier does. */
    bool interrupting_receive = receiving || state->link_active;
    uint64_t idle = now_ms - state->last_activity_ms;
    if (receiving || state->link_active) {
        if (!state->receiving) {
            state->key_idle_ms = idle;
        }
        state->last_activity_ms = now_ms;
        ra_id_activity(state->states, state->count);
    }
    state->receiving = receiving;
    if (interrupting_receive && state->playing != SIZE_MAX) {
        /* Carrier events interrupt prepared audio even without a sample tick. */
        (void)ra_playback_render(&state->playback, true, NULL, 0);
    }
    size_t selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                   state->full_duplex);
    bool may_transmit = state->full_duplex || !receiving;
    if (may_transmit && status_pending(state) && !state->status_playing) {
        /* Status is operator feedback, so it preempts but never satisfies an identifier. */
        state->playing = SIZE_MAX;
        status_start(state);
    }
    bool demand = state->link_active || (state->full_duplex && receiving) ||
                  state->playing != SIZE_MAX || state->status_playing || status_pending(state) ||
                  (samples && selected != SIZE_MAX);
    if (may_transmit && demand && !state->duplex.keyed) {
        ra_id_first_key(state->rules, state->states, state->count,
                        state->key_idle_ms > idle ? state->key_idle_ms : idle);
        state->key_idle_ms = 0;
        selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                state->full_duplex);
    }
    if (samples && !state->status_playing && state->playing == SIZE_MAX && selected != SIZE_MAX) {
        const struct ra_controller_id *id = &state->ids[selected];
        /* Startup validated this immutable media and configuration at this rate. */
        (void)ra_playback_init(&state->playback, id->audio, id->samples, &id->settings, state->rate,
                               interrupting_receive);
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
        if (may_transmit && state->status_playing) {
            made = ra_morse_render(&state->status_morse, identifier, capacity);
            identifier_audio |= made != 0;
            if (made < capacity) {
                status_finish(state);
            }
        } else if (may_transmit && state->playing != SIZE_MAX) {
            made = ra_playback_render(&state->playback, interrupting_receive, identifier, capacity);
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
                            state->link_active || identifier_audio ||
                                (state->playing != SIZE_MAX) || state->status_playing ||
                                status_pending(state),
                            now_ms, state->hang_ms);
}
