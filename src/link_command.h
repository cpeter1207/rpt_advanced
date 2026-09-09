/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Parse linking-only DTMF commands without performing network operations.
 */
#ifndef RPT_ADVANCED_LINK_COMMAND_H
#define RPT_ADVANCED_LINK_COMMAND_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/** @brief Supported linking actions, independent of their configurable DTMF prefixes. */
enum ra_link_action {
    RA_LINK_DISCONNECT,              /**< Disconnect a nonpermanent link. */
    RA_LINK_MONITOR,                 /**< Receive remote audio without sending audio. */
    RA_LINK_TRANSCEIVE,              /**< Exchange audio with a remote node. */
    RA_LINK_COMMAND,                 /**< Forward subsequent commands to the selected node. */
    RA_LINK_STATUS,                  /**< Report this node's connections. */
    RA_LINK_DISCONNECT_ALL,          /**< Disconnect links while retaining reconnect information. */
    RA_LINK_LAST_KEYED,              /**< Report the last transmitting node. */
    RA_LINK_LOCAL_MONITOR,           /**< Receive remote audio locally without forwarding it. */
    RA_LINK_DISCONNECT_PERMANENT,    /**< Remove a permanent connection. */
    RA_LINK_PERMANENT_MONITOR,       /**< Maintain a monitor connection across failures. */
    RA_LINK_PERMANENT_TRANSCEIVE,    /**< Maintain a transceive connection across failures. */
    RA_LINK_FULL_STATUS,             /**< Report the connected network's topology. */
    RA_LINK_RECONNECT_ALL,           /**< Restore links saved by disconnect-all. */
    RA_LINK_PERMANENT_LOCAL_MONITOR, /**< Maintain a local-monitor connection. */
    RA_LINK_DISCONNECT_NONPERMANENT_ALL, /**< Disconnect every active nonpermanent link. */
    RA_LINK_TIME,                        /**< Announce this node's local clock. */
    RA_LINK_ACTION_COUNT                 /**< Number of configurable linking actions. */
};

/** @brief One configured command prefix, excluding its initiating asterisk. */
struct ra_link_command_mapping {
    const char *digits;         /**< Nonempty DTMF prefix; empty disables this mapping. */
    enum ra_link_action action; /**< Linking operation selected by this prefix. */
};

/** @brief Result for a complete command; node is a borrowed substring. */
struct ra_link_command {
    enum ra_link_action action; /**< Selected operation. */
    const char *node;           /**< Decimal destination, or empty for node-free actions. */
};

/** @brief Bounded command collection; zero initialization starts idle. */
struct ra_link_collector {
    char digits[128]; /**< Prefix and destination, without star/hash delimiters. */
    size_t length;    /**< Stored digit count. */
    uint64_t last_ms; /**< Last digit timestamp. */
    bool active;      /**< A leading star has started collection. */
};

/** @brief Build standard linking-only mappings in action order.
 * @param mappings Array of RA_LINK_ACTION_COUNT entries.
 */
void ra_link_commands_default(struct ra_link_command_mapping *mappings);

/** @brief Collect a complete command without dialing or consulting the network.
 * @param collector Local command state.
 * @param mappings Validated mappings.
 * @param count Number of mappings.
 * @param digit DTMF character; zero checks the three-second interdigit timeout.
 * @param now_ms Monotonic event time.
 * @param completed Receives completed digits; must have room for 128 bytes.
 * @return True when a nonempty command is ready for interpretation.
 */
bool ra_link_collect(struct ra_link_collector *collector,
                     const struct ra_link_command_mapping *mappings, size_t count, char digit,
                     uint64_t now_ms, char *completed);

/** @brief Check a mapping table before accepting it as configuration.
 * @param mappings Mapping array with valid action enumerators.
 * @param count Mapping count; zero permits null.
 * @return Null on success or a diagnostic for invalid or ambiguous prefixes.
 * Prefix overlap is rejected because commands must not execute prematurely.
 */
const char *ra_link_commands_validate(const struct ra_link_command_mapping *mappings, size_t count);

/** @brief Interpret a complete DTMF command, after removing its initiating asterisk.
 * @param mappings Validated mapping table.
 * @param count Mapping count.
 * @param digits Complete command with no terminator; empty input is rejected.
 * @param result Written only for a valid command. Destination zero denotes the last node.
 * @return True for a recognized linking command with valid arguments.
 */
bool ra_link_command_parse(const struct ra_link_command_mapping *mappings, size_t count,
                           const char *digits, struct ra_link_command *result);
#endif
