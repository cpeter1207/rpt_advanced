/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Hardware-paced prepared-audio playback with signal-triggered Morse fallback.
 */
#ifndef RPT_ADVANCED_PLAYBACK_H
#define RPT_ADVANCED_PLAYBACK_H
#include "morse.h"
#include "settings.h"

/** @brief One playing identifier; audio and configuration remain owned by its node. */
struct ra_playback {
    const int16_t *audio;  /**< Prepared file or speech PCM at the playback rate. */
    size_t samples;        /**< Number of prepared samples. */
    size_t offset;         /**< Next prepared sample to play. */
    struct ra_morse morse; /**< Ready fallback, not advanced during prepared playback. */
    bool prepared;         /**< Prepared audio is still selected. */
    bool finished;         /**< Completion is terminal, including subsequent receive changes. */
};

/** @brief Initialize one ID, choosing Morse immediately during local or linked reception.
 * @param state Receives playback state on success only.
 * @param audio Borrowed prepared PCM, or null when file/speech preparation failed.
 * @param samples Prepared sample count; zero permits null audio.
 * @param settings Resolved ID settings whose borrowed strings remain valid.
 * @param rate Playback samples per second, matching prepared PCM.
 * @param receiving True selects Morse without playing file/speech audio.
 * @return False if the configured Morse fallback cannot be rendered at this rate.
 */
bool ra_playback_init(struct ra_playback *state, const int16_t *audio, size_t samples,
                      const struct ra_identifier_settings *settings, unsigned int rate,
                      bool receiving);

/** @brief Consume one hardware tick's sample capacity without I/O or a separate clock.
 * @param state Initialized playback state.
 * @param receiving True for local or linked reception; interrupts prepared audio and selects Morse.
 * @param output Sample destination, nullable only for zero capacity.
 * @param capacity Sample slots available in this hardware tick.
 * @return Samples produced; a short final block is not padded here.
 * Half-duplex callers defer this call while receiving, preserving playback position.
 */
size_t ra_playback_render(struct ra_playback *state, bool receiving, int16_t *output,
                          size_t capacity);
#endif
