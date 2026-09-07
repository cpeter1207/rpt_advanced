/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Detector ownership, rate selection, squelch gating, and event-frame lifetime.
 */
#include "dtmf.h"
#include <assert.h>
#include <asterisk.h>
#include <asterisk/dsp.h>
#include <asterisk/frame.h>
#include <stdlib.h>

struct ast_format {
    unsigned int rate; /**< Samples per second. */
};

/** @brief Borrowed 8 kHz format returned by the fixture cache. */
static struct ast_format pcm8 = {.rate = 8000};
/** @brief Source-rate format used to exercise reduction. */
static struct ast_format pcm48 = {.rate = 48000};
/** @brief Inject detector allocation failure. */
static bool allocation_failure;

/** @brief Provide Asterisk allocation ABI for the isolated detector fixture.
 * @param count Number of elements.
 * @param size Element size.
 * @param file Source file supplied by the allocator macro.
 * @param line Source line supplied by the allocator macro.
 * @param function Calling function supplied by the allocator macro.
 * @return Zeroed allocation.
 */
void *__ast_calloc(size_t count, size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    if (allocation_failure) {
        return NULL;
    }
    return calloc(count, size);
}

/** @brief Provide Asterisk deallocation ABI for the isolated detector fixture.
 * @param pointer Allocation to release.
 * @param file Source file supplied by the allocator macro.
 * @param line Source line supplied by the allocator macro.
 * @param function Calling function supplied by the allocator macro.
 */
void __ast_free(void *pointer, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    free(pointer);
}

/** @brief Return the fixture's only detector-rate format.
 * @param rate Requested rate.
 * @return Borrowed 8 kHz signed-linear format.
 */
struct ast_format *ast_format_cache_get_slin_by_rate(unsigned int rate) {
    assert(rate == 8000);
    return &pcm8;
}

/** @brief Read a fixture format's sample rate.
 * @param format Format object.
 * @return Samples per second.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) { return format->rate; }

/** @brief Opaque DSP fixture. */
static int identity;
/** @brief Allocation or mode-selection failure. */
static unsigned int failure;
/** @brief Selected detector output: null, original, begin, or end. */
static unsigned int output;
/** @brief Count detector releases. */
static unsigned int released;
/** @brief Independent digit-event frame. */
static struct ast_frame event;

/** @brief Verify non-8-kHz detector creation.
 * @param rate Actual local PCM rate.
 * @return Fixture or injected allocation failure.
 */
struct ast_dsp *ast_dsp_new_with_rate(unsigned int rate) {
    assert(rate == 8000);
    return failure == 1 ? NULL : (struct ast_dsp *)&identity;
}

/** @brief Verify only digit detection is enabled.
 * @param detector Fixture.
 * @param features DTMF-only feature selection.
 */
void ast_dsp_set_features(struct ast_dsp *detector, int features) {
    assert(detector == (struct ast_dsp *)&identity && features == DSP_FEATURE_DIGIT_DETECT);
}

/** @brief Verify standard DTMF detection and inject mode rejection.
 * @param detector Fixture.
 * @param mode Standard DTMF mode.
 * @return Injected status.
 */
int ast_dsp_set_digitmode(struct ast_dsp *detector, int mode) {
    assert(detector == (struct ast_dsp *)&identity && mode == DSP_DIGITMODE_DTMF);
    return failure == 2;
}

/** @brief Track detector disposal.
 * @param detector Fixture.
 */
void ast_dsp_free(struct ast_dsp *detector) {
    assert(detector == (struct ast_dsp *)&identity);
    ++released;
}

/** @brief Return selected DSP event and emulate in-place tone muting.
 * @param channel Null prevents audio from being queued back into a hardware channel.
 * @param detector Fixture.
 * @param input Borrowed stack frame with no ownership flags.
 * @return Selected DSP output.
 */
struct ast_frame *ast_dsp_process(struct ast_channel *channel, struct ast_dsp *detector,
                                  struct ast_frame *input) {
    assert(!channel && detector == (struct ast_dsp *)&identity);
    assert(!input->mallocd && input->frametype == AST_FRAME_VOICE &&
           (input->samples == 1 || input->samples == 2));
    assert((size_t)input->datalen == input->samples * sizeof(int16_t));
    ((int16_t *)input->data.ptr)[0] = 0;
    if (!output) {
        return NULL;
    }
    if (output == 1) {
        return input;
    }
    event.frametype = output == 2 ? AST_FRAME_DTMF_BEGIN : AST_FRAME_DTMF_END;
    event.subclass.integer = '5';
    return &event;
}

/** @brief Only independently returned DSP events are released.
 * @param frame Event frame.
 * @param cache Asterisk default cache policy.
 */
void ast_frame_free(struct ast_frame *frame, int cache) { assert(frame == &event && cache == 1); }

/** @brief Exercise failures and every returned-frame ownership case.
 * @return Zero after assertions.
 */
int main(void) {
    allocation_failure = true;
    assert(!ra_dtmf_open(48000));
    allocation_failure = false;
    failure = 1;
    assert(!ra_dtmf_open(48000));
    failure = 2;
    assert(!ra_dtmf_open(48000) && released == 1);
    failure = 0;
    struct ra_dtmf_detector *detector = ra_dtmf_open(48000);
    int16_t audio[2] = {100, 200};
    assert(!ra_dtmf_process(detector, NULL, true, NULL, 0));
    for (output = 0; output <= 3; ++output) {
        assert(ra_dtmf_process(detector, NULL, true, audio, 2) == (output == 3 ? '5' : 0));
    }
    output = 0;
    int16_t single = 100;
    assert(!ra_dtmf_process(detector, &pcm48, true, &single, 1));
    int16_t pair[2] = {100, 200};
    assert(!ra_dtmf_process(detector, &pcm48, true, pair, 2));
    struct ast_format zero_rate = {.rate = 0};
    assert(!ra_dtmf_process(detector, &zero_rate, true, &single, 1));
    output = 3;
    assert(ra_dtmf_process(detector, NULL, false, audio, 2) == '5' && !audio[1]);
    ra_dtmf_close(detector);
    ra_dtmf_close(NULL);
    assert(released == 2);
    return 0;
}
