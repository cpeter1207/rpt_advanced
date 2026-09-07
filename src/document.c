/** @file
 * @brief Own parsed strings outside the real-time path, with failure-atomic loading.
 */
/** @brief Request Debian's overflow-checking reallocarray and POSIX strdup. */
#define _DEFAULT_SOURCE
#include "document.h"
#include <stdlib.h>
#include <string.h>

void ra_document_destroy(struct ra_document *document) {
    for (size_t i = 0; i < document->count; ++i) {
        free((void *)document->entries[i].key);
        free((void *)document->entries[i].value);
    }
    for (size_t i = 0; i < document->section_count; ++i) {
        free(document->sections[i]);
    }
    free(document->entries);
    free(document->sections);
    *document = (struct ra_document){0};
}

/** @brief Retain a parser event; uncommitted allocations are freed locally.
 * @param context Temporary document.
 * @param kind Section or option, supplied by the syntax reader.
 * @param name Borrowed event name.
 * @param value Borrowed value, null for sections.
 * @return Null on success or an allocation diagnostic.
 */
static const char *retain(void *context, enum ra_config_line_kind kind, const char *name,
                          const char *value) {
    struct ra_document *document = context;
    char *owned_name = strdup(name);
    if (!owned_name) {
        return "out of memory retaining configuration name";
    }
    if (kind == RA_CONFIG_SECTION) {
        char **sections =
            reallocarray(document->sections, document->section_count + 1, sizeof(*sections));
        if (!sections) {
            free(owned_name);
            return "out of memory retaining configuration section";
        }
        document->sections = sections;
        document->sections[document->section_count++] = owned_name;
    } else {
        char *owned_value = strdup(value);
        if (!owned_value) {
            free(owned_name);
            return "out of memory retaining configuration value";
        }
        struct ra_config_entry *entries =
            reallocarray(document->entries, document->count + 1, sizeof(*entries));
        if (!entries) {
            free(owned_name);
            free(owned_value);
            return "out of memory retaining configuration option";
        }
        document->entries = entries;
        document->entries[document->count].section =
            document->sections[document->section_count - 1];
        document->entries[document->count].key = owned_name;
        document->entries[document->count].value = owned_value;
        ++document->count;
    }
    return NULL;
}

const char *ra_document_read(FILE *stream, struct ra_document *document, size_t *line) {
    struct ra_document temporary = {0};
    const char *error = ra_config_read(stream, retain, &temporary, line);
    if (error) {
        ra_document_destroy(&temporary);
        return error;
    }
    *document = temporary;
    return NULL;
}
