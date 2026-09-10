/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Verify scheduled macros reject all non-controller actions.
 */
#include "scheduled_action.h"
#include <assert.h>
#include <stdio.h>

/** @brief Exercise every allowed action and reject invalid names and destinations.
 * @return Zero after all assertions pass.
 */
int main(void) {
    enum ra_scheduled_action action;
    assert(!ra_scheduled_action_parse(NULL, &action));
    assert(!ra_scheduled_action_parse("connect", NULL));
    assert(ra_scheduled_action_parse("connect", &action) && action == RA_SCHEDULED_ACTION_CONNECT);
    assert(ra_scheduled_action_parse("disconnect", &action) &&
           action == RA_SCHEDULED_ACTION_DISCONNECT);
    assert(ra_scheduled_action_parse("disconnect_all", &action) &&
           action == RA_SCHEDULED_ACTION_DISCONNECT_ALL);
    assert(ra_scheduled_action_parse("reconnect_all", &action) &&
           action == RA_SCHEDULED_ACTION_RECONNECT_ALL);
    assert(!ra_scheduled_action_parse("", &action));
    assert(!ra_scheduled_action_parse("/bin/sh", &action));
    assert(!ra_scheduled_action_parse("connect; rm", &action));
    puts("scheduled action tests passed");
    return 0;
}
