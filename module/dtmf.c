/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Fixed-state in-band DTMF recognition for hardware-paced PCM.
 */
#include "dtmf.h"
#include <math.h>
#include <stdlib.h>

/** @brief Number of low-frequency DTMF tones. */
#define RA_DTMF_ROWS 4U
/** @brief Number of high-frequency DTMF tones. */
#define RA_DTMF_COLUMNS 4U
/** @brief Total Goertzel filters used by one detector. */
#define RA_DTMF_TONES (RA_DTMF_ROWS + RA_DTMF_COLUMNS)
/** @brief Reference rate for the established 102-sample analysis interval. */
#define RA_DTMF_REFERENCE_RATE 8000U
/** @brief Analysis samples per reference-rate interval. */
#define RA_DTMF_REFERENCE_SAMPLES 102U
/** @brief Lowest usable sample rate, safely above twice the highest DTMF tone. */
#define RA_DTMF_MINIMUM_RATE 4000U
/** @brief Minimum reference-interval Goertzel power for either DTMF group. */
#define RA_DTMF_MINIMUM_POWER 8.0e7
/** @brief Largest permitted high-group to low-group power ratio. */
#define RA_DTMF_REVERSE_TWIST 2.51
/** @brief Largest permitted low-group to high-group power ratio. */
#define RA_DTMF_NORMAL_TWIST 6.31
/** @brief Required dominance of a selected low-group tone. */
#define RA_DTMF_ROW_DOMINANCE 6.3
/** @brief Required dominance of a selected high-group tone. */
#define RA_DTMF_COLUMN_DOMINANCE 6.3
/** @brief Required selected-tone energy relative to broadband frame energy. */
#define RA_DTMF_TOTAL_ENERGY_RATIO 42.0
/** @brief Consecutive matching analysis intervals required to start a digit. */
#define RA_DTMF_HITS_TO_BEGIN 2U
/** @brief Consecutive nonmatching intervals required to finish a digit. */
#define RA_DTMF_MISSES_TO_END 3U
/** @brief Circle constant used only while constructing the detector. */
#define RA_DTMF_PI 3.14159265358979323846

/** @brief A streaming Goertzel accumulator for one DTMF frequency. */
struct ra_dtmf_filter {
    double coefficient; /**< Precomputed recurrence coefficient for the configured sample rate. */
    double previous;    /**< Most recent recurrence result. */
    double before;      /**< Recurrence result immediately before @ref previous. */
};

/** @brief Native detector state whose callback path uses only fixed storage. */
struct ra_dtmf_detector {
    size_t interval_samples; /**< Samples in one rate-scaled analysis interval. */
    size_t samples;          /**< Samples accumulated in the current interval. */
    double energy;           /**< Broadband energy accumulated in the current interval. */
    double minimum_power;    /**< Rate-scaled minimum power for either selected tone. */
    double total_multiplier; /**< Rate-scaled selected-tone versus broadband requirement. */
    struct ra_dtmf_filter filters[RA_DTMF_TONES]; /**< Low then high DTMF accumulators. */
    char active;         /**< Confirmed digit awaiting its terminating silence or replacement. */
    char last_hit;       /**< Candidate found in the preceding analysis interval. */
    unsigned int hits;   /**< Consecutive candidate intervals. */
    unsigned int misses; /**< Consecutive intervals unlike @ref active. */
};

/** @brief Standard DTMF low-group frequencies. */
static const double ra_dtmf_rows[RA_DTMF_ROWS] = {697.0, 770.0, 852.0, 941.0};
/** @brief Standard DTMF high-group frequencies. */
static const double ra_dtmf_columns[RA_DTMF_COLUMNS] = {1209.0, 1336.0, 1477.0, 1633.0};
/** @brief Character layout indexed by low-group then high-group tone. */
static const char ra_dtmf_positions[] = "123A"
                                        "456B"
                                        "789C"
                                        "*0#D";

/** @brief Reset accumulators after one complete analysis interval.
 * @param detector Detector whose fixed recurrence state is reused.
 */
static void reset_interval(struct ra_dtmf_detector *detector) {
    detector->samples = 0;
    detector->energy = 0.0;
    for (size_t index = 0; index < RA_DTMF_TONES; ++index) {
        detector->filters[index].previous = 0.0;
        detector->filters[index].before = 0.0;
    }
}

/** @brief Calculate one filter's energy at the end of its analysis interval.
 * @param filter Completed Goertzel accumulator.
 * @return Nonnegative power concentrated at the filter's configured frequency.
 */
static double filter_power(const struct ra_dtmf_filter *filter) {
    return filter->previous * filter->previous + filter->before * filter->before -
           filter->coefficient * filter->previous * filter->before;
}

/** @brief Find the strongest filter in one contiguous DTMF frequency group.
 * @param powers Measured power for every low and high group filter.
 * @param first First filter index in the requested group.
 * @param count Number of filters in the requested group.
 * @return Index of the group's strongest filter.
 */
static size_t strongest_filter(const double powers[RA_DTMF_TONES], size_t first, size_t count) {
    size_t strongest = first;
    for (size_t index = first + 1; index < first + count; ++index) {
        if (powers[index] > powers[strongest]) {
            strongest = index;
        }
    }
    return strongest;
}

/** @brief Classify the just-completed analysis interval without retaining its audio.
 * @param detector Detector containing completed fixed-state accumulators.
 * @return Candidate DTMF digit, or zero if the interval is not a valid DTMF pair.
 */
