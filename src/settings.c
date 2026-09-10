/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Table-driven settings resolution outside the real-time audio path.
 */
#include "settings.h"
#include "link_access.h"
#include <limits.h>
#include <string.h>

/** @brief Storage types used by the setting descriptors. */
enum field_type {
    FIELD_STRING,
    FIELD_NODE_LIST,
    FIELD_PREFIX,
    FIELD_LOOKUP_METHOD,
    FIELD_BOOLEAN,
    FIELD_NUMBER,
    FIELD_SIGNED,
    FIELD_TIME_FORMAT,
    FIELD_NODE_ID,
    FIELD_COURTESY_INPUT
};

/** @brief One schema entry mapping a public name to a typed settings member. */
struct field {
    const char name[64];  /**< Configuration option. */
    enum field_type type; /**< Destination representation. */
    size_t offset;        /**< Offset within its settings structure. */
    uint64_t minimum;     /**< Inclusive unsigned minimum or signed magnitude. */
    uint64_t maximum;     /**< Inclusive numeric maximum. */
};

/** @brief Node schema; zero rate requests automatic selection. */
static const struct field node_fields[] = {
    {"node_enabled", FIELD_BOOLEAN, offsetof(struct ra_node_settings, enabled), 0, 0},
    {"full_duplex", FIELD_BOOLEAN, offsetof(struct ra_node_settings, full_duplex), 0, 0},
    {"dtmf_muting", FIELD_BOOLEAN, offsetof(struct ra_node_settings, dtmf_muting), 0, 0},
    {"transmit_hang_ms", FIELD_NUMBER, offsetof(struct ra_node_settings, hang_ms), 0, UINT64_MAX},
    {"telemetry_duck_db", FIELD_SIGNED, offsetof(struct ra_node_settings, telemetry_duck_db), 60,
     0},
    {"courtesy_delay_ms", FIELD_NUMBER, offsetof(struct ra_node_settings, courtesy_delay_ms), 0,
     UINT64_MAX},
    {"sample_rate_hz", FIELD_NUMBER, offsetof(struct ra_node_settings, sample_rate), 0, UINT_MAX},
    {"radio_channel", FIELD_STRING, offsetof(struct ra_node_settings, channel), 0, 0},
    {"codec", FIELD_STRING, offsetof(struct ra_node_settings, codec), 0, 0},
    {"link_allow_nodes", FIELD_NODE_LIST, offsetof(struct ra_node_settings, link_allow_nodes), 0,
     0},
    {"link_deny_nodes", FIELD_NODE_LIST, offsetof(struct ra_node_settings, link_deny_nodes), 0, 0},
    {"link_static_directory_file", FIELD_STRING,
     offsetof(struct ra_node_settings, link_static_directory_file), 0, 0},
    {"link_directory_file", FIELD_STRING, offsetof(struct ra_node_settings, link_directory_file), 0,
     0},
    {"link_lookup_method", FIELD_LOOKUP_METHOD,
     offsetof(struct ra_node_settings, link_lookup_method), 0, 0},
    {"link_command_disconnect", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_DISCONNECT].digits), 0, 0},
    {"link_command_monitor", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_MONITOR].digits), 0, 0},
    {"link_command_transceive", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_TRANSCEIVE].digits), 0, 0},
    {"link_command_remote", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_COMMAND].digits), 0, 0},
    {"link_command_status", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_STATUS].digits), 0, 0},
    {"link_command_disconnect_all", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_DISCONNECT_ALL].digits), 0, 0},
    {"link_command_last_keyed", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_LAST_KEYED].digits), 0, 0},
    {"link_command_local_monitor", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_LOCAL_MONITOR].digits), 0, 0},
    {"link_command_disconnect_permanent", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_DISCONNECT_PERMANENT].digits), 0, 0},
    {"link_command_permanent_monitor", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_PERMANENT_MONITOR].digits), 0, 0},
    {"link_command_permanent_transceive", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_PERMANENT_TRANSCEIVE].digits), 0, 0},
    {"link_command_full_status", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_FULL_STATUS].digits), 0, 0},
    {"link_command_reconnect_all", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_RECONNECT_ALL].digits), 0, 0},
    {"link_command_permanent_local_monitor", FIELD_PREFIX,
     offsetof(struct ra_node_settings, link_commands[RA_LINK_PERMANENT_LOCAL_MONITOR].digits), 0,
     0},
};

