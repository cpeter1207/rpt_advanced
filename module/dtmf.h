/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Fixed-state DTMF detection on the negotiated local signed-linear stream.
 */
#ifndef RPT_ADVANCED_DTMF_H
#define RPT_ADVANCED_DTMF_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
struct ra_dtmf_detector;

/** @brief Create a fixed-state DTMF-only detector at the actual PCM rate.
 * @param rate Samples per second.
 * @return Owned detector, or null for an unsupported rate or allocation failure.
 *
 * Construction precomputes the rate-specific coefficients. The hardware callback performed by
 * @ref ra_dtmf_process does not allocate, lock, log, or call into Asterisk.
 */
struct ra_dtmf_detector *ra_dtmf_open(unsigned int rate);

/** @brief Detect one hardware block and mute a completed digit in that same frame.
 * @param detector Owned detector.
 * @param receiving Qualified carrier; otherwise feed silence to finish a pending digit.
 * @param audio Mutable signed-linear PCM block.
 * @param samples Block length bounded by the originating Asterisk frame.
 * @return Completed DTMF character, or zero when no digit completed.
 *
 * The detector retains only fixed recurrence state. It zeroes the complete input block exactly
 * when it returns a completed digit, matching the previous callback-visible muting behavior.
 */
char ra_dtmf_process(struct ra_dtmf_detector *detector, bool receiving, int16_t *audio,
                     size_t samples);

/** @brief Release a successfully created detector outside the hardware callback.
 * @param detector Owned detector or null.
 */
void ra_dtmf_close(struct ra_dtmf_detector *detector);
#endif
