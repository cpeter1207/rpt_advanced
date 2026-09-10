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
    ANNOUNCEMENT_DEFAULT,
    ANNOUNCEMENT_NODE,
    ANNOUNCEMENT_SET,
    COURTESY_DEFAULT,
    COURTESY_NODE,
    COURTESY_SET,
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
    if (!strcmp(name, "announcement")) {
        return (struct scope){ANNOUNCEMENT_DEFAULT, NULL, 0};
    }
    if (!strcmp(name, "courtesy")) {
        return (struct scope){COURTESY_DEFAULT, NULL, 0};
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
    const char *set_prefixes[] = {"identifier ", "announcement ", "courtesy "};
    const enum scope_kind set_node_kinds[] = {ID_NODE, ANNOUNCEMENT_NODE, COURTESY_NODE};
    const enum scope_kind set_kinds[] = {ID_SET, ANNOUNCEMENT_SET, COURTESY_SET};
    for (size_t i = 0; i < sizeof(set_prefixes) / sizeof(set_prefixes[0]); ++i) {
        size_t prefix_length = strlen(set_prefixes[i]);
        if (!strncmp(name, set_prefixes[i], prefix_length)) {
            const char *node = name + prefix_length;
            size_t length = strcspn(node, " \t\r\n[]");
            if (!length) {
                return (struct scope){INVALID, NULL, 0};
            }
            const char *end = node + length;
            if (!*end) {
                return (struct scope){set_node_kinds[i], node, length};
            }
            if (*end == ' ' && end[1] && !strpbrk(end + 1, " \t\r\n[]")) {
                return (struct scope){set_kinds[i], node, length};
            }
            return (struct scope){INVALID, NULL, 0};
        }
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

/** @brief Compare the node components of two parsed scoped-section names.
 * @param first First parsed scoped section.
 * @param second Second parsed scoped section.
 * @return True when both scopes name the same node exactly.
 */
static bool same_node(struct scope first, struct scope second) {
    return first.node_length == second.node_length &&
           !memcmp(first.node, second.node, first.node_length);
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

const char *ra_document_announcement(const struct ra_document *document, const char *node,
                                     size_t index) {
    return select_section(document, ANNOUNCEMENT_SET, node, index);
}

const char *ra_document_courtesy(const struct ra_document *document, const char *node,
                                 size_t index) {
    return select_section(document, COURTESY_SET, node, index);
}

/** @brief Validate named courtesy assignments that require resolved inherited values.
 * @param document Fully parsed, option-valid document.
 * @param section Receives the offending section on failure.
 * @param key Receives null because the conflict spans complete sections.
 * @return Null on success or a stable assignment diagnostic.
 */
static const char *validate_courtesies(const struct ra_document *document, const char **section,
                                       const char **key) {
    for (size_t current = 0; current < document->section_count; ++current) {
        struct scope current_scope = parse(document->sections[current]);
        if (current_scope.kind != COURTESY_SET || duplicate(document, current)) {
            continue;
        }
        const char *node = NULL;
        for (size_t index = 0; index < document->section_count; ++index) {
            if (parse(document->sections[index]).kind == NODE &&
                matches(current_scope, document->sections[index])) {
                /* Scan through duplicates too: every exact match names the same resolver scope. */
                node = document->sections[index];
            }
        }
        struct ra_courtesy_settings current_settings;
        const char *error =
            ra_courtesy_settings_resolve(document->entries, document->count, node,
                                         document->sections[current], &current_settings);
        if (error) {
            *section = document->sections[current];
            *key = NULL;
            return error;
        }
        for (size_t prior = 0; prior < current; ++prior) {
            struct scope prior_scope = parse(document->sections[prior]);
            if (prior_scope.kind != COURTESY_SET || duplicate(document, prior) ||
                !same_node(prior_scope, current_scope)) {
                continue;
            }
            struct ra_courtesy_settings prior_settings;
            (void)ra_courtesy_settings_resolve(document->entries, document->count, node,
                                               document->sections[prior], &prior_settings);
            bool duplicate_receiver = current_settings.input == RA_COURTESY_INPUT_RECEIVER &&
                                      prior_settings.input == RA_COURTESY_INPUT_RECEIVER;
            bool duplicate_generic_link = current_settings.input == RA_COURTESY_INPUT_LINK &&
                                          prior_settings.input == RA_COURTESY_INPUT_LINK &&
                                          !*current_settings.remote_node &&
                                          !*prior_settings.remote_node;
            bool duplicate_peer_link =
                current_settings.input == RA_COURTESY_INPUT_LINK &&
                prior_settings.input == RA_COURTESY_INPUT_LINK && *current_settings.remote_node &&
                !strcmp(current_settings.remote_node, prior_settings.remote_node);
            if (duplicate_receiver || duplicate_generic_link || duplicate_peer_link) {
                *section = document->sections[current];
                *key = NULL;
                return "duplicate courtesy input assignment";
            }
        }
    }
    return NULL;
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
        if (scope.kind == ID_NODE || scope.kind == ID_SET || scope.kind == ANNOUNCEMENT_NODE ||
            scope.kind == ANNOUNCEMENT_SET || scope.kind == COURTESY_NODE ||
            scope.kind == COURTESY_SET || scope.kind == MORSE_NODE || scope.kind == SPEECH_NODE ||
            scope.kind == TIME_NODE) {
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
        } else if (kind == ANNOUNCEMENT_DEFAULT || kind == ANNOUNCEMENT_NODE ||
                   kind == ANNOUNCEMENT_SET) {
            settings_kind = RA_SETTINGS_ANNOUNCEMENT;
        } else if (kind == COURTESY_DEFAULT || kind == COURTESY_NODE) {
            settings_kind = RA_SETTINGS_COURTESY;
        } else if (kind == COURTESY_SET) {
            settings_kind = RA_SETTINGS_COURTESY_SET;
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
    const char *courtesy_error = validate_courtesies(document, section, key);
    if (courtesy_error) {
        return courtesy_error;
    }
    *section = NULL;
    return NULL;
}
