/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Strict civil-time matching for the initial scheduled-event vocabulary.
 */
#include "scheduled_event.h"
#include <ctype.h>
#include <stddef.h>
#include <string.h>

/** @brief Return true for one Gregorian leap year.
 * @param year Gregorian year.
 * @return True when February has 29 days.
 */
static bool leap_year(unsigned int year) {
    return year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
}

/** @brief Return the number of days in one validated Gregorian month.
 * @param year Gregorian year used for the February leap-day rule.
 * @param month One-based month number.
 * @return Days in the month, or zero for an invalid month.
 */
static unsigned int month_days(unsigned int year, unsigned int month) {
    static const unsigned char days[] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};
    if (!month || month > sizeof(days)) {
        return 0;
    }
    return month == 2 && leap_year(year) ? 29 : days[month - 1];
}

/** @brief Parse exactly two decimal digits at a fixed position.
 * @param text At least two input bytes.
 * @param value Receives the parsed value on success.
 * @return True when both bytes are decimal digits.
 */
static bool two_digits(const char *text, unsigned int *value) {
    if (!isdigit((unsigned char)text[0]) || !isdigit((unsigned char)text[1])) {
        return false;
    }
    *value = (unsigned int)(text[0] - '0') * 10U + (unsigned int)(text[1] - '0');
    return true;
}

/** @brief Parse one exact `HH:MM` suffix and reject trailing text.
 * @param text Complete candidate clock string.
 * @param hour Receives the zero-based 24-hour value on success.
 * @param minute Receives the minute value on success.
 * @return True for a valid 24-hour clock value.
 */
static bool clock_time(const char *text, unsigned int *hour, unsigned int *minute) {
    if (strlen(text) != 5 || text[2] != ':' || !two_digits(text, hour) ||
        !two_digits(text + 3, minute)) {
        return false;
    }
    return *hour < 24 && *minute < 60;
}

/** @brief Compare one complete ASCII word without allocating a lower-case copy.
 * @param text Candidate bytes.
 * @param length Candidate byte count.
 * @param word Lowercase reference word.
 * @return True when the two complete words are equal without case sensitivity.
 */
static bool word_equal(const char *text, size_t length, const char *word) {
    if (strlen(word) != length) {
        return false;
    }
    for (size_t index = 0; index < length; ++index) {
        if (tolower((unsigned char)text[index]) != (unsigned char)word[index]) {
            return false;
        }
    }
    return true;
}

/** @brief Parse one full English local weekday name into the tm_wday convention.
 * @param text Candidate weekday bytes.
 * @param length Candidate byte count.
 * @param value Receives Sunday-zero weekday number on success.
 * @return True for one documented full weekday name.
 */
static bool weekday(const char *text, size_t length, unsigned int *value) {
    static const char *const names[] = {"sunday",   "monday", "tuesday", "wednesday",
                                        "thursday", "friday", "saturday"};
    for (size_t index = 0; index < sizeof(names) / sizeof(*names); ++index) {
        if (word_equal(text, length, names[index])) {
            *value = (unsigned int)index;
            return true;
        }
    }
    return false;
}

/** @brief Parse an exact four-digit date and its following exact local clock suffix.
 * @param text Complete `YYYY-MM-DD HH:MM` candidate.
 * @param result Receives parsed date and clock members.
 * @return True for one valid Gregorian local date and time.
 */
static bool date_time(const char *text, struct ra_scheduled_event_time *result) {
    if (strlen(text) != 16 || text[4] != '-' || text[7] != '-' || text[10] != ' ') {
        return false;
    }
    unsigned int century;
    unsigned int year_part;
    if (!two_digits(text, &century) || !two_digits(text + 2, &year_part) ||
        !two_digits(text + 5, &result->month) || !two_digits(text + 8, &result->day) ||
        !clock_time(text + 11, &result->hour, &result->minute)) {
        return false;
    }
    result->year = century * 100U + year_part;
    return result->year != 0 && result->day <= month_days(result->year, result->month) &&
           result->day != 0;
}

bool ra_scheduled_event_parse_at(const char *text, struct ra_scheduled_event_time *result) {
    if (!text || !result) {
        return false;
    }
    struct ra_scheduled_event_time parsed = {0};
    static const char daily[] = "daily ";
    static const char weekly[] = "weekly ";
    static const char once[] = "once ";
    if (!strncmp(text, daily, sizeof(daily) - 1)) {
        parsed.kind = RA_SCHEDULED_EVENT_DAILY;
        if (!clock_time(text + sizeof(daily) - 1, &parsed.hour, &parsed.minute)) {
            return false;
        }
    } else if (!strncmp(text, weekly, sizeof(weekly) - 1)) {
        const char *weekday_start = text + sizeof(weekly) - 1;
        const char *space = strchr(weekday_start, ' ');
        parsed.kind = RA_SCHEDULED_EVENT_WEEKLY;
        if (!space || !weekday(weekday_start, (size_t)(space - weekday_start), &parsed.weekday) ||
            !clock_time(space + 1, &parsed.hour, &parsed.minute)) {
            return false;
        }
    } else if (!strncmp(text, once, sizeof(once) - 1)) {
        parsed.kind = RA_SCHEDULED_EVENT_ONCE;
        if (!date_time(text + sizeof(once) - 1, &parsed)) {
            return false;
        }
    } else {
        return false;
    }
    *result = parsed;
    return true;
}

/** @brief Encode one valid civil date and time without relying on platform epoch width.
 * @param local Valid local civil clock.
 * @return Monotonically ordered civil calendar-minute key.
 */
static uint64_t civil_minute_key(const struct tm *local) {
    uint64_t year = (uint64_t)(local->tm_year + 1900);
    uint64_t month = (uint64_t)(local->tm_mon + 1);
    uint64_t day = (uint64_t)local->tm_mday;
    return ((((year * 13U + month) * 32U + day) * 24U + (uint64_t)local->tm_hour) * 60U +
            (uint64_t)local->tm_min);
}

bool ra_scheduled_event_due(const struct ra_scheduled_event_time *trigger, const struct tm *local,
                            uint64_t *occurrence) {
    if (!trigger || !local || local->tm_hour < 0 || local->tm_hour >= 24 || local->tm_min < 0 ||
        local->tm_min >= 60 || local->tm_mon < 0 || local->tm_mon >= 12 || local->tm_mday <= 0 ||
        (unsigned int)local->tm_mday >
            month_days((unsigned int)(local->tm_year + 1900), (unsigned int)(local->tm_mon + 1))) {
        return false;
    }
    if ((unsigned int)local->tm_hour != trigger->hour ||
        (unsigned int)local->tm_min != trigger->minute) {
        return false;
    }
    switch (trigger->kind) {
    case RA_SCHEDULED_EVENT_DAILY:
        break;
    case RA_SCHEDULED_EVENT_WEEKLY:
        if (local->tm_wday < 0 || local->tm_wday > 6 ||
            (unsigned int)local->tm_wday != trigger->weekday) {
            return false;
        }
        break;
    case RA_SCHEDULED_EVENT_ONCE:
        if ((unsigned int)(local->tm_year + 1900) != trigger->year ||
            (unsigned int)(local->tm_mon + 1) != trigger->month ||
            (unsigned int)local->tm_mday != trigger->day) {
            return false;
        }
        break;
    default:
        return false;
    }
    if (occurrence) {
        *occurrence = civil_minute_key(local);
    }
    return true;
}