/** @brief Parse one documented post-static directory-source selection.
 * @param text Borrowed configuration value.
 * @param method Receives the corresponding enum value.
 * @return True for `dns`, `file`, or `both`.
 */
static bool lookup_method(const char *text, enum ra_link_lookup_method *method) {
    if (!strcmp(text, "both")) {
        *method = RA_LINK_LOOKUP_BOTH;
    } else if (!strcmp(text, "dns")) {
        *method = RA_LINK_LOOKUP_DNS;
    } else if (!strcmp(text, "file")) {
        *method = RA_LINK_LOOKUP_FILE;
    } else {
        return false;
    }
    return true;
}

/** @brief Parse one mandatory named-courtesy input assignment.
 * @param text Trimmed configuration value.
 * @param input Receives the selected input only on success.
 * @return True for `receiver` or `link`.
 */
static bool courtesy_input(const char *text, enum ra_courtesy_input *input) {
    if (!strcmp(text, "receiver")) {
        *input = RA_COURTESY_INPUT_RECEIVER;
    } else if (!strcmp(text, "link")) {
        *input = RA_COURTESY_INPUT_LINK;
    } else {
        return false;
    }
    return true;
}

/** @brief Validate an optional exact decimal remote-node identity.
 * @param text Trimmed configuration value.
 * @return True for empty text or one or more decimal digits.
 */
static bool remote_node_valid(const char *text) {
    return !*text || strspn(text, "0123456789") == strlen(text);
}

/** @brief Identifier schema; media availability is evaluated when preparing playback. */
static const struct field identifier_fields[] = {
    {"interval_ms", FIELD_NUMBER, offsetof(struct ra_identifier_settings, interval_ms), 1,
     UINT64_MAX},
    {"priority", FIELD_NUMBER, offsetof(struct ra_identifier_settings, priority), 0, INT_MAX},
    {"first_key_only", FIELD_BOOLEAN, offsetof(struct ra_identifier_settings, first_key_only), 0,
     0},
    {"regardless_of_activity", FIELD_BOOLEAN,
     offsetof(struct ra_identifier_settings, regardless_of_activity), 0, 0},
    {"polite", FIELD_BOOLEAN, offsetof(struct ra_identifier_settings, polite), 0, 0},
    {"polite_maximum_wait_ms", FIELD_NUMBER,
     offsetof(struct ra_identifier_settings, polite_maximum_wait_ms), 1, UINT64_MAX},
    {"sound_file", FIELD_STRING, offsetof(struct ra_identifier_settings, file), 0, 0},
    {"speech_text", FIELD_STRING, offsetof(struct ra_identifier_settings, speech_text), 0, 0},
    {"speech_model", FIELD_STRING, offsetof(struct ra_identifier_settings, speech_model), 0, 0},
    {"speech_speed_percent", FIELD_NUMBER,
     offsetof(struct ra_identifier_settings, speech_speed_percent), 1, 1000},
    {"speech_level_db", FIELD_SIGNED, offsetof(struct ra_identifier_settings, speech_level_db), 60,
     0},
    {"morse_text", FIELD_STRING, offsetof(struct ra_identifier_settings, morse_text), 0, 0},
    {"morse_speed_wpm", FIELD_NUMBER, offsetof(struct ra_identifier_settings, morse_speed_wpm), 1,
     100},
    {"morse_frequency_hz", FIELD_NUMBER,
     offsetof(struct ra_identifier_settings, morse_frequency_hz), 1, UINT_MAX},
    {"morse_level_db", FIELD_SIGNED, offsetof(struct ra_identifier_settings, morse_level_db), 60,
     0},
};

/** @brief Announcement schema; media fields match identifier fallback preparation exactly. */
static const struct field announcement_fields[] = {
    {"interval_ms", FIELD_NUMBER, offsetof(struct ra_announcement_settings, interval_ms), 0,
     UINT64_MAX},
    {"sound_file", FIELD_STRING,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, file),
     0, 0},
    {"speech_text", FIELD_STRING,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, speech_text),
     0, 0},
    {"morse_text", FIELD_STRING,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, morse_text),
     0, 0},
    {"speech_model", FIELD_STRING,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, speech_model),
     0, 0},
    {"speech_speed_percent", FIELD_NUMBER,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, speech_speed_percent),
     1, 1000},
    {"speech_level_db", FIELD_SIGNED,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, speech_level_db),
     60, 0},
    {"morse_speed_wpm", FIELD_NUMBER,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, morse_speed_wpm),
     1, 100},
    {"morse_frequency_hz", FIELD_NUMBER,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, morse_frequency_hz),
     1, UINT_MAX},
    {"morse_level_db", FIELD_SIGNED,
     offsetof(struct ra_announcement_settings, media) +
         offsetof(struct ra_identifier_settings, morse_level_db),
     60, 0},
};

