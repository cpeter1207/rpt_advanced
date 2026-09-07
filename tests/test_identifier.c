/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Deterministic unit and state-sequence tests for identifier scheduling.
 */
#include "identifier.h"
#include <assert.h>
#include <stdio.h>

/** @brief Exhaust the playback fallback truth table, including receiver interruption. */
static void test_media(void) {
    for (unsigned flags = 0; flags < 16; ++flags) {
        bool rx = (flags & 8) != 0;
        bool file = (flags & 4) != 0;
        bool speech = (flags & 2) != 0;
        bool morse = (flags & 1) != 0;
        enum ra_id_media expected = morse ? RA_ID_MORSE : RA_ID_NONE;
        if (!rx && speech) {
            expected = RA_ID_SPEECH;
        }
        if (!rx && file) {
            expected = RA_ID_FILE;
        }
        assert(ra_id_media_select(rx, file, speech, morse) == expected);
    }
}

/** @brief Check empty configuration and harmless empty-array activity events. */
static void test_empty(void) {
    ra_id_activity(NULL, 0);
    ra_id_first_key(NULL, NULL, 0, 0);
    assert(ra_id_select(NULL, NULL, 0, 0, false, false) == SIZE_MAX);
}

/** @brief Verify exact interval boundaries, priority, ties, and hierarchy completion. */
static void test_periods(void) {
    const struct ra_id_rule rules[] = {{100, 1, false, false},
                                       {100, 3, false, true},
                                       {100, 3, false, true},
                                       {100, 2, false, true}};
    struct ra_id_state states[4] = {{0}};
    assert(ra_id_select(rules, states, 4, 99, false, true) == SIZE_MAX);
    assert(ra_id_select(rules, states, 4, 100, false, true) == 1);
    assert(ra_id_select(rules, states, 4, 100, true, false) == SIZE_MAX);
    assert(ra_id_select(rules, states, 4, 100, true, true) == 1);
    ra_id_activity(states, 4);
    assert(ra_id_select(rules, states, 4, 100, false, true) == 1);
    ra_id_complete(rules, states, 4, 3, 100);
    assert(states[0].satisfied_ms == 100 && !states[0].activity);
    assert(states[1].satisfied_ms == 0 && states[1].activity);
    ra_id_complete(rules, states, 4, 1, 100);
    assert(states[2].satisfied_ms == 0 && states[2].activity);
    assert(ra_id_select(rules, states, 4, 100, false, true) == 2);
    ra_id_complete(rules, states, 4, 2, 100);
    for (size_t i = 0; i < 4; ++i) {
        assert(states[i].satisfied_ms == 100 && !states[i].activity);
    }
    assert(ra_id_select(rules, states, 4, 199, false, false) == SIZE_MAX);
    assert(ra_id_select(rules, states, 4, 200, false, false) == 1);
}

/** @brief Verify a quiet node stops periodic IDs unless explicitly configured otherwise. */
static void test_activity(void) {
    const struct ra_id_rule rule = {100, 0, false, false};
    struct ra_id_state state = {0};
    assert(ra_id_select(&rule, &state, 1, 1000, false, false) == SIZE_MAX);
    ra_id_activity(&state, 1);
    assert(ra_id_select(&rule, &state, 1, 1000, false, false) == 0);
    ra_id_complete(&rule, &state, 1, 0, 1000);
    assert(ra_id_select(&rule, &state, 1, 2000, false, false) == SIZE_MAX);
}

/** @brief Exercise welcome-only IDs, reception deferral, and conversation IDs together. */
static void test_welcome(void) {
    const struct ra_id_rule rules[] = {{100, 10, true, true}, {100, 1, false, false}};
    struct ra_id_state states[2] = {{0}};
    ra_id_first_key(rules, states, 2, 99);
    assert(!states[0].first_key_pending);
    assert(ra_id_select(rules, states, 2, 1000, false, true) == SIZE_MAX);
    ra_id_first_key(rules, states, 2, 100);
    ra_id_activity(states, 2);
    assert(ra_id_select(rules, states, 2, 1000, true, false) == SIZE_MAX);
    assert(ra_id_select(rules, states, 2, 1000, false, false) == 0);
    /* Playback failure does not satisfy the scheduled ID. */
    assert(ra_id_select(rules, states, 2, 1001, false, false) == 0);
    ra_id_complete(rules, states, 2, 0, 1001);
    assert(!states[0].first_key_pending);
    ra_id_activity(states, 2);
    assert(ra_id_select(rules, states, 2, 1101, false, true) == 1);
    ra_id_complete(rules, states, 2, 1, 1101);
    assert(ra_id_select(rules, states, 2, 10000, false, true) == SIZE_MAX);
}

/** @brief Run all policy tests.
 * @return Zero if every assertion passes.
 */
int main(void) {
    test_empty();
    test_media();
    test_periods();
    test_activity();
    test_welcome();
    puts("identifier policy tests passed");
    return 0;
}
