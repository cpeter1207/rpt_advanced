/** @file
 * @brief Original identifier policy using monotonic deadlines and explicit completion.
 */
#include "identifier.h"

void ra_id_activity(struct ra_id_state *states, size_t count) {
    for (size_t i = 0; i < count; ++i) {
        states[i].activity = true;
    }
}

void ra_id_first_key(const struct ra_id_rule *rules, struct ra_id_state *states, size_t count,
                     uint64_t idle_ms) {
    for (size_t i = 0; i < count; ++i) {
        if (rules[i].first_key_only && idle_ms >= rules[i].interval_ms) {
            states[i].first_key_pending = true;
        }
    }
}

size_t ra_id_select(const struct ra_id_rule *rules, const struct ra_id_state *states, size_t count,
                    uint64_t now_ms, bool receiver_active, bool full_duplex) {
    size_t selected = SIZE_MAX;
    if (receiver_active && !full_duplex) {
        return selected;
    }
    for (size_t i = 0; i < count; ++i) {
        bool due;
        if (rules[i].first_key_only) {
            due = states[i].first_key_pending;
        } else {
            due = (now_ms - states[i].satisfied_ms >= rules[i].interval_ms) &&
                  (rules[i].regardless_of_activity || states[i].activity);
        }
        if (due && (selected == SIZE_MAX || rules[i].priority > rules[selected].priority)) {
            selected = i;
        }
    }
    return selected;
}

void ra_id_complete(const struct ra_id_rule *rules, struct ra_id_state *states, size_t count,
                    size_t selected, uint64_t now_ms) {
    for (size_t i = 0; i < count; ++i) {
        if (i == selected || rules[i].priority < rules[selected].priority) {
            states[i].satisfied_ms = now_ms;
            states[i].activity = false;
            states[i].first_key_pending = false;
        }
    }
}

enum ra_id_media ra_id_media_select(bool receiver_active, bool file_available,
                                    bool speech_available, bool morse_available) {
    if (!receiver_active) {
        if (file_available) {
            return RA_ID_FILE;
        }
        if (speech_available) {
            return RA_ID_SPEECH;
        }
    }
    return morse_available ? RA_ID_MORSE : RA_ID_NONE;
}
