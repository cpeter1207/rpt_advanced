/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Read configuration outside the audio thread and preserve useful error locations.
 */
/** @brief Request the POSIX getline interface on supported Debian targets. */
#define _POSIX_C_SOURCE 200809L
#include "config_reader.h"
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

const char *ra_config_read(FILE *stream, ra_config_consumer consume, void *context, size_t *line) {
    char *buffer = NULL;
    size_t capacity = 0;
    bool section_seen = false;
    const char *error = NULL;
    *line = 0;
    for (;;) {
        ssize_t length = getline(&buffer, &capacity, stream);
        if (length < 0) {
            if (!feof(stream)) {
                ++*line;
                error = "cannot read configuration line";
            }
            break;
        }
        ++*line;
        if (memchr(buffer, '\0', (size_t)length)) {
            error = "embedded null byte";
            break;
        }
        char *name;
        char *value;
        enum ra_config_line_kind kind = ra_config_parse_line(buffer, &name, &value);
        if (kind == RA_CONFIG_EMPTY) {
            continue;
        }
        if (kind == RA_CONFIG_INVALID) {
            error = "malformed section or option";
            break;
        }
        if (kind == RA_CONFIG_SECTION) {
            section_seen = true;
        } else if (!section_seen) {
            error = "option appears before any section";
            break;
        }
        error = consume(context, kind, name, value);
        if (error) {
            break;
        }
    }
    free(buffer);
    return error;
}
