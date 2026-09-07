/** @file
 * @brief Streaming configuration reader with physical-line diagnostics.
 */
#ifndef RPT_ADVANCED_CONFIG_READER_H
#define RPT_ADVANCED_CONFIG_READER_H
#include "config.h"
#include <stdio.h>

/** @brief Consume one section or option while loading a temporary configuration.
 * @param context Caller-owned temporary configuration builder.
 * @param kind Section or option classification; comments are not delivered.
 * @param name Section or option name, valid only for this call.
 * @param value Option value, or null for a section; valid only for this call.
 * @return Null on success, or a stable diagnostic string to abort loading.
 * The builder must copy any strings it retains and discard partial state on error.
 */
typedef const char *(*ra_config_consumer)(void *context, enum ra_config_line_kind kind,
                                          const char *name, const char *value);

/** @brief Read an already opened configuration stream without fixed line-length limits.
 * @param stream Input stream; ownership remains with the caller.
 * @param consume Builder callback, called in file order.
 * @param context Builder state passed unchanged to consume.
 * @param line Receives the failing physical line, or number of lines read on success.
 * @return Null on success; otherwise a stable diagnostic string. Caller adds the filename.
 * Embedded null bytes and options outside sections are rejected. Empty files are valid.
 */
const char *ra_config_read(FILE *stream, ra_config_consumer consume, void *context, size_t *line);
#endif
