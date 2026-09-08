/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Native DTMF timing, qualification, muting, and fixed-allocation tests.
 */
#include "dtmf.h"
#include <assert.h>
#include <math.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/** @brief Reference DTMF analysis-frame size at 8 kHz. */
#define TEST_INTERVAL 102U
/** @brief Sample rate used by the primary detector behavior checks. */
#define TEST_RATE 8000U
/** @brief Maximum frame used to exercise two completed digits in one callback. */
#define TEST_MAX_SAMPLES 1020U
/** @brief Native-rate interval used by the rate-scaling test. */
#define TEST_NATIVE_INTERVAL 612U
/** @brief Audible test-tone amplitude that remains below signed-linear clipping. */
#define TEST_AMPLITUDE 1000.0
/** @brief Circle constant used only to synthesize deterministic fixture audio. */
#define TEST_PI 3.14159265358979323846

/** @brief Inject allocation failure while retaining normal C allocation for all other cases. */
static bool allocation_failure;
/** @brief Standard DTMF character layout used to synthesize every supported key. */
static const char test_positions[] = "123A456B789C*0#D";
/** @brief Standard low-group DTMF frequencies. */
static const double test_rows[] = {697.0, 770.0, 852.0, 941.0};
/** @brief Standard high-group DTMF frequencies. */
static const double test_columns[] = {1209.0, 1336.0, 1477.0, 1633.0};

/** @brief Declare the linker-provided unwrapped allocator.
 * @param count Requested allocation element count.
 * @param size Requested element size.
 * @return System allocation result.
 */
void *__real_calloc(size_t count, size_t size);

/** @brief Intercept detector construction allocation.
 * @param count Requested allocation element count.
 * @param size Requested element size.
 * @return Null for the injected failure, otherwise the system allocation.
 */
void *__wrap_calloc(size_t count, size_t size) {
    return allocation_failure ? NULL : __real_calloc(count, size);
}

/** @brief Map the selected test digit to its low- and high-group frequencies.
 * @param digit Supported test digit.
 * @param row Receives its low-group frequency.
 * @param column Receives its high-group frequency.
 */
static void frequencies(char digit, double *row, double *column) {
    const char *position = strchr(test_positions, digit);
    assert(position);
    size_t index = (size_t)(position - test_positions);
    *row = test_rows[index / 4U];
    *column = test_columns[index % 4U];
}

/** @brief Synthesize one deterministic dual-tone interval.
 * @param audio Destination signed-linear samples.
 * @param samples Number of destination samples.
 * @param rate Sample rate in samples per second.
 * @param first Absolute first-sample offset for continuous phase.
 * @param row Low-group frequency in hertz.
 * @param row_amplitude Low-group peak amplitude.
 * @param column High-group frequency in hertz.
 * @param column_amplitude High-group peak amplitude.
 */
static void dual_tone(int16_t *audio, size_t samples, unsigned int rate, size_t first, double row,
                      double row_amplitude, double column, double column_amplitude) {
    for (size_t index = 0; index < samples; ++index) {
        double phase = (double)(first + index) / rate;
        audio[index] = (int16_t)(row_amplitude * sin(2.0 * TEST_PI * row * phase) +
                                 column_amplitude * sin(2.0 * TEST_PI * column * phase));
    }
}

/** @brief Fill one ordinary non-DTMF audio interval whose muting is observable.
 * @param audio Destination signed-linear samples.
 * @param samples Number of destination samples.
 * @param rate Sample rate in samples per second.
 * @param first Absolute first-sample offset for continuous phase.
 */
static void voice_like(int16_t *audio, size_t samples, unsigned int rate, size_t first) {
    dual_tone(audio, samples, rate, first, 440.0, TEST_AMPLITUDE, 0.0, 0.0);
}

/** @brief Verify every sample in a frame has been muted.
 * @param audio PCM frame to inspect.
 * @param samples Number of samples in @p audio.
 */
static void assert_muted(const int16_t *audio, size_t samples) {
    for (size_t index = 0; index < samples; ++index) {
        assert(!audio[index]);
    }
}

/** @brief Feed enough intervals to qualify the selected DTMF digit.
 * @param detector Native detector under test.
 * @param digit DTMF digit to synthesize.
 * @param audio Reusable interval buffer.
 * @param samples Exact detector interval length.
 * @param rate Detector sample rate.
 * @param first Receives and advances the absolute source-sample offset.
 */
