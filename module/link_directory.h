/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief ASL DNS destination lookup and source-address verification.
 */
#ifndef RPT_ADVANCED_LINK_DIRECTORY_H
#define RPT_ADVANCED_LINK_DIRECTORY_H
#include <stdbool.h>

/** @brief Resolve a registered node and optionally verify an incoming source address.
 * @param node Decimal ASL node identity.
 * @param peer_ip Numeric source address to verify, or null for an outbound lookup.
 * @param directory_file Optional static directory path, checked before DNS.
 * @return Owned Asterisk-allocated IAX destination, or null on lookup/verification failure.
 * Call outside audio threads. The caller releases the result with ast_free.
 */
char *ra_link_directory_lookup(const char *node, const char *peer_ip, const char *directory_file);
#endif
