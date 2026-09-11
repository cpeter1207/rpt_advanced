/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Strict parsing and matching for bounded weekly or date-selected civil windows.
 */
#include "scheduled_window.h"
#include <ctype.h>
#include <string.h>

/** @brief Number of local civil minutes in one 24-hour day. */
#define RA_SCHEDULED_WINDOW_MINUTES_PER_DAY 1440U

/** @brief Return true for one Gregorian leap year.
 * @param year Gregorian year.
 * @return True when February has 29 days.
 */
static bool leap_year(unsigned int year) {
    return year % 4U == 0U && (year % 100U != 0U || year % 400U == 0U);
}

/** @brief Return the day count for one Gregorian month.
 * @param year Gregorian year used for the February leap-day rule.
 * @param month One-based Gregorian month.
 * @return The number of days, or zero for an invalid month.
 */
static unsigned int month_days(unsigned int year, unsigned int month) {
    static const unsigned char days[] = {31U, 28U, 31U, 30U, 31U, 30U,
                                         31U, 31U, 30U, 31U, 30U, 31U};
    if (!month || month > sizeof(days)) {
        return 0;
    }
    return month == 2U && leap_year(year) ? 29U : days[month - 1U];
}

/** @brief Test one Gregorian date without interpreting it in a time zone.
 * @param date Candidate date.
 * @return True when all date members are in their valid Gregorian ranges.
 */
static bool date_valid(const struct ra_scheduled_window_date *date) {
    return date->year && date->year <= 9999U && date->day &&
           date->day <= month_days(date->year, date->month);
}

/** @brief Skip ASCII whitespace in a mutable parser cursor.
 * @param cursor Mutable parser position.
 */
static void skip_space(const char **cursor) {
    while (isspace((unsigned char)**cursor)) {
        ++*cursor;
    }
}

/** @brief Parse an exact number of decimal digits.
 * @param cursor Mutable parser position.
 * @param digits Number of required decimal digits.
 * @param value Receives the parsed unsigned value on success.
 * @return True when exactly @p digits decimal bytes were available.
 */
static bool fixed_digits(const char **cursor, size_t digits, unsigned int *value) {
    unsigned int parsed = 0;
    for (size_t index = 0; index < digits; ++index) {
        if (!isdigit((unsigned char)**cursor)) {
            return false;
        }
        parsed = parsed * 10U + (unsigned int)(**cursor - '0');
        ++*cursor;
    }
    *value = parsed;
    return true;
}

/** @brief Parse one exact 24-hour clock into a local minute after midnight.
 * @param text Complete candidate `HH:MM` string.
 * @param minute Receives the parsed minute after midnight on success.
 * @return True when @p text is exactly one valid 24-hour minute.
 */
static bool clock_time(const char *text, unsigned int *minute) {
    const char *cursor = text;
    unsigned int hour;
    unsigned int parsed_minute;
    if (!text || strlen(text) != 5U || !fixed_digits(&cursor, 2U, &hour) || *cursor++ != ':' ||
        !fixed_digits(&cursor, 2U, &parsed_minute) || hour >= 24U || parsed_minute >= 60U) {
        return false;
    }
    *minute = hour * 60U + parsed_minute;
    return true;
}

/** @brief Compare a candidate ASCII word with a lowercase reference without allocating memory.
 * @param begin First candidate byte.
 * @param end One past the final candidate byte.
 * @param word Lowercase reference word.
 * @return True when the complete words are equal without case sensitivity.
 */
static bool word_equal(const char *begin, const char *end, const char *word) {
    size_t length = (size_t)(end - begin);
    if (strlen(word) != length) {
        return false;
    }
    for (size_t index = 0; index < length; ++index) {
        if (tolower((unsigned char)begin[index]) != (unsigned char)word[index]) {
            return false;
        }
    }
    return true;
}

/** @brief Parse one full English weekday name.
 * @param cursor Mutable parser position.
 * @param weekday Receives the Sunday-zero weekday value on success.
 * @return True when the cursor starts with a supported full English weekday name.
 */
static bool weekday_name(const char **cursor, unsigned int *weekday) {
    static const char *const names[] = {"sunday",   "monday", "tuesday", "wednesday",
                                        "thursday", "friday", "saturday"};
    const char *begin = *cursor;
    while (isalpha((unsigned char)**cursor)) {
        ++*cursor;
    }
    if (begin == *cursor) {
        return false;
    }
    for (size_t index = 0; index < sizeof(names) / sizeof(*names); ++index) {
        if (word_equal(begin, *cursor, names[index])) {
            *weekday = (unsigned int)index;
            return true;
        }
    }
    return false;
}

/** @brief Set every weekday bit in one inclusive circular weekday range.
 * @param mask Mutable Sunday-zero weekday bit mask.
 * @param first First weekday in the range.
 * @param last Final weekday in the range.
 */
static void add_weekday_range(unsigned int *mask, unsigned int first, unsigned int last) {
    for (unsigned int weekday = first;; weekday = (weekday + 1U) % 7U) {
        *mask |= 1U << weekday;
        if (weekday == last) {
            return;
        }
    }
}

/** @brief Parse an optional comma-separated weekday selector.
 * @param text Candidate selector, or null for an omitted selector.
 * @param mask Receives zero for no selector or the parsed Sunday-zero weekday bits.
 * @return True when all comma-separated terms are complete weekday names or inclusive ranges.
 */
