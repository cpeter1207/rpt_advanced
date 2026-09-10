/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Streaming Morse scheduled-media generation at the negotiated PCM rate.
 */
#ifndef RPT_ADVANCED_MORSE_H
#define RPT_ADVANCED_MORSE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/** @brief One playback instance; text remains borrowed until playback ends. */
struct ra_morse {
    const char *text;    /**< Next input character. */
    const char *pattern; /**< Remaining dots and dashes for the current character. */
    uint64_t remaining;  /**< Samples remaining in the current tone or gap. */
    uint64_t fraction;   /**< Fractional sample carry, preventing cumulative timing drift. */
    unsigned int rate;   /**< Output samples per second. */
    unsigned int speed;  /**< PARIS words per minute. */
    unsigned int gap;    /**< Pending gap in dot units. */
    double phase;        /**< Oscillator phase in cycles. */
    double step;         /**< Oscillator cycles per sample. */
    int16_t amplitude;   /**< Configured non-clipping tone amplitude. */
    bool tone;           /**< Current segment contains keyed audio. */
};

/** @brief Validate text and initialize playback without allocation.
 * @param state Receives initialized state only on success.
 * @param text Borrowed ASCII text, accepting letters, digits, and common punctuation.
 * @param rate PCM sample rate, greater than zero.
 * @param speed PARIS words per minute, from 1 through 100.
 * @param frequency Positive tone frequency strictly below Nyquist.
 * @param level_db Tone level from -60 through 0 dB relative to full-scale PCM.
 * @return True on success; false for unsupported characters or invalid parameters.
 */
bool ra_morse_init(struct ra_morse *state, const char *text, unsigned int rate, unsigned int speed,
                   unsigned int frequency, int level_db);

/** @brief Render up to capacity samples, preserving timing across arbitrary block sizes.
 * @param state Initialized playback state.
 * @param output Signed 16-bit mono output; may be null when capacity is zero.
 * @param capacity Available sample slots.
 * @return Samples generated; zero indicates completion or zero capacity.
 * Tone amplitude is selected during initialization. Gaps are zero; no trailing gap is appended.
 */
size_t ra_morse_render(struct ra_morse *state, int16_t *output, size_t capacity);
#endif
