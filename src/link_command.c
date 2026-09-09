/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exact linking command recognition with no macro or autopatch interpretation.
 */
#include "link_command.h"
#include <string.h>

void ra_link_commands_default(struct ra_link_command_mapping *mappings) {
    static const char *const prefixes[RA_LINK_ACTION_COUNT] = {
        "1",   "2",   "3",   "4",  "70",  "806", "72", "75",
        "811", "812", "813", "73", "816", "818", "10", "722"};
    for (size_t i = 0; i < RA_LINK_ACTION_COUNT; ++i) {
        mappings[i] = (struct ra_link_command_mapping){prefixes[i], (enum ra_link_action)i};
    }
}

/** @brief Distinguish commands taking a destination from node-free actions.
 * @param action Valid linking operation.
 * @return True when a decimal node argument is required.
 */
static bool takes_node(enum ra_link_action action) {
    switch (action) {
    case RA_LINK_STATUS:
    case RA_LINK_DISCONNECT_ALL:
    case RA_LINK_LAST_KEYED:
    case RA_LINK_FULL_STATUS:
    case RA_LINK_RECONNECT_ALL:
    case RA_LINK_DISCONNECT_NONPERMANENT_ALL:
    case RA_LINK_TIME:
        return false;
    default:
        return true;
    }
}

const char *ra_link_commands_validate(const struct ra_link_command_mapping *mappings,
                                      size_t count) {
    for (size_t i = 0; i < count; ++i) {
        const char *digits = mappings[i].digits;
        size_t length = strlen(digits);
        if (length > 63 || strspn(digits, "0123456789ABCD") != length) {
            return "link command prefixes require digits or uppercase A through D";
        }
        if (!length) {
            continue;
        }
        for (size_t j = 0; j < i; ++j) {
            size_t other = strlen(mappings[j].digits);
            size_t common = other < length ? other : length;
            if (other && !strncmp(digits, mappings[j].digits, common)) {
                const struct ra_link_command_mapping *longer =
                    length > other ? &mappings[i] : &mappings[j];
                if (other == length || takes_node(longer->action)) {
                    return "link command prefixes overlap";
                }
            }
        }
    }
    return NULL;
}

/** @brief Check whether a complete node-free command can still gain a valid digit.
 * @param mappings Validated command mappings.
 * @param count Mapping count.
 * @param digits Collected command digits with no initiating asterisk.
 * @return True when another node-free command has this command as a strict prefix.
 */
static bool may_extend(const struct ra_link_command_mapping *mappings, size_t count,
                       const char *digits) {
    size_t length = strlen(digits);
    for (size_t index = 0; index < count; ++index) {
        size_t candidate = strlen(mappings[index].digits);
        if (!takes_node(mappings[index].action) && candidate > length &&
            !strncmp(mappings[index].digits, digits, length)) {
            return true;
        }
    }
    return false;
}

bool ra_link_collect(struct ra_link_collector *collector,
                     const struct ra_link_command_mapping *mappings, size_t count, char digit,
                     uint64_t now_ms, char *completed) {
    if (digit == '*') {
        *collector = (struct ra_link_collector){.active = true, .last_ms = now_ms};
        return false;
    }
    if (!collector->active) {
        return false;
    }
    bool finish = digit == '#' || (!digit && now_ms - collector->last_ms >= 3000);
    if (digit && digit != '#') {
        if (!strchr("0123456789ABCD", digit) ||
            collector->length == sizeof(collector->digits) - 1) {
            collector->active = false;
            return false;
        }
        collector->digits[collector->length++] = digit;
        collector->digits[collector->length] = '\0';
        collector->last_ms = now_ms;
        struct ra_link_command command;
        finish = ra_link_command_parse(mappings, count, collector->digits, &command) &&
                 !*command.node && !may_extend(mappings, count, collector->digits);
    }
    if (!finish) {
        return false;
    }
    collector->active = false;
    if (!collector->length) {
        return false;
    }
    for (size_t i = 0; i <= collector->length; ++i) {
        completed[i] = collector->digits[i];
    }
    return true;
}

bool ra_link_command_parse(const struct ra_link_command_mapping *mappings, size_t count,
                           const char *digits, struct ra_link_command *result) {
    for (size_t i = 0; i < count; ++i) {
        if (*mappings[i].digits && !takes_node(mappings[i].action) &&
            !strcmp(mappings[i].digits, digits)) {
            *result = (struct ra_link_command){mappings[i].action, digits + strlen(digits)};
            return true;
        }
    }
    for (size_t i = 0; i < count; ++i) {
        size_t length = strlen(mappings[i].digits);
        if (!length || strncmp(digits, mappings[i].digits, length)) {
            continue;
        }
        const char *node = digits + length;
        if (!takes_node(mappings[i].action)) {
            continue;
        }
        if (!*node || strspn(node, "0123456789") != strlen(node)) {
            continue;
        }
        *result = (struct ra_link_command){mappings[i].action, node};
        return true;
    }
    return false;
}
