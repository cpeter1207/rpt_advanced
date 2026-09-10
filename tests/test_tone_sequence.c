/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Verify bounded tone-sequence grammar, PCM preparation, and failure atomicity.
 */
#include "tone_sequence.h"
#include <assert.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>

/** @brief Assert that one failed call leaves caller-owned outputs unchanged.
 * @param text Input sequence, or null.
 * @param rate Requested output rate.
 * @param level Default segment level.
 * @param expected Stable diagnostic expected from the call.
 */
static void rejected(const char *text, unsigned int rate, int level, const char *expected) {
    int16_t sentinel = 123;
    int16_t *audio = &sentinel;
    size_t samples = 456;
    assert(!strcmp(ra_tone_sequence_prepare(text, rate, level, &audio, &samples), expected));
    assert(audio == &sentinel && samples == 456);
}

/** @brief Check argument validation and output-preservation rules. */
static void arguments(void) {
    int16_t sentinel;
    size_t samples = 0;
    assert(!strcmp(ra_tone_sequence_prepare("1000/1", 8000, -6, NULL, &samples),
                   "invalid output arguments"));
    int16_t *audio = &sentinel;
    assert(!strcmp(ra_tone_sequence_prepare("1000/1", 8000, -6, &audio, NULL),
                   "invalid output arguments"));
    rejected(NULL, 8000, -6, "invalid tone sequence");
    rejected("1000/1", 0, -6, "invalid sample rate");
    rejected("1000/1", 1, -6, "invalid sample rate");
    rejected("1000/1", 8000, -61, "invalid default level");
    rejected("1000/1", 8000, 1, "invalid default level");
}

/** @brief Check readable grammar, compact grammar, silence, and exact sample counts. */
static void grammar(void) {
    int16_t *audio = NULL;
    size_t samples = 0;
    assert(!ra_tone_sequence_prepare(" 1000 Hz + 1100.5hZ / 2 ms / -6 dBFS, silence / 1ms, "
                                     "1100.5@-12/2 ",
                                     8000, -20, &audio, &samples));
    assert(samples == 40 && audio);
    for (size_t index = 16; index < 24; ++index) {
        assert(!audio[index]);
    }
    ra_tone_sequence_free(audio);
    audio = NULL;
    assert(!ra_tone_sequence_prepare("silence/1ms,silence/1MS", 44100, -6, &audio, &samples));
    assert(samples == 88 && audio);
    for (size_t index = 0; index < samples; ++index) {
        assert(!audio[index]);
    }
    ra_tone_sequence_free(audio);
    audio = (int16_t *)(uintptr_t)1;
    samples = 1;
    assert(!ra_tone_sequence_prepare("silence / 1", 2, -6, &audio, &samples));
    assert(!audio && !samples);
}

/** @brief Check default levels, phase carry, dual-tone headroom, and PCM rail protection. */
static void rendering(void) {
    int16_t *audio = NULL;
    size_t samples = 0;
    assert(!ra_tone_sequence_prepare("1100/1,1100/1", 8000, -20, &audio, &samples));
    assert(samples == 16 && audio[8] > 1800);
    assert(audio[2] > 3000 && audio[2] < 3300);
    ra_tone_sequence_free(audio);
    assert(!ra_tone_sequence_prepare("1100@-6/1", 8000, -20, &audio, &samples));
    assert(samples == 8 && audio[2] > 16000 && audio[2] < 16450);
    ra_tone_sequence_free(audio);
    assert(!ra_tone_sequence_prepare("1000+1000@0/1", 8000, -20, &audio, &samples));
    assert(samples == 8 && audio[6] == -32767);
    ra_tone_sequence_free(audio);
}

/** @brief Exercise malformed syntax, safety bounds, and numeric validation. */
static void invalid(void) {
    rejected("", 8000, -6, "invalid tone sequence");
    rejected("1000", 8000, -6, "invalid tone sequence");
    rejected("1000//1", 8000, -6, "invalid tone sequence");
    rejected("1000+/1", 8000, -6, "invalid tone sequence");
    rejected("silence+1000/1", 8000, -6, "invalid tone sequence");
    rejected("1000+0/1", 8000, -6, "invalid tone sequence");
    rejected("1000+4000/1", 8000, -6, "frequency exceeds Nyquist");
    rejected("1000@/1", 8000, -6, "invalid tone sequence");
    rejected("1000@-6/1/-5", 8000, -6, "invalid tone sequence");
    rejected("1000/1/-", 8000, -6, "invalid tone sequence");
    rejected("1000/1/1", 8000, -6, "invalid tone sequence");
    rejected("1000/1/-61", 8000, -6, "invalid tone sequence");
    rejected("1000.2.3/1", 8000, -6, "invalid tone sequence");
    rejected("silences/1", 8000, -6, "invalid tone sequence");
    rejected("1000/1x", 8000, -6, "invalid tone sequence");
    rejected("1000/1,", 8000, -6, "invalid tone sequence");
    rejected("4000/1", 8000, -6, "frequency exceeds Nyquist");
    rejected("1000/0", 8000, -6, "duration out of range");
    rejected("1000/60001", 8000, -6, "duration out of range");
    rejected("1000/18446744073709551615", 48000, -6, "sample count overflow");
    rejected("1000/18446744073709551616", 48000, -6, "invalid tone sequence");
    rejected("silence/2", UINT_MAX, -6, "tone sequence too long");
    rejected("silence/60000,silence/60000,silence/1", 48000, -6, "tone sequence too long");
    char sequence[2048] = "";
    size_t length = 0;
    for (size_t index = 0; index < 257; ++index) {
        int written =
            snprintf(sequence + length, sizeof(sequence) - length, "%s0/1", index ? "," : "");
        assert(written > 0 && (size_t)written < sizeof(sequence) - length);
        length += (size_t)written;
    }
    rejected(sequence, 1000, -6, "tone sequence too long");
    char long_frequency[80] = "";
    memset(long_frequency, '1', 65);
    strcpy(long_frequency + 65, "/1");
    rejected(long_frequency, 8000, -6, "invalid tone sequence");
    long_frequency[0] = '0';
    long_frequency[1] = '.';
    memset(long_frequency + 2, '1', 65);
    strcpy(long_frequency + 67, "/1");
    rejected(long_frequency, 8000, -6, "invalid tone sequence");
}

/** @brief Force the renderer's checked allocation path without changing production code. */
static void allocation_failure(void) {
    struct rlimit original;
    assert(!getrlimit(RLIMIT_AS, &original));
    struct rlimit exhausted = original;
    exhausted.rlim_cur = 0;
    assert(!setrlimit(RLIMIT_AS, &exhausted));
    int16_t sentinel;
    int16_t *audio = &sentinel;
    size_t samples = 1;
    assert(!strcmp(ra_tone_sequence_prepare("silence/60000", 48000, -6, &audio, &samples),
                   "allocation failed"));
    assert(audio == &sentinel && samples == 1);
    assert(!setrlimit(RLIMIT_AS, &original));
}

/** @brief Execute the complete tone-sequence preparation test set.
 * @return Zero after every assertion succeeds.
 */
int main(void) {
    arguments();
    grammar();
    rendering();
    invalid();
    allocation_failure();
    puts("tone-sequence preparation tests passed");
    return 0;
}
