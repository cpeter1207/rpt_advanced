/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Prepare bounded PCM tone and silence sequences outside the audio callback.
 */
#ifndef RPT_ADVANCED_TONE_SEQUENCE_H
#define RPT_ADVANCED_TONE_SEQUENCE_H

#include <stddef.h>
#include <stdint.h>

/** @brief Parse and pre-render one configured tone sequence.
 * @param text Comma-separated segments using `frequency[Hz][+frequency[Hz]] / duration[ms]`
 *             with an optional ` / level[dB|dBFS]`; the compact `@level / duration` form also
 *             works.
 *             Whitespace and unit suffix case are ignored. `silence / duration` and
 *             `0 / duration` create a silent segment.
 * @param rate Output sample rate in Hz, at least two.
 * @param default_level_db Non-positive level used when a segment omits its level, from -60 to 0.
 * @param audio Receives owned mono signed-linear PCM on success; release it with
 *              ra_tone_sequence_free(). It is null when all positive durations round below one
 *              sample at an extremely low rate.
 * @param samples Receives the corresponding PCM sample count on success, which can be zero only
 *                with that sub-sample-duration case.
 * @return Null on success. On failure, returns one stable diagnostic: `invalid output arguments`,
 *         `invalid sample rate`, `invalid default level`, `invalid tone sequence`,
 *         `duration out of range`, `frequency exceeds Nyquist`, `sample count overflow`,
 *         `tone sequence too long`, or `allocation failed`. Failure does not alter @p audio or
 *         @p samples.
 *
 * A segment holds one or two sine frequencies. A two-tone segment divides its level equally
 * between the components, avoiding intentional rail clipping; final PCM saturation remains a
 * defensive bound. The renderer preserves oscillator phase across adjacent tone segments.
 */
const char *ra_tone_sequence_prepare(const char *text, unsigned int rate, int default_level_db,
                                     int16_t **audio, size_t *samples);

/** @brief Release PCM allocated by ra_tone_sequence_prepare().
 * @param audio Prepared PCM, or null.
 *
 * This keeps the standard-C allocator boundary inside the renderer when an
 * Asterisk module consumes its output.
 */
void ra_tone_sequence_free(int16_t *audio);

#endif
