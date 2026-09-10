/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Complete document schema and named-media-set enumeration tests.
 */
#include "message_template.h"
#include "schema.h"
#include "settings.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Validate a multi-node document with duplicate headers and scoped sets. */
static void valid(void) {
    char *sections[] = {"general",
                        "identifier",
                        "announcement",
                        "courtesy",
                        "morse",
                        "speech",
                        "time",
                        "template greeting",
                        "template usb greeting",
                        "template inherited",
                        "template usb inherited",
                        "template same",
                        "template abc same",
                        "macro clear",
                        "macro usb connect_news",
                        "macro same",
                        "macro abc same",
                        "abc",
                        "usb",
                        "usb1",
                        "usb",
                        "identifier usb",
                        "morse usb",
                        "speech usb",
                        "time usb",
                        "identifier usb welcome",
                        "identifier usb regular",
                        "identifier usb welcome",
                        "identifier usb1 welcome",
                        "announcement usb",
                        "announcement usb release",
                        "announcement usb periodic",
                        "announcement usb release",
                        "announcement usb1 periodic",
                        "courtesy usb",
                        "courtesy usb receiver",
                        "courtesy usb link",
                        "courtesy usb north",
                        "courtesy usb north",
                        "event usb morning",
                        "event abc evening"};
    struct ra_config_entry entries[] = {
        {"general", "full_duplex", "yes"},
        {"usb", "radio_channel", "receiver"},
        {"identifier", "interval_ms", "600000"},
        {"identifier usb", "priority", "1"},
        {"morse", "level_db", "-6"},
        {"morse usb", "frequency_hz", "800"},
        {"speech", "voice", "shared.onnx"},
        {"speech usb", "speed_percent", "100"},
        {"time usb", "format", "24"},
        {"template greeting", "text", "Good ${greeting}, ${callsign}."},
        {"template usb greeting", "text", "Welcome to ${node}."},
        {"template inherited", "text", "Inherited ${node}."},
        {"template same", "text", "Global."},
        {"template abc same", "text", "ABC."},
        {"macro clear", "action", "disconnect_all"},
        {"macro usb connect_news", "action", "connect"},
        {"macro usb connect_news", "target_node", "123456"},
        {"macro same", "action", "disconnect_all"},
        {"macro abc same", "action", "disconnect_all"},
        {"identifier usb welcome", "first_key_only", "yes"},
        {"announcement", "interval_ms", "0"},
        {"announcement usb", "speech_text", "Node default"},
        {"announcement usb release", "morse_text", "K"},
        {"announcement usb periodic", "interval_ms", "1800000"},
        {"courtesy", "level_db", "-20"},
        {"courtesy usb", "morse_text", "C"},
        {"courtesy usb receiver", "input", "receiver"},
        {"courtesy usb receiver", "morse_text", "R"},
        {"courtesy usb link", "input", "link"},
        {"courtesy usb link", "morse_text", "L"},
        {"courtesy usb north", "input", "link"},
        {"courtesy usb north", "remote_node", "123456"},
        {"courtesy usb north", "tone_sequence", "900Hz+1200Hz / 75ms, silence / 25ms"},
        {"event usb morning", "at", "daily 08:30"},
        {"event usb morning", "template", "greeting"},
        {"event usb morning", "macro", "connect_news"},
        {"event abc evening", "at", "weekly Friday 17:00"},
        {"event abc evening", "message", "Good ${greeting}."},
    };
    struct ra_document document = {entries, sizeof(entries) / sizeof(entries[0]), sections,
                                   sizeof(sections) / sizeof(sections[0])};
    const char *section;
    const char *key;
    assert(!ra_document_validate(&document, &section, &key) && !section && !key);
    assert(!strcmp(ra_document_node(&document, 0), "abc"));
    assert(!strcmp(ra_document_node(&document, 1), "usb"));
    assert(!strcmp(ra_document_node(&document, 2), "usb1"));
    assert(!ra_document_node(&document, 3));
    assert(!strcmp(ra_document_identifier(&document, "usb", 0), "identifier usb welcome"));
    assert(!strcmp(ra_document_identifier(&document, "usb", 1), "identifier usb regular"));
    assert(!ra_document_identifier(&document, "usb", 2));
    assert(!strcmp(ra_document_identifier(&document, "usb1", 0), "identifier usb1 welcome"));
    assert(!ra_document_identifier(&document, "abc", 0));
    assert(!strcmp(ra_document_announcement(&document, "usb", 0), "announcement usb release"));
    assert(!strcmp(ra_document_announcement(&document, "usb", 1), "announcement usb periodic"));
    assert(!ra_document_announcement(&document, "usb", 2));
    assert(!strcmp(ra_document_announcement(&document, "usb1", 0), "announcement usb1 periodic"));
    assert(!ra_document_announcement(&document, "abc", 0));
    assert(!strcmp(ra_document_courtesy(&document, "usb", 0), "courtesy usb receiver"));
    assert(!strcmp(ra_document_courtesy(&document, "usb", 1), "courtesy usb link"));
    assert(!strcmp(ra_document_courtesy(&document, "usb", 2), "courtesy usb north"));
    assert(!ra_document_courtesy(&document, "usb", 3));
    assert(!ra_document_courtesy(&document, "abc", 0));
    assert(
        !strcmp(ra_document_template_named(&document, "usb", "greeting"), "template usb greeting"));
    assert(!strcmp(ra_document_template_named(&document, "abc", "greeting"), "template greeting"));
    assert(!strcmp(ra_document_template_named(&document, "usb", "inherited"),
                   "template usb inherited"));
    assert(!strcmp(ra_document_template_named(&document, "usb", "same"), "template same"));
    assert(!strcmp(ra_document_template_named(&document, "abc", "same"), "template abc same"));
    assert(!ra_document_template_named(&document, NULL, "same"));
    assert(!ra_document_template_named(&document, "usb", NULL));
    assert(!strcmp(ra_document_macro_named(&document, "usb", "clear"), "macro clear"));
    assert(!strcmp(ra_document_macro_named(&document, "usb", "connect_news"),
                   "macro usb connect_news"));
    assert(!ra_document_template_named(&document, "usb", "missing"));
    assert(!ra_document_macro_named(&document, "abc", "connect_news"));
    assert(!strcmp(ra_document_macro_named(&document, "usb", "same"), "macro same"));
    assert(!strcmp(ra_document_macro_named(&document, "abc", "same"), "macro abc same"));
    assert(!ra_document_macro_named(&document, NULL, "same"));
    assert(!ra_document_macro_named(&document, "usb", NULL));
    const char *event_node;
    assert(!strcmp(ra_document_event(&document, 0, &event_node), "event usb morning") &&
           !strcmp(event_node, "usb"));
    assert(!strcmp(ra_document_event(&document, 1, &event_node), "event abc evening") &&
           !strcmp(event_node, "abc"));
    assert(!strcmp(ra_document_event(&document, 0, NULL), "event usb morning"));
    assert(!ra_document_event(&document, 2, &event_node) && !event_node);
    struct ra_document empty = {0};
    assert(!ra_document_validate(&empty, &section, &key) && !section && !key);
    assert(!ra_document_node(&empty, 0));
}

