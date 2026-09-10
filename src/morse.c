/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Morse tone synthesis with PARIS timing and fractional sample carry.
 */
#include "morse.h"
#include <math.h>
#include <string.h>

/** @brief Characters accepted in scheduled Morse text, in pattern-table order. */
static const char alphabet[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/.,?-=+@()'!\":;_$&";
/** @brief International Morse representations corresponding to alphabet. */
static const char *const patterns[] = {
    ".-",     "-...",   "-.-.",   "-..",    ".",      "..-.",   "--.",    "....",    "..",
    ".---",   "-.-",    ".-..",   "--",     "-.",     "---",    ".--.",   "--.-",    ".-.",
    "...",    "-",      "..-",    "...-",   ".--",    "-..-",   "-.--",   "--..",    "-----",
    ".----",  "..---",  "...--",  "....-",  ".....",  "-....",  "--...",  "---..",   "----.",
    "-..-.",  ".-.-.-", "--..--", "..--..", "-....-", "-...-",  ".-.-.",  ".--.-.",  "-.--.",
    "-.--.-", ".----.", "-.-.--", ".-..-.", "---...", "-.-.-.", "..--.-", "...-..-", ".-..."};

/** @brief Recognize ASCII word separators independently of locale.
 * @param character Input character.
 * @return True for a supported whitespace character.
 */
static bool space(char character) {
    return character == ' ' || character == '\t' || character == '\r' || character == '\n';
}

/** @brief Look up an already validated, non-whitespace character.
 * @param character ASCII character.
 * @return Pattern or null for unsupported input.
 */
static const char *pattern(char character) {
    if (character >= 'a' && character <= 'z') {
        character = (char)(character - 'a' + 'A');
    }
    const char *found = strchr(alphabet, character);
    return found ? patterns[found - alphabet] : NULL;
}

bool ra_morse_init(struct ra_morse *state, const char *text, unsigned int rate, unsigned int speed,
                   unsigned int frequency, int level_db) {
    if (!rate || !speed || speed > 100 || !frequency || frequency >= rate / 2.0 || level_db < -60 ||
        level_db > 0) {
        return false;
    }
    for (const char *cursor = text; *cursor; ++cursor) {
        if (!space(*cursor) && !pattern(*cursor)) {
            return false;
        }
    }
    while (space(*text)) {
        ++text;
    }
    *state = (struct ra_morse){.text = text,
                               .pattern = "",
                               .rate = rate,
                               .speed = speed,
                               .amplitude = (int16_t)lround(32767.0 * pow(10.0, level_db / 20.0)),
                               .step = (double)frequency / rate};
    return true;
}

/** @brief Start the next key or gap segment, carrying fractional sample timing.
 * @param state Playback cursor.
 * @return False when no segment remains.
 */
static bool advance(struct ra_morse *state) {
    unsigned int units = state->gap;
    state->tone = units == 0;
    state->gap = 0;
    if (state->tone) {
        if (!*state->pattern) {
            if (!*state->text) {
                return false;
            }
            state->pattern = pattern(*state->text++);
            /* Borrowed text may have changed: stop instead of dereferencing a missing code. */
            if (!state->pattern) {
                state->pattern = "";
                state->text = "";
                return false;
            }
        }
        units = *state->pattern++ == '.' ? 1 : 3;
        if (*state->pattern) {
            state->gap = 1;
        } else {
            bool word = false;
            while (space(*state->text)) {
                word = true;
                ++state->text;
            }
            if (*state->text) {
                state->gap = word ? 7 : 3;
            }
        }
        state->phase = 0;
    }
    /* A PARIS dot lasts 6/(5*WPM) seconds; carry the remainder across segments. */
    uint64_t numerator = (uint64_t)units * state->rate * 6 + state->fraction;
    unsigned int denominator = 5 * state->speed;
    state->remaining = numerator / denominator;
    state->fraction = numerator % denominator;
    return true;
}

size_t ra_morse_render(struct ra_morse *state, int16_t *output, size_t capacity) {
    size_t count = 0;
    while (count < capacity) {
        if (!state->remaining) {
            if (!advance(state)) {
                break;
            }
            continue;
        }
        output[count++] =
            state->tone ? (int16_t)(state->amplitude * sin(6.283185307179586 * state->phase)) : 0;
        state->phase += state->step;
        if (state->phase >= 1) {
            state->phase -= 1;
        }
        --state->remaining;
    }
    return count;
}
