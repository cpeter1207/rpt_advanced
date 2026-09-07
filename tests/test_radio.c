/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Verify audio-clock preservation, carrier events, and frame ownership.
 */
#include <asterisk.h>

#include "radio.h"
#include <assert.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/frame.h>
#include <asterisk/translate.h>
#include <stdio.h>

/** @brief Opaque format identity for the public-format comparison fixture. */
struct ast_format {
    int identity; /**< Distinguishes negotiated and unexpected formats. */
};
/** @brief Borrowed PCM format at the configured channel rate. */
static struct ast_format linear;
/** @brief Deliberately mismatched media format. */
static struct ast_format other;
/** @brief Next owned frame returned by the channel fixture. */
static struct ast_frame *incoming;
/** @brief Number of released input frames. */
static unsigned int freed;
/** @brief Number of emitted audio frames. */
static unsigned int written;
/** @brief Number of PTT requests. */
static unsigned int indicated;
/** @brief Last requested PTT condition. */
static int last_condition;
/** @brief Injected output failure. */
static int write_result;
/** @brief Injected signaling failure. */
static int indicate_result;
/** @brief Desired controller PTT state. */
static bool request_key;
/** @brief Last receiver state delivered to the controller. */
static bool received;
/** @brief Total audio samples, unaffected by control messages. */
static size_t rendered;
/** @brief Receive translation path fixture. */
static struct ast_trans_pvt decoder;
/** @brief Transmit translation path fixture. */
static struct ast_trans_pvt encoder;
/** @brief Hold decoded output until a complete conversion block is available. */
static bool decode_buffered;
/** @brief Hold encoded output until a complete codec packet is available. */
static bool encode_buffered;
/** @brief Number of codec conversion calls. */
static unsigned int translated;

/** @brief Model consuming translation with delayed or in-place output.
 * @param path Receive or transmit converter.
 * @param frame Owned input frame.
 * @param consume Required ownership-transfer flag.
 * @return Converted frame, or null while buffering.
 */
struct ast_frame *ast_translate(struct ast_trans_pvt *path, struct ast_frame *frame, int consume) {
    assert(frame == incoming && consume == 1);
    ++translated;
    bool decode = path == &decoder;
    if (decode ? decode_buffered : encode_buffered) {
        ++freed;
        return NULL;
    }
    frame->subclass.format = decode ? &linear : &other;
    return frame;
}

/** @brief Supply the next test frame.
 * @param channel Unused opaque channel.
 * @return Frame or simulated hangup.
 */
struct ast_frame *ast_read(struct ast_channel *channel) {
    (void)channel;
    return incoming;
}

/** @brief Verify that each emitted frame is the owned input block.
 * @param channel Unused opaque channel.
 * @param frame Rendered voice frame.
 * @return Injected success or failure.
 */
int ast_write(struct ast_channel *channel, struct ast_frame *frame) {
    (void)channel;
    assert(frame == incoming && frame->frametype == AST_FRAME_VOICE);
    ++written;
    return write_result;
}

/** @brief Record PTT signaling.
 * @param channel Unused opaque channel.
 * @param condition Requested radio state.
 * @return Injected success or failure.
 */
int ast_indicate(struct ast_channel *channel, int condition) {
    (void)channel;
    last_condition = condition;
    ++indicated;
    return indicate_result;
}

/** @brief Check input ownership release, including error paths.
 * @param frame Owned input frame.
 * @param cache Public free API cache flag.
 */
void ast_frame_free(struct ast_frame *frame, int cache) {
    assert(frame == incoming && cache == 1);
    ++freed;
}

/** @brief Compare fixture format identity.
 * @param first Input format.
 * @param second Negotiated format.
 * @return Equal only for the negotiated object.
 */
enum ast_format_cmp_res ast_format_cmp(const struct ast_format *first,
                                       const struct ast_format *second) {
    return first == second ? AST_FORMAT_CMP_EQUAL : AST_FORMAT_CMP_NOT_EQUAL;
}

/** @brief Simple controller fixture whose audio advances only on samples.
 * @param context Expected state identity.
 * @param receiving Qualified receiver state.
 * @param audio Input/output samples, null for control events.
 * @param samples Hardware-paced sample count.
 * @return Requested PTT state.
 */
