/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exact node-list parsing and deny-first incoming link authorization.
 */
#ifndef RPT_ADVANCED_LINK_ACCESS_H
#define RPT_ADVANCED_LINK_ACCESS_H
#include <stdbool.h>

/** @brief Validate comma-separated decimal node identities; empty disables a list.
 * @param list Configuration value, with optional spaces around entries.
 * @return True for a well-formed list; empty entries and wildcards are rejected.
 */
bool ra_link_access_list_valid(const char *list);

/** @brief Apply access policy after the caller's independent identity verification.
 * @param allow Validated allowlist; empty permits all verified identities.
 * @param deny Validated denylist; matching entries always deny access.
 * @param node Nonempty verified decimal node identity, compared exactly.
 * @param verified True only after directory/address authentication succeeds.
 * @param same_server True for an independently verified local node.
 * @return True if access is permitted. Same-server status never bypasses denial.
 */
bool ra_link_access_allowed(const char *allow, const char *deny, const char *node, bool verified,
                            bool same_server);
#endif
