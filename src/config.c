/** @file
 * @brief Resolve inheritance without copying strings or limiting the number of nodes.
 */
#include "config.h"
#include <string.h>

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