/** @brief Reject malformed scopes and missing node references before examining options. */
static void sections_invalid(void) {
    char *bad[] = {"",
                   "bad name",
                   "identifier  usb",
                   "identifier \tusb",
                   "identifier usb ",
                   "identifier usb\tset",
                   "identifier usb bad name",
                   "identifier usb[set",
                   "announcement  usb",
                   "announcement \tusb",
                   "announcement usb ",
                   "announcement usb\tset",
                   "announcement usb bad name",
                   "announcement usb[set",
                   "courtesy  usb",
                   "courtesy \tusb",
                   "courtesy usb ",
                   "courtesy usb\tset",
                   "courtesy usb bad name",
                   "courtesy usb[set",
                   "morse  usb",
                   "morse usb extra",
                   "speech usb ",
                   "speech usb extra",
                   "time usb ",
                   "time usb extra",
                   "template",
                   "template ",
                   "template  welcome",
                   "template usb ",
                   "template usb welcome extra",
                   "macro",
                   "macro usb ",
                   "macro usb clear extra",
                   "event",
                   "event usb",
                   "event usb ",
                   "event usb morning extra"};
    for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); ++i) {
        struct ra_document document = {.sections = &bad[i], .section_count = 1};
        const char *section;
        const char *key;
        assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid section name"));
        assert(section == bad[i] && !key);
    }
    char *unknown[] = {"identifier missing",   "identifier missing welcome",
                       "announcement missing", "announcement missing release",
                       "courtesy missing",     "courtesy missing receiver",
                       "morse missing",        "speech missing",
                       "time missing",         "template missing welcome",
                       "macro missing clear",  "event missing morning"};
    for (size_t i = 0; i < sizeof(unknown) / sizeof(unknown[0]); ++i) {
        struct ra_document document = {.sections = &unknown[i], .section_count = 1};
        const char *section;
        const char *key;
        assert(!strcmp(ra_document_validate(&document, &section, &key),
                       "scoped section references an unknown node"));
        assert(section == unknown[i] && !key);
    }

    char oversized_node[RA_NODE_NAME_MAX + 1];
    memset(oversized_node, 'n', sizeof(oversized_node) - 1);
    oversized_node[sizeof(oversized_node) - 1] = '\0';
    char *oversized_node_sections[] = {oversized_node};
    struct ra_document document = {.sections = oversized_node_sections,
                                   .section_count = sizeof(oversized_node_sections) /
                                                    sizeof(oversized_node_sections[0])};
    const char *section;
    const char *key;
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "node name exceeds transport limit"));
    assert(section == oversized_node && !key);

    char oversized_event[sizeof("event ") - 1 + RA_NODE_NAME_MAX + sizeof(" test")];
    snprintf(oversized_event, sizeof(oversized_event), "event %s test", oversized_node);
    char *oversized_event_sections[] = {oversized_event};
    document = (struct ra_document){.sections = oversized_event_sections,
                                    .section_count = sizeof(oversized_event_sections) /
                                                     sizeof(oversized_event_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "node name exceeds transport limit"));
    assert(section == oversized_event && !key);

    char *unbound_event_sections[] = {"event missing test", "event missing test"};
    document = (struct ra_document){.sections = unbound_event_sections,
                                    .section_count = sizeof(unbound_event_sections) /
                                                     sizeof(unbound_event_sections[0])};
    const char *node = "not-null";
    assert(!strcmp(ra_document_event(&document, 0, &node), "event missing test") && !node);
    assert(!strcmp(ra_document_event(&document, 0, NULL), "event missing test"));
    assert(!ra_document_event(&document, 1, NULL));
}

