/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Verify strict zero-time event parsing and civil-time matching.
 */
#include "scheduled_event.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Make a complete valid local civil-time fixture.
 * @param year Gregorian year.
 * @param month One-based month.
 * @param day One-based day of month.
 * @param weekday Sunday-zero weekday.
 * @param hour 24-hour clock hour.
 * @param minute Clock minute.
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

/** @brief Assert that one rejected trigger leaves its output unchanged.
 * @param text Candidate trigger text.
 */
static void assert_rejected(const char *text) {
    struct ra_scheduled_event_time retained = {
        .kind = RA_SCHEDULED_EVENT_WEEKLY, .weekday = 4, .hour = 9, .minute = 30};
    const struct ra_scheduled_event_time expected = retained;
    assert(!ra_scheduled_event_parse_at(text, &retained));
    assert(!memcmp(&retained, &expected, sizeof(retained)));
}

/** @brief Verify every strict grammar rejection and each documented trigger form. */
static void verify_parser(void) {
    struct ra_scheduled_event_time trigger = {0};
    assert(!ra_scheduled_event_parse_at(NULL, &trigger));
    assert(!ra_scheduled_event_parse_at("daily 07:05", NULL));

    assert(ra_scheduled_event_parse_at("daily 07:05", &trigger));
    assert(trigger.kind == RA_SCHEDULED_EVENT_DAILY && trigger.hour == 7 && trigger.minute == 5);
    assert(ra_scheduled_event_parse_at("weekly Wednesday 13:07", &trigger));
    assert(trigger.kind == RA_SCHEDULED_EVENT_WEEKLY && trigger.weekday == 3);
    assert(ra_scheduled_event_parse_at("weekly saturday 23:59", &trigger));
    assert(trigger.kind == RA_SCHEDULED_EVENT_WEEKLY && trigger.weekday == 6);
    assert(ra_scheduled_event_parse_at("once 2028-02-29 23:59", &trigger));
    assert(trigger.kind == RA_SCHEDULED_EVENT_ONCE && trigger.year == 2028 && trigger.month == 2 &&
           trigger.day == 29);
    assert(ra_scheduled_event_parse_at("once 2000-02-29 00:00", &trigger));

    /* Exercise fixed-width clock parsing, including every short-circuit rejection. */
    assert_rejected("daily 7:05");
    assert_rejected("daily 07005");
    assert_rejected("daily x7:05");
    assert_rejected("daily 0x:05");
    assert_rejected("daily 07:x5");
    assert_rejected("daily 07:0x");
    assert_rejected("daily 24:00");
    assert_rejected("daily 00:60");

    /* Exercise weekday length, word-comparison, absent-space, and clock branches. */
    assert_rejected("weekly Wed 13:07");
    assert_rejected("weekly Funday 13:07");
    assert_rejected("weekly Monday");
    assert_rejected("weekly Monday 24:00");

    /* Exercise date separators, every decimal pair, Gregorian dates, and suffix handling. */
    assert_rejected("once 2026/01-01 12:00");
    assert_rejected("once 2026-01/01 12:00");
    assert_rejected("once 2026-01-01T12:00");
    assert_rejected("once x026-01-01 12:00");
    assert_rejected("once 2x26-01-01 12:00");
    assert_rejected("once 20x6-01-01 12:00");
    assert_rejected("once 202x-01-01 12:00");
    assert_rejected("once 2026-x1-01 12:00");
    assert_rejected("once 2026-0x-01 12:00");
    assert_rejected("once 2026-01-x1 12:00");
    assert_rejected("once 2026-01-0x 12:00");
    assert_rejected("once 0000-01-01 12:00");
    assert_rejected("once 2026-00-01 12:00");
    assert_rejected("once 2026-13-01 12:00");
    assert_rejected("once 2026-01-00 12:00");
    assert_rejected("once 2026-01-32 12:00");
    assert_rejected("once 2027-02-29 12:00");
    assert_rejected("once 2100-02-29 12:00");
    assert_rejected("once 2026-01-01 24:00");
    assert_rejected("once 2026-01-01 12:00 UTC");
    assert_rejected("cron * * * * *");
}

/** @brief Verify daily, weekly, and one-time civil-time occurrence matching. */
static void verify_matching(void) {
    struct ra_scheduled_event_time trigger = {0};
    uint64_t occurrence = 0;
    assert(ra_scheduled_event_parse_at("daily 13:07", &trigger));
    struct tm now = local_time(2026, 9, 9, 3, 13, 7);
    assert(ra_scheduled_event_due(&trigger, &now, &occurrence));
    const uint64_t repeated_local_minute = occurrence;
    assert(ra_scheduled_event_due(&trigger, &now, &occurrence));
    assert(occurrence == repeated_local_minute);
    now.tm_hour = 12;
    assert(!ra_scheduled_event_due(&trigger, &now, &occurrence));
    now.tm_hour = 13;
    now.tm_min = 8;
    assert(!ra_scheduled_event_due(&trigger, &now, &occurrence));

    assert(ra_scheduled_event_parse_at("weekly wednesday 13:07", &trigger));
    now = local_time(2026, 9, 9, 3, 13, 7);
    assert(ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_wday = -1;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_wday = 7;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_wday = 4;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));

    assert(ra_scheduled_event_parse_at("once 2026-09-09 13:07", &trigger));
    now = local_time(2026, 9, 9, 3, 13, 7);
    assert(ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_year = 127;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_year = 126;
    now.tm_mon = 7;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_mon = 8;
    now.tm_mday = 10;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));

    /* The public matcher rejects every malformed local clock before matching. */
    now = local_time(2026, 9, 9, 3, 13, 7);
    assert(!ra_scheduled_event_due(NULL, &now, NULL));
    assert(!ra_scheduled_event_due(&trigger, NULL, NULL));
    now.tm_hour = -1;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_hour = 24;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_hour = 13;
    now.tm_min = -1;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_min = 60;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_min = 7;
    now.tm_mon = -1;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_mon = 12;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_mon = 8;
    now.tm_mday = 0;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
    now.tm_mday = 31;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));

    now = local_time(2026, 9, 9, 3, 13, 7);
    trigger.kind = (enum ra_scheduled_event_kind)99;
    assert(!ra_scheduled_event_due(&trigger, &now, NULL));
}

/** @brief Exercise documented parsing and matching behavior.
 * @return Zero after all assertions pass.
 */
int main(void) {
    verify_parser();
    verify_matching();
    puts("scheduled event tests passed");
    return 0;
}
