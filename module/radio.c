/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Translate carrier events and exchange hardware-paced linear PCM frames.
 */
#include <asterisk.h>

#include "radio.h"
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/frame.h>
#include <asterisk/translate.h>

int ra_radio_exchange(struct ra_radio *state, struct ast_channel *channel) {
    struct ast_frame *frame = ast_read(channel);
    if (!frame) {
        return -1;
    }
    int result = 0;
    bool audio = frame->frametype == AST_FRAME_VOICE;
    if (audio && state->decode) {
        if (ast_format_cmp(frame->subclass.format, state->codec) != AST_FORMAT_CMP_EQUAL) {
            ast_frfree(frame);
            return -1;
        }
        frame = ast_translate(state->decode, frame, 1);
        if (!frame) {
            return 0;
        }
    }
    bool carrier = frame->frametype == AST_FRAME_CONTROL &&
                   (frame->subclass.integer == AST_CONTROL_RADIO_KEY ||
                    frame->subclass.integer == AST_CONTROL_RADIO_UNKEY);
    if (audio && (!frame->data.ptr || frame->samples <= 0 || frame->datalen <= 0 ||
                  (size_t)frame->datalen != (size_t)frame->samples * sizeof(int16_t) ||
                  ast_format_cmp(frame->subclass.format, state->linear) != AST_FORMAT_CMP_EQUAL)) {
        result = -1;
    } else if (audio || carrier) {
        if (carrier) {
            state->receiving = frame->subclass.integer == AST_CONTROL_RADIO_KEY;
        }
        bool keyed = state->render(state->context, state->receiving, audio ? frame->data.ptr : NULL,
                                   audio ? frame->samples : 0);
        if (keyed != state->keyed) {
            result = ast_indicate(channel, keyed ? AST_CONTROL_RADIO_KEY : AST_CONTROL_RADIO_UNKEY);
            if (!result) {
                state->keyed = keyed;
            }
        }
        if (!result && audio) {
            if (state->encode) {
                frame = ast_translate(state->encode, frame, 1);
                if (!frame) {
                    return 0;
                }
            }
            result = ast_write(channel, frame);
        }
    }
    ast_frfree(frame);
    return result ? -1 : 0;
}
