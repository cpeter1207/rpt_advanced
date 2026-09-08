/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Format local clock announcements for speech and Morse playback.
 */
#include "time_announcement.h"
#include <string.h>

/** @brief Append fixed ASCII text without leaving a partial announcement.
 * @param output Destination text buffer.
 * @param capacity Total destination capacity.
 * @param length Current text length, updated after a complete append.
 * @param text Null-terminated fixed text to append.
 * @return True when the complete text fits.
 */
static bool append(char *output, size_t capacity, size_t *length, const char *text) {
    size_t count = strlen(text);
    if (count >= capacity - *length) {
        return false;
    }
    memcpy(output + *length, text, count + 1);
    *length += count;
    return true;
}

/** @brief Return the requested day-part greeting for a local hour.
 * @param hour Local hour from zero through 23.
 * @return Fixed greeting text.
 */
static const char *greeting(int hour) {
    if (hour < 12) {
        return "Good Morning";
    }
    if (hour < 17) {
        return "Good Afternoon";
    }
    return "Good Evening";
}

/** @brief Append one zero-padded decimal number between zero and 99.
 * @param output Destination text buffer.
 * @param capacity Total destination capacity.
 * @param length Current text length, updated after the append.
 * @param value Decimal value in the supported range.
 * @return True when the complete number fits.
 */
static bool append_two_digits(char *output, size_t capacity, size_t *length, int value) {
    char digits[] = {(char)('0' + value / 10), (char)('0' + value % 10), '\0'};
    return append(output, capacity, length, digits);
}

/** @brief Append the requested clock time with no prose.
 * @param output Destination text buffer.
 * @param capacity Total destination capacity.
 * @param length Current text length, updated after the append.
 * @param hour Valid local hour.
 * @param minute Valid local minute.
 * @param format Requested 12- or 24-hour format.
 * @return True when the complete time fits.
 */
static bool append_time(char *output, size_t capacity, size_t *length, int hour, int minute,
                        unsigned int format) {
    if (format == 12) {
        int twelve_hour = hour % 12;
        if (!twelve_hour) {
            twelve_hour = 12;
        }
        if (twelve_hour >= 10 && !append_two_digits(output, capacity, length, twelve_hour)) {
            return false;
        }
        if (twelve_hour < 10) {
            char digit[] = {(char)('0' + twelve_hour), '\0'};
            if (!append(output, capacity, length, digit)) {
                return false;
            }
        }
        if (!append(output, capacity, length, ":")) {
            return false;
        }
        if (!append_two_digits(output, capacity, length, minute)) {
            return false;
        }
        return append(output, capacity, length, hour < 12 ? " AM" : " PM");
    }
    if (!append_two_digits(output, capacity, length, hour)) {
        return false;
    }
    if (!append(output, capacity, length, ":")) {
        return false;
    }
    return append_two_digits(output, capacity, length, minute);
}

bool ra_time_announcement_format(const struct tm *local_time, unsigned int format, char *speech,
                                 size_t speech_capacity, char *morse, size_t morse_capacity) {
    if (!local_time || !speech || !speech_capacity || !morse || !morse_capacity ||
        (format != 12 && format != 24)) {
        return false;
    }
    int hour = local_time->tm_hour;
    int minute = local_time->tm_min;
    if (hour < 0 || hour > 23 || minute < 0 || minute > 59) {
        return false;
    }
    size_t speech_length = 0;
    size_t morse_length = 0;
    speech[0] = '\0';
    morse[0] = '\0';
    return append(speech, speech_capacity, &speech_length, greeting(hour)) &&
           append(speech, speech_capacity, &speech_length, ". The time is ") &&
           append_time(speech, speech_capacity, &speech_length, hour, minute, format) &&
           append(speech, speech_capacity, &speech_length, ".") &&
           append_time(morse, morse_capacity, &morse_length, hour, minute, format);
}
