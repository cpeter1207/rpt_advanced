/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief File-level parser tests, including actual stream errors and long lines.
 */
/** @brief Enable GNU stdio cookie streams for deterministic input-error tests. */
#define _GNU_SOURCE
#include "config_reader.h"
#include <assert.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

/** @brief Verify callback order and the complete untruncated option value.
 * @param context Event counter.
 * @param kind Parsed event.
 * @param name Parsed name.
 * @param value Parsed value.
 * @return Null for these valid events.
 */
static const char *accept(void *context, enum ra_config_line_kind kind, const char *name,
                          const char *value) {
    size_t *events = context;
    if ((*events)++ == 0) {
        assert(kind == RA_CONFIG_SECTION && !strcmp(name, "usb") && !value);
    } else {
        assert(kind == RA_CONFIG_OPTION && !strcmp(name, "speech_text"));
        assert(strlen(value) == 8192);
    }
    return NULL;
}

/** @brief Simulate schema or allocation rejection by the configuration builder.
 * @param context Unused.
 * @param kind Unused.
 * @param name Unused.
 * @param value Unused.
 * @return Stable builder diagnostic.
 */
static const char *reject(void *context, enum ra_config_line_kind kind, const char *name,
                          const char *value) {
    (void)context;
    (void)kind;
    (void)name;
    (void)value;
    return "builder rejected section";
}

/** @brief Produce a real stdio input error through a cookie stream.
 * @param cookie Unused.
 * @param buffer Unused.
 * @param size Unused.
 * @return Minus one with EIO.
 */
static ssize_t fail_read(void *cookie, char *buffer, size_t size) {
    (void)cookie;
    (void)buffer;
    (void)size;
    errno = EIO;
    return -1;
}

/** @brief Exercise errors with their exact physical line numbers. */
static void errors(void) {
    const struct {
        const char *text;
        size_t size;
        const char *error;
        size_t line;
    } cases[] = {
        {"bad=1\n", 6, "option appears before any section", 1},
        {"; comment\n[bad\n", 15, "malformed section or option", 2},
        {"[usb]\n", 6, "builder rejected section", 1},
        {"abc\0def\n", 8, "embedded null byte", 1},
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        FILE *stream = fmemopen((void *)cases[i].text, cases[i].size, "r");
        assert(stream);
        size_t line;
        assert(!strcmp(ra_config_read(stream, reject, NULL, &line), cases[i].error));
        assert(line == cases[i].line);
        assert(fclose(stream) == 0);
    }
    FILE *stream = fopencookie(NULL, "r", (cookie_io_functions_t){.read = fail_read});
    assert(stream);
    size_t line;
    assert(!strcmp(ra_config_read(stream, reject, NULL, &line), "cannot read configuration line"));
    assert(line == 1 && ferror(stream));
    assert(fclose(stream) == 0);
}

/** @brief Verify comments, final lines without newlines, and arbitrary line lengths. */
static void valid(void) {
    FILE *stream = tmpfile();
    assert(stream);
    assert(fputs("; ignored\n\n[usb]\nspeech_text=", stream) >= 0);
    for (size_t i = 0; i < 8192; ++i) {
        assert(fputc('a', stream) != EOF);
    }
    rewind(stream);
    size_t events = 0;
    size_t line;
    assert(!ra_config_read(stream, accept, &events, &line));
    assert(events == 2 && line == 4);
    assert(fclose(stream) == 0);
    stream = tmpfile();
    assert(stream);
    assert(!ra_config_read(stream, reject, NULL, &line) && line == 0);
    assert(fclose(stream) == 0);
}

/** @brief Run all stream reader tests.
 * @return Zero after passing assertions.
 */
int main(void) {
    errors();
    valid();
    puts("configuration stream tests passed");
    return 0;
}
