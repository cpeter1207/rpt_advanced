/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Strict, bounded parsing and PCM rendering for configured tone sequences.
 */
#include "tone_sequence.h"
#include <ctype.h>
#include <limits.h>
#include <math.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

/** @brief Maximum independent segments accepted from one configuration value. */
#define RA_TONE_SEQUENCE_MAX_SEGMENTS 256U
/** @brief Longest individual segment accepted from one configuration value. */
#define RA_TONE_SEQUENCE_MAX_DURATION_MS 60000U
/** @brief Maximum allocated PCM samples for one prepared sequence. */
#define RA_TONE_SEQUENCE_MAX_SAMPLES 5760000U
/** @brief Full turn in radians. */
#define RA_TONE_SEQUENCE_TAU 6.28318530717958647692

_Static_assert(RA_TONE_SEQUENCE_MAX_SAMPLES <= SIZE_MAX / sizeof(int16_t),
               "bounded tone PCM allocation must fit size_t");

/** @brief One parsed segment before its bounded PCM allocation is made. */
struct tone_segment {
    double first_hz;      /**< Primary sine frequency, or zero for silence. */
    double second_hz;     /**< Optional secondary sine frequency. */
    uint64_t duration_ms; /**< Requested duration before sample-rate conversion. */
    size_t samples;       /**< Exact rendered samples after fractional carry. */
    int level_db;         /**< Per-segment total peak level. */
};

/** @brief Advance a parser cursor over ASCII whitespace.
 * @param cursor Mutable parser position.
 */
static void skip_space(const char **cursor) {
    while (isspace((unsigned char)**cursor)) {
        ++*cursor;
    }
}

/** @brief Consume an ASCII letter suffix without locale-dependent matching.
 * @param cursor Mutable parser position.
 * @param suffix Lowercase suffix to consume.
 * @return True when the suffix was present, false without changing @p cursor.
 */
static bool consume_suffix(const char **cursor, const char *suffix) {
    const char *position = *cursor;
    for (; *suffix; ++suffix, ++position) {
        if (tolower((unsigned char)*position) != *suffix) {
            return false;
        }
    }
    *cursor = position;
    return true;
}

/** @brief Consume an exact ASCII word without treating a prefix as a word.
 * @param cursor Mutable parser position.
 * @param word Lowercase word to consume.
 * @return True when the complete word was present, false without changing @p cursor.
 */
static bool consume_word(const char **cursor, const char *word) {
    const char *position = *cursor;
    if (!consume_suffix(&position, word) || isalpha((unsigned char)*position)) {
        return false;
    }
    *cursor = position;
    return true;
}

/** @brief Parse one bounded unsigned integer without accepting signs or decimal points.
 * @param cursor Mutable parser position.
 * @param value Receives the parsed value on success.
 * @return True for one complete unsigned decimal value that fits uint64_t.
 */
static bool read_unsigned(const char **cursor, uint64_t *value) {
    const char *position = *cursor;
    uint64_t result = 0;
    if (!isdigit((unsigned char)*position)) {
        return false;
    }
    do {
        unsigned int digit = (unsigned int)(*position - '0');
        if (result > (UINT64_MAX - digit) / 10) {
            return false;
        }
        result = result * 10 + digit;
        ++position;
    } while (isdigit((unsigned char)*position));
    *cursor = position;
    *value = result;
    return true;
}

/** @brief Parse one finite decimal frequency with at most a small bounded digit count.
 * @param cursor Mutable parser position.
 * @param value Receives the parsed nonnegative frequency on success.
 * @return True for an unsigned decimal frequency that fits a double.
 */
static bool read_frequency(const char **cursor, double *value) {
    const char *position = *cursor;
    double result = 0;
    unsigned int digits = 0;
    bool any = false;
    while (isdigit((unsigned char)*position)) {
        if (++digits > 64) {
            return false;
        }
        result = result * 10.0 + (*position++ - '0');
        any = true;
    }
    if (*position == '.') {
        double place = 0.1;
        ++position;
        while (isdigit((unsigned char)*position)) {
            if (++digits > 64) {
                return false;
            }
            result += (*position++ - '0') * place;
            place *= 0.1;
            any = true;
        }
    }
    if (!any) {
        return false;
    }
    *cursor = position;
    *value = result;
    return true;
}

/** @brief Parse one frequency or the `silence` keyword.
 * @param cursor Mutable parser position.
 * @param frequency Receives zero for silence or a decimal frequency in Hz.
 * @return True when one complete frequency term was parsed.
 */
