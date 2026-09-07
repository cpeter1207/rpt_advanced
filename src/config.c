/** @file
 * @brief Resolve inheritance without copying strings or limiting the number of nodes.
 */
#include "config.h"
#include <ctype.h>
#include <string.h>

/** @brief Strip surrounding whitespace in place.
 * @param text Writable null-terminated text.
 * @return First non-whitespace byte, possibly the terminating null.
 */
static char *trim(char *text) {
    while (isspace((unsigned char)*text)) {
        ++text;
    }
    size_t length = strlen(text);
    while (length > 0 && isspace((unsigned char)text[length - 1])) {
        text[--length] = '\0';
    }
    return text;
}

enum ra_config_line_kind ra_config_parse_line(char *line, char **name, char **value) {
    *name = NULL;
    *value = NULL;
    char *comment = strchr(line, ';');
    if (comment) {
        *comment = '\0';
    }
    char *text = trim(line);
    if (!*text) {
        return RA_CONFIG_EMPTY;
    }
    if (*text == '[') {
        char *end = strchr(text + 1, ']');
        if (!end) {
            return RA_CONFIG_INVALID;
        }
        *end = '\0';
        char *section = trim(text + 1);
        if (!*section || *trim(end + 1)) {
            return RA_CONFIG_INVALID;
        }
        *name = section;
        return RA_CONFIG_SECTION;
    }
    char *equals = strchr(text, '=');
    if (!equals) {
        return RA_CONFIG_INVALID;
    }
    *equals = '\0';
    char *key = trim(text);
    if (!*key) {
        return RA_CONFIG_INVALID;
    }
    *name = key;
    *value = trim(equals + 1);
    return RA_CONFIG_OPTION;
}

const char *ra_config_lookup(const struct ra_config_entry *entries, size_t count, const char *key,
                             const char *shared, const char *node, const char *set) {
    const char *scopes[] = {shared, node, set};
    const char *result = NULL;
    for (size_t scope = 0; scope < sizeof(scopes) / sizeof(scopes[0]); ++scope) {
        if (!scopes[scope]) {
            continue;
        }
        for (size_t i = 0; i < count; ++i) {
            if (strcmp(entries[i].section, scopes[scope]) == 0 &&
                strcmp(entries[i].key, key) == 0) {
                result = entries[i].value;
            }
        }
    }
    return result;
}
