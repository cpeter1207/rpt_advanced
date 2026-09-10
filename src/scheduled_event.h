/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Parse and match the bounded civil-time triggers for scheduled events.
 */
#ifndef RPT_ADVANCED_SCHEDULED_EVENT_H
#define RPT_ADVANCED_SCHEDULED_EVENT_H

#include <stdbool.h>
#include <stdint.h>
#include <time.h>

/** @brief One supported wall-clock trigger kind. */
enum ra_scheduled_event_kind {
    RA_SCHEDULED_EVENT_DAILY,  /**< Every local calendar day at one hour and minute. */
    RA_SCHEDULED_EVENT_WEEKLY, /**< One local weekday at one hour and minute. */
    RA_SCHEDULED_EVENT_ONCE    /**< One exact local calendar date, hour, and minute. */
};

/** @brief Parsed configuration for one zero-time scheduled event. */
struct ra_scheduled_event_time {
    enum ra_scheduled_event_kind kind; /**< Trigger cadence. */
    unsigned int hour;                 /**< Local 24-hour clock hour, from zero through 23. */
    unsigned int minute;               /**< Local clock minute, from zero through 59. */
    unsigned int weekday; /**< Sunday-zero local weekday for a weekly event; otherwise zero. */
    unsigned int year;    /**< Four-digit local year for a one-time event; otherwise zero. */
    unsigned int month;   /**< One-based local month for a one-time event; otherwise zero. */
    unsigned int day;     /**< One-based local day for a one-time event; otherwise zero. */
};

/** @brief Parse one documented zero-time event trigger.
 * @param text Exact `daily HH:MM`, `weekly weekday HH:MM`, or `once YYYY-MM-DD HH:MM` text.
 * @param result Receives a fully parsed trigger on success and is unchanged on failure.
 * @return True when @p text is a supported trigger with a valid local civil date and time.
 *
 * Weekday names are the English names Sunday through Saturday and are matched case-insensitively.
 * The parser deliberately rejects aliases, seconds, time zones, ranges, and cron expressions so
 * the configuration remains unambiguous and small.
 */
bool ra_scheduled_event_parse_at(const char *text, struct ra_scheduled_event_time *result);

/** @brief Match one parsed trigger against a local civil clock and produce its calendar-minute key.
 * @param trigger Valid parsed trigger.
 * @param local Current local civil time.
 * @param occurrence Receives a stable unique local calendar-minute key when non-null.
 * @return True exactly in the local minute selected by @p trigger.
 *
 * The key uses civil date and time rather than a UTC epoch. A daily or weekly event therefore
 * runs once for a repeated daylight-saving local minute rather than twice, which is the least
 * surprising behavior for a configuration expressed in local wall time.
 */
bool ra_scheduled_event_due(const struct ra_scheduled_event_time *trigger, const struct tm *local,
                            uint64_t *occurrence);

#endif