static char classify_interval(const struct ra_dtmf_detector *detector) {
    double powers[RA_DTMF_TONES];
    for (size_t index = 0; index < RA_DTMF_TONES; ++index) {
        powers[index] = filter_power(&detector->filters[index]);
    }
    size_t row = strongest_filter(powers, 0, RA_DTMF_ROWS);
    size_t column = strongest_filter(powers, RA_DTMF_ROWS, RA_DTMF_COLUMNS);
    bool valid = powers[row] >= detector->minimum_power;
    valid &= powers[column] >= detector->minimum_power;
    valid &= powers[column] < powers[row] * RA_DTMF_REVERSE_TWIST;
    valid &= powers[row] < powers[column] * RA_DTMF_NORMAL_TWIST;
    valid &= powers[row] + powers[column] > detector->total_multiplier * detector->energy;
    for (size_t index = 0; index < RA_DTMF_ROWS; ++index) {
        valid &= index == row || powers[index] * RA_DTMF_ROW_DOMINANCE <= powers[row];
    }
    for (size_t index = 0; index < RA_DTMF_COLUMNS; ++index) {
        valid &= RA_DTMF_ROWS + index == column ||
                 powers[RA_DTMF_ROWS + index] * RA_DTMF_COLUMN_DOMINANCE <= powers[column];
    }
    return valid ? ra_dtmf_positions[row * RA_DTMF_COLUMNS + column - RA_DTMF_ROWS] : 0;
}

/** @brief Advance debounce state and return a digit that has just ended.
 * @param detector Detector whose completed interval is ready for classification.
 * @return The completed digit, or zero while no recognized digit has ended.
 *
 * A digit starts after two matching intervals and ends after three unlike intervals. A newly
 * confirmed different digit completes the previous one immediately, preserving rapid dialing.
 */
static char finish_interval(struct ra_dtmf_detector *detector) {
    char completed = 0;
    char hit = classify_interval(detector);
    if (detector->active == hit) {
        detector->misses = 0;
    } else if (detector->active && ++detector->misses == RA_DTMF_MISSES_TO_END) {
        completed = detector->active;
        detector->active = 0;
    }
    if (hit != detector->last_hit) {
        detector->last_hit = hit;
        detector->hits = 0;
    }
    if (hit && hit != detector->active && ++detector->hits == RA_DTMF_HITS_TO_BEGIN) {
        if (detector->active) {
            completed = detector->active;
        }
        detector->active = hit;
        detector->misses = 0;
    }
    reset_interval(detector);
    return completed;
}

/** @brief Add one PCM sample to the preallocated detector state.
 * @param detector Detector receiving the sample.
 * @param sample Signed-linear PCM sample.
 */
static void add_sample(struct ra_dtmf_detector *detector, int16_t sample) {
    detector->energy += (double)sample * sample;
    for (size_t index = 0; index < RA_DTMF_TONES; ++index) {
        struct ra_dtmf_filter *filter = &detector->filters[index];
        double current = filter->coefficient * filter->previous - filter->before + sample;
        filter->before = filter->previous;
        filter->previous = current;
    }
    ++detector->samples;
}

/** @brief Silence one mutable PCM frame in place.
 * @param audio PCM samples to overwrite.
 * @param samples Number of samples in @p audio.
 */
static void mute_frame(int16_t *audio, size_t samples) {
    for (size_t index = 0; index < samples; ++index) {
        audio[index] = 0;
    }
}

struct ra_dtmf_detector *ra_dtmf_open(unsigned int rate) {
    if (rate < RA_DTMF_MINIMUM_RATE) {
        return NULL;
    }
    struct ra_dtmf_detector *detector = calloc(1, sizeof(*detector));
    if (!detector) {
        return NULL;
    }
    detector->interval_samples =
        (size_t)(((uint64_t)rate * RA_DTMF_REFERENCE_SAMPLES + RA_DTMF_REFERENCE_RATE / 2U) /
                 RA_DTMF_REFERENCE_RATE);
    double scale = (double)detector->interval_samples / RA_DTMF_REFERENCE_SAMPLES;
    detector->minimum_power = RA_DTMF_MINIMUM_POWER * scale * scale;
    detector->total_multiplier = RA_DTMF_TOTAL_ENERGY_RATIO * scale;
    for (size_t index = 0; index < RA_DTMF_ROWS; ++index) {
        detector->filters[index].coefficient =
            2.0 * cos(2.0 * RA_DTMF_PI * ra_dtmf_rows[index] / rate);
        detector->filters[RA_DTMF_ROWS + index].coefficient =
            2.0 * cos(2.0 * RA_DTMF_PI * ra_dtmf_columns[index] / rate);
    }
    return detector;
}

char ra_dtmf_process(struct ra_dtmf_detector *detector, bool receiving, int16_t *audio,
                     size_t samples) {
    if (!detector || !audio || !samples) {
        return 0;
    }
    if (!receiving) {
        mute_frame(audio, samples);
    }
    char completed = 0;
    for (size_t index = 0; index < samples; ++index) {
        add_sample(detector, audio[index]);
        if (detector->samples == detector->interval_samples) {
            char digit = finish_interval(detector);
            if (!completed) {
                completed = digit;
            }
        }
    }
    if (completed) {
        mute_frame(audio, samples);
    }
    return completed;
}

void ra_dtmf_close(struct ra_dtmf_detector *detector) { free(detector); }
