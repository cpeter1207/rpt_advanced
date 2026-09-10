/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Integrate scheduled-media policy and playback with hardware-paced local repeat.
 */
#include "controller.h"
#include <limits.h>
#include <math.h>
#include <string.h>

/** @brief Require atomics that never fall back to a library mutex in the radio worker. */
_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "status queue requires lock-free unsigned atomics");

/** @brief Attack duration for receive-active telemetry ducking. */
#define RA_CONTROLLER_DUCK_ATTACK_MS 10.0
/** @brief Release duration for telemetry after local or linked receive clears. */
#define RA_CONTROLLER_DUCK_RELEASE_MS 100.0
/** @brief Brief PTT tail after an identifier or announcement ends, avoiding ordinary hang time. */
#define RA_CONTROLLER_IDENTIFIER_RELEASE_MS 50U

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
    return courtesy &&
           (courtesy->audio || (courtesy->settings.morse_text && *courtesy->settings.morse_text));
}

/** @brief Select one permanent-peer override or the generic linked-receiver media.
 * @param state Started controller with immutable courtesy bindings.
 * @param remote Exact direct-peer identity supplied by the routing hub.
 * @param permanent True only for a configured permanent direct link.
 * @return Renderable peer-specific media when available, otherwise generic link media.
 *
 * The lookup runs only at a link falling edge in the hardware-paced worker. It
 * intentionally reads the current controller binding rather than retaining a
 * media pointer in a link port: ports survive a configuration reload while
 * prepared media does not.
 */
static const struct ra_controller_id *courtesy_link_select(const struct ra_controller *state,
                                                           const char *remote, bool permanent) {
    if (permanent && remote && *remote) {
        for (size_t index = 0; index < state->peer_courtesy_count; ++index) {
            const struct ra_controller_peer_courtesy *override = &state->peer_courtesies[index];
            /* ra_controller_start() rejects null or empty immutable peer identities. */
            if (!strcmp(override->remote, remote) && courtesy_available(override->media)) {
                return override->media;
            }
        }
    }
    return state->link_courtesy;
}

/** @brief Select the first announcement currently due in configuration order.
 * @param state Started controller owning immutable announcement media and mutable schedules.
 * @param now_ms Current monotonic time.
 * @return Announcement index, or SIZE_MAX when none is due.
 *
 * A zero interval does not use a wall-clock deadline: an ordinary transmission marks that
 * announcement pending and consumes exactly one playback at its eventual release.
 */
static size_t announcement_select(const struct ra_controller *state, uint64_t now_ms) {
    for (size_t index = 0; index < state->announcement_count; ++index) {
        const struct ra_controller_announcement *announcement = &state->announcements[index];
        const struct ra_controller_announcement_state *schedule =
            &state->announcement_states[index];
        bool due = announcement->interval_ms
                       ? now_ms - schedule->satisfied_ms >= announcement->interval_ms
                       : schedule->release_pending;
        if (due) {
            return index;
        }
    }
    return SIZE_MAX;
}

/** @brief Mark zero-interval announcements for one later transmitter release.
 * @param state Started controller with announcement scheduling state.
 *
 * IDs count as ordinary transmission so an every-release announcement follows a standalone ID.
 * Announcements themselves do not call this helper, avoiding self-triggered replay loops.
 */
static void announcement_mark_release(struct ra_controller *state) {
    for (size_t index = 0; index < state->announcement_count; ++index) {
        if (!state->announcements[index].interval_ms) {
            state->announcement_states[index].release_pending = true;
        }
    }
}

/** @brief Start a selected announcement after identifiers and other telemetry have cleared.
 * @param state Started controller with a valid due announcement index.
 * @param index Due announcement index returned by announcement_select().
 */
static void announcement_start(struct ra_controller *state, size_t index) {
    const struct ra_controller_announcement *announcement = &state->announcements[index];
    if (!announcement->interval_ms) {
        state->announcement_states[index].release_pending = false;
    }
    (void)ra_playback_init(&state->announcement_playback, announcement->media.audio,
                           announcement->media.samples, &announcement->media.settings, state->rate,
                           false);
    state->announcement_playing = index;
}