static bool read_frequency_term(const char **cursor, double *frequency) {
    skip_space(cursor);
    if (consume_word(cursor, "silence")) {
        *frequency = 0;
        skip_space(cursor);
        return true;
    }
    if (!read_frequency(cursor, frequency)) {
        return false;
    }
    skip_space(cursor);
    (void)consume_suffix(cursor, "hz");
    skip_space(cursor);
    return true;
}

/** @brief Parse a duration term with an optional milliseconds suffix.
 * @param cursor Mutable parser position.
 * @param duration_ms Receives a positive bounded duration on success.
 * @return True when a valid duration was parsed.
 */
static bool read_duration(const char **cursor, uint64_t *duration_ms) {
    skip_space(cursor);
    if (!read_unsigned(cursor, duration_ms)) {
        return false;
    }
    skip_space(cursor);
    (void)consume_suffix(cursor, "ms");
    skip_space(cursor);
    return true;
}

/** @brief Parse a dB level with an optional dB or dBFS suffix.
 * @param cursor Mutable parser position.
 * @param level_db Receives a value from -60 through zero on success.
 * @return True when a valid integer level was parsed.
 */
static bool read_level(const char **cursor, int *level_db) {
    skip_space(cursor);
    bool negative = **cursor == '-';
    if (negative) {
        ++*cursor;
    }
    uint64_t magnitude;
    if (!read_unsigned(cursor, &magnitude)) {
        return false;
    }
    if (magnitude > 60) {
        return false;
    }
    if (!negative && magnitude != 0) {
        return false;
    }
    skip_space(cursor);
    if (consume_suffix(cursor, "db")) {
        /* `dBFS` is the familiar explicit spelling; plain `dB` remains concise. */
        (void)consume_suffix(cursor, "fs");
    }
    skip_space(cursor);
    *level_db = negative ? -(int)magnitude : 0;
    return true;
}

/** @brief Check that one non-silent frequency is strictly below the selected Nyquist limit.
 * @param frequency Parsed positive frequency in Hz.
 * @param rate Output sample rate.
 * @return True when the frequency can be rendered without aliasing.
 */
static bool frequency_valid(double frequency, unsigned int rate) { return frequency < rate / 2.0; }

/** @brief Parse one complete segment while leaving the cursor at its comma or terminator.
 * @param cursor Mutable parser position.
 * @param rate Output sample rate.
 * @param default_level_db Caller-selected level for segments without an explicit level.
 * @param segment Receives the validated segment.
 * @return Null on success or a stable diagnostic.
 */
static const char *parse_segment(const char **cursor, unsigned int rate, int default_level_db,
                                 struct tone_segment *segment) {
    struct tone_segment parsed = {.level_db = default_level_db};
    if (!read_frequency_term(cursor, &parsed.first_hz)) {
        return "invalid tone sequence";
    }
    if (**cursor == '+') {
        ++*cursor;
        if (!read_frequency_term(cursor, &parsed.second_hz)) {
            return "invalid tone sequence";
        }
        if (!parsed.first_hz || !parsed.second_hz) {
            return "invalid tone sequence";
        }
    }
    bool compact_level = false;
    if (**cursor == '@') {
        ++*cursor;
        compact_level = true;
        if (!read_level(cursor, &parsed.level_db)) {
            return "invalid tone sequence";
        }
    }
    if (**cursor != '/') {
        return "invalid tone sequence";
    }
    ++*cursor;
    if (!read_duration(cursor, &parsed.duration_ms)) {
        return "invalid tone sequence";
    }
    if (**cursor == '/') {
        if (compact_level) {
            return "invalid tone sequence";
        }
        ++*cursor;
        if (!read_level(cursor, &parsed.level_db)) {
            return "invalid tone sequence";
        }
    }
    if (**cursor && **cursor != ',') {
        return "invalid tone sequence";
    }
    if (!parsed.duration_ms) {
        return "duration out of range";
    }
    if (parsed.first_hz && !frequency_valid(parsed.first_hz, rate)) {
        return "frequency exceeds Nyquist";
    }
    if (parsed.second_hz && !frequency_valid(parsed.second_hz, rate)) {
        return "frequency exceeds Nyquist";
    }
    *segment = parsed;
    return NULL;
}

/** @brief Calculate bounded samples for one duration while retaining fractional milliseconds.
 * @param segment Parsed segment whose sample count is updated on success.
 * @param rate Output sample rate.
 * @param fractional Remainder in thousandths of a sample, updated on success.
 * @param total Existing bounded PCM samples, updated on success.
 * @return Null on success or a stable size diagnostic.
 */