/** @brief Unknown and invalid options are rejected even when later overridden. */
static void options_invalid(void) {
    char *sections[] = {"usb", "identifier", "courtesy", "morse", "speech", "time"};
    struct ra_config_entry entry = {"usb", "bogus", "yes"};
    struct ra_document document = {&entry, 1, sections, 2};
    const char *section;
    const char *key;
    assert(!strcmp(ra_document_validate(&document, &section, &key), "unknown option"));
    assert(!strcmp(section, "usb") && !strcmp(key, "bogus"));
    entry.section = "identifier";
    assert(!strcmp(ra_document_validate(&document, &section, &key), "unknown option"));
    entry.key = "interval_ms";
    entry.value = "0";
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid option value"));
    entry.section = "announcement";
    assert(!ra_document_validate(&document, &section, &key));
    entry.key = "priority";
    assert(!strcmp(ra_document_validate(&document, &section, &key), "unknown option"));
    entry.section = "courtesy";
    entry.key = "input";
    entry.value = "receiver";
    assert(!strcmp(ra_document_validate(&document, &section, &key), "unknown option"));
    entry.section = "courtesy usb receiver";
    entry.key = "input";
    entry.value = "bad";
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid option value"));
    struct ra_config_entry overrides[] = {{"usb", "full_duplex", "maybe"},
                                          {"usb", "full_duplex", "yes"}};
    document.entries = overrides;
    document.count = 2;
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid option value"));
    assert(!strcmp(section, "usb") && !strcmp(key, "full_duplex"));
    entry.section = "morse";
    entry.key = "level_db";
    entry.value = "-61";
    document.entries = &entry;
    document.count = 1;
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid option value"));
    entry.section = "speech";
    entry.key = "level_db";
    entry.value = "-3";
    assert(!ra_document_validate(&document, &section, &key));
    entry.section = "time";
    entry.key = "format";
    entry.value = "13";
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid option value"));
}