/** @brief Discard courtesy tones made obsolete by the same source resuming.
 * @param state Started controller that owns the pending courtesy queue.
 * @param receiver True selects local-receiver courtesy media.
 * @param remote Exact direct-peer identity when @p receiver is false.
 *
 * A short RF flutter or network packet-loss gap must not announce an end of
 * transmission while its source has already resumed. Other input sources stay
 * queued, and active media is retained and ducked.
 */
static void courtesy_cancel_pending(struct ra_controller *state, bool receiver,
                                    const char *remote) {
    size_t retained = 0;
    for (size_t index = 0; index < state->courtesy_pending_count; ++index) {
        const struct ra_controller_courtesy_pending *pending = &state->courtesy_pending[index];
        bool matching_source = pending->receiver == receiver &&
                               (receiver || (remote && !strcmp(pending->remote, remote)));
        if (!matching_source) {
            state->courtesy_pending[retained++] = *pending;
        }
    }
    state->courtesy_pending_count = retained;
}

/** @brief Compute a monotonic courtesy deadline without wrapping a configured duration.
 * @param now_ms Monotonic source-unkey timestamp.
 * @param delay_ms Configured source-to-courtesy delay.
 * @return Saturated earliest playback timestamp.
 */
static uint64_t courtesy_deadline(uint64_t now_ms, uint64_t delay_ms) {
    return delay_ms > UINT64_MAX - now_ms ? UINT64_MAX : now_ms + delay_ms;
}

/** @brief Schedule one source-specific courtesy announcement.
 * @param state Started controller that owns the bounded pending queue.
 * @param courtesy Already selected prepared media.
 * @param receiver True for local receiver, false for a direct linked peer.
 * @param remote Exact direct-peer identity when @p receiver is false.
 * @param now_ms Monotonic unkey time.
 */
static void courtesy_schedule(struct ra_controller *state, const struct ra_controller_id *courtesy,
                              bool receiver, const char *remote, uint64_t now_ms) {
    if (!courtesy_available(courtesy) ||
        state->courtesy_pending_count == RA_CONTROLLER_COURTESY_QUEUE_DEPTH) {
        return;
    }
    if (!receiver && (!remote || !*remote)) {
        return;
    }
    size_t remote_length = 0;
    if (!receiver) {
        while (remote_length + 1 < RA_CONTROLLER_COURTESY_REMOTE_MAX && remote[remote_length]) {
            ++remote_length;
        }
        if (remote[remote_length]) {
            return;
        }
    }
    struct ra_controller_courtesy_pending *pending =
        &state->courtesy_pending[state->courtesy_pending_count++];
    *pending = (struct ra_controller_courtesy_pending){
        .media = courtesy,
        .receiver = receiver,
        .due_ms = courtesy_deadline(now_ms, state->courtesy_delay_ms)};
    for (size_t index = 0; index <= remote_length; ++index) {
        pending->remote[index] = receiver ? '\0' : remote[index];
    }
}

/** @brief Start the oldest due courtesy announcement.
 * @param state Started controller with at least one pending source.
 */
static void courtesy_start(struct ra_controller *state) {
    const struct ra_controller_id *courtesy = state->courtesy_pending[0].media;
    (void)ra_playback_init(&state->courtesy_playback, courtesy->audio, courtesy->samples,
                           &courtesy->settings, state->rate, false);
    for (size_t index = 1; index < state->courtesy_pending_count; ++index) {
        state->courtesy_pending[index - 1] = state->courtesy_pending[index];
    }
    --state->courtesy_pending_count;
    state->courtesy_playing = true;
}

void ra_controller_link_unkeyed_kerchunk(struct ra_controller *state, const char *remote,
                                         bool permanent, bool kerchunk, uint64_t now_ms) {
    if (kerchunk) {
        state->suppress_release = true;
        return;
    }
    courtesy_schedule(state, courtesy_link_select(state, remote, permanent), false, remote, now_ms);
}

void ra_controller_link_unkeyed(struct ra_controller *state, const char *remote, bool permanent,
                                uint64_t now_ms) {
    ra_controller_link_unkeyed_kerchunk(state, remote, permanent, false, now_ms);
}

void ra_controller_link_keyed(struct ra_controller *state, const char *remote) {
    courtesy_cancel_pending(state, false, remote);
}

/** @brief Check whether a control-prepared RF status awaits radio playback.
 * @param state Started controller shared by one control producer and one radio consumer.
 * @return True when the queue retains an active or pending status slot.
 */
