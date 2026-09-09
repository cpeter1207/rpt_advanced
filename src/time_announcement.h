/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Format local clock announcements for speech and Morse playback.
 */
#ifndef RPT_ADVANCED_TIME_ANNOUNCEMENT_H
#define RPT_ADVANCED_TIME_ANNOUNCEMENT_H
#include <stdbool.h>
#include <stddef.h>
#include <time.h>

/** @brief Format one local civil time without reading or changing the system clock.
 * @param local_time Local civil time returned by localtime_r.
 * @param format Requested 12 or 24 hour clock format.
 * @param speech Speech announcement destination.
 * @param speech_capacity Size of @p speech in bytes.
 * @param morse Morse time-only destination.
 * @param morse_capacity Size of @p morse in bytes.
 * @return True when both complete strings fit and @p format is supported.
 */
bool ra_time_announcement_format(const struct tm *local_time, unsigned int format, char *speech,
                                 size_t speech_capacity, char *morse, size_t morse_capacity);
#endif
