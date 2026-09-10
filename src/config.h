/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Resolve flat, node, identifier-set, and announcement-set configuration defaults.
 */
#ifndef RPT_ADVANCED_CONFIG_H
#define RPT_ADVANCED_CONFIG_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/** @brief Parse a bounded unsigned decimal configuration value.
 * @param text Null-terminated value after whitespace trimming.
 * @param minimum Inclusive lower bound.
 * @param maximum Inclusive upper bound, no smaller than minimum.
 * @param result Receives the value only on success.
 * @return True for digits only within bounds; false leaves result unchanged.
 * Reject signs, suffixes, and overflow instead of silently changing timing or rates.
 */
bool ra_config_unsigned(const char *text, uint64_t minimum, uint64_t maximum, uint64_t *result);

/** @brief Parse a bounded signed decimal configuration value.
 * @param text Null-terminated value after whitespace trimming.
 * @param minimum Inclusive lower bound.
 * @param maximum Inclusive upper bound, no smaller than minimum.
 * @param result Receives the value only on success.
 * @return True for a decimal value within bounds; false leaves result unchanged.
 */
bool ra_config_signed(const char *text, int64_t minimum, int64_t maximum, int64_t *result);

/** @brief Parse an explicit yes/no switch, independent of the process locale.
 * @param text Null-terminated trimmed configuration value.
 * @param result Receives the switch only on success.
 * @return True for yes or no, ignoring ASCII letter case; otherwise false.
 */
bool ra_config_boolean(const char *text, bool *result);

/** @brief Result of parsing one configuration line. */
enum ra_config_line_kind {
    RA_CONFIG_EMPTY,   /**< Blank line or comment. */
    RA_CONFIG_SECTION, /**< Section name returned in name. */
    RA_CONFIG_OPTION,  /**< Option name and value returned. */
    RA_CONFIG_INVALID  /**< Malformed section or option. */
};

/** @brief Parse a writable line without allocation or fixed length limits.
 * @param line Null-terminated line; modified in place.
 * @param name Receives section or option name, otherwise null.
 * @param value Receives option value, otherwise null.
 * @return Line classification; caller supplies the file name and line number in errors.
 * Semicolon starts a comment. Whitespace surrounding names and values is removed.
 * Empty option values intentionally clear inherited values. This parses syntax,
 * not section membership or option-specific ranges.
 */
enum ra_config_line_kind ra_config_parse_line(char *line, char **name, char **value);

/** @brief Borrowed configuration entry; strings remain owned by the configuration loader. */
struct ra_config_entry {
    const char
        *section;      /**< Section name, including any node, identifier, or announcement scope. */
    const char *key;   /**< Option name. */
    const char *value; /**< Explicit value; an empty string clears an inherited value. */
};

/** @brief Find an option using shared defaults, node defaults, then set overrides.
 * @param entries Configuration entries with non-null strings.
 * @param count Number of entries; zero permits a null array.
 * @param key Option name to resolve.
 * @param shared Flat default section, such as identifier.
 * @param node Scoped node-default section, or null if not applicable.
 * @param set Scoped identifier- or announcement-set section, or null when not applicable.
 * @return Borrowed value, including an explicitly empty string, or null if absent.
 * Later entries win within one section; scoped values always win over defaults
 * regardless of file order. Validation of names and values belongs to the loader.
 */
const char *ra_config_lookup(const struct ra_config_entry *entries, size_t count, const char *key,
                             const char *shared, const char *node, const char *set);

#endif