static bool status_pending(const struct ra_controller *state) {
    return atomic_load_explicit(&state->status_read, memory_order_relaxed) !=
           atomic_load_explicit(&state->status_write, memory_order_acquire);
}

/** @brief Decide whether a due ID remains politely deferred.
 * @param rule Resolved policy for the selected ID.
 * @param id Runtime scheduling state for the selected ID.
 * @param now_ms Current monotonic controller time.
 * @param busy True while reception or other telemetry remains active or queued.
 * @return True while the bounded polite wait still applies.
 */
static bool id_polite_deferred(const struct ra_id_rule *rule, const struct ra_id_state *id,
                               uint64_t now_ms, bool busy) {
    if (!rule->polite || !busy) {
        return false;
    }
    uint64_t waited_ms = 0;
    if (rule->first_key_only) {
        waited_ms = now_ms - id->polite_due_ms;
    } else {
        uint64_t elapsed_ms = now_ms - id->satisfied_ms;
        waited_ms = elapsed_ms > rule->interval_ms ? elapsed_ms - rule->interval_ms : 0;
    }
    return waited_ms < rule->polite_maximum_wait_ms;
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
    const struct ra_controller_id *courtesy[] = {state->receiver_courtesy, state->link_courtesy};
    for (size_t i = 0; i < sizeof(courtesy) / sizeof(*courtesy); ++i) {
        if (courtesy_available(courtesy[i])) {
            struct ra_playback validated;
            if (!ra_playback_init(&validated, courtesy[i]->audio, courtesy[i]->samples,
                                  &courtesy[i]->settings, state->rate, false)) {
                return false;
            }
        }
    }
    if (state->peer_courtesy_count && !state->peer_courtesies) {
        return false;
    }
    for (size_t i = 0; i < state->peer_courtesy_count; ++i) {
        const struct ra_controller_peer_courtesy *override = &state->peer_courtesies[i];
        if (!override->remote || !*override->remote || !override->media) {
            return false;
        }
        if (courtesy_available(override->media)) {
            struct ra_playback validated;
            if (!ra_playback_init(&validated, override->media->audio, override->media->samples,
                                  &override->media->settings, state->rate, false)) {
                return false;
            }
        }
    }
    if (state->announcement_count && (!state->announcements || !state->announcement_states)) {
        return false;
    }
    for (size_t i = 0; i < state->announcement_count; ++i) {
        const struct ra_controller_id *media = &state->announcements[i].media;
        struct ra_playback validated;
        if (!ra_playback_init(&validated, media->audio, media->samples, &media->settings,
                              state->rate, false)) {
            return false;
        }
    }
    for (size_t i = 0; i < state->count; ++i) {
        const struct ra_identifier_settings *settings = &state->ids[i].settings;
        state->rules[i] =
            (struct ra_id_rule){.interval_ms = settings->interval_ms,
                                .priority = (int)settings->priority,
                                .first_key_only = settings->first_key_only,
                                .regardless_of_activity = settings->regardless_of_activity,
                                .polite = settings->polite,
                                .polite_maximum_wait_ms = settings->polite_maximum_wait_ms};
        state->states[i] = (struct ra_id_state){.satisfied_ms = now_ms};
    }
    state->duplex = (struct ra_duplex_state){0};
    state->playing = SIZE_MAX;
    state->receiving = false;
    state->receiver_key_ms = now_ms;
    state->transmit_key_ms = now_ms;
    state->timeout_until_ms = 0;
    state->timeout_wait_unkey = false;
    state->suppress_release = false;
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
    state->announcement_playback = (struct ra_playback){0};
    state->announcement_playing = SIZE_MAX;
    state->announcement_release_pending = false;
    for (size_t i = 0; i < state->announcement_count; ++i) {
        state->announcement_states[i] =
            (struct ra_controller_announcement_state){.satisfied_ms = now_ms};
    }
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
    bool was_receiving = state->receiving || state->link_was_active;
    if (interrupting_receive) {
        if (!was_receiving) {
            state->key_idle_ms = idle;
            state->suppress_release = false;
        }
        state->last_activity_ms = now_ms;
        ra_id_activity(state->states, state->count);
    }
    bool receiver_unkeyed = state->receiving && !receiving;
    bool receiver_keyed = !state->receiving && receiving;
    if (receiver_keyed) {
        state->receiver_key_ms = now_ms;
    }
    bool receiver_kerchunk = receiver_unkeyed && state->kerchunk_max_ms &&
                             now_ms - state->receiver_key_ms <= state->kerchunk_max_ms;
    if (receiver_unkeyed) {
        state->receiver_unkey_ms = now_ms;
    }
    state->receiving = receiving;
    state->link_was_active = state->link_active;
    if (receiver_unkeyed && !receiver_kerchunk) {
        courtesy_schedule(state, state->receiver_courtesy, true, NULL, now_ms);
    }
    if (receiver_kerchunk) {
        state->suppress_release = true;
    }
    if (receiver_keyed) {
        courtesy_cancel_pending(state, true, NULL);
    }
    if (interrupting_receive && state->playing != SIZE_MAX) {
        /* Carrier events interrupt prepared audio even without a sample tick. */
        (void)ra_playback_render(&state->playback, true, NULL, 0);
    }
    size_t selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                   state->full_duplex);
    bool may_transmit = state->full_duplex || !receiving;
    bool telemetry_idle = !interrupting_receive;
    bool pending_status = status_pending(state);
    bool status_ready = telemetry_idle &&
                        now_ms - state->receiver_unkey_ms >= RA_CONTROLLER_STATUS_UNKEY_DELAY_MS &&
                        pending_status;
    bool courtesy_ready = state->courtesy_pending_count != 0 && telemetry_idle &&
                          now_ms >= state->courtesy_pending[0].due_ms;
    bool ordinary_activity = interrupting_receive || state->status_playing ||
                             state->courtesy_playing || state->playing != SIZE_MAX;
    if (interrupting_receive || state->status_playing || state->courtesy_playing) {
        /* A resumed transmission restarts the ordinary-hang portion of a tail sequence. */
        state->announcement_release_pending = false;
    }
    size_t announcement_due = announcement_select(state, now_ms);
    bool announcement_playing = state->announcement_playing != SIZE_MAX;
    bool release_expired = state->duplex.keyed && !ordinary_activity && !announcement_playing &&
                           now_ms - state->duplex.last_audio_ms >= state->duplex.release_hang_ms;
    if (announcement_due != SIZE_MAX &&
        (release_expired || (!state->duplex.keyed && telemetry_idle &&
                             state->announcements[announcement_due].interval_ms))) {
        /* A periodic announcement keys from idle; every-release media waits for a true release. */
        state->announcement_release_pending = true;
    }
    bool telemetry_busy = pending_status || state->status_playing || state->courtesy_playing ||
                          state->courtesy_pending_count != 0 || announcement_playing;
    /* Preserve the ordinary hang before a pending ID and its following tail announcement. */
    bool id_after_ordinary_hang =
        announcement_due == SIZE_MAX || !state->duplex.keyed || release_expired;
    bool id_ready = selected != SIZE_MAX &&
                    !id_polite_deferred(&state->rules[selected], &state->states[selected], now_ms,
                                        interrupting_receive || telemetry_busy) &&
                    id_after_ordinary_hang;
    unsigned int courtesy_start_ready =
        (unsigned int)may_transmit & courtesy_ready & (unsigned int)!state->courtesy_playing &
        (unsigned int)!state->status_playing & (unsigned int)!announcement_playing &
        (unsigned int)(state->playing == SIZE_MAX);
    unsigned int status_start_ready =
        (unsigned int)may_transmit & (unsigned int)status_ready &
        (unsigned int)!state->status_playing & (unsigned int)!state->courtesy_playing &
        (unsigned int)!announcement_playing & (unsigned int)(state->playing == SIZE_MAX);
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
    /* Keep PTT asserted through each source's configured courtesy delay. */
    demand |= state->courtesy_pending_count != 0;
    demand |= announcement_playing;
    demand |= courtesy_ready;
    demand |= status_ready;
    demand |= samples && id_ready;
    /* Count independent flags so all are sampled without short-circuiting the radio callback. */
    unsigned int telemetry_busy_flags =
        (unsigned int)pending_status + (unsigned int)(state->courtesy_pending_count != 0) +
        (unsigned int)state->status_playing + (unsigned int)state->courtesy_playing +
        (unsigned int)announcement_playing;
    bool announcement_telemetry_busy = telemetry_busy_flags != 0;
    unsigned int announcement_ready_flags = (unsigned int)state->announcement_release_pending +
                                            (unsigned int)(announcement_due != SIZE_MAX) +
                                            (unsigned int)!announcement_telemetry_busy;
    bool announcement_ready = announcement_ready_flags == 3;
    demand |= announcement_ready;
    if (may_transmit && demand && !state->duplex.keyed) {
        ra_id_first_key(state->rules, state->states, state->count,
                        state->key_idle_ms > idle ? state->key_idle_ms : idle, now_ms);
        state->key_idle_ms = 0;
        selected = ra_id_select(state->rules, state->states, state->count, now_ms, receiving,
                                state->full_duplex);
        /* This branch only runs while !state->duplex.keyed, so the ordinary-hang gate is true. */
        id_ready = selected != SIZE_MAX &&
                   !id_polite_deferred(&state->rules[selected], &state->states[selected], now_ms,
                                       interrupting_receive || telemetry_busy);
    }
    if (samples && !state->status_playing && !state->courtesy_playing && !announcement_playing &&
        state->playing == SIZE_MAX && id_ready) {
        const struct ra_controller_id *id = &state->ids[selected];
        /* Startup validated this immutable media and configuration at this rate. */
        (void)ra_playback_init(&state->playback, id->audio, id->samples, &id->settings, state->rate,
                               interrupting_receive);
        state->playing = selected;
    } else if (samples && announcement_ready && state->playing == SIZE_MAX) {
        /* Identifiers always receive the final priority check before an announcement begins. */
        announcement_start(state, announcement_due);
    }
    bool identifier_audio = false;
    bool ordinary_audio = false;
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
            ordinary_audio |= made != 0;
            if (made < capacity) {
                status_finish(state);
            }
        } else if (may_transmit && state->courtesy_playing) {
            /* Courtesy media is ducked during a renewed input, not replaced by Morse like an ID. */
            made = ra_playback_render(&state->courtesy_playback, false, identifier, capacity);
            identifier_audio |= made != 0;
            ordinary_audio |= made != 0;
            if (made < capacity) {
                state->courtesy_playing = false;
            }
        } else if (may_transmit && state->playing != SIZE_MAX) {
            made = ra_playback_render(&state->playback, interrupting_receive, identifier, capacity);
            identifier_audio |= made != 0;
            ordinary_audio |= made != 0;
        } else if (may_transmit && state->announcement_playing != SIZE_MAX) {
            /* Announcements are ducked during activity but retain their selected media. */
            made = ra_playback_render(&state->announcement_playback, false, identifier, capacity);
            identifier_audio |= made != 0;
            if (state->announcement_playback.finished) {
                state->announcement_states[state->announcement_playing].satisfied_ms = now_ms;
                state->announcement_playing = SIZE_MAX;
            }
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
    if (state->announcement_playing == SIZE_MAX && announcement_select(state, now_ms) == SIZE_MAX) {
        state->announcement_release_pending = false;
    }
    bool identifier_active = identifier_audio || state->playing != SIZE_MAX;
    bool other_transmit = state->link_active || (state->full_duplex && receiving) ||
                          state->status_playing || state->courtesy_playing ||
                          state->courtesy_pending_count != 0 ||
                          state->announcement_playing != SIZE_MAX || status_ready;
    bool was_keyed = state->duplex.keyed;
    bool keyed = ra_duplex_update(
        &state->duplex, state->full_duplex, receiving, identifier_active || other_transmit, now_ms,
        identifier_active && !other_transmit ? RA_CONTROLLER_IDENTIFIER_RELEASE_MS
                                             : state->hang_ms);
    if (state->timeout_wait_unkey && !interrupting_receive && now_ms >= state->timeout_until_ms) {
        state->timeout_wait_unkey = false;
    }
    if (state->timeout_wait_unkey) {
        state->duplex.keyed = false;
        keyed = false;
    } else if (keyed && !was_keyed) {
        state->transmit_key_ms = now_ms;
    } else if (keyed && state->transmit_timeout_ms &&
               now_ms - state->transmit_key_ms >= state->transmit_timeout_ms) {
        state->timeout_until_ms = now_ms + state->timeout_lockout_ms;
        state->timeout_wait_unkey = true;
        state->duplex.keyed = false;
        keyed = false;
    }
    /* An every-release announcement follows only traffic that actually keyed the transmitter. */
    if (keyed && (ordinary_activity || ordinary_audio) && !state->suppress_release) {
        announcement_mark_release(state);
    }
    return keyed;
}
