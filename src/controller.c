/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Integrate identifier policy and playback with hardware-paced local repeat.
 */
#include "controller.h"
#include <limits.h>
#include <math.h>

/** @brief Require atomics that never fall back to a library mutex in the radio worker. */
_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "status queue requires lock-free unsigned atomics");

/** @brief Attack duration for receive-active telemetry ducking. */
#define RA_CONTROLLER_DUCK_ATTACK_MS 10.0
/** @brief Release duration for telemetry after local or linked receive clears. */
#define RA_CONTROLLER_DUCK_RELEASE_MS 100.0

/** @brief Advance telemetry gain by one sample without a discontinuity.
 * @param current Prior linear gain.
 * @param target Receive-active or idle target gain.
 * @param rate Hardware-paced controller sample rate.
 * @return Next bounded gain.
 */
static double telemetry_gain_step(double current, double target, unsigned int rate) {
    double duration =
        target < current ? RA_CONTROLLER_DUCK_ATTACK_MS : RA_CONTROLLER_DUCK_RELEASE_MS;
    double step = 1000.0 / ((double)rate * duration);
    if (current > target) {
        current -= step;
        return current < target ? target : current;
    }
    current += step;
    return current > target ? target : current;
}

/** @brief Check whether a courtesy source has prepared audio or a Morse fallback.
 * @param courtesy Prepared source-specific courtesy media.
 * @return True when speech/file PCM or Morse fallback is available.
 */
static bool courtesy_available(const struct ra_controller_id *courtesy) {
    return courtesy->audio || (courtesy->settings.morse_text && *courtesy->settings.morse_text);
}

/** @brief Discard courtesy tones made obsolete by resumed receive activity.
 * @param state Started controller that owns the pending courtesy queue.
 *
 * A short RF flutter or network packet-loss gap must not announce an end of
 * transmission while the same source has already resumed. Active media is
 * retained and ducked; only tones that have not started are discarded.
 */
static void courtesy_cancel_pending(struct ra_controller *state) {
    state->courtesy_pending_count = 0;
}

/** @brief Schedule one source-specific courtesy announcement.
 * @param state Started controller that owns the bounded pending queue.
 * @param source Receiver or link unkey source.
 * @param now_ms Monotonic unkey time.
 */
static void courtesy_schedule(struct ra_controller *state, enum ra_courtesy_source source,
                              uint64_t now_ms) {
    if (!courtesy_available(&state->courtesy[source]) ||
        state->courtesy_pending_count == RA_CONTROLLER_COURTESY_QUEUE_DEPTH) {
        return;
    }
    if (!state->courtesy_pending_count && !state->courtesy_playing) {
        state->courtesy_due_ms = now_ms + state->courtesy_delay_ms;
    }
    state->courtesy_pending[state->courtesy_pending_count++] = source;
}

/** @brief Start the oldest due courtesy announcement.
 * @param state Started controller with at least one pending source.
 */
static void courtesy_start(struct ra_controller *state) {
    enum ra_courtesy_source source = state->courtesy_pending[0];
    const struct ra_controller_id *courtesy = &state->courtesy[source];
    (void)ra_playback_init(&state->courtesy_playback, courtesy->audio, courtesy->samples,
                           &courtesy->settings, state->rate, state->link_active);
    for (size_t index = 1; index < state->courtesy_pending_count; ++index) {
        state->courtesy_pending[index - 1] = state->courtesy_pending[index];
    }
    --state->courtesy_pending_count;
    state->courtesy_playing = true;
}

/** @brief Check whether a control-prepared RF status awaits radio playback.
 * @param state Started controller shared by one control producer and one radio consumer.
 * @return True when the queue retains an active or pending status slot.
 */
static bool status_pending(const struct ra_controller *state) {
    return atomic_load_explicit(&state->status_read, memory_order_relaxed) !=
           atomic_load_explicit(&state->status_write, memory_order_acquire);
}

