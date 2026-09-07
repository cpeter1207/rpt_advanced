/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Prepare immutable identifier PCM outside the real-time audio worker.
 */
#ifndef RPT_ADVANCED_ASSETS_H
#define RPT_ADVANCED_ASSETS_H
#include "settings.h"
#include <stdint.h>

/** @brief Try configured file, then offline speech, leaving Morse as terminal fallback.
 * @param settings Resolved identifier configuration.
 * @param rate Negotiated PCM sample rate.
 * @param audio Receives owned PCM or null; release with ast_free after worker join.
 * @param samples Receives PCM length or zero.
 * Preparation runs during setup, never in the hardware audio callback. Each
 * external preparation process has a thirty-second bound. Temporary files are
 * removed on every path. Failure leaves the configured Morse renderer available.
 */
void ra_identifier_prepare(const struct ra_identifier_settings *settings, unsigned int rate,
                           int16_t **audio, size_t *samples);
#endif
