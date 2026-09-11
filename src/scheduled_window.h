/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Parse and match bounded local-civil-time schedule windows.
 */
#ifndef RPT_ADVANCED_SCHEDULED_WINDOW_H
#define RPT_ADVANCED_SCHEDULED_WINDOW_H

#include <stdbool.h>
#include <stddef.h>
#include <time.h>

/** @brief Maximum explicitly dated calendar days accepted by one window. */
#define RA_SCHEDULED_WINDOW_MAX_DATES 64U

/** @brief Bit mask containing every Sunday-zero weekday bit. */
#define RA_SCHEDULED_WINDOW_WEEKDAY_MASK 0x7fU

/** @brief One Gregorian civil date used to select a schedule window. */
struct ra_scheduled_window_date {
    unsigned int year;  /**< Four-digit Gregorian year, from one through 9999. */
    unsigned int month; /**< One-based Gregorian month, from one through 12. */
    unsigned int day;   /**< One-based day of the selected month. */
};

/** @brief One bounded same-day civil-time schedule window. */
struct ra_scheduled_window {
    unsigned int
        start_minute;        /**< Inclusive local minute after midnight, from zero through 1439. */
    unsigned int end_minute; /**< Exclusive local minute after midnight, from one through 1439. */
    unsigned int weekday_mask; /**< Sunday-zero weekday selector, or zero for every calendar day. */
    size_t date_count; /**< Number of explicit entries in @ref dates, or zero when unused. */
    struct ra_scheduled_window_date
        dates[RA_SCHEDULED_WINDOW_MAX_DATES]; /**< Explicit calendar-date selector. */
};

/** @brief Parse one configured same-day schedule window.
 * @param days Optional comma-separated full weekday names or inclusive weekday ranges, such as
 *             `Monday-Friday` or `Tuesday,Thursday`. Matching is case-insensitive. Null or an
 *             empty value selects every calendar day.
 * @param dates Optional comma-separated `YYYY-MM-DD` dates. Null or an empty value selects no
 *              explicit dates. A date selector cannot be combined with @p days.
 * @param start Exact local `HH:MM` inclusive start time.
 * @param end Exact local `HH:MM` exclusive end time, later than @p start on the same date.
 * @param result Receives the fully parsed window on success and is unchanged on failure.
 * @return True when all selectors and times are valid.
 *
 * Whitespace around weekday, date, range, and comma separators is accepted. The time grammar is
 * deliberately exact: it accepts only a five-byte 24-hour `HH:MM` value. A missing weekday and
 * date selector makes the window daily. Overnight windows are rejected, keeping their local-date
 * behavior unambiguous.
 */
bool ra_scheduled_window_parse(const char *days, const char *dates, const char *start,
                               const char *end, struct ra_scheduled_window *result);

/** @brief Validate a programmatically constructed schedule window.
 * @param window Candidate window.
 * @return True when the window uses valid same-day bounds and exactly zero or one selector type.
 *
 * This permits callers that construct a window without text parsing to retain the same bounded
 * semantics as @ref ra_scheduled_window_parse().
 */
bool ra_scheduled_window_valid(const struct ra_scheduled_window *window);

/** @brief Test whether one local civil clock falls inside a validated schedule window.
 * @param window Parsed or otherwise valid schedule window.
 * @param local Current local civil time.
 * @return True when @p local is within the inclusive-start, exclusive-end selected window.
 *
 * A weekday selector uses @c tm_wday in the standard Sunday-zero convention. An explicit-date
 * selector uses the Gregorian @c tm_year, @c tm_mon, and @c tm_mday fields. A window without a
 * selector matches every valid local calendar day.
 */
bool ra_scheduled_window_matches(const struct ra_scheduled_window *window, const struct tm *local);

#endif
