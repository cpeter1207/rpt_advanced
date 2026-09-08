/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Local-time speech and Morse announcement formatting tests.
 */
#include "time_announcement.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

/** @brief Verify every greeting transition and both supported clock formats. */
static void formats(void) {
    char speech[64];
    char morse[32];
    struct tm midnight = {.tm_hour = 0, .tm_min = 5};
    struct tm late_morning = {.tm_hour = 11, .tm_min = 59};
    struct tm noon = {.tm_hour = 12, .tm_min = 0};
    struct tm one_afternoon = {.tm_hour = 13, .tm_min = 1};
    struct tm evening = {.tm_hour = 17, .tm_min = 9};
    assert(
        ra_time_announcement_format(&midnight, 12, speech, sizeof(speech), morse, sizeof(morse)));
    assert(!strcmp(speech, "Good Morning. The time is 12:05 AM."));
    assert(!strcmp(morse, "12:05 AM"));
    assert(ra_time_announcement_format(&late_morning, 12, speech, sizeof(speech), morse,
                                       sizeof(morse)));
    assert(!strcmp(speech, "Good Morning. The time is 11:59 AM."));
    assert(!strcmp(morse, "11:59 AM"));
    assert(ra_time_announcement_format(&noon, 12, speech, sizeof(speech), morse, sizeof(morse)));
    assert(!strcmp(speech, "Good Afternoon. The time is 12:00 PM."));
    assert(!strcmp(morse, "12:00 PM"));
    assert(ra_time_announcement_format(&one_afternoon, 12, speech, sizeof(speech), morse,
                                       sizeof(morse)));
    assert(!strcmp(speech, "Good Afternoon. The time is 1:01 PM."));
    assert(!strcmp(morse, "1:01 PM"));
    assert(ra_time_announcement_format(&evening, 24, speech, sizeof(speech), morse, sizeof(morse)));
    assert(!strcmp(speech, "Good Evening. The time is 17:09."));
    assert(!strcmp(morse, "17:09"));
}

/** @brief Reject invalid input and destinations that cannot hold complete output. */
static void invalid(void) {
    struct tm valid = {.tm_hour = 1, .tm_min = 2};
    struct tm invalid_hour = {.tm_hour = 24, .tm_min = 2};
    struct tm negative_hour = {.tm_hour = -1, .tm_min = 2};
    struct tm invalid_minute = {.tm_hour = 1, .tm_min = 60};
    struct tm negative_minute = {.tm_hour = 1, .tm_min = -1};
    char speech[64];
    char morse[32];
    assert(!ra_time_announcement_format(NULL, 12, speech, sizeof(speech), morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, NULL, sizeof(speech), morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, 0, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, sizeof(speech), NULL, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, sizeof(speech), morse, 0));
    assert(!ra_time_announcement_format(&valid, 13, speech, sizeof(speech), morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&invalid_hour, 12, speech, sizeof(speech), morse,
                                        sizeof(morse)));
    assert(!ra_time_announcement_format(&negative_hour, 12, speech, sizeof(speech), morse,
                                        sizeof(morse)));
    assert(!ra_time_announcement_format(&invalid_minute, 24, speech, sizeof(speech), morse,
                                        sizeof(morse)));
    assert(!ra_time_announcement_format(&negative_minute, 24, speech, sizeof(speech), morse,
                                        sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, 1, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, sizeof(speech), morse, 1));
    assert(!ra_time_announcement_format(&valid, 12, speech, 13, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, 27, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, 34, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, sizeof(speech), morse, 4));
    struct tm eleven = {.tm_hour = 11, .tm_min = 2};
    struct tm twenty_three = {.tm_hour = 23, .tm_min = 2};
    assert(!ra_time_announcement_format(&eleven, 12, speech, 27, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, 28, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&valid, 12, speech, 30, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&twenty_three, 24, speech, 28, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&twenty_three, 24, speech, 29, morse, sizeof(morse)));
    assert(!ra_time_announcement_format(&twenty_three, 24, speech, 30, morse, sizeof(morse)));
}

/** @brief Run local-time announcement tests.
 * @return Zero after assertions.
 */
int main(void) {
    formats();
    invalid();
    puts("time announcement tests passed");
    return 0;
}
