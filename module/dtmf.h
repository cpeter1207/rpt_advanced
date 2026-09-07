/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Asterisk DTMF detection on the negotiated local signed-linear stream.
 */
#ifndef RPT_ADVANCED_DTMF_H
#define RPT_ADVANCED_DTMF_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
struct ra_dtmf_detector;
struct ast_format;

/** @brief Create a DTMF-only detector at the actual PCM rate.
 * @param rate Samples per second.
 * @return Owned detector, or null if allocation or mode selection fails.
 */
struct ra_dtmf_detector *ra_dtmf_open(unsigned int rate);

/** @brief Detect one hardware block and mute recognized digits in place.
 * @param detector Owned detector.
 * @param linear Cached signed-linear format matching the detector rate.
 * @param receiving Qualified carrier; otherwise supply silence to finish pending digits.
 * @param audio Mutable PCM block.
 * @param samples Block length bounded by the originating Asterisk frame.
 * @return Completed DTMF character, or zero when no digit completed.
 */
char ra_dtmf_process(struct ra_dtmf_detector *detector, struct ast_format *linear, bool receiving,
                     int16_t *audio, size_t samples);

/** @brief Release a successfully created detector.
 * @param detector Owned detector.
 */
void ra_dtmf_close(struct ra_dtmf_detector *detector);
#endif
