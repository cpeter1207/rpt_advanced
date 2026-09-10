/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Identifier scheduling policy, independent of audio playback and Asterisk.
 */
#ifndef RPT_ADVANCED_IDENTIFIER_H
#define RPT_ADVANCED_IDENTIFIER_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/** @brief One resolved identifier set; omitted configuration has already inherited defaults. */
struct ra_id_rule {
    uint64_t interval_ms; /**< Positive interval measured with monotonic time. */
    int priority;         /**< Larger values take precedence; ties retain configuration order. */
    bool first_key_only;  /**< Eligible only following a sufficiently long idle period. */
    bool regardless_of_activity; /**< Periodic identification also occurs during inactivity. */
    bool polite; /**< Defer while reception or telemetry is active until a bounded deadline. */
    uint64_t polite_maximum_wait_ms; /**< Maximum elapsed polite delay after the ID becomes due. */
};

/** @brief Per-set runtime state owned by the node's single control thread. */
struct ra_id_state {
    uint64_t satisfied_ms;  /**< Time this set was last satisfied, initially node startup. */
    bool activity;          /**< Conversation activity has occurred since this set was satisfied. */
    bool first_key_pending; /**< A qualifying first-key event awaits identification. */
    uint64_t polite_due_ms; /**< First-key due time; periodic IDs derive it from satisfied time. */
};

/** @brief Record conversation activity, excluding IDs and transmitter hang time.
 * @param states Node-owned array of identifier state.
 * @param count Number of states; zero permits a null array.
 */
void ra_id_activity(struct ra_id_state *states, size_t count);

/** @brief Arm welcome sets when a transmitter first keys following inactivity.
 * @param rules Resolved set definitions, parallel to states.
 * @param states Node-owned runtime states.
 * @param count Number of sets; zero permits null arrays.
 * @param idle_ms Inactivity immediately preceding the activity that led to this key.
 * @param now_ms Monotonic time of the qualifying key event.
 */
void ra_id_first_key(const struct ra_id_rule *rules, struct ra_id_state *states, size_t count,
                     uint64_t idle_ms, uint64_t now_ms);

/** @brief Choose the highest-priority due set, without marking it played.
 * @param rules Resolved definitions with positive intervals.
 * @param states Corresponding runtime states.
 * @param count Number of sets; zero permits null arrays.
 * @param now_ms Monotonic time, no earlier than each state's satisfied_ms.
 * @param receiver_active Whether qualified receive activity is present.
 * @param full_duplex Whether identification may transmit during reception.
 * @return Selected array index, or SIZE_MAX when nothing can play.
 */
size_t ra_id_select(const struct ra_id_rule *rules, const struct ra_id_state *states, size_t count,
                    uint64_t now_ms, bool receiver_active, bool full_duplex);

/** @brief Satisfy the selected set and all sets of lower priority.
 * @param rules Resolved definitions.
 * @param states Corresponding runtime states.
 * @param count Number of sets.
 * @param selected Valid index returned by ra_id_select, not SIZE_MAX.
 * @param now_ms Monotonic completion time. Call only after successful playback.
 */
void ra_id_complete(const struct ra_id_rule *rules, struct ra_id_state *states, size_t count,
                    size_t selected, uint64_t now_ms);

#endif
