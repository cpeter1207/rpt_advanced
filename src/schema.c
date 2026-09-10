/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Validate scope names, references, and options before starting any controller.
 */
#include "schema.h"
#include "message_template.h"
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
    TIME_NODE,
    TEMPLATE_GLOBAL,
    TEMPLATE_NODE,
    MACRO_GLOBAL,
    MACRO_NODE,
    EVENT_NODE
};
/** @brief Borrowed section interpretation; node and label may be substrings. */
struct scope {
    enum scope_kind kind; /**< Section role. */
    const char *node;     /**< Node substring, empty for flat/global sections. */
    size_t node_length;   /**< Node substring length. */
    const char *label;    /**< Named template, macro, or event substring when present. */
    size_t label_length;  /**< Named label length. */
};

/** @brief Construct a scope without a node or named-definition label. */
#define FLAT_SCOPE(role) ((struct scope){.kind = (role), .node = ""})

/** @brief Construct a scope that names a complete node but no named-definition label. */
#define NODE_SCOPE(role, name, length)                                                             \
    ((struct scope){.kind = (role), .node = (name), .node_length = (length)})

/** @brief Interpret the documented flat and space-delimited scoped sections.
 * @param name Section name.
 * @return Role and referenced node; malformed scope is INVALID.
 */
static struct scope parse(const char *name) {
    if (!strcmp(name, "general")) {
        return FLAT_SCOPE(GENERAL);
    }
    if (!strcmp(name, "identifier")) {
        return FLAT_SCOPE(ID_DEFAULT);
    }
    if (!strcmp(name, "announcement")) {
        return FLAT_SCOPE(ANNOUNCEMENT_DEFAULT);
    }
    if (!strcmp(name, "courtesy")) {
        return FLAT_SCOPE(COURTESY_DEFAULT);
    }
    if (!strcmp(name, "morse")) {
        return FLAT_SCOPE(MORSE_DEFAULT);
    }
    if (!strcmp(name, "speech")) {
        return FLAT_SCOPE(SPEECH_DEFAULT);
    }
    if (!strcmp(name, "time")) {
        return FLAT_SCOPE(TIME_DEFAULT);
    }
    if (!strcmp(name, "template") || !strcmp(name, "macro") || !strcmp(name, "event")) {
        return FLAT_SCOPE(INVALID);
    }
    const char *named_prefixes[] = {"template ", "macro ", "event "};
    const enum scope_kind named_global_kinds[] = {TEMPLATE_GLOBAL, MACRO_GLOBAL, INVALID};
    const enum scope_kind named_node_kinds[] = {TEMPLATE_NODE, MACRO_NODE, EVENT_NODE};
    for (size_t i = 0; i < sizeof(named_prefixes) / sizeof(named_prefixes[0]); ++i) {
        size_t prefix_length = strlen(named_prefixes[i]);
        if (!strncmp(name, named_prefixes[i], prefix_length)) {
            const char *first = name + prefix_length;
            size_t first_length = strcspn(first, " \t\r\n[]");
            if (!first_length) {
                return FLAT_SCOPE(INVALID);
            }
            const char *end = first + first_length;
            if (!*end && named_global_kinds[i] != INVALID) {
                return (struct scope){named_global_kinds[i], "", 0, first, first_length};
            }
            if (*end != ' ' || !end[1] || strpbrk(end + 1, " \t\r\n[]")) {
                return FLAT_SCOPE(INVALID);
            }
            return (struct scope){named_node_kinds[i], first, first_length, end + 1,
                                  strlen(end + 1)};
        }
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
                return FLAT_SCOPE(INVALID);
            }
            const char *end = node + length;
            if (!*end) {
                return NODE_SCOPE(set_node_kinds[i], node, length);
            }
            if (*end == ' ' && end[1] && !strpbrk(end + 1, " \t\r\n[]")) {
                return NODE_SCOPE(set_kinds[i], node, length);
            }
            return FLAT_SCOPE(INVALID);
        }
    }
    const char *prefixes[] = {"morse ", "speech ", "time "};
    const enum scope_kind kinds[] = {MORSE_NODE, SPEECH_NODE, TIME_NODE};
    for (size_t i = 0; i < sizeof(prefixes) / sizeof(prefixes[0]); ++i) {
        size_t prefix_length = strlen(prefixes[i]);
        if (!strncmp(name, prefixes[i], prefix_length)) {
            const char *node = name + prefix_length;
            size_t length = strcspn(node, " \t\r\n[]");
            return length && !node[length] ? NODE_SCOPE(kinds[i], node, length)
                                           : FLAT_SCOPE(INVALID);
        }
    }
    if (!*name || strpbrk(name, " \t\r\n[]")) {
        return FLAT_SCOPE(INVALID);
    }
    return NODE_SCOPE(NODE, name, strlen(name));
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

