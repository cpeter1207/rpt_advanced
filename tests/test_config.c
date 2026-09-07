/** @file
 * @brief Exercise configuration inheritance independently of input file ordering.
 */
#include "config.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief All permutations of shared, node, and set values retain specificity. */
static void test_order(void) {
    const struct ra_config_entry definitions[] = {
        {"identifier", "interval_ms", "600000"},
        {"identifier usb", "interval_ms", "300000"},
        {"identifier usb welcome", "interval_ms", "900000"}};
    for (size_t a = 0; a < 3; ++a) {
        for (size_t b = 0; b < 3; ++b) {
            if (a == b) {
                continue;
            }
            size_t c = 3 - a - b;
            struct ra_config_entry entries[] = {definitions[a], definitions[b], definitions[c]};
            assert(strcmp(ra_config_lookup(entries, 3, "interval_ms", "identifier",
                                           "identifier usb", "identifier usb welcome"),
                          "900000") == 0);
            assert(strcmp(ra_config_lookup(entries, 3, "interval_ms", "identifier",
                                           "identifier usb", NULL),
                          "300000") == 0);
            assert(strcmp(ra_config_lookup(entries, 3, "interval_ms", "identifier", NULL, NULL),
                          "600000") == 0);
        }
    }
}

/** @brief Empty overrides clear inherited values; unrelated nodes never leak settings. */
static void test_clearing_and_missing(void) {
    const struct ra_config_entry entries[] = {
        {"identifier", "speech_text", "Default"},     {"identifier usb", "speech_text", ""},
        {"identifier other", "speech_text", "Other"}, {"identifier", "morse_text", "KG0BP"},
        {"identifier usb", "interval_ms", "1000"},    {"identifier usb", "interval_ms", "2000"}};
    assert(strcmp(ra_config_lookup(entries, 6, "speech_text", "identifier", "identifier usb", NULL),
                  "") == 0);
    assert(
        strcmp(ra_config_lookup(entries, 6, "speech_text", "identifier", "identifier absent", NULL),
               "Default") == 0);
    assert(strcmp(ra_config_lookup(entries, 6, "morse_text", "identifier", "identifier usb", NULL),
                  "KG0BP") == 0);
    assert(strcmp(ra_config_lookup(entries, 6, "interval_ms", "identifier", "identifier usb", NULL),
                  "2000") == 0);
    assert(ra_config_lookup(entries, 6, "unknown", "identifier", NULL, NULL) == NULL);
    assert(ra_config_lookup(NULL, 0, "speech_text", "identifier", NULL, NULL) == NULL);
}

/** @brief Run all inheritance tests.
 * @return Zero if every assertion passes.
 */
int main(void) {
    test_order();
    test_clearing_and_missing();
    puts("configuration inheritance tests passed");
    return 0;
}
