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
};

/** @brief Per-set runtime state owned by the node's single control thread. */
struct ra_id_state {
    uint64_t satisfied_ms;  /**< Time this set was last satisfied, initially node startup. */
    bool activity;          /**< Conversation activity has occurred since this set was satisfied. */
    bool first_key_pending; /**< A qualifying first-key event awaits identification. */
};

/** @brief Available identifier playback choices, ordered by fallback preference. */
enum ra_id_media {
    RA_ID_NONE,   /**< No playable identifier; do not substitute an unrelated message. */
    RA_ID_FILE,   /**< Configured file is available. */
    RA_ID_SPEECH, /**< Configured offline speech can be produced. */
    RA_ID_MORSE   /**< Configured Morse text; the terminal fallback. */
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
 */
void ra_id_first_key(const struct ra_id_rule *rules, struct ra_id_state *states, size_t count,
                     uint64_t idle_ms);

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

/** @brief Select playback or its next fallback from currently available sources.
 * @param receiver_active Whether reception requires Morse-only playback.
 * @param file_available Whether the configured sound file is usable.
 * @param speech_available Whether configured speech can be synthesized.
 * @param morse_available Whether Morse text is configured.
 * @return Preferred available medium, or RA_ID_NONE. Re-evaluate on receiver key
 * or playback failure, marking a failed source unavailable for that attempt.
 */
enum ra_id_media ra_id_media_select(bool receiver_active, bool file_available,
                                    bool speech_available, bool morse_available);

#endif
