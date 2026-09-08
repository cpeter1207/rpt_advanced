/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Validate scope names, references, and options before starting any controller.
 */
#include "schema.h"
#include "settings.h"
#include <string.h>

/** @brief Section roles, with invalid input kept distinct from node names. */
enum scope_kind {
    INVALID,
    GENERAL,
    NODE,
    ID_DEFAULT,
    ID_NODE,
    ID_SET,
    MORSE_DEFAULT,
    MORSE_NODE,
    SPEECH_DEFAULT,
    SPEECH_NODE,
    TIME_DEFAULT,
    TIME_NODE
};
/** @brief Borrowed section interpretation; node may be a substring. */
struct scope {
    enum scope_kind kind; /**< Section role. */
    const char *node;     /**< Node substring, null for flat sections. */
    size_t node_length;   /**< Node substring length. */
};

/** @brief Interpret the documented flat and space-delimited scoped sections.
 * @param name Section name.
 * @return Role and referenced node; malformed scope is INVALID.
 */
static struct scope parse(const char *name) {
    if (!strcmp(name, "general")) {
        return (struct scope){GENERAL, NULL, 0};
    }
    if (!strcmp(name, "identifier")) {
        return (struct scope){ID_DEFAULT, NULL, 0};
    }
    if (!strcmp(name, "morse")) {
        return (struct scope){MORSE_DEFAULT, NULL, 0};
    }
    if (!strcmp(name, "speech")) {
        return (struct scope){SPEECH_DEFAULT, NULL, 0};
    }
    if (!strcmp(name, "time")) {
        return (struct scope){TIME_DEFAULT, NULL, 0};
    }
    if (!strncmp(name, "identifier ", 11)) {
        const char *node = name + 11;
        size_t length = strcspn(node, " \t\r\n[]");
        if (!length) {
            return (struct scope){INVALID, NULL, 0};
        }
        const char *end = node + length;
        if (!*end) {
            return (struct scope){ID_NODE, node, length};
        }
        if (*end == ' ' && end[1] && !strpbrk(end + 1, " \t\r\n[]")) {
            return (struct scope){ID_SET, node, length};
        }
        return (struct scope){INVALID, NULL, 0};
    }
    const char *prefixes[] = {"morse ", "speech ", "time "};
    const enum scope_kind kinds[] = {MORSE_NODE, SPEECH_NODE, TIME_NODE};
    for (size_t i = 0; i < sizeof(prefixes) / sizeof(prefixes[0]); ++i) {
        size_t prefix_length = strlen(prefixes[i]);
        if (!strncmp(name, prefixes[i], prefix_length)) {
            const char *node = name + prefix_length;
            size_t length = strcspn(node, " \t\r\n[]");
            return length && !node[length] ? (struct scope){kinds[i], node, length}
                                           : (struct scope){INVALID, NULL, 0};
        }
    }
    if (!*name || strpbrk(name, " \t\r\n[]")) {
        return (struct scope){INVALID, NULL, 0};
    }
    return (struct scope){NODE, name, strlen(name)};
}

/** @brief Compare a node substring with a complete node name.
 * @param scope Parsed node-bearing scope.
 * @param node Complete node name.
 * @return True for an exact match, never a prefix match.
 */
static bool matches(struct scope scope, const char *node) {
    return strlen(node) == scope.node_length && !strncmp(scope.node, node, scope.node_length);
}

/** @brief Detect duplicate section headers without changing option precedence.
 * @param document Parsed configuration.
 * @param index Header under consideration.
 * @return True if an identical earlier header exists.
 */
static bool duplicate(const struct ra_document *document, size_t index) {
    for (size_t i = 0; i < index; ++i) {
        if (!strcmp(document->sections[i], document->sections[index])) {
            return true;
        }
    }
    return false;
}

/** @brief Select a unique node or node-scoped section.
 * @param document Validated document.
 * @param kind Desired scope role.
 * @param node Optional node filter; null for node enumeration.
 * @param index Matching unique section index.
 * @return Borrowed complete section name or null.
 */
static const char *select_section(const struct ra_document *document, enum scope_kind kind,
                                  const char *node, size_t index) {
    for (size_t i = 0; i < document->section_count; ++i) {
        struct scope scope = parse(document->sections[i]);
        if (scope.kind == kind && (!node || matches(scope, node)) && !duplicate(document, i)) {
            if (!index) {
                return document->sections[i];
            }
            --index;
        }
    }
    return NULL;
}

const char *ra_document_node(const struct ra_document *document, size_t index) {
    return select_section(document, NODE, NULL, index);
}

const char *ra_document_identifier(const struct ra_document *document, const char *node,
                                   size_t index) {
    return select_section(document, ID_SET, node, index);
}

const char *ra_document_validate(const struct ra_document *document, const char **section,
                                 const char **key) {
    *section = NULL;
    *key = NULL;
    for (size_t i = 0; i < document->section_count; ++i) {
        struct scope scope = parse(document->sections[i]);
        *section = document->sections[i];
        if (scope.kind == INVALID) {
            return "invalid section name";
        }
        if (scope.kind == ID_NODE || scope.kind == ID_SET || scope.kind == MORSE_NODE ||
            scope.kind == SPEECH_NODE || scope.kind == TIME_NODE) {
            bool found = false;
            for (size_t j = 0; j < document->section_count; ++j) {
                if (parse(document->sections[j]).kind == NODE &&
                    matches(scope, document->sections[j])) {
                    found = true;
                    break;
                }
            }
            if (!found) {
                return "scoped section references an unknown node";
            }
        }
    }
    for (size_t i = 0; i < document->count; ++i) {
        const struct ra_config_entry *entry = &document->entries[i];
        enum scope_kind kind = parse(entry->section).kind;
        enum ra_settings_kind settings_kind = RA_SETTINGS_NODE;
        if (kind == ID_DEFAULT || kind == ID_NODE || kind == ID_SET) {
            settings_kind = RA_SETTINGS_IDENTIFIER;
        } else if (kind == MORSE_DEFAULT || kind == MORSE_NODE) {
            settings_kind = RA_SETTINGS_MORSE;
        } else if (kind == SPEECH_DEFAULT || kind == SPEECH_NODE) {
            settings_kind = RA_SETTINGS_SPEECH;
        } else if (kind == TIME_DEFAULT || kind == TIME_NODE) {
            settings_kind = RA_SETTINGS_TIME;
        }
        const char *error = ra_settings_validate_kind(settings_kind, entry->key, entry->value);
        if (error) {
            *section = entry->section;
            *key = entry->key;
            return error;
        }
    }
    *section = NULL;
    return NULL;
}