/** @brief Named courtesy assignments reject only same-node input conflicts. */
static void courtesy_assignments(void) {
    const char *section;
    const char *key;
    char *receiver_sections[] = {"usb", "courtesy usb receiver", "courtesy usb spare"};
    struct ra_config_entry receiver_entries[] = {
        {"courtesy usb receiver", "input", "receiver"},
        {"courtesy usb spare", "input", "receiver"},
    };
    struct ra_document document = {
        receiver_entries, sizeof(receiver_entries) / sizeof(receiver_entries[0]), receiver_sections,
        sizeof(receiver_sections) / sizeof(receiver_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "duplicate courtesy input assignment"));
    assert(!strcmp(section, "courtesy usb spare") && !key);

    char *generic_sections[] = {"usb", "courtesy usb link", "courtesy usb spare"};
    struct ra_config_entry generic_entries[] = {
        {"courtesy usb link", "input", "link"},
        {"courtesy usb spare", "input", "link"},
    };
    document = (struct ra_document){
        generic_entries, sizeof(generic_entries) / sizeof(generic_entries[0]), generic_sections,
        sizeof(generic_sections) / sizeof(generic_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "duplicate courtesy input assignment"));
    assert(!strcmp(section, "courtesy usb spare") && !key);

    char *peer_sections[] = {"usb", "courtesy usb north", "courtesy usb south"};
    struct ra_config_entry peer_entries[] = {
        {"courtesy usb north", "input", "link"},
        {"courtesy usb north", "remote_node", "100"},
        {"courtesy usb south", "input", "link"},
        {"courtesy usb south", "remote_node", "100"},
    };
    document =
        (struct ra_document){peer_entries, sizeof(peer_entries) / sizeof(peer_entries[0]),
                             peer_sections, sizeof(peer_sections) / sizeof(peer_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "duplicate courtesy input assignment"));
    assert(!strcmp(section, "courtesy usb south") && !key);

    struct ra_config_entry distinct_peer_entries[] = {
        {"courtesy usb north", "input", "link"},
        {"courtesy usb north", "remote_node", "100"},
        {"courtesy usb south", "input", "link"},
        {"courtesy usb south", "remote_node", "200"},
    };
    document = (struct ra_document){
        distinct_peer_entries, sizeof(distinct_peer_entries) / sizeof(distinct_peer_entries[0]),
        peer_sections, sizeof(peer_sections) / sizeof(peer_sections[0])};
    assert(!ra_document_validate(&document, &section, &key) && !section && !key);

    /* Distinct node names exercise both exact comparison and unequal-length fast rejection. */
    char *distinct_sections[] = {"usb",
                                 "xyz",
                                 "usb1",
                                 "courtesy usb receiver",
                                 "courtesy usb link",
                                 "courtesy usb north",
                                 "courtesy xyz receiver",
                                 "courtesy xyz link",
                                 "courtesy xyz north",
                                 "courtesy usb1 receiver"};
    struct ra_config_entry distinct_entries[] = {
        {"courtesy usb receiver", "input", "receiver"},
        {"courtesy usb link", "input", "link"},
        {"courtesy usb north", "input", "link"},
        {"courtesy usb north", "remote_node", "100"},
        {"courtesy xyz receiver", "input", "receiver"},
        {"courtesy xyz link", "input", "link"},
        {"courtesy xyz north", "input", "link"},
        {"courtesy xyz north", "remote_node", "100"},
        {"courtesy usb1 receiver", "input", "receiver"},
    };
    document = (struct ra_document){
        distinct_entries, sizeof(distinct_entries) / sizeof(distinct_entries[0]), distinct_sections,
        sizeof(distinct_sections) / sizeof(distinct_sections[0])};
    assert(!ra_document_validate(&document, &section, &key) && !section && !key);

    /* A duplicate earlier set is ignored while later distinct sets retain their validation. */
    char *duplicate_prior_sections[] = {"usb", "courtesy usb peer", "courtesy usb peer",
                                        "courtesy usb link"};
    struct ra_config_entry duplicate_prior_entries[] = {
        {"courtesy usb peer", "input", "link"},
        {"courtesy usb peer", "remote_node", "100"},
        {"courtesy usb link", "input", "link"},
    };
    document = (struct ra_document){
        duplicate_prior_entries,
        sizeof(duplicate_prior_entries) / sizeof(duplicate_prior_entries[0]),
        duplicate_prior_sections,
        sizeof(duplicate_prior_sections) / sizeof(duplicate_prior_sections[0])};
    assert(!ra_document_validate(&document, &section, &key) && !section && !key);

    /* A generic link may follow a peer-specific link, and a receiver may follow either. */
    char *mixed_sections[] = {"usb", "courtesy usb peer", "courtesy usb link",
                              "courtesy usb receiver"};
    struct ra_config_entry mixed_entries[] = {
        {"courtesy usb peer", "input", "link"},
        {"courtesy usb peer", "remote_node", "100"},
        {"courtesy usb link", "input", "link"},
        {"courtesy usb receiver", "input", "receiver"},
    };
    document =
        (struct ra_document){mixed_entries, sizeof(mixed_entries) / sizeof(mixed_entries[0]),
                             mixed_sections, sizeof(mixed_sections) / sizeof(mixed_sections[0])};
    assert(!ra_document_validate(&document, &section, &key) && !section && !key);

    struct ra_config_entry missing_entries[] = {{"courtesy usb missing", "morse_text", "M"}};
    char *missing_sections[] = {"usb", "courtesy usb missing"};
    document = (struct ra_document){
        missing_entries, sizeof(missing_entries) / sizeof(missing_entries[0]), missing_sections,
        sizeof(missing_sections) / sizeof(missing_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key), "courtesy input is required"));
    assert(!strcmp(section, "courtesy usb missing") && !key);
    struct ra_config_entry receiver_remote[] = {
        {"courtesy usb receiver", "input", "receiver"},
        {"courtesy usb receiver", "remote_node", "100"},
    };
    char *receiver_remote_sections[] = {"usb", "courtesy usb receiver"};
    document = (struct ra_document){
        receiver_remote, sizeof(receiver_remote) / sizeof(receiver_remote[0]),
        receiver_remote_sections,
        sizeof(receiver_remote_sections) / sizeof(receiver_remote_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "courtesy remote node requires link input"));
}