/** @brief Compare one parsed named-section label with a complete label string.
 * @param scope Parsed template, macro, or event section.
 * @param label Complete label to compare.
 * @return True for an exact case-sensitive match.
 */
static bool same_label(struct scope scope, const char *label) {
    return scope.label && strlen(label) == scope.label_length &&
           !strncmp(scope.label, label, scope.label_length);
}

/** @brief Return the complete declared node name that owns one scoped section.
 * @param document Validated document.
 * @param scope Parsed node-scoped section.
 * @return Borrowed node section name, or null when no node matches.
 */
static const char *scope_node(const struct ra_document *document, struct scope scope) {
    for (size_t index = 0; index < document->section_count; ++index) {
        if (parse(document->sections[index]).kind == NODE &&
            matches(scope, document->sections[index])) {
            return document->sections[index];
        }
    }
    return NULL;
}

/** @brief Select a global named definition with an optional node-specific override.
 * @param document Validated document.
 * @param global_kind Global named section role.
 * @param node_kind Node-specific named section role.
 * @param node Local node requesting the definition.
 * @param label Case-sensitive definition label.
 * @return Borrowed full node override or global section name, or null when absent.
 */
static const char *select_named(const struct ra_document *document, enum scope_kind global_kind,
                                enum scope_kind node_kind, const char *node, const char *label) {
    const char *global = NULL;
    const char *override = NULL;
    for (size_t index = 0; index < document->section_count; ++index) {
        struct scope scope = parse(document->sections[index]);
        if (duplicate(document, index) || !same_label(scope, label)) {
            continue;
        }
        if (scope.kind == global_kind) {
            global = document->sections[index];
        } else if (scope.kind == node_kind && matches(scope, node)) {
            override = document->sections[index];
        }
    }
    return override ? override : global;
}

const char *ra_document_template_named(const struct ra_document *document, const char *node,
                                       const char *label) {
    return node && label ? select_named(document, TEMPLATE_GLOBAL, TEMPLATE_NODE, node, label)
                         : NULL;
}

const char *ra_document_macro_named(const struct ra_document *document, const char *node,
                                    const char *label) {
    return node && label ? select_named(document, MACRO_GLOBAL, MACRO_NODE, node, label) : NULL;
}

