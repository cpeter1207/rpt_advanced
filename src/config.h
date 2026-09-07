/** @file
 * @brief Resolve flat, node, and identifier-set configuration defaults.
 */
#ifndef RPT_ADVANCED_CONFIG_H
#define RPT_ADVANCED_CONFIG_H

#include <stddef.h>

/** @brief Borrowed configuration entry; strings remain owned by the configuration loader. */
struct ra_config_entry {
    const char *section; /**< Section name, including any node and ID-set scope. */
    const char *key;     /**< Option name. */
    const char *value;   /**< Explicit value; an empty string clears an inherited value. */
};

/** @brief Find an option using shared defaults, node defaults, then set overrides.
 * @param entries Configuration entries with non-null strings.
 * @param count Number of entries; zero permits a null array.
 * @param key Option name to resolve.
 * @param shared Flat default section, such as identifier.
 * @param node Scoped node-default section, or null if not applicable.
 * @param set Scoped ID-set section, or null if not applicable.
 * @return Borrowed value, including an explicitly empty string, or null if absent.
 * Later entries win within one section; scoped values always win over defaults
 * regardless of file order. Validation of names and values belongs to the loader.
 */
const char *ra_config_lookup(const struct ra_config_entry *entries, size_t count, const char *key,
                             const char *shared, const char *node, const char *set);

#endif