static bool weekdays(const char *text, unsigned int *mask) {
    const char *cursor = text ? text : "";
    unsigned int parsed = 0;
    skip_space(&cursor);
    if (!*cursor) {
        *mask = 0;
        return true;
    }
    for (;;) {
        unsigned int first;
        if (!weekday_name(&cursor, &first)) {
            return false;
        }
        skip_space(&cursor);
        if (*cursor == '-') {
            unsigned int last;
            ++cursor;
            skip_space(&cursor);
            if (!weekday_name(&cursor, &last)) {
                return false;
            }
            add_weekday_range(&parsed, first, last);
        } else {
            parsed |= 1U << first;
        }
        skip_space(&cursor);
        if (!*cursor) {
            *mask = parsed;
            return true;
        }
        if (*cursor != ',') {
            return false;
        }
        ++cursor;
        skip_space(&cursor);
        if (!*cursor) {
            return false;
        }
    }
}

/** @brief Parse one exact Gregorian `YYYY-MM-DD` date.
 * @param cursor Mutable parser position.
 * @param date Receives the parsed date on success.
 * @return True when the cursor starts with a valid complete Gregorian date.
 */
static bool date(const char **cursor, struct ra_scheduled_window_date *date) {
    struct ra_scheduled_window_date parsed = {0};
    if (!fixed_digits(cursor, 4U, &parsed.year) || **cursor != '-') {
        return false;
    }
    ++*cursor;
    if (!fixed_digits(cursor, 2U, &parsed.month) || **cursor != '-') {
        return false;
    }
    ++*cursor;
    if (!fixed_digits(cursor, 2U, &parsed.day) || !date_valid(&parsed)) {
        return false;
    }
    *date = parsed;
    return true;
}

/** @brief Parse an optional comma-separated explicit-date selector.
 * @param text Candidate selector, or null for an omitted selector.
 * @param dates Receives parsed dates on success.
 * @param count Receives the parsed date count on success.
 * @return True when all terms are bounded valid Gregorian dates.
 */
static bool dates(const char *text, struct ra_scheduled_window_date *dates, size_t *count) {
    const char *cursor = text ? text : "";
    size_t parsed_count = 0;
    skip_space(&cursor);
    if (!*cursor) {
        *count = 0;
        return true;
    }
    for (;;) {
        if (parsed_count == RA_SCHEDULED_WINDOW_MAX_DATES || !date(&cursor, &dates[parsed_count])) {
            return false;
        }
        ++parsed_count;
        skip_space(&cursor);
        if (!*cursor) {
            *count = parsed_count;
            return true;
        }
        if (*cursor != ',') {
            return false;
        }
        ++cursor;
        skip_space(&cursor);
        if (!*cursor) {
            return false;
        }
    }
}

bool ra_scheduled_window_valid(const struct ra_scheduled_window *window) {
    if (!window || window->start_minute >= RA_SCHEDULED_WINDOW_MINUTES_PER_DAY ||
        window->end_minute >= RA_SCHEDULED_WINDOW_MINUTES_PER_DAY ||
        window->start_minute >= window->end_minute ||
        (window->weekday_mask & ~RA_SCHEDULED_WINDOW_WEEKDAY_MASK) ||
        (window->weekday_mask && window->date_count) ||
        window->date_count > RA_SCHEDULED_WINDOW_MAX_DATES) {
        return false;
    }
    for (size_t index = 0; index < window->date_count; ++index) {
        if (!date_valid(&window->dates[index])) {
            return false;
        }
    }
    return true;
}

bool ra_scheduled_window_parse(const char *days, const char *date_text, const char *start,
                               const char *end, struct ra_scheduled_window *result) {
    if (!result) {
        return false;
    }
    struct ra_scheduled_window parsed = {0};
    if (!clock_time(start, &parsed.start_minute) || !clock_time(end, &parsed.end_minute) ||
        !weekdays(days, &parsed.weekday_mask) ||
        !dates(date_text, parsed.dates, &parsed.date_count) ||
        !ra_scheduled_window_valid(&parsed)) {
        return false;
    }
    *result = parsed;
    return true;
}

/** @brief Convert and validate a local @c struct tm date without calling time-zone functions.
 * @param local Candidate local civil time.
 * @param date Receives its Gregorian date on success.
 * @return True when the date, hour, and minute fields are usable for civil matching.
 */
static bool local_date(const struct tm *local, struct ra_scheduled_window_date *date) {
    if (!local || local->tm_year < -1899 || local->tm_year > 8099 || local->tm_mon < 0 ||
        local->tm_mon > 11 || local->tm_mday <= 0 || local->tm_hour < 0 || local->tm_hour >= 24 ||
        local->tm_min < 0 || local->tm_min >= 60) {
        return false;
    }
    *date = (struct ra_scheduled_window_date){.year = (unsigned int)(local->tm_year + 1900),
                                              .month = (unsigned int)(local->tm_mon + 1),
                                              .day = (unsigned int)local->tm_mday};
    return date_valid(date);
}

bool ra_scheduled_window_matches(const struct ra_scheduled_window *window, const struct tm *local) {
    struct ra_scheduled_window_date today;
    if (!ra_scheduled_window_valid(window) || !local_date(local, &today)) {
        return false;
    }
    unsigned int minute = (unsigned int)local->tm_hour * 60U + (unsigned int)local->tm_min;
    if (minute < window->start_minute || minute >= window->end_minute) {
        return false;
    }
    if (window->weekday_mask) {
        return local->tm_wday >= 0 && local->tm_wday < 7 &&
               (window->weekday_mask & (1U << (unsigned int)local->tm_wday));
    }
    if (!window->date_count) {
        return true;
    }
    for (size_t index = 0; index < window->date_count; ++index) {
        const struct ra_scheduled_window_date *selected = &window->dates[index];
        if (selected->year == today.year && selected->month == today.month &&
            selected->day == today.day) {
            return true;
        }
    }
    return false;
}