/** @brief Courtesy defaults share media selection but use one uniform output level. */
static const struct field courtesy_media_fields[] = {
    {"sound_file", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) + offsetof(struct ra_identifier_settings, file),
     0, 0},
    {"speech_text", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, speech_text),
     0, 0},
    {"morse_text", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, morse_text),
     0, 0},
    {"speech_model", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, speech_model),
     0, 0},
    {"speech_speed_percent", FIELD_NUMBER,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, speech_speed_percent),
     1, 1000},
    {"morse_speed_wpm", FIELD_NUMBER,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, morse_speed_wpm),
     1, 100},
    {"morse_frequency_hz", FIELD_NUMBER,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, morse_frequency_hz),
     1, UINT_MAX},
    {"tone_sequence", FIELD_STRING, offsetof(struct ra_courtesy_settings, tone_sequence), 0, 0},
    {"level_db", FIELD_SIGNED, offsetof(struct ra_courtesy_settings, level_db), 60, 0},
};

/** @brief A named courtesy tone adds one input assignment and optional permanent peer. */
static const struct field courtesy_set_fields[] = {
    {"sound_file", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) + offsetof(struct ra_identifier_settings, file),
     0, 0},
    {"speech_text", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, speech_text),
     0, 0},
    {"morse_text", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, morse_text),
     0, 0},
    {"speech_model", FIELD_STRING,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, speech_model),
     0, 0},
    {"speech_speed_percent", FIELD_NUMBER,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, speech_speed_percent),
     1, 1000},
    {"morse_speed_wpm", FIELD_NUMBER,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, morse_speed_wpm),
     1, 100},
    {"morse_frequency_hz", FIELD_NUMBER,
     offsetof(struct ra_courtesy_settings, media) +
         offsetof(struct ra_identifier_settings, morse_frequency_hz),
     1, UINT_MAX},
    {"tone_sequence", FIELD_STRING, offsetof(struct ra_courtesy_settings, tone_sequence), 0, 0},
    {"level_db", FIELD_SIGNED, offsetof(struct ra_courtesy_settings, level_db), 60, 0},
    {"input", FIELD_COURTESY_INPUT, offsetof(struct ra_courtesy_settings, input), 0, 0},
    {"remote_node", FIELD_NODE_ID, offsetof(struct ra_courtesy_settings, remote_node), 0, 0},
};

/** @brief Per-node offline speech defaults. */
static const struct field speech_fields[] = {
    {"voice", FIELD_STRING, offsetof(struct ra_identifier_settings, speech_model), 0, 0},
    {"speed_percent", FIELD_NUMBER, offsetof(struct ra_identifier_settings, speech_speed_percent),
     1, 1000},
    {"level_db", FIELD_SIGNED, offsetof(struct ra_identifier_settings, speech_level_db), 60, 0},
};

/** @brief Per-node Morse defaults. */
static const struct field morse_fields[] = {
    {"frequency_hz", FIELD_NUMBER, offsetof(struct ra_identifier_settings, morse_frequency_hz), 1,
     UINT_MAX},
    {"speed_wpm", FIELD_NUMBER, offsetof(struct ra_identifier_settings, morse_speed_wpm), 1, 100},
    {"level_db", FIELD_SIGNED, offsetof(struct ra_identifier_settings, morse_level_db), 60, 0},
};

/** @brief Per-node clock-announcement settings. */
static const struct field time_fields[] = {
    {"format", FIELD_TIME_FORMAT, offsetof(struct ra_time_settings, format), 0, 0},
};

/** @brief Return the common file, speech, and Morse defaults used by scheduled media.
 * @return Fully initialized default identifier-media settings.
 */
