/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Verify strict local-civil weekly and explicit-date schedule windows.
 */
#include "scheduled_window.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Make one complete valid local-civil-time test fixture.
 * @param year Gregorian year.
 * @param month One-based Gregorian month.
 * @param day One-based day of month.
 * @param weekday Sunday-zero local weekday.
 * @param hour Local 24-hour clock hour.
 * @param minute Local clock minute.
 * @return Fully initialized local civil-time fixture.
 */
static struct tm local_time(int year, int month, int day, int weekday, int hour, int minute) {
    return (struct tm){.tm_year = year - 1900,
                       .tm_mon = month - 1,
                       .tm_mday = day,
                       .tm_wday = weekday,
                       .tm_hour = hour,
                       .tm_min = minute};
}

/** @brief Assert that one parse failure leaves the caller's window unchanged.
 * @param days Candidate weekday selector.
 * @param dates Candidate explicit-date selector.
 * @param start Candidate start clock.
 * @param end Candidate end clock.
 */
static void rejected(const char *days, const char *dates, const char *start, const char *end) {
    struct ra_scheduled_window retained;
    memset(&retained, 0xa5, sizeof(retained));
    const struct ra_scheduled_window expected = retained;
    assert(!ra_scheduled_window_parse(days, dates, start, end, &retained));
    assert(!memcmp(&retained, &expected, sizeof(retained)));
}

/** @brief Verify no-selector daily parsing and comma-separated weekday range grammar. */
static void parser_days(void) {
    struct ra_scheduled_window window = {0};
    assert(ra_scheduled_window_parse(NULL, NULL, "11:00", "12:00", &window));
    assert(window.start_minute == 660U && window.end_minute == 720U && !window.weekday_mask &&
           !window.date_count);

    assert(ra_scheduled_window_parse(" Monday - Friday ", "", "00:00", "23:59", &window));
    assert(window.weekday_mask == 0x3eU);
    assert(
        ra_scheduled_window_parse("Tuesday, Thursday-Saturday", NULL, "11:00", "12:00", &window));
    assert(window.weekday_mask == 0x74U);
    assert(ra_scheduled_window_parse("Friday-Monday", NULL, "11:00", "12:00", &window));
    assert(window.weekday_mask == 0x63U);
    assert(ra_scheduled_window_parse("Sunday-Sunday", NULL, "11:00", "12:00", &window));
    assert(window.weekday_mask == 0x01U);

    rejected("Mon", NULL, "11:00", "12:00");
    rejected("Funday", NULL, "11:00", "12:00");
    rejected("Monday Tuesday", NULL, "11:00", "12:00");
    rejected("Monday-", NULL, "11:00", "12:00");
    rejected("Monday--Friday", NULL, "11:00", "12:00");
    rejected("-Friday", NULL, "11:00", "12:00");
    rejected(",Monday", NULL, "11:00", "12:00");
    rejected("Monday,,Friday", NULL, "11:00", "12:00");
    rejected("Monday,", NULL, "11:00", "12:00");
}

/** @brief Verify explicit Gregorian-date selectors, syntax rejections, and fixed capacity. */
static void parser_dates(void) {
    struct ra_scheduled_window window = {0};
    assert(ra_scheduled_window_parse(NULL, "2026-09-10, 2028-02-29", "11:00", "12:00", &window));
    assert(!window.weekday_mask && window.date_count == 2U && window.dates[0].year == 2026U &&
           window.dates[1].year == 2028U && window.dates[1].month == 2U &&
           window.dates[1].day == 29U);
    assert(ra_scheduled_window_parse(NULL, "2000-02-29", "11:00", "12:00", &window));
    rejected("Monday", "2026-09-10", "11:00", "12:00");
    rejected(NULL, "2027-02-29", "11:00", "12:00");
    rejected(NULL, "2100-02-29", "11:00", "12:00");
    rejected(NULL, "0000-01-01", "11:00", "12:00");
    rejected(NULL, "2026", "11:00", "12:00");
    rejected(NULL, "2026-", "11:00", "12:00");
    rejected(NULL, "2026-09", "11:00", "12:00");
    rejected(NULL, "x026-09-10", "11:00", "12:00");
    rejected(NULL, "2026-x9-10", "11:00", "12:00");
    rejected(NULL, "2026-09-x0", "11:00", "12:00");
    rejected(NULL, "2026-00-01", "11:00", "12:00");
    rejected(NULL, "2026-13-01", "11:00", "12:00");
    rejected(NULL, "2026/09-10", "11:00", "12:00");
    rejected(NULL, "2026-09/10", "11:00", "12:00");
    rejected(NULL, "2026-09-10x", "11:00", "12:00");
    rejected(NULL, "2026-09-10,", "11:00", "12:00");
    rejected(NULL, ",2026-09-10", "11:00", "12:00");
    rejected(NULL, "2026-09-10,,2026-09-11", "11:00", "12:00");

    char date_list[1024] = "";
    size_t length = 0;
    for (size_t index = 0; index <= RA_SCHEDULED_WINDOW_MAX_DATES; ++index) {
        int written = snprintf(date_list + length, sizeof(date_list) - length, "%s2026-01-01",
                               index ? "," : "");
        assert(written > 0 && (size_t)written < sizeof(date_list) - length);
        length += (size_t)written;
    }
    rejected(NULL, date_list, "11:00", "12:00");
}

