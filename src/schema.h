/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Whole-document validation and unlimited named-node/identifier discovery.
 */
#ifndef RPT_ADVANCED_SCHEMA_H
#define RPT_ADVANCED_SCHEMA_H
#include "document.h"

/** @brief Validate all sections and options, including overridden entries.
 * @param document Parsed document.
 * @param section Receives offending section, or null on success.
 * @param key Receives offending option, or null for section errors and success.
 * @return Null on success or stable diagnostic text.
 */
const char *ra_document_validate(const struct ra_document *document, const char **section,
                                 const char **key);

/** @brief Find a unique node by its configuration-order index.
 * @param document Validated document.
 * @param index Zero-based node index.
 * @return Borrowed section name, or null after the last node.
 */
const char *ra_document_node(const struct ra_document *document, size_t index);

/** @brief Find a unique identifier set for one node, in configuration order.
 * @param document Validated document.
 * @param node Node name.
 * @param index Zero-based set index.
 * @return Borrowed complete section name, or null after the last set.
 */
const char *ra_document_identifier(const struct ra_document *document, const char *node,
                                   size_t index);

/** @brief Find a node's optional identifier-default section.
 * @param document Validated document.
 * @param node Node name.
 * @return Borrowed complete section name or null when only flat defaults apply.
 */
const char *ra_document_identifier_defaults(const struct ra_document *document, const char *node);
#endif