static struct ra_identifier_settings identifier_defaults(void) {
    return (struct ra_identifier_settings){.interval_ms = 600000,
                                           .priority = 0,
                                           .first_key_only = false,
                                           .regardless_of_activity = false,
                                           .polite = false,
                                           .polite_maximum_wait_ms = 60000,
                                           .file = "",
                                           .speech_text = "",
                                           .speech_model = "en_US-lessac-medium.onnx",
                                           .speech_speed_percent = 100,
                                           .speech_level_db = 0,
                                           .morse_text = "",
                                           .morse_speed_wpm = 20,
                                           .morse_frequency_hz = 800,
                                           .morse_level_db = -6};
}

/** @brief Assign a validated value at its schema-declared, naturally aligned member offset.
 * @param field Schema descriptor.
 * @param text Borrowed configuration value.
 * @param output Structure to update.
 * @return True on success, false for an invalid typed value.
 */
static bool assign(const struct field *field, const char *text, void *output) {
    unsigned char *destination = (unsigned char *)output + field->offset;
    if (field->type == FIELD_STRING || field->type == FIELD_NODE_LIST ||
        field->type == FIELD_PREFIX || field->type == FIELD_NODE_ID) {
        if (field->type == FIELD_NODE_LIST && !ra_link_access_list_valid(text)) {
            return false;
        }
        if (field->type == FIELD_NODE_ID && !remote_node_valid(text)) {
            return false;
        }
        struct ra_link_command_mapping mapping = {text, RA_LINK_DISCONNECT};
        if (field->type == FIELD_PREFIX && ra_link_commands_validate(&mapping, 1)) {
            return false;
        }
        *(const char **)destination = text;
    } else if (field->type == FIELD_LOOKUP_METHOD) {
        enum ra_link_lookup_method method;
        if (!lookup_method(text, &method)) {
            return false;
        }
        *(enum ra_link_lookup_method *)destination = method;
    } else if (field->type == FIELD_BOOLEAN) {
        bool value;
        if (!ra_config_boolean(text, &value)) {
            return false;
        }
        *(bool *)destination = value;
    } else if (field->type == FIELD_COURTESY_INPUT) {
        return courtesy_input(text, (enum ra_courtesy_input *)destination);
    } else if (field->type == FIELD_NUMBER || field->type == FIELD_TIME_FORMAT) {
        uint64_t value;
        if (!ra_config_unsigned(text, field->type == FIELD_TIME_FORMAT ? 12 : field->minimum,
                                field->type == FIELD_TIME_FORMAT ? 24 : field->maximum, &value) ||
            (field->type == FIELD_TIME_FORMAT && value != 12 && value != 24)) {
            return false;
        }
        *(uint64_t *)destination = value;
    } else {
        int64_t value;
        if (!ra_config_signed(text, -(int64_t)field->minimum, (int64_t)field->maximum, &value)) {
            return false;
        }
        *(int64_t *)destination = value;
    }
    return true;
}

const char *ra_settings_validate(bool identifier, const char *key, const char *value) {
    return ra_settings_validate_kind(identifier ? RA_SETTINGS_IDENTIFIER : RA_SETTINGS_NODE, key,
                                     value);
}

/** @brief Validate an option using its documented section schema.
 * @param kind Section category selecting the valid options.
 * @param key Option name.
 * @param value Trimmed option value.
 * @return Null when valid, otherwise a stable diagnostic.
 */
const char *ra_settings_validate_kind(enum ra_settings_kind kind, const char *key,
                                      const char *value) {
    const struct field *fields = node_fields;
    size_t count = sizeof(node_fields) / sizeof(node_fields[0]);
    if (kind == RA_SETTINGS_IDENTIFIER) {
        fields = identifier_fields;
        count = sizeof(identifier_fields) / sizeof(identifier_fields[0]);
    } else if (kind == RA_SETTINGS_ANNOUNCEMENT) {
        fields = announcement_fields;
        count = sizeof(announcement_fields) / sizeof(announcement_fields[0]);
    } else if (kind == RA_SETTINGS_MORSE) {
        fields = morse_fields;
        count = sizeof(morse_fields) / sizeof(morse_fields[0]);
    } else if (kind == RA_SETTINGS_SPEECH) {
        fields = speech_fields;
        count = sizeof(speech_fields) / sizeof(speech_fields[0]);
    } else if (kind == RA_SETTINGS_TIME) {
        fields = time_fields;
        count = sizeof(time_fields) / sizeof(time_fields[0]);
    }
    struct ra_node_settings node;
    struct ra_identifier_settings id;
    struct ra_announcement_settings announcement;
    struct ra_courtesy_settings courtesy;
    struct ra_time_settings time;
    if (kind == RA_SETTINGS_COURTESY) {
        fields = courtesy_media_fields;
        count = sizeof(courtesy_media_fields) / sizeof(courtesy_media_fields[0]);
    } else if (kind == RA_SETTINGS_COURTESY_SET) {
        fields = courtesy_set_fields;
        count = sizeof(courtesy_set_fields) / sizeof(courtesy_set_fields[0]);
    }
    void *destination = kind == RA_SETTINGS_NODE           ? (void *)&node
                        : kind == RA_SETTINGS_TIME         ? (void *)&time
                        : kind == RA_SETTINGS_ANNOUNCEMENT ? (void *)&announcement
                        : kind == RA_SETTINGS_COURTESY || kind == RA_SETTINGS_COURTESY_SET
                            ? (void *)&courtesy
                            : (void *)&id;
    for (size_t i = 0; i < count; ++i) {
        if (!strcmp(key, fields[i].name)) {
            return assign(&fields[i], value, destination) ? NULL : "invalid option value";
        }
    }
    return "unknown option";
}

