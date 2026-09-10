/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Complete document schema and named-media-set enumeration tests.
 */
#include "schema.h"
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
                        "courtesy usb north"};
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
                   "time usb extra"};
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
                       "time missing"};
    for (size_t i = 0; i < sizeof(unknown) / sizeof(unknown[0]); ++i) {
        struct ra_document document = {.sections = &unknown[i], .section_count = 1};
        const char *section;
        const char *key;
        assert(!strcmp(ra_document_validate(&document, &section, &key),
                       "scoped section references an unknown node"));
        assert(section == unknown[i] && !key);
    }
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

/** @brief Execute all schema and enumeration tests.
 * @return Zero after all assertions pass.
 */
int main(void) {
    valid();
    sections_invalid();
    options_invalid();
    courtesy_assignments();
    puts("whole-document schema and enumeration tests passed");
    return 0;
}