/** @brief Begin the oldest queued status without releasing its backing text or PCM slot.
 * @param state Started controller, called only by the radio worker.
 */
static void status_start(struct ra_controller *state) {
    unsigned int read = atomic_load_explicit(&state->status_read, memory_order_relaxed);
    const struct ra_controller_status *slot =
        &state->status_queue[read % RA_CONTROLLER_STATUS_QUEUE_DEPTH];
    struct ra_identifier_settings settings = {.morse_text = slot->text,
                                              .morse_speed_wpm = state->status_speed_wpm,
                                              .morse_frequency_hz = state->status_frequency_hz,
                                              .morse_level_db = state->status_level_db};
    (void)ra_playback_init(&state->status_playback, slot->audio, slot->samples, &settings,
                           state->rate, false);
    state->status_playing = true;
}

/** @brief Release the active status slot after its speech or Morse renderer completes.
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
    for (size_t i = 0; i < RA_CONTROLLER_COURTESY_QUEUE_DEPTH; ++i) {
        if (courtesy_available(&state->courtesy[i])) {
            struct ra_playback validated;
            if (!ra_playback_init(&validated, state->courtesy[i].audio, state->courtesy[i].samples,
                                  &state->courtesy[i].settings, state->rate, false)) {
                return false;
            }
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
    state->status_reclaimed = 0;
    state->status_playback = (struct ra_playback){0};
    state->status_playing = false;
    state->receiver_unkey_ms = now_ms;
    state->telemetry_duck_gain = pow(10.0, state->telemetry_duck_db / 20.0);
    state->telemetry_gain = 1.0;
    state->courtesy_playback = (struct ra_playback){0};
    state->courtesy_pending_count = 0;
    state->courtesy_playing = false;
    state->link_was_active = false;
    return true;
}

bool ra_controller_queue_status(struct ra_controller *state, const char *text, int16_t *audio,
                                size_t samples) {
    if (!text || !*text) {
        return false;
    }
    if ((audio == NULL) != (samples == 0)) {
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
    struct ra_morse validation;
    if (!ra_morse_init(&validation, slot->text, state->rate, state->status_speed_wpm,
                       state->status_frequency_hz, state->status_level_db)) {
        return false;
    }
    slot->audio = audio;
    slot->samples = samples;
    atomic_store_explicit(&state->status_write, write + 1, memory_order_release);
    return true;
}

size_t ra_controller_reclaim_status(struct ra_controller *state, int16_t **audio, size_t capacity) {
    unsigned int read = atomic_load_explicit(&state->status_read, memory_order_acquire);
    size_t count = 0;
    while (state->status_reclaimed != read && count < capacity) {
        struct ra_controller_status *slot =
            &state->status_queue[state->status_reclaimed % RA_CONTROLLER_STATUS_QUEUE_DEPTH];
        audio[count++] = slot->audio;
        slot->audio = NULL;
        slot->samples = 0;
        ++state->status_reclaimed;
    }
    return count;
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
    bool receiver_unkeyed = state->receiving && !receiving;
    bool link_unkeyed = state->link_was_active && !state->link_active;
    if (receiver_unkeyed) {
        state->receiver_unkey_ms = now_ms;
    }
    state->receiving = receiving;
    state->link_was_active = state->link_active;
    if (receiver_unkeyed) {
        courtesy_schedule(state, RA_COURTESY_RECEIVER, now_ms);
    }
    if (link_unkeyed) {
        courtesy_schedule(state, RA_COURTESY_LINK, now_ms);
    }
    if (interrupting_receive) {
        courtesy_cancel_pending(state);
    }
    if (interrupting_receive && state->playing != SIZE_MAX) {
        /* Carrier events interrupt prepared audio even without a sample tick. */
        (void)ra_playback_render(&state->playback, true, NULL, 0);
    }
    size_t selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                   state->full_duplex);
    bool may_transmit = state->full_duplex || !receiving;
    bool telemetry_idle = !interrupting_receive;
    bool status_ready = telemetry_idle &&
                        now_ms - state->receiver_unkey_ms >= RA_CONTROLLER_STATUS_UNKEY_DELAY_MS &&
                        status_pending(state);
    unsigned int courtesy_ready =
        (state->courtesy_pending_count != 0) & telemetry_idle & (now_ms >= state->courtesy_due_ms);
    unsigned int courtesy_start_ready =
        (unsigned int)may_transmit & courtesy_ready & (unsigned int)!state->courtesy_playing &
        (unsigned int)!state->status_playing & (unsigned int)(state->playing == SIZE_MAX);
    unsigned int status_start_ready = (unsigned int)may_transmit & (unsigned int)status_ready &
                                      (unsigned int)!state->status_playing &
                                      (unsigned int)!state->courtesy_playing &
                                      (unsigned int)(state->playing == SIZE_MAX);
    if (courtesy_start_ready) {
        courtesy_start(state);
    } else if (status_start_ready) {
        /* Telemetry uses one renderer so announcements never overlap on RF. */
        status_start(state);
    }
    bool demand = state->link_active;
    demand |= state->full_duplex && receiving;
    demand |= state->playing != SIZE_MAX;
    demand |= state->status_playing;
    demand |= state->courtesy_playing;
    demand |= courtesy_ready;
    demand |= status_ready;
    demand |= samples && selected != SIZE_MAX;
    if (may_transmit && demand && !state->duplex.keyed) {
        ra_id_first_key(state->rules, state->states, state->count,
                        state->key_idle_ms > idle ? state->key_idle_ms : idle);
        state->key_idle_ms = 0;
        selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                state->full_duplex);
    }
    if (samples && !state->status_playing && !state->courtesy_playing &&
        state->playing == SIZE_MAX && selected != SIZE_MAX) {
        const struct ra_controller_id *id = &state->ids[selected];
        /* Startup validated this immutable media and configuration at this rate. */
        (void)ra_playback_init(&state->playback, id->audio, id->samples, &id->settings, state->rate,
                               interrupting_receive);
        state->playing = selected;
    }
    bool identifier_audio = false;
    double telemetry_target = interrupting_receive ? state->telemetry_duck_gain : 1.0;
    for (size_t offset = 0; offset < samples;) {
        int16_t identifier[256];
        size_t capacity = samples - offset;
        if (capacity > sizeof(identifier) / sizeof(*identifier)) {
            capacity = sizeof(identifier) / sizeof(*identifier);
        }
        size_t made = 0;
        if (may_transmit && state->status_playing) {
            made = ra_playback_render(&state->status_playback, interrupting_receive, identifier,
                                      capacity);
            identifier_audio |= made != 0;
            if (made < capacity) {
                status_finish(state);
            }
        } else if (may_transmit && state->courtesy_playing) {
            made = ra_playback_render(&state->courtesy_playback, interrupting_receive, identifier,
                                      capacity);
            identifier_audio |= made != 0;
            if (made < capacity) {
                state->courtesy_playing = false;
                if (state->courtesy_pending_count) {
                    state->courtesy_due_ms = now_ms;
                }
            }
        } else if (may_transmit && state->playing != SIZE_MAX) {
            made = ra_playback_render(&state->playback, interrupting_receive, identifier, capacity);
            identifier_audio |= made != 0;
        }
        for (size_t i = 0; i < capacity; ++i) {
            state->telemetry_gain =
                telemetry_gain_step(state->telemetry_gain, telemetry_target, state->rate);
            int mixed = state->full_duplex && receiving ? audio[offset + i] : 0;
            if (may_transmit && state->link_active && state->link_audio) {
                mixed += state->link_audio[offset + i];
            }
            if (i < made) {
                mixed += (int)lround(identifier[i] * state->telemetry_gain);
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
                                state->courtesy_playing || courtesy_ready || status_ready,
                            now_ms, state->hang_ms);
}