/** @brief Apply inherited options to a temporary settings object.
 * @param fields Schema descriptors.
 * @param fields_count Descriptor count.
 * @param entries Parsed configuration.
 * @param count Entry count.
 * @param scopes Shared, node, and set scopes.
 * @param output Temporary typed settings.
 * @return Invalid option name or null.
 */
static const char *resolve(const struct field *fields, size_t fields_count,
                           const struct ra_config_entry *entries, size_t count,
                           const char *const scopes[3], void *output) {
    for (size_t i = 0; i < fields_count; ++i) {
        const char *text =
            ra_config_lookup(entries, count, fields[i].name, scopes[0], scopes[1], scopes[2]);
        if (text && !assign(&fields[i], text, output)) {
            return fields[i].name;
        }
    }
    return NULL;
}

/** @brief Apply flat defaults followed by an exact node-qualified override.
 * @param fields Schema descriptors.
 * @param fields_count Descriptor count.
 * @param entries Parsed configuration.
 * @param count Entry count.
 * @param prefix Flat section name.
 * @param node Optional node name.
 * @param output Temporary typed settings.
 * @return Invalid option name or null. Node values take precedence regardless
 *         of their position relative to flat values in the configuration.
 */
static const char *resolve_prefixed(const struct field *fields, size_t fields_count,
                                    const struct ra_config_entry *entries, size_t count,
                                    const char *prefix, const char *node, void *output) {
    size_t prefix_length = strlen(prefix);
    size_t node_length = node ? strlen(node) : 0;
    size_t scopes = node ? 2U : 1U;
    for (size_t scope = 0; scope < scopes; ++scope) {
        for (size_t i = 0; i < fields_count; ++i) {
            const char *text = NULL;
            for (size_t entry = 0; entry < count; ++entry) {
                const char *section = entries[entry].section;
                bool matches = scope == 0
                                   ? !strcmp(section, prefix)
                                   : !strncmp(section, prefix, prefix_length) &&
                                         section[prefix_length] == ' ' &&
                                         !strncmp(section + prefix_length + 1, node, node_length) &&
                                         !section[prefix_length + node_length + 1];
                if (matches && !strcmp(entries[entry].key, fields[i].name)) {
                    text = entries[entry].value;
                }
            }
            if (text && !assign(&fields[i], text, output)) {
                return fields[i].name;
            }
        }
    }
    return NULL;
}

const char *ra_node_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                     const char *node, struct ra_node_settings *result) {
    struct ra_node_settings temporary = {.enabled = true,
                                         .full_duplex = true,
                                         .dtmf_muting = true,
                                         .telemetry_duck_db = -20,
                                         .courtesy_delay_ms = 250,
                                         .channel = node,
                                         .codec = "",
                                         .link_allow_nodes = "",
                                         .link_deny_nodes = "",
                                         .link_static_directory_file = "",
                                         .link_directory_file = "",
                                         .link_lookup_method = RA_LINK_LOOKUP_BOTH};
    ra_link_commands_default(temporary.link_commands);
    const char *scopes[] = {"general", node, NULL};
    const char *error = resolve(node_fields, sizeof(node_fields) / sizeof(node_fields[0]), entries,
                                count, scopes, &temporary);
    if (!error) {
        error = ra_link_commands_validate(temporary.link_commands, RA_LINK_ACTION_COUNT);
    }
    if (!error) {
        *result = temporary;
    }
    return error;
}

