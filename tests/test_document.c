/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief End-to-end settings loading and deterministic allocation-failure cleanup.
 */
/** @brief Enable memory-backed FILE streams for document tests. */
#define _GNU_SOURCE
#include "document.h"
#include "schema.h"
#include "settings.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>

/** @brief Number of owned-document allocation attempts in this test. */
static size_t allocations;
/** @brief One-based allocation to fail; zero disables injection. */
static size_t fail_at;

/** @brief Linker-provided original strdup.
 * @param text Text to copy.
 * @return Allocated copy or null.
 */
char *__real_strdup(const char *text);
/** @brief Linker-provided original reallocarray.
 * @param pointer Previous allocation.
 * @param count Element count.
 * @param size Element size.
 * @return Reallocated storage or null.
 */
void *__real_reallocarray(void *pointer, size_t count, size_t size);

/** @brief Inject a single strdup failure without changing production allocation code.
 * @param text Text to copy.
 * @return Allocated copy or injected null.
 */
char *__wrap_strdup(const char *text) {
    return ++allocations == fail_at ? NULL : __real_strdup(text);
}

/** @brief Inject a single array-allocation failure.
 * @param pointer Existing array.
 * @param count Element count.
 * @param size Element size.
 * @return New array or injected null, preserving the old array on failure.
 */
void *__wrap_reallocarray(void *pointer, size_t count, size_t size) {
    return ++allocations == fail_at ? NULL : __real_reallocarray(pointer, count, size);
}

/** @brief Exercise real file reading through owned storage into inherited settings. */
static void load_and_resolve(void) {
    char input[] = "[usb]\ntransmit_hang_ms=500\n[general]\ntransmit_hang_ms=100\n"
                   "[other]\n[identifier]\nspeech_text=Welcome\n"
                   "[identifier usb welcome]\nspeech_text=\n";
    FILE *stream = fmemopen(input, strlen(input), "r");
    assert(stream);
    struct ra_document document = {0};
    size_t line;
    allocations = 0;
    assert(!ra_document_read(stream, &document, &line));
    size_t attempts = allocations;
    assert(line == 9 && document.section_count == 5 && document.count == 4);
    const char *section;
    const char *key;
    assert(!ra_document_validate(&document, &section, &key));
    assert(!strcmp(ra_document_node(&document, 0), "usb"));
    assert(!strcmp(ra_document_node(&document, 1), "other"));
    assert(!ra_document_node(&document, 2));
    assert(!strcmp(ra_document_identifier(&document, "usb", 0), "identifier usb welcome"));
    assert(!strcmp(document.sections[2], "other"));
    struct ra_node_settings node;
    struct ra_identifier_settings id;
    assert(!ra_node_settings_resolve(document.entries, document.count, "usb", &node));
    assert(node.hang_ms == 500 && !strcmp(node.channel, "usb"));
    assert(!ra_node_settings_resolve(document.entries, document.count, "other", &node));
    assert(node.hang_ms == 100);
    assert(!ra_identifier_settings_resolve(document.entries, document.count, NULL,
                                           "identifier usb welcome", &id));
    assert(!*id.speech_text);
    ra_document_destroy(&document);
    ra_document_destroy(&document);
    assert(!document.entries && !document.sections && !document.count && !document.section_count);
    for (fail_at = 1; fail_at <= attempts; ++fail_at) {
        rewind(stream);
        allocations = 0;
        assert(ra_document_read(stream, &document, &line));
        assert(!document.entries && !document.sections && !document.count &&
               !document.section_count);
    }
    fail_at = 0;
    assert(fclose(stream) == 0);
}

/** @brief Syntax failure discards previously retained options and sections. */
static void syntax_failure(void) {
    char input[] = "[usb]\nnode_enabled=yes\n[invalid";
    FILE *stream = fmemopen(input, strlen(input), "r");
    assert(stream);
    struct ra_document document = {0};
    size_t line;
    assert(!strcmp(ra_document_read(stream, &document, &line), "malformed section or option"));
    assert(line == 3 && !document.entries && !document.sections);
    assert(fclose(stream) == 0);
}

/** @brief Retain and discover global/node named definitions and globally ordered events. */
static void scheduler_document(void) {
    char input[] = "[general]\ncallsign=KG0BP\n[usb]\n"
                   "[template greeting]\ntext=Good ${greeting}, ${callsign}.\n"
                   "[template usb greeting]\ntext=Welcome to ${node}.\n"
                   "[macro clear]\naction=disconnect_all\n"
                   "[event usb first]\nat=daily 08:00\ntemplate=greeting\nmacro=clear\n"
                   "[event usb second]\nat=weekly Tuesday 19:00\nmessage=Net at ${time}.\n";
    FILE *stream = fmemopen(input, strlen(input), "r");
    assert(stream);
    struct ra_document document = {0};
    size_t line;
    assert(!ra_document_read(stream, &document, &line));
    const char *section;
    const char *key;
    assert(!ra_document_validate(&document, &section, &key));
    assert(
        !strcmp(ra_document_template_named(&document, "usb", "greeting"), "template usb greeting"));
    assert(!strcmp(ra_document_macro_named(&document, "usb", "clear"), "macro clear"));
    const char *node;
    assert(!strcmp(ra_document_event(&document, 0, &node), "event usb first") &&
           !strcmp(node, "usb"));
    assert(!strcmp(ra_document_event(&document, 1, &node), "event usb second") &&
           !strcmp(node, "usb"));
    assert(!ra_document_event(&document, 2, &node) && !node);
    struct ra_node_settings settings;
    assert(!ra_node_settings_resolve(document.entries, document.count, "usb", &settings));
    assert(!strcmp(settings.callsign, "KG0BP"));
    ra_document_destroy(&document);
    assert(fclose(stream) == 0);
}

/** @brief Parse and validate the complete shipped configuration example. */
static void shipped_example(void) {
    FILE *stream = fopen("examples/rpt_advanced.conf", "r");
    assert(stream);
    struct ra_document document = {0};
    size_t line;
    assert(!ra_document_read(stream, &document, &line));
    const char *section;
    const char *key;
    assert(!ra_document_validate(&document, &section, &key));
    assert(!strcmp(ra_document_node(&document, 0), "example"));
    assert(!ra_document_node(&document, 1));
    ra_document_destroy(&document);
    assert(fclose(stream) == 0);
}

/** @brief Run document ownership and settings integration tests.
 * @return Zero when every assertion passes.
 */
int main(void) {
    load_and_resolve();
    syntax_failure();
    scheduler_document();
    shipped_example();
    puts("owned configuration and settings integration tests passed");
    return 0;
}