/** @brief Verify strict clock handling, argument validation, and same-date bounds. */
static void parser_times(void) {
    struct ra_scheduled_window window = {0};
    assert(!ra_scheduled_window_parse(NULL, NULL, "11:00", "12:00", NULL));
    rejected(NULL, NULL, NULL, "12:00");
    rejected(NULL, NULL, "11:00", NULL);
    rejected(NULL, NULL, "1:00", "12:00");
    rejected(NULL, NULL, "11-00", "12:00");
    rejected(NULL, NULL, "x1:00", "12:00");
    rejected(NULL, NULL, "11:x0", "12:00");
    rejected(NULL, NULL, "24:00", "12:00");
    rejected(NULL, NULL, "11:60", "12:00");
    rejected(NULL, NULL, "11:00 ", "12:00");
    rejected(NULL, NULL, "12:00", "12:00");
    rejected(NULL, NULL, "12:01", "12:00");
    assert(ra_scheduled_window_parse("", "", "00:00", "00:01", &window));
}

/** @brief Verify programmatic window validation separately from text parsing. */
static void validation(void) {
    struct ra_scheduled_window window = {0};
    assert(!ra_scheduled_window_valid(NULL));
    assert(ra_scheduled_window_parse(NULL, NULL, "11:00", "12:00", &window));
    assert(ra_scheduled_window_valid(&window));
    window.start_minute = 1440U;
    assert(!ra_scheduled_window_valid(&window));
    window.start_minute = 660U;
    window.end_minute = 1440U;
    assert(!ra_scheduled_window_valid(&window));
    window.end_minute = 660U;
    assert(!ra_scheduled_window_valid(&window));
    window.end_minute = 720U;
    window.weekday_mask = 0x80U;
    assert(!ra_scheduled_window_valid(&window));
    window.weekday_mask = 1U;
    window.date_count = 1U;
    assert(!ra_scheduled_window_valid(&window));
    window.weekday_mask = 0;
    window.date_count = RA_SCHEDULED_WINDOW_MAX_DATES + 1U;
    assert(!ra_scheduled_window_valid(&window));
    window.date_count = 1U;
    window.dates[0] = (struct ra_scheduled_window_date){.year = 2026U, .month = 2U, .day = 29U};
    assert(!ra_scheduled_window_valid(&window));
    window.dates[0] = (struct ra_scheduled_window_date){.year = 10000U, .month = 1U, .day = 1U};
    assert(!ra_scheduled_window_valid(&window));
    window.dates[0] = (struct ra_scheduled_window_date){.year = 2026U, .month = 1U, .day = 0U};
    assert(!ra_scheduled_window_valid(&window));
}

/** @brief Verify daily, weekly, and explicit-date local-civil matching. */
static void matching(void) {
    struct ra_scheduled_window window = {0};
    struct tm now = local_time(2026, 9, 9, 3, 11, 0);
    assert(ra_scheduled_window_parse("Monday-Friday", NULL, "11:00", "12:00", &window));
    assert(ra_scheduled_window_matches(&window, &now));
    now.tm_min = 59;
    assert(ra_scheduled_window_matches(&window, &now));
    now.tm_hour = 12;
    now.tm_min = 0;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_hour = 10;
    now.tm_min = 59;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_hour = 11;
    now.tm_min = 0;
    now.tm_wday = 6;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_wday = -1;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_wday = 7;
    assert(!ra_scheduled_window_matches(&window, &now));

    assert(ra_scheduled_window_parse(NULL, NULL, "11:00", "12:00", &window));
    now.tm_wday = -1;
    assert(ra_scheduled_window_matches(&window, &now));
    assert(ra_scheduled_window_parse(NULL, "2026-09-10,2026-09-09", "11:00", "12:00", &window));
    now.tm_wday = 3;
    assert(ra_scheduled_window_matches(&window, &now));
    now.tm_mday = 10;
    assert(ra_scheduled_window_matches(&window, &now));
    now.tm_mday = 11;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_mon = 7;
    now.tm_mday = 9;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_mon = 8;
    now.tm_mday = 8;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_year = 127;
    now.tm_mday = 9;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_year = 126;

    assert(!ra_scheduled_window_matches(NULL, &now));
    assert(!ra_scheduled_window_matches(&window, NULL));
    now = local_time(2026, 9, 9, 3, 11, 0);
    now.tm_hour = -1;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_hour = 24;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_hour = 11;
    now.tm_min = -1;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_min = 60;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_min = 0;
    now.tm_mon = -1;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_mon = 12;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_mon = 1;
    now.tm_mday = 29;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_mon = 8;
    now.tm_mday = 0;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_mday = 9;
    now.tm_year = -1900;
    assert(!ra_scheduled_window_matches(&window, &now));
    now.tm_year = 8100;
    assert(!ra_scheduled_window_matches(&window, &now));
}

/** @brief Execute all bounded civil-window parser and matcher tests.
 * @return Zero after every assertion passes.
 */
int main(void) {
    parser_days();
    parser_dates();
    parser_times();
    validation();
    matching();
    puts("scheduled window tests passed");
    return 0;
}
