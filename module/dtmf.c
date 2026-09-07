/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Preserve hardware cadence while Asterisk returns digit-event frames.
 */
#include "dtmf.h"
#include <asterisk.h>
#include <asterisk/dsp.h>
#include <asterisk/format.h>
#include <asterisk/format_cache.h>
#include <asterisk/frame.h>

/** @brief 8 kHz ASL-compatible detector fed by negotiated-rate PCM. */
struct ra_dtmf_detector {
    struct ast_dsp *dsp;     /**< Internal 8 kHz Asterisk detector. */
    unsigned int input_rate; /**< Negotiated source rate retained for diagnostics. */
};

struct ra_dtmf_detector *ra_dtmf_open(unsigned int rate) {
    struct ra_dtmf_detector *detector = ast_calloc(1, sizeof(*detector));
    if (!detector) {
        return NULL;
    }
    detector->input_rate = rate;
    detector->dsp = ast_dsp_new_with_rate(8000);
    if (!detector->dsp) {
        ast_free(detector);
        return NULL;
    }
    ast_dsp_set_features(detector->dsp, DSP_FEATURE_DIGIT_DETECT);
    if (ast_dsp_set_digitmode(detector->dsp, DSP_DIGITMODE_DTMF)) {
        ast_dsp_free(detector->dsp);
        ast_free(detector);
        return NULL;
    }
    return detector;
}

char ra_dtmf_process(struct ra_dtmf_detector *detector, struct ast_format *linear, bool receiving,
                     int16_t *audio, size_t samples) {
    if (!samples) {
        return 0;
    }
    if (!receiving) {
        for (size_t i = 0; i < samples; ++i) {
            audio[i] = 0;
        }
    }
    unsigned int rate = linear ? ast_format_get_sample_rate(linear) : 8000;
    if (!rate) {
        rate = 8000;
    }
    size_t reduced = (samples * 8000U) / rate;
    if (!reduced) {
        reduced = 1;
    }
    int16_t reduced_audio[reduced];
    for (size_t i = 0; i < reduced; ++i) {
        size_t source = (i * rate) / 8000U;
        reduced_audio[i] = audio[source];
    }
    struct ast_format *detector_format = ast_format_cache_get_slin_by_rate(8000);
    /* Neither the stack frame nor its borrowed data may be freed by the detector. */
    struct ast_frame input = {.frametype = AST_FRAME_VOICE,
                              .subclass.format = detector_format,
                              .data.ptr = reduced_audio,
                              .datalen = reduced * sizeof(*reduced_audio),
                              .samples = reduced};
    struct ast_frame *event = ast_dsp_process(NULL, detector->dsp, &input);
    char digit = event && event->frametype == AST_FRAME_DTMF_END ? event->subclass.integer : 0;
    if (event && event->frametype == AST_FRAME_DTMF_END) {
        for (size_t i = 0; i < samples; ++i) {
            audio[i] = 0;
        }
    }
    if (event && event != &input) {
        ast_frfree(event);
    }
    return digit;
}

void ra_dtmf_close(struct ra_dtmf_detector *detector) {
    if (detector) {
        ast_dsp_free(detector->dsp);
        ast_free(detector);
    }
}
