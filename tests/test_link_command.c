/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Linking command grammar, ambiguity, and destination validation tests.
 */
#include "link_command.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Commands observed in 524950's app_rpt configuration, plus remaining link actions. */
static const struct ra_link_command_mapping mappings[] = {
    {"1", RA_LINK_DISCONNECT},
    {"2", RA_LINK_MONITOR},
    {"3", RA_LINK_TRANSCEIVE},
    {"4", RA_LINK_COMMAND},
    {"70", RA_LINK_STATUS},
    {"806", RA_LINK_DISCONNECT_ALL},
    {"72", RA_LINK_LAST_KEYED},
    {"75", RA_LINK_LOCAL_MONITOR},
    {"811", RA_LINK_DISCONNECT_PERMANENT},
    {"812", RA_LINK_PERMANENT_MONITOR},
    {"813", RA_LINK_PERMANENT_TRANSCEIVE},
    {"73", RA_LINK_FULL_STATUS},
    {"816", RA_LINK_RECONNECT_ALL},
    {"818", RA_LINK_PERMANENT_LOCAL_MONITOR},
    {"", RA_LINK_STATUS},
};

/** @brief Every supported action accepts only its documented argument shape. */
static void actions(void) {
    size_t count = sizeof(mappings) / sizeof(*mappings);
    assert(!ra_link_commands_validate(mappings, count));
    const char *commands[] = {"1524950",   "2524950", "3524950",  "4524950",   "70",
                              "806",       "72",      "75524950", "811524950", "812524950",
                              "813524950", "73",      "816",      "818524950"};
    for (size_t i = 0; i < sizeof(commands) / sizeof(*commands); ++i) {
        struct ra_link_command command;
        assert(ra_link_command_parse(mappings, count, commands[i], &command));
        assert(command.action == mappings[i].action);
        assert(!strcmp(command.node, commands[i] + strlen(mappings[i].digits)));
        char invalid[64];
        snprintf(invalid, sizeof(invalid), "%sA", commands[i]);
        command.node = "unchanged";
        assert(!ra_link_command_parse(mappings, count, invalid, &command));
        assert(!strcmp(command.node, "unchanged"));
    }
    struct ra_link_command command;
    assert(ra_link_command_parse(mappings, count, "30", &command));
    assert(!strcmp(command.node, "0"));
    const char *invalid[] = {"", "3", "*3524950", "3524950#", "80", "81", "99", "511", "6"};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(*invalid); ++i) {
        assert(!ra_link_command_parse(mappings, count, invalid[i], &command));
    }
    assert(!ra_link_command_parse(NULL, 0, "30", &command));
}

/** @brief Disabled mappings are valid; duplicate or prefix-ambiguous mappings are not. */
static void validation(void) {
    assert(!ra_link_commands_validate(NULL, 0));
    struct ra_link_command_mapping table[] = {
        {"", RA_LINK_STATUS}, {"A", RA_LINK_STATUS}, {"BCD", RA_LINK_STATUS}};
    assert(!ra_link_commands_validate(table, 3));
    table[2].digits = "A";
    assert(ra_link_commands_validate(table, 3));
    table[2].digits = "AB";
    assert(ra_link_commands_validate(table, 3));
    table[1].digits = "AB";
    table[2].digits = "A";
    assert(ra_link_commands_validate(table, 3));
    table[2].digits = "a";
    assert(ra_link_commands_validate(table, 3));
    table[2].digits = "1234567890123456789012345678901234567890123456789012345678901234";
    assert(ra_link_commands_validate(table, 3));
}

/** @brief Verify explicit completion, timeout, immediate status, cancellation, and overflow. */
static void collection(void) {
    struct ra_link_command_mapping table[RA_LINK_ACTION_COUNT];
    ra_link_commands_default(table);
    assert(!ra_link_commands_validate(table, RA_LINK_ACTION_COUNT));
    struct ra_link_collector collector = {0};
    char completed[128];
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '3', 0, completed));
    const char *request = "*3123";
    for (size_t i = 0; request[i]; ++i) {
        assert(
            !ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, request[i], 100, completed));
    }
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, 0, 3099, completed));
    assert(ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, 0, 3100, completed));
    assert(!strcmp(completed, "3123"));
    request = "*3123";
    for (size_t i = 0; request[i]; ++i) {
        assert(
            !ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, request[i], 4000, completed));
    }
    assert(ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '#', 4000, completed));
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '*', 5000, completed));
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '#', 5000, completed));
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '*', 5000, completed));
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '7', 5000, completed));
    assert(ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '0', 5000, completed));
    assert(!strcmp(completed, "70"));
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '*', 6000, completed));
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, 'x', 6000, completed));
    assert(!collector.active);
    assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '*', 6000, completed));
    for (size_t i = 0; i < sizeof(collector.digits); ++i) {
        assert(!ra_link_collect(&collector, table, RA_LINK_ACTION_COUNT, '9', 6000, completed));
    }
    assert(!collector.active);
}

/** @brief Run linking grammar tests.
 * @return Zero after assertions.
 */
int main(void) {
    actions();
    validation();
    collection();
    puts("link command grammar tests passed");
    return 0;
}