static const char *count_samples(struct tone_segment *segment, unsigned int rate,
                                 uint64_t *fractional, size_t *total) {
    if (segment->duration_ms > (UINT64_MAX - *fractional) / rate) {
        return "sample count overflow";
    }
    if (segment->duration_ms > RA_TONE_SEQUENCE_MAX_DURATION_MS) {
        return "duration out of range";
    }
    uint64_t numerator = segment->duration_ms * rate + *fractional;
    uint64_t samples = numerator / 1000;
    if (samples > RA_TONE_SEQUENCE_MAX_SAMPLES) {
        return "tone sequence too long";
    }
    if (*total > RA_TONE_SEQUENCE_MAX_SAMPLES - samples) {
        return "tone sequence too long";
    }
    segment->samples = (size_t)samples;
    *fractional = numerator % 1000;
    *total += segment->samples;
    return NULL;
}

/** @brief Saturate one mathematically mixed sample to signed 16-bit PCM.
 * @param mixed Floating-point mixed amplitude.
 * @return Nearest signed PCM sample without wrapping.
 */
static int16_t saturate(double mixed) {
    return (int16_t)lround(fmax(INT16_MIN, fmin(INT16_MAX, mixed)));
}

/** @brief Render all prevalidated segments into an owned PCM buffer.
 * @param segments Parsed segment array.
 * @param count Segment count.
 * @param rate Output sample rate.
 * @param output Allocated PCM destination of exactly the summed segment length.
 */
static void render(const struct tone_segment *segments, size_t count, unsigned int rate,
                   int16_t *output) {
    double first_phase = 0;
    double second_phase = 0;
    size_t offset = 0;
    for (size_t segment = 0; segment < count; ++segment) {
        const struct tone_segment *current = &segments[segment];
        bool dual = current->second_hz != 0;
        double amplitude = 32767.0 * pow(10.0, current->level_db / 20.0) / (dual ? 2.0 : 1.0);
        double first_step = current->first_hz / rate;
        double second_step = current->second_hz / rate;
        for (size_t sample = 0; sample < current->samples; ++sample) {
            double mixed = 0;
            if (current->first_hz) {
                mixed += amplitude * sin(RA_TONE_SEQUENCE_TAU * first_phase);
                first_phase += first_step;
                if (first_phase >= 1) {
                    first_phase -= 1;
                }
            }
            if (current->second_hz) {
                mixed += amplitude * sin(RA_TONE_SEQUENCE_TAU * second_phase);
                second_phase += second_step;
                if (second_phase >= 1) {
                    second_phase -= 1;
                }
            }
            output[offset++] = saturate(mixed);
        }
    }
}

const char *ra_tone_sequence_prepare(const char *text, unsigned int rate, int default_level_db,
                                     int16_t **audio, size_t *samples) {
    if (!audio || !samples) {
        return "invalid output arguments";
    }
    if (!text) {
        return "invalid tone sequence";
    }
    if (rate < 2) {
        return "invalid sample rate";
    }
    if (default_level_db < -60 || default_level_db > 0) {
        return "invalid default level";
    }
    struct tone_segment segments[RA_TONE_SEQUENCE_MAX_SEGMENTS];
    size_t count = 0;
    size_t total = 0;
    uint64_t fractional = 0;
    const char *cursor = text;
    skip_space(&cursor);
    if (!*cursor) {
        return "invalid tone sequence";
    }
    do {
        if (count == RA_TONE_SEQUENCE_MAX_SEGMENTS) {
            return "tone sequence too long";
        }
        const char *error = parse_segment(&cursor, rate, default_level_db, &segments[count]);
        if (error) {
            return error;
        }
        error = count_samples(&segments[count], rate, &fractional, &total);
        if (error) {
            return error;
        }
        ++count;
        if (*cursor == ',') {
            ++cursor;
            skip_space(&cursor);
            if (!*cursor) {
                return "invalid tone sequence";
            }
        }
    } while (*cursor);
    if (!total) {
        *audio = NULL;
        *samples = 0;
        return NULL;
    }
    int16_t *prepared = malloc(total * sizeof(*prepared));
    if (!prepared) {
        return "allocation failed";
    }
    render(segments, count, rate, prepared);
    *audio = prepared;
    *samples = total;
    return NULL;
}

void ra_tone_sequence_free(int16_t *audio) { free(audio); }
