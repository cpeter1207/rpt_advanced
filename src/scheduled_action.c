/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Restrict scheduled macros to explicit controller operations.
 */
#include "scheduled_action.h"
#include <string.h>

bool ra_scheduled_action_parse(const char *text, enum ra_scheduled_action *action) {
    if (!text || !action)
        return false;
    if (!strcmp(text, "connect"))
        *action = RA_SCHEDULED_ACTION_CONNECT;
    else if (!strcmp(text, "disconnect"))
        *action = RA_SCHEDULED_ACTION_DISCONNECT;
    else if (!strcmp(text, "disconnect_all"))
        *action = RA_SCHEDULED_ACTION_DISCONNECT_ALL;
    else if (!strcmp(text, "reconnect_all"))
        *action = RA_SCHEDULED_ACTION_RECONNECT_ALL;
    else
        return false;
    return true;
}
