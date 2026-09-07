/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Owned configuration storage for node discovery and settings resolution.
 */
#ifndef RPT_ADVANCED_DOCUMENT_H
#define RPT_ADVANCED_DOCUMENT_H
#include "config_reader.h"

/** @brief Complete parsed file; all strings remain valid until destruction. */
struct ra_document {
    struct ra_config_entry *entries; /**< Options in file order, referring to owned sections. */
    size_t count;                    /**< Number of options. */
    char **sections;      /**< Section headers in file order, including empty sections. */
    size_t section_count; /**< Number of section headers. */
};

/** @brief Release a document and reset it to an empty reusable value.
 * @param document Initialized document, including a zero-initialized empty one.
 */
void ra_document_destroy(struct ra_document *document);

/** @brief Retain a complete parsed stream, discarding partial allocations on failure.
 * @param stream Already opened input, still owned by the caller.
 * @param document Receives the document on success; must initially be empty.
 * @param line Receives the failing physical line or successful line count.
 * @return Null on success, or stable diagnostic text; failure leaves document unchanged.
 * Syntax reading is separate from schema validation and runtime capability checks.
 */
const char *ra_document_read(FILE *stream, struct ra_document *document, size_t *line);
#endif
