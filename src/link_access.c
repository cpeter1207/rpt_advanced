/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Validate complete node-list tokens and enforce explicit denial first.
 */
#include "link_access.h"
#include <stddef.h>
#include <string.h>

bool ra_link_access_list_valid(const char *list) {
    const char *cursor = list + strspn(list, " \t");
    if (!*cursor) {
        return true;
    }
    for (;;) {
        size_t digits = strspn(cursor, "0123456789");
        if (!digits) {
            return false;
        }
        cursor += digits;
        cursor += strspn(cursor, " \t");
        if (!*cursor) {
            return true;
        }
        if (*cursor != ',') {
            return false;
        }
        ++cursor;
        cursor += strspn(cursor, " \t");
    }
}

/** @brief Search only complete node tokens in a validated list.
 * @param list Validated configuration list.
 * @param node Decimal node identity.
 * @return True on exact match, without numeric truncation or prefix matching.
 */
static bool contains(const char *list, const char *node) {
    size_t length = strlen(node);
    const char *cursor = list;
    while (*cursor) {
        cursor += strspn(cursor, " \t");
        size_t digits = strspn(cursor, "0123456789");
        if (digits == length && !strncmp(cursor, node, length)) {
            return true;
        }
        cursor += digits;
        cursor += strspn(cursor, " \t");
        if (*cursor) {
            ++cursor;
        }
    }
    return false;
}

bool ra_link_access_allowed(const char *allow, const char *deny, const char *node, bool verified) {
    if (!verified || contains(deny, node)) {
        return false;
    }
    return !allow[strspn(allow, " \t")] || contains(allow, node);
}