const char *ra_identifier_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                           const char *node, const char *set,
                                           struct ra_identifier_settings *result) {
    struct ra_identifier_settings temporary = identifier_defaults();
    const char *error = resolve_prefixed(identifier_fields,
                                         sizeof(identifier_fields) / sizeof(identifier_fields[0]),
                                         entries, count, "identifier", node, &temporary);
    if (!error) {
        error = resolve_prefixed(speech_fields, sizeof(speech_fields) / sizeof(speech_fields[0]),
                                 entries, count, "speech", node, &temporary);
    }
    if (!error) {
        error = resolve_prefixed(morse_fields, sizeof(morse_fields) / sizeof(morse_fields[0]),
                                 entries, count, "morse", node, &temporary);
    }
    if (!error && set) {
        const char *set_scopes[] = {NULL, NULL, set};
        error = resolve(identifier_fields, sizeof(identifier_fields) / sizeof(identifier_fields[0]),
                        entries, count, set_scopes, &temporary);
    }
    if (!error) {
        *result = temporary;
    }
    return error;
}

const char *ra_announcement_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                             const char *node, const char *set,
                                             struct ra_announcement_settings *result) {
    struct ra_announcement_settings temporary = {.interval_ms = 0};
    temporary.media = identifier_defaults();
    const char *error = resolve_prefixed(
        announcement_fields, sizeof(announcement_fields) / sizeof(announcement_fields[0]), entries,
        count, "announcement", node, &temporary);
    if (!error) {
        error = resolve_prefixed(speech_fields, sizeof(speech_fields) / sizeof(speech_fields[0]),
                                 entries, count, "speech", node, &temporary.media);
    }
    if (!error) {
        error = resolve_prefixed(morse_fields, sizeof(morse_fields) / sizeof(morse_fields[0]),
                                 entries, count, "morse", node, &temporary.media);
    }
    if (!error && set) {
        const char *set_scopes[] = {NULL, NULL, set};
        error = resolve(announcement_fields,
                        sizeof(announcement_fields) / sizeof(announcement_fields[0]), entries,
                        count, set_scopes, &temporary);
    }
    if (!error) {
        *result = temporary;
    }
    return error;
}

const char *ra_courtesy_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                         const char *node, const char *set,
                                         struct ra_courtesy_settings *result) {
    if (!set) {
        return "courtesy set is required";
    }
    struct ra_courtesy_settings temporary = {
        .media = identifier_defaults(), .tone_sequence = "", .remote_node = "", .level_db = -20};
    const char *error = resolve_prefixed(
        courtesy_media_fields, sizeof(courtesy_media_fields) / sizeof(courtesy_media_fields[0]),
        entries, count, "courtesy", node, &temporary);
    if (!error) {
        error = resolve_prefixed(speech_fields, sizeof(speech_fields) / sizeof(speech_fields[0]),
                                 entries, count, "speech", node, &temporary.media);
    }
    if (!error) {
        error = resolve_prefixed(morse_fields, sizeof(morse_fields) / sizeof(morse_fields[0]),
                                 entries, count, "morse", node, &temporary.media);
    }
    if (!error) {
        const char *set_scopes[] = {NULL, NULL, set};
        error = resolve(courtesy_set_fields,
                        sizeof(courtesy_set_fields) / sizeof(courtesy_set_fields[0]), entries,
                        count, set_scopes, &temporary);
    }
    if (error) {
        return error;
    }
    if (temporary.input == RA_COURTESY_INPUT_NONE) {
        return "courtesy input is required";
    }
    if (*temporary.remote_node && temporary.input != RA_COURTESY_INPUT_LINK) {
        return "courtesy remote node requires link input";
    }
    temporary.media.speech_level_db = 0;
    temporary.media.morse_level_db = temporary.level_db;
    *result = temporary;
    return NULL;
}

const char *ra_time_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                     const char *node, struct ra_time_settings *result) {
    struct ra_time_settings temporary = {.format = 12};
    const char *error = resolve_prefixed(time_fields, sizeof(time_fields) / sizeof(time_fields[0]),
                                         entries, count, "time", node, &temporary);
    if (!error) {
        *result = temporary;
    }
    return error;
}