static void begin_digit(struct ra_dtmf_detector *detector, char digit, int16_t *audio,
                        size_t samples, unsigned int rate, size_t *first) {
    double row;
    double column;
    frequencies(digit, &row, &column);
    for (unsigned int interval = 0; interval < 2; ++interval) {
        dual_tone(audio, samples, rate, *first, row, TEST_AMPLITUDE, column, TEST_AMPLITUDE);
        *first += samples;
        assert(!ra_dtmf_process(detector, true, audio, samples));
    }
}

/** @brief Finish a qualified digit and verify same-frame output muting.
 * @param detector Native detector under test.
 * @param expected Previously qualified digit.
 * @param audio Reusable interval buffer.
 * @param samples Exact detector interval length.
 * @param rate Detector sample rate.
 * @param first Receives and advances the absolute source-sample offset.
 */
static void end_digit(struct ra_dtmf_detector *detector, char expected, int16_t *audio,
                      size_t samples, unsigned int rate, size_t *first) {
    for (unsigned int interval = 0; interval < 3; ++interval) {
        voice_like(audio, samples, rate, *first);
        *first += samples;
        char digit = ra_dtmf_process(detector, true, audio, samples);
        assert(digit == (interval == 2 ? expected : 0));
        if (digit) {
            assert_muted(audio, samples);
        }
    }
}

/** @brief Exercise allocation, invalid-rate, null-input, and standard digit handling.
 * @return Zero after all assertions.
 */