const char *ra_document_event(const struct ra_document *document, size_t index, const char **node) {
    if (node) {
        *node = NULL;
    }
    for (size_t current = 0; current < document->section_count; ++current) {
        struct scope scope = parse(document->sections[current]);
        if (scope.kind != EVENT_NODE || duplicate(document, current)) {
            continue;
        }
        if (!index) {
            if (node) {
                *node = scope_node(document, scope);
            }
            return document->sections[current];
        }
        --index;
    }
    return NULL;
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

/** @brief Validate every resolved template and macro definition before worker creation.
 * @param document Fully parsed, option-valid document.
 * @param section Receives the offending section on failure.
 * @param key Receives a direct offending option when available.
 * @return Null on success or a stable named-definition diagnostic.
 */
static const char *validate_named_definitions(const struct ra_document *document,
                                              const char **section, const char **key) {
    for (size_t index = 0; index < document->section_count; ++index) {
        struct scope scope = parse(document->sections[index]);
        if (scope.kind != TEMPLATE_GLOBAL && scope.kind != TEMPLATE_NODE &&
            scope.kind != MACRO_GLOBAL && scope.kind != MACRO_NODE) {
            continue;
        }
        const char *node = scope.kind == TEMPLATE_NODE || scope.kind == MACRO_NODE
                               ? scope_node(document, scope)
                               : NULL;
        const char *error;
        if (scope.kind == TEMPLATE_GLOBAL || scope.kind == TEMPLATE_NODE) {
            struct ra_template_settings settings;
            error = ra_template_settings_resolve(document->entries, document->count, node,
                                                 document->sections[index], &settings);
            if (!error && !ra_message_template_validate(settings.text)) {
                *section = document->sections[index];
                *key = "text";
                return "invalid message template";
            }
            if (!error && !ra_message_template_validate_output(settings.text)) {
                *section = document->sections[index];
                *key = "text";
                return "scheduled message exceeds maximum output";
            }
        } else {
            struct ra_macro_settings settings;
            error = ra_macro_settings_resolve(document->entries, document->count, node,
                                              document->sections[index], &settings);
        }
        if (error) {
            *section = document->sections[index];
            *key = NULL;
            return error;
        }
    }
    return NULL;
}

/** @brief Validate all event references after named global/node inheritance resolves.
 * @param document Fully parsed, option-valid document.
 * @param section Receives the offending section on failure.
 * @param key Receives the invalid event option when available.
 * @return Null on success or a stable event-reference diagnostic.
 */
static const char *validate_events(const struct ra_document *document, const char **section,
                                   const char **key) {
    for (size_t index = 0;; ++index) {
        const char *node;
        const char *event_section = ra_document_event(document, index, &node);
        if (!event_section) {
            return NULL;
        }
        struct ra_event_settings event;
        const char *error =
            ra_event_settings_resolve(document->entries, document->count, event_section, &event);
        if (error) {
            *section = event_section;
            *key = NULL;
            return error;
        }
        if (*event.message && !ra_message_template_validate(event.message)) {
            *section = event_section;
            *key = "message";
            return "invalid message template";
        }
        if (*event.message && !ra_message_template_validate_output(event.message)) {
            *section = event_section;
            *key = "message";
            return "scheduled message exceeds maximum output";
        }
        if (*event.template_name &&
            !ra_document_template_named(document, node, event.template_name)) {
            *section = event_section;
            *key = "template";
            return "event references an unknown template";
        }
        if (*event.macro_name && !ra_document_macro_named(document, node, event.macro_name)) {
            *section = event_section;
            *key = "macro";
            return "event references an unknown macro";
        }
    }
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
        if (scope.node_length >= RA_NODE_NAME_MAX) {
            return "node name exceeds transport limit";
        }
        if ((scope.kind == TEMPLATE_GLOBAL || scope.kind == TEMPLATE_NODE ||
             scope.kind == MACRO_GLOBAL || scope.kind == MACRO_NODE || scope.kind == EVENT_NODE) &&
            duplicate(document, i)) {
            return "duplicate named definition";
        }
        if (scope.kind == ID_NODE || scope.kind == ID_SET || scope.kind == ANNOUNCEMENT_NODE ||
            scope.kind == ANNOUNCEMENT_SET || scope.kind == COURTESY_NODE ||
            scope.kind == COURTESY_SET || scope.kind == MORSE_NODE || scope.kind == SPEECH_NODE ||
            scope.kind == TIME_NODE || scope.kind == TEMPLATE_NODE || scope.kind == MACRO_NODE ||
            scope.kind == EVENT_NODE) {
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
        } else if (kind == TEMPLATE_GLOBAL || kind == TEMPLATE_NODE) {
            settings_kind = RA_SETTINGS_TEMPLATE;
        } else if (kind == MACRO_GLOBAL || kind == MACRO_NODE) {
            settings_kind = RA_SETTINGS_MACRO;
        } else if (kind == EVENT_NODE) {
            settings_kind = RA_SETTINGS_EVENT;
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
    const char *named_error = validate_named_definitions(document, section, key);
    if (named_error) {
        return named_error;
    }
    const char *event_error = validate_events(document, section, key);
    if (event_error) {
        return event_error;
    }
    *section = NULL;
    return NULL;
}
