/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Incoming access list grammar and authentication/precedence truth tables.
 */
#include "link_access.h"
#include <assert.h>
#include <stdio.h>

/** @brief Run list grammar and the complete access-policy truth table.
 * @return Zero after assertions.
 */
int main(void) {
    const char *valid[] = {"", " \t", "524950", " 524950 , 508422\t", "1,2,3", "0001"};
    const char *invalid[] = {",", "1,", "1,,2", "1, ", "1 2", "*", "-1", "1.0", "1\n"};
    for (size_t i = 0; i < sizeof(valid) / sizeof(*valid); ++i) {
        assert(ra_link_access_list_valid(valid[i]));
    }
    for (size_t i = 0; i < sizeof(invalid) / sizeof(*invalid); ++i) {
        assert(!ra_link_access_list_valid(invalid[i]));
    }
    for (unsigned int flags = 0; flags < 16; ++flags) {
        bool verified = (flags & 1) != 0;
        bool denied = (flags & 2) != 0;
        bool restricted = (flags & 4) != 0;
        bool listed = (flags & 8) != 0;
        const char *allow = restricted ? (listed ? "508422, 524950" : "508422") : "";
        const char *deny = denied ? "524950" : "508422";
        bool expected = verified && !denied && (!restricted || listed);
        assert(ra_link_access_allowed(allow, deny, "524950", verified) == expected);
    }
    assert(ra_link_access_allowed(" \t", "", "524950", true));
    assert(!ra_link_access_allowed("5249500,52495, 508422 ", "", "524950", true));
    assert(ra_link_access_allowed(" 524950 ", "508422, 52495 ", "524950", true));
    puts("deny-first link access tests passed");
    return 0;
}
