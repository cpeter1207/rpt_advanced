/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Complete document schema and node/identifier enumeration tests.
 */
#include "schema.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Validate a multi-node document with duplicate headers and scoped sets. */
static void valid(void) {
    char *sections[] = {"general",
                        "identifier",
                        "abc",
                        "usb",
                        "usb1",
                        "usb",
                        "identifier usb",
                        "identifier usb welcome",
                        "identifier usb regular",
                        "identifier usb welcome",
                        "identifier usb1 welcome"};
    struct ra_config_entry entries[] = {
        {"general", "full_duplex", "yes"},
        {"usb", "radio_channel", "receiver"},
        {"identifier", "interval_ms", "600000"},
        {"identifier usb welcome", "first_key_only", "yes"},
    };
    struct ra_document document = {entries, 4, sections, sizeof(sections) / sizeof(sections[0])};
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
    assert(!strcmp(ra_document_identifier_defaults(&document, "usb"), "identifier usb"));
    assert(!ra_document_identifier_defaults(&document, "abc"));
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
                   "identifier usb[set"};
    for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); ++i) {
        struct ra_document document = {.sections = &bad[i], .section_count = 1};
        const char *section;
        const char *key;
        assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid section name"));
        assert(section == bad[i] && !key);
    }
    char *unknown[] = {"identifier missing", "identifier missing welcome"};
    for (size_t i = 0; i < 2; ++i) {
        struct ra_document document = {.sections = &unknown[i], .section_count = 1};
        const char *section;
        const char *key;
        assert(!strcmp(ra_document_validate(&document, &section, &key),
                       "identifier section references an unknown node"));
        assert(section == unknown[i] && !key);
    }
}

/** @brief Unknown and invalid options are rejected even when later overridden. */
static void options_invalid(void) {
    char *sections[] = {"usb", "identifier"};
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
    struct ra_config_entry overrides[] = {{"usb", "full_duplex", "maybe"},
                                          {"usb", "full_duplex", "yes"}};
    document.entries = overrides;
    document.count = 2;
    assert(!strcmp(ra_document_validate(&document, &section, &key), "invalid option value"));
    assert(!strcmp(section, "usb") && !strcmp(key, "full_duplex"));
}

/** @brief Execute all schema and enumeration tests.
 * @return Zero after all assertions pass.
 */
int main(void) {
    valid();
    sections_invalid();
    options_invalid();
    puts("whole-document schema and enumeration tests passed");
    return 0;
}
