/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Ownership of configured radio controllers and their running workers.
 */
#ifndef RPT_ADVANCED_RUNTIME_H
#define RPT_ADVANCED_RUNTIME_H
#include "document.h"

struct ra_runtime_node;
/** @brief Module-owned nodes; configuration strings must outlive this runtime. */
struct ra_runtime {
    struct ra_runtime_node *nodes; /**< Private list, initially null. */
};

/** @brief Start all enabled nodes from an already validated configuration.
 * @param runtime Empty destination; unchanged on failure.
 * @param document Immutable configuration retained until stop completes.
 * @return Null on success or a diagnostic after releasing all partial resources.
 */
const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document);

/** @brief Stop, unkey, and join all workers before releasing their media state.
 * @param runtime Owned runtime, safe when empty.
 */
void ra_runtime_stop(struct ra_runtime *runtime);
#endif
