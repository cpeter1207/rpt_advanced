/** @file
 * @brief Resolve inheritance without copying strings or limiting the number of nodes.
 */
#include "config.h"
#include <ctype.h>
#include <string.h>

bool ra_config_unsigned(const char *text, uint64_t minimum, uint64_t maximum, uint64_t *result) {
    uint64_t number = 0;
    if (!*text) {
        return false;
    }
    for (; *text; ++text) {
        if (*text < '0' || *text > '9') {
            return false;
        }
        unsigned int digit = (unsigned int)(*text - '0');
        if (number > UINT64_MAX / 10 || (number == UINT64_MAX / 10 && digit > UINT64_MAX % 10)) {
            return false;
        }
        number = number * 10 + digit;
    }
    if (number < minimum || number > maximum) {
        return false;
    }
    *result = number;
    return true;
}

bool ra_config_boolean(const char *text, bool *result) {
    if (strlen(text) == 3 && (text[0] == 'y' || text[0] == 'Y') &&
        (text[1] == 'e' || text[1] == 'E') && (text[2] == 's' || text[2] == 'S')) {
        *result = true;
        return true;
    }
    if (strlen(text) == 2 && (text[0] == 'n' || text[0] == 'N') &&
        (text[1] == 'o' || text[1] == 'O')) {
        *result = false;
        return true;
    }
    return false;
}

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
