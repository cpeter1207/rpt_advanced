/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Whole-document validation and unlimited named-node media-set discovery.
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

/** @brief Find a unique announcement set for one node, in configuration order.
 * @param document Validated document.
 * @param node Node name.
 * @param index Zero-based set index.
 * @return Borrowed complete section name, or null after the last set.
 */
const char *ra_document_announcement(const struct ra_document *document, const char *node,
                                     size_t index);

/** @brief Find a unique named courtesy tone for one node, in configuration order.
 * @param document Validated document.
 * @param node Node name.
 * @param index Zero-based tone index.
 * @return Borrowed complete section name, or null after the last tone.
 */
const char *ra_document_courtesy(const struct ra_document *document, const char *node,
                                 size_t index);

/** @brief Find a resolved named template visible to one node.
 * @param document Validated document.
 * @param node Node whose same-label override takes precedence.
 * @param label Case-sensitive template label.
 * @return Borrowed complete section name, or null when no global or node definition exists.
 */
const char *ra_document_template_named(const struct ra_document *document, const char *node,
                                       const char *label);

/** @brief Find a resolved named macro visible to one node.
 * @param document Validated document.
 * @param node Node whose same-label override takes precedence.
 * @param label Case-sensitive macro label.
 * @return Borrowed complete section name, or null when no global or node definition exists.
 */
const char *ra_document_macro_named(const struct ra_document *document, const char *node,
                                    const char *label);

/** @brief Find a zero-time event in global configuration-section order.
 * @param document Validated document.
 * @param index Zero-based event index.
 * @param node Receives the complete owning node name when non-null.
 * @return Borrowed complete event section name, or null after the final event.
 *
 * Validation rejects repeated event headers. Enumeration returns each unique definition once in
 * source order, so the scheduler can serialize same-minute events exactly as configured.
 */
const char *ra_document_event(const struct ra_document *document, size_t index, const char **node);

#endif