/** @brief Reject incomplete named definitions, invalid template syntax, and unresolved event names.
 */
static void scheduler_definitions(void) {
    const char *section;
    const char *key;
    char *template_sections[] = {"usb", "template broken"};
    struct ra_config_entry template_entries[] = {{"template broken", "text", "${unknown}"}};
    struct ra_document document = {
        template_entries, sizeof(template_entries) / sizeof(template_entries[0]), template_sections,
        sizeof(template_sections) / sizeof(template_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid message template"));
    assert(!strcmp(section, "template broken") && !strcmp(key, "text"));

    char *empty_template_sections[] = {"usb", "template empty"};
    struct ra_config_entry empty_template[] = {{"template empty", "text", ""}};
    document = (struct ra_document){
        empty_template, sizeof(empty_template) / sizeof(empty_template[0]), empty_template_sections,
        sizeof(empty_template_sections) / sizeof(empty_template_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key), "template text is required"));
    assert(!strcmp(section, "template empty") && !key);

    char *macro_sections[] = {"usb", "macro call"};
    struct ra_config_entry macro_entries[] = {{"macro call", "action", "connect"}};
    document =
        (struct ra_document){macro_entries, sizeof(macro_entries) / sizeof(macro_entries[0]),
                             macro_sections, sizeof(macro_sections) / sizeof(macro_sections[0])};
    assert(
        !strcmp(ra_document_validate(&document, &section, &key), "macro target node is required"));
    assert(!strcmp(section, "macro call") && !key);

    char *event_sections[] = {"usb", "event usb test"};
    struct ra_config_entry missing_at[] = {{"event usb test", "message", "Hello"}};
    document =
        (struct ra_document){missing_at, sizeof(missing_at) / sizeof(missing_at[0]), event_sections,
                             sizeof(event_sections) / sizeof(event_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key), "event at is required"));
    assert(!strcmp(section, "event usb test") && !key);

    struct ra_config_entry missing_template[] = {{"event usb test", "at", "daily 01:00"},
                                                 {"event usb test", "template", "missing"}};
    document = (struct ra_document){
        missing_template, sizeof(missing_template) / sizeof(missing_template[0]), event_sections,
        sizeof(event_sections) / sizeof(event_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "event references an unknown template"));
    assert(!strcmp(section, "event usb test") && !strcmp(key, "template"));

    struct ra_config_entry missing_macro[] = {{"event usb test", "at", "daily 01:00"},
                                              {"event usb test", "macro", "missing"}};
    document =
        (struct ra_document){missing_macro, sizeof(missing_macro) / sizeof(missing_macro[0]),
                             event_sections, sizeof(event_sections) / sizeof(event_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "event references an unknown macro"));
    assert(!strcmp(section, "event usb test") && !strcmp(key, "macro"));

    struct ra_config_entry invalid_message[] = {{"event usb test", "at", "daily 01:00"},
                                                {"event usb test", "message", "${bad}"}};
    document =
        (struct ra_document){invalid_message, sizeof(invalid_message) / sizeof(invalid_message[0]),
                             event_sections, sizeof(event_sections) / sizeof(event_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid message template"));
    assert(!strcmp(section, "event usb test") && !strcmp(key, "message"));

    char oversized_text[RA_MESSAGE_TEMPLATE_OUTPUT_MAX + 1];
    memset(oversized_text, 'x', sizeof(oversized_text) - 1);
    oversized_text[sizeof(oversized_text) - 1] = '\0';
    char *oversized_template_sections[] = {"usb", "template oversized"};
    struct ra_config_entry oversized_template[] = {{"template oversized", "text", oversized_text}};
    document = (struct ra_document){
        oversized_template, sizeof(oversized_template) / sizeof(oversized_template[0]),
        oversized_template_sections,
        sizeof(oversized_template_sections) / sizeof(oversized_template_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "scheduled message exceeds maximum output"));
    assert(!strcmp(section, "template oversized") && !strcmp(key, "text"));

    struct ra_config_entry oversized_message[] = {
        {"event usb test", "at", "daily 01:00"},
        {"event usb test", "message", oversized_text},
    };
    document = (struct ra_document){
        oversized_message, sizeof(oversized_message) / sizeof(oversized_message[0]), event_sections,
        sizeof(event_sections) / sizeof(event_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key),
                   "scheduled message exceeds maximum output"));
    assert(!strcmp(section, "event usb test") && !strcmp(key, "message"));

    char oversized_target[RA_NODE_NAME_MAX + 1];
    memset(oversized_target, '1', sizeof(oversized_target) - 1);
    oversized_target[sizeof(oversized_target) - 1] = '\0';
    struct ra_config_entry oversized_macro[] = {{"macro call", "action", "connect"},
                                                {"macro call", "target_node", oversized_target}};
    document =
        (struct ra_document){oversized_macro, sizeof(oversized_macro) / sizeof(oversized_macro[0]),
                             macro_sections, sizeof(macro_sections) / sizeof(macro_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid option value"));
    assert(!strcmp(section, "macro call") && !strcmp(key, "target_node"));

    char *duplicate_sections[] = {"usb", "template same", "template same"};
    struct ra_config_entry duplicate_entries[] = {{"template same", "text", "Hello"}};
    document = (struct ra_document){
        duplicate_entries, sizeof(duplicate_entries) / sizeof(duplicate_entries[0]),
        duplicate_sections, sizeof(duplicate_sections) / sizeof(duplicate_sections[0])};
    assert(!strcmp(ra_document_validate(&document, &section, &key), "duplicate named definition"));
    assert(!strcmp(section, "template same") && !key);
}

/** @brief Execute all schema and enumeration tests.
 * @return Zero after all assertions pass.
 */
int main(void) {
    valid();
    sections_invalid();
    options_invalid();
    courtesy_assignments();
    scheduler_definitions();
    puts("whole-document schema and enumeration tests passed");
    return 0;
}
