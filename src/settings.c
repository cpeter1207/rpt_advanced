/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Table-driven settings resolution outside the real-time audio path.
 */
#include "settings.h"
#include "link_access.h"
#include <limits.h>
#include <string.h>

/** @brief Storage types used by the setting descriptors. */
enum field_type { FIELD_STRING, FIELD_NODE_LIST, FIELD_PREFIX, FIELD_BOOLEAN, FIELD_NUMBER };

/** @brief One schema entry mapping a public name to a typed settings member. */
struct field {
    const char *name;     /**< Configuration option. */
    enum field_type type; /**< Destination representation. */
    size_t offset;        /**< Offset within its settings structure. */
    uint64_t minimum;     /**< Inclusive numeric minimum. */
    uint64_t maximum;     /**< Inclusive numeric maximum. */
};

/** @brief Node schema; zero rate requests automatic selection. */
static const struct field node_fields[] = {
    {"node_enabled", FIELD_BOOLEAN, offsetof(struct ra_node_settings, enabled), 0, 0},
    {"full_duplex", FIELD_BOOLEAN, offsetof(struct ra_node_settings, full_duplex), 0, 0},
    {"transmit_hang_ms", FIELD_NUMBER, offsetof(struct ra_node_settings, hang_ms), 0, UINT64_MAX},
    {"sample_rate_hz", FIELD_NUMBER, offsetof(struct ra_node_settings, sample_rate), 0, UINT_MAX},
    {"radio_channel", FIELD_STRING, offsetof(struct ra_node_settings, channel), 0, 0},
    {"codec", FIELD_STRING, offsetof(struct ra_node_settings, codec), 0, 0},
    {"link_allow_nodes", FIELD_NODE_LIST, offsetof(struct ra_node_settings, link_allow_nodes), 0,
     0},
    {"link_deny_nodes", FIELD_NODE_LIST, offsetof(struct ra_node_settings, link_deny_nodes), 0, 0},
    {"link_directory_file", FIELD_STRING, offsetof(struct ra_node_settings, link_directory_file), 0,
     0},
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

/** @brief Identifier schema; media availability is evaluated when preparing playback. */
static const struct field identifier_fields[] = {
    {"interval_ms", FIELD_NUMBER, offsetof(struct ra_identifier_settings, interval_ms), 1,
     UINT64_MAX},
    {"priority", FIELD_NUMBER, offsetof(struct ra_identifier_settings, priority), 0, INT_MAX},
    {"first_key_only", FIELD_BOOLEAN, offsetof(struct ra_identifier_settings, first_key_only), 0,
     0},
    {"regardless_of_activity", FIELD_BOOLEAN,
     offsetof(struct ra_identifier_settings, regardless_of_activity), 0, 0},
    {"sound_file", FIELD_STRING, offsetof(struct ra_identifier_settings, file), 0, 0},
    {"speech_text", FIELD_STRING, offsetof(struct ra_identifier_settings, speech_text), 0, 0},
    {"speech_model", FIELD_STRING, offsetof(struct ra_identifier_settings, speech_model), 0, 0},
    {"speech_speed_percent", FIELD_NUMBER,
     offsetof(struct ra_identifier_settings, speech_speed_percent), 1, 1000},
    {"morse_text", FIELD_STRING, offsetof(struct ra_identifier_settings, morse_text), 0, 0},
    {"morse_speed_wpm", FIELD_NUMBER, offsetof(struct ra_identifier_settings, morse_speed_wpm), 1,
     100},
    {"morse_frequency_hz", FIELD_NUMBER,
     offsetof(struct ra_identifier_settings, morse_frequency_hz), 1, UINT_MAX},
};

/** @brief Assign a validated value at its schema-declared, naturally aligned member offset.
 * @param field Schema descriptor.
 * @param text Borrowed configuration value.
 * @param output Structure to update.
 * @return True on success, false for an invalid typed value.
 */
static bool assign(const struct field *field, const char *text, void *output) {
    unsigned char *destination = (unsigned char *)output + field->offset;
    if (field->type <= FIELD_PREFIX) {
        if (field->type == FIELD_NODE_LIST && !ra_link_access_list_valid(text)) {
            return false;
        }
        struct ra_link_command_mapping mapping = {text, RA_LINK_DISCONNECT};
        if (field->type == FIELD_PREFIX && ra_link_commands_validate(&mapping, 1)) {
            return false;
        }
        *(const char **)destination = text;
    } else if (field->type == FIELD_BOOLEAN) {
        bool value;
        if (!ra_config_boolean(text, &value)) {
            return false;
        }
        *(bool *)destination = value;
    } else {
        uint64_t value;
        if (!ra_config_unsigned(text, field->minimum, field->maximum, &value)) {
            return false;
        }
        *(uint64_t *)destination = value;
    }
    return true;
}

const char *ra_settings_validate(bool identifier, const char *key, const char *value) {
    const struct field *fields = identifier ? identifier_fields : node_fields;
    size_t count = identifier ? sizeof(identifier_fields) / sizeof(identifier_fields[0])
                              : sizeof(node_fields) / sizeof(node_fields[0]);
    struct ra_node_settings node;
    struct ra_identifier_settings id;
    void *destination = identifier ? (void *)&id : (void *)&node;
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

const char *ra_node_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                     const char *node, struct ra_node_settings *result) {
    struct ra_node_settings temporary = {.enabled = true,
                                         .full_duplex = true,
                                         .channel = node,
                                         .codec = "",
                                         .link_allow_nodes = "",
                                         .link_deny_nodes = "",
                                         .link_directory_file = ""};
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
    struct ra_identifier_settings temporary = {
        600000, 0, false, false, "", "", "en_US-lessac-medium.onnx", 100, "", 20, 800};
    const char *scopes[] = {"identifier", node, set};
    const char *error =
        resolve(identifier_fields, sizeof(identifier_fields) / sizeof(identifier_fields[0]),
                entries, count, scopes, &temporary);
    if (!error) {
        *result = temporary;
    }
    return error;
}