int main(void) {
    allocation_failure = true;
    assert(!ra_dtmf_open(TEST_RATE));
    allocation_failure = false;
    assert(!ra_dtmf_open(0));
    assert(!ra_dtmf_open(3999));
    struct ra_dtmf_detector *detector = ra_dtmf_open(TEST_RATE);
    assert(detector);
    int16_t audio[TEST_MAX_SAMPLES] = {0};
    ra_dtmf_set_muting(NULL, false);
    assert(!ra_dtmf_process(NULL, true, audio, 1));
    assert(!ra_dtmf_process(detector, true, NULL, 1));
    assert(!ra_dtmf_process(detector, true, audio, 0));

    size_t first = 0;
    begin_digit(detector, '5', audio, TEST_INTERVAL, TEST_RATE, &first);
    dual_tone(audio, TEST_INTERVAL, TEST_RATE, first, 770.0, TEST_AMPLITUDE, 1336.0,
              TEST_AMPLITUDE);
    first += TEST_INTERVAL;
    assert(!ra_dtmf_process(detector, true, audio, TEST_INTERVAL));
    end_digit(detector, '5', audio, TEST_INTERVAL, TEST_RATE, &first);

    begin_digit(detector, 'D', audio, TEST_INTERVAL, TEST_RATE, &first);
    memset(audio, 1, TEST_INTERVAL * sizeof(*audio));
    assert(!ra_dtmf_process(detector, false, audio, TEST_INTERVAL));
    assert(audio[0]);
    memset(audio, 1, TEST_INTERVAL * sizeof(*audio));
    assert(!ra_dtmf_process(detector, false, audio, TEST_INTERVAL));
    assert(audio[0]);
    memset(audio, 1, TEST_INTERVAL * sizeof(*audio));
    assert(ra_dtmf_process(detector, false, audio, TEST_INTERVAL) == 'D');
    assert_muted(audio, TEST_INTERVAL);
    ra_dtmf_close(detector);

    detector = ra_dtmf_open(TEST_RATE);
    assert(detector);
    ra_dtmf_set_muting(detector, false);
    first = 0;
    begin_digit(detector, '5', audio, TEST_INTERVAL, TEST_RATE, &first);
    for (unsigned int interval = 0; interval < 3; ++interval) {
        voice_like(audio, TEST_INTERVAL, TEST_RATE, first);
        first += TEST_INTERVAL;
        char digit = ra_dtmf_process(detector, true, audio, TEST_INTERVAL);
        assert(digit == (interval == 2 ? '5' : 0));
        if (digit) {
            assert(audio[0]);
        }
    }
    ra_dtmf_close(detector);

    detector = ra_dtmf_open(TEST_RATE);
    assert(detector);
    first = 0;
    for (unsigned int interval = 0; interval < 2; ++interval) {
        dual_tone(audio, TEST_INTERVAL, TEST_RATE, first, 770.0, TEST_AMPLITUDE, 0.0, 0.0);
        first += TEST_INTERVAL;
        assert(!ra_dtmf_process(detector, true, audio, TEST_INTERVAL));
    }
    for (unsigned int interval = 0; interval < 2; ++interval) {
        dual_tone(audio, TEST_INTERVAL, TEST_RATE, first, 770.0, 5000.0, 1336.0, 1500.0);
        first += TEST_INTERVAL;
        assert(!ra_dtmf_process(detector, true, audio, TEST_INTERVAL));
    }
    for (unsigned int interval = 0; interval < 2; ++interval) {
        dual_tone(audio, TEST_INTERVAL, TEST_RATE, first, 770.0, 1000.0, 1336.0, 4000.0);
        first += TEST_INTERVAL;
        assert(!ra_dtmf_process(detector, true, audio, TEST_INTERVAL));
    }
    ra_dtmf_close(detector);

    detector = ra_dtmf_open(TEST_RATE);
    assert(detector);
    first = 0;
    begin_digit(detector, '5', audio, TEST_INTERVAL, TEST_RATE, &first);
    double row;
    double column;
    frequencies('6', &row, &column);
    dual_tone(audio, TEST_INTERVAL, TEST_RATE, first, row, TEST_AMPLITUDE, column, TEST_AMPLITUDE);
    first += TEST_INTERVAL;
    assert(!ra_dtmf_process(detector, true, audio, TEST_INTERVAL));
    dual_tone(audio, TEST_INTERVAL, TEST_RATE, first, row, TEST_AMPLITUDE, column, TEST_AMPLITUDE);
    first += TEST_INTERVAL;
    assert(ra_dtmf_process(detector, true, audio, TEST_INTERVAL) == '5');
    assert_muted(audio, TEST_INTERVAL);
    end_digit(detector, '6', audio, TEST_INTERVAL, TEST_RATE, &first);
    ra_dtmf_close(detector);

    detector = ra_dtmf_open(TEST_RATE);
    assert(detector);
    first = 0;
    begin_digit(detector, '5', audio, TEST_INTERVAL, TEST_RATE, &first);
    for (size_t interval = 0; interval < 8; ++interval) {
        int16_t *block = audio + interval * TEST_INTERVAL;
        if (interval == 3 || interval == 4) {
            dual_tone(block, TEST_INTERVAL, TEST_RATE, first, 770.0, TEST_AMPLITUDE, 1477.0,
                      TEST_AMPLITUDE);
        } else {
            voice_like(block, TEST_INTERVAL, TEST_RATE, first);
        }
        first += TEST_INTERVAL;
    }
    assert(ra_dtmf_process(detector, true, audio, 8 * TEST_INTERVAL) == '5');
    assert_muted(audio, 8 * TEST_INTERVAL);
    ra_dtmf_close(detector);

    detector = ra_dtmf_open(TEST_RATE);
    assert(detector);
    first = 0;
    begin_digit(detector, '5', audio, TEST_INTERVAL, TEST_RATE, &first);
    voice_like(audio, 160, TEST_RATE, first);
    first += 160;
    assert(!ra_dtmf_process(detector, true, audio, 160));
    voice_like(audio, 160, TEST_RATE, first);
    assert(ra_dtmf_process(detector, true, audio, 160) == '5');
    assert_muted(audio, 160);
    ra_dtmf_close(detector);

    detector = ra_dtmf_open(48000);
    assert(detector);
    first = 0;
    begin_digit(detector, 'D', audio, TEST_NATIVE_INTERVAL, 48000, &first);
    end_digit(detector, 'D', audio, TEST_NATIVE_INTERVAL, 48000, &first);
    ra_dtmf_close(detector);

    for (size_t index = 0; test_positions[index]; ++index) {
        detector = ra_dtmf_open(TEST_RATE);
        assert(detector);
        first = 0;
        begin_digit(detector, test_positions[index], audio, TEST_INTERVAL, TEST_RATE, &first);
        end_digit(detector, test_positions[index], audio, TEST_INTERVAL, TEST_RATE, &first);
        ra_dtmf_close(detector);
    }
    ra_dtmf_close(NULL);
    return 0;
}