static bool render(void *context, bool receiving, int16_t *audio, size_t samples) {
    assert(context == &rendered);
    received = receiving;
    rendered += samples;
    for (size_t i = 0; i < samples; ++i) {
        audio[i] += 10;
    }
    return request_key;
}

/** @brief Cover valid and malformed voice, idle/control frames, PTT, and failures.
 * @return Zero after all assertions.
 */
int main(void) {
    struct ra_radio radio = {.linear = &linear, .render = render, .context = &rendered};
    int16_t pcm[960] = {0};
    struct ast_frame voice = {.frametype = AST_FRAME_VOICE,
                              .subclass.format = &linear,
                              .samples = 960,
                              .datalen = sizeof(pcm),
                              .data.ptr = pcm};
    struct ast_frame frame = voice;
    assert(ra_radio_exchange(&radio, NULL) == -1 && freed == 0);
    incoming = &frame;
    assert(ra_radio_exchange(&radio, NULL) == 0);
    assert(rendered == 960 && written == 1 && freed == 1 && pcm[959] == 10);
    frame.frametype = AST_FRAME_CONTROL;
    frame.subclass.integer = AST_CONTROL_RADIO_KEY;
    request_key = true;
    assert(ra_radio_exchange(&radio, NULL) == 0 && received && radio.keyed);
    assert(rendered == 960 && written == 1 && indicated == 1);
    assert(last_condition == AST_CONTROL_RADIO_KEY);
    frame.subclass.integer = AST_CONTROL_RADIO_UNKEY;
    request_key = false;
    assert(ra_radio_exchange(&radio, NULL) == 0 && !received && !radio.keyed);
    assert(rendered == 960 && last_condition == AST_CONTROL_RADIO_UNKEY);
    frame.subclass.integer = AST_CONTROL_ANSWER;
    assert(ra_radio_exchange(&radio, NULL) == 0);
    frame.frametype = AST_FRAME_NULL;
    assert(ra_radio_exchange(&radio, NULL) == 0);
    assert(rendered == 960 && written == 1 && indicated == 2);
    frame = voice;
    request_key = true;
    indicate_result = -1;
    assert(ra_radio_exchange(&radio, NULL) == -1 && !radio.keyed && written == 1);
    indicate_result = 0;
    write_result = -1;
    assert(ra_radio_exchange(&radio, NULL) == -1 && radio.keyed && written == 2);
    write_result = 0;
    assert(ra_radio_exchange(&radio, NULL) == 0 && written == 3);
    /* Invalid blocks never advance the controller or reach the transmitter. */
    size_t before = rendered;
    frame.data.ptr = NULL;
    assert(ra_radio_exchange(&radio, NULL) == -1);
    frame = voice;
    frame.samples = 0;
    assert(ra_radio_exchange(&radio, NULL) == -1);
    frame = voice;
    frame.datalen = 0;
    assert(ra_radio_exchange(&radio, NULL) == -1);
    frame = voice;
    --frame.datalen;
    assert(ra_radio_exchange(&radio, NULL) == -1);
    frame = voice;
    frame.subclass.format = &other;
    assert(ra_radio_exchange(&radio, NULL) == -1);
    assert(rendered == before && written == 3 && freed == 13);
    radio.codec = &other;
    radio.decode = &decoder;
    radio.encode = &encoder;
    frame = voice;
    assert(ra_radio_exchange(&radio, NULL) == -1 && translated == 0);
    frame.subclass.format = &other;
    decode_buffered = true;
    assert(ra_radio_exchange(&radio, NULL) == 0 && rendered == before && written == 3);
    decode_buffered = false;
    encode_buffered = true;
    assert(ra_radio_exchange(&radio, NULL) == 0 && rendered == before + 960 && written == 3);
    encode_buffered = false;
    frame.subclass.format = &other;
    assert(ra_radio_exchange(&radio, NULL) == 0 && written == 4);
    assert(frame.subclass.format == &other && freed == 17 && translated == 5);
    frame.frametype = AST_FRAME_CONTROL;
    frame.subclass.integer = AST_CONTROL_RADIO_UNKEY;
    assert(ra_radio_exchange(&radio, NULL) == 0 && translated == 5);
    puts("hardware-paced Asterisk frame exchange tests passed");
    return 0;
}
