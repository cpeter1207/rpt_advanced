/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exercise configuration inheritance independently of input file ordering.
 */
#include "config.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Numeric limits and invalid switches never partially update settings. */
static void test_values(void) {
    const char *invalid[] = {"",
                             "-1",
                             "+1",
                             "/",
                             ":",
                             "1s",
                             " 1",
                             "1 ",
                             "18446744073709551616",
                             "99999999999999999999",
                             "184467440737095516150"};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        uint64_t value = 42;
        assert(!ra_config_unsigned(invalid[i], 0, UINT64_MAX, &value));
        assert(value == 42);
    }
    uint64_t value = 42;
    assert(ra_config_unsigned("18446744073709551615", 0, UINT64_MAX, &value));
    assert(value == UINT64_MAX);
    assert(ra_config_unsigned("0", 0, 0, &value) && value == 0);
    assert(ra_config_unsigned("0010", 10, 10, &value) && value == 10);
    assert(!ra_config_unsigned("9", 10, 20, &value) && value == 10);
    assert(!ra_config_unsigned("21", 10, 20, &value) && value == 10);
    int64_t signed_value = 42;
    assert(ra_config_signed("-60", -60, 0, &signed_value) && signed_value == -60);
    assert(ra_config_signed("-9223372036854775808", INT64_MIN, INT64_MAX, &signed_value) &&
           signed_value == INT64_MIN);
    assert(ra_config_signed("0", -60, 0, &signed_value) && !signed_value);
    assert(!ra_config_signed("-61", -60, 0, &signed_value) && !signed_value);
    assert(!ra_config_signed("1", -60, 0, &signed_value) && !signed_value);
    assert(!ra_config_signed("+1", -60, 0, &signed_value) && !signed_value);
    assert(!ra_config_signed("", -60, 0, &signed_value) && !signed_value);
    const char *yes[] = {"yes", "Yes", "yEs", "yeS", "YES"};
    const char *no[] = {"no", "No", "nO", "NO"};
    const char *bad[] = {"", "1", "true", "xes", "yxs", "yex", "xo", "nx", "yes ", "no "};
    bool flag = false;
    for (size_t i = 0; i < sizeof(yes) / sizeof(yes[0]); ++i) {
        assert(ra_config_boolean(yes[i], &flag) && flag);
    }
    for (size_t i = 0; i < sizeof(no) / sizeof(no[0]); ++i) {
        assert(ra_config_boolean(no[i], &flag) && !flag);
    }
    for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); ++i) {
        assert(!ra_config_boolean(bad[i], &flag) && !flag);
    }
}

/** @brief Check syntax, whitespace, comments, and empty overrides. */
static void test_lines(void) {
    const struct {
        const char *text;
        enum ra_config_line_kind kind;
        const char *name;
        const char *value;
    } cases[] = {
        {"", RA_CONFIG_EMPTY, NULL, NULL},
        {" \t\r\n", RA_CONFIG_EMPTY, NULL, NULL},
        {"; comment", RA_CONFIG_EMPTY, NULL, NULL},
        {" [ identifier usb welcome ] ; test", RA_CONFIG_SECTION, "identifier usb welcome", NULL},
        {"[node]", RA_CONFIG_SECTION, "node", NULL},
        {"[missing", RA_CONFIG_INVALID, NULL, NULL},
        {"[]", RA_CONFIG_INVALID, NULL, NULL},
        {"[node] trailing", RA_CONFIG_INVALID, NULL, NULL},
        {"option", RA_CONFIG_INVALID, NULL, NULL},
        {" = value", RA_CONFIG_INVALID, NULL, NULL},
        {" speech_text = Hello = world \r\n", RA_CONFIG_OPTION, "speech_text", "Hello = world"},
        {"speech_text= ; clear", RA_CONFIG_OPTION, "speech_text", ""},
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        char line[128];
        strcpy(line, cases[i].text);
        char *name;
        char *value;
        assert(ra_config_parse_line(line, &name, &value) == cases[i].kind);
        assert(cases[i].name ? name && strcmp(name, cases[i].name) == 0 : name == NULL);
        assert(cases[i].value ? value && strcmp(value, cases[i].value) == 0 : value == NULL);
    }
}

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
    test_values();
    test_lines();
    test_order();
    test_clearing_and_missing();
    puts("configuration inheritance tests passed");
    return 0;
}
