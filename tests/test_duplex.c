/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Duplex truth-table, hang-time boundaries, and identifier integration tests.
 */
#include "duplex.h"
#include "identifier.h"
#include <assert.h>
#include <stdio.h>

/** @brief Test every input combination, including idle startup and zero hang time. */
static void test_modes(void) {
    for (unsigned flags = 0; flags < 8; ++flags) {
        struct ra_duplex_state state = {0};
        bool full = (flags & 4) != 0;
        bool rx = (flags & 2) != 0;
        bool id = (flags & 1) != 0;
        bool expected = full ? (rx || id) : (!rx && id);
        assert(ra_duplex_update(&state, full, rx, id, 100, 0) == expected);
        assert(!ra_duplex_update(&state, full, false, false, 100, 0));
    }
}

/** @brief Hold PTT through short gaps, restart hang time with audio, and yield to half-duplex RX.
 */
static void test_hang(void) {
    struct ra_duplex_state state = {0};
    assert(!ra_duplex_update(&state, true, false, false, 0, 100));
    assert(ra_duplex_update(&state, true, true, false, 1, 100));
    assert(ra_duplex_update(&state, true, false, false, 100, 100));
    assert(ra_duplex_update(&state, true, true, false, 100, 100));
    assert(ra_duplex_update(&state, true, false, false, 199, 100));
    assert(!ra_duplex_update(&state, true, false, false, 200, 100));
    assert(ra_duplex_update(&state, false, false, true, 201, 100));
    assert(!ra_duplex_update(&state, false, true, false, 202, 100));
}

/** @brief Run an ID deadline through half-duplex deferral, playback, and PTT release. */
static void test_identifier_sequence(void) {
    struct ra_duplex_state radio = {0};
    const struct ra_id_rule rule = {.interval_ms = 100};
    struct ra_id_state id = {0};
    ra_id_activity(&id, 1);
    assert(ra_id_select(&rule, &id, 1, 100, true, false) == SIZE_MAX);
    assert(!ra_duplex_update(&radio, false, true, false, 100, 20));
    assert(ra_id_select(&rule, &id, 1, 120, false, false) == 0);
    assert(ra_duplex_update(&radio, false, false, true, 120, 20));
    ra_id_complete(&rule, &id, 1, 0, 130);
    assert(ra_duplex_update(&radio, false, false, false, 139, 20));
    assert(!ra_duplex_update(&radio, false, false, false, 140, 20));
    assert(ra_id_select(&rule, &id, 1, 1000, false, false) == SIZE_MAX);
}

/** @brief Run duplex unit and integration sequences.
 * @return Zero when assertions pass.
 */
int main(void) {
    test_modes();
    test_hang();
    test_identifier_sequence();
    puts("duplex policy and identifier integration tests passed");
    return 0;
}
