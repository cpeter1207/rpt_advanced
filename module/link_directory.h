/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief ASL static, DNS, and external-directory lookup with source-address verification.
 */
#ifndef RPT_ADVANCED_LINK_DIRECTORY_H
#define RPT_ADVANCED_LINK_DIRECTORY_H
#include "settings.h"
#include <stdbool.h>

/** @brief Per-node control-plane lookup sources after settings inheritance. */
struct ra_link_directory_policy {
    const char *static_file;           /**< Optional local-priority `[extnodes]` source. */
    const char *external_file;         /**< Optional ASL external `[extnodes]` source. */
    enum ra_link_lookup_method method; /**< DNS/file selection after static lookup. */
};

/** @brief Resolve a registered node and optionally verify an incoming source address.
 * @param node Decimal ASL node identity.
 * @param peer_ip Numeric source address to verify, or null for an outbound lookup.
 * @param policy Static override plus selected DNS/external lookup sources.
 * @return Owned Asterisk-allocated IAX destination, or null on lookup/verification failure.
 *
 * The static source is authoritative when it contains the requested node. A malformed record,
 * a dial target whose final `/node` identity differs from its `[extnodes]` key, or an incoming
 * source-address mismatch therefore rejects the request without falling through to DNS or an
 * external source. In `both` mode DNS is tried before the external file; a DNS
 * source-address mismatch is likewise rejected rather than bypassed. Call outside audio threads.
 * The caller releases a non-null result with ast_free.
 */
char *ra_link_directory_lookup(const char *node, const char *peer_ip,
                               const struct ra_link_directory_policy *policy);
#endif
