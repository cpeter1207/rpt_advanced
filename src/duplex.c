/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Half/full-duplex transmit policy without audio or hardware dependencies.
 */
#include "duplex.h"

bool ra_duplex_update(struct ra_duplex_state *state, bool full_duplex, bool receiver_active,
                      bool transmit_active, uint64_t now_ms, uint64_t hang_ms) {
    if (!full_duplex && receiver_active) {
        state->keyed = false;
    } else if (transmit_active || (full_duplex && receiver_active)) {
        state->keyed = true;
        state->last_audio_ms = now_ms;
        state->release_hang_ms = hang_ms;
    } else if (state->keyed && now_ms - state->last_audio_ms >= state->release_hang_ms) {
        state->keyed = false;
    }
    return state->keyed;
}
