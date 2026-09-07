/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Keep network readiness and Asterisk conversion independent of radio cadence.
 */
#include "link_peer.h"
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/frame.h>
#include <asterisk/logger.h>
#include <asterisk/translate.h>
#include <stdlib.h>
#include <string.h>

/** @brief Match bounded IAX text with or without its optional terminating NUL.
 * @param frame Borrowed text frame.
 * @param text Protocol token.
 * @return True for an exact token, never an unbounded string read.
 */
static bool text_is(const struct ast_frame *frame, const char *text) {
    size_t length = strlen(text);
    return frame->data.ptr &&
           (frame->datalen == (int)length ||
            (frame->datalen == (int)length + 1 && ((char *)frame->data.ptr)[length] == '\0')) &&
           !memcmp(frame->data.ptr, text, length);
}

/** @brief Consume a checked network frame without retaining Asterisk's buffer.
 * @param peer Reader-owned transport state.
 * @param frame Owned input frame.
 * @return Minus one on malformed PCM or hangup; zero otherwise.
 */
static int accept_frame(struct ra_link_peer *peer, struct ast_frame *frame) {
    int result = 0;
    if (frame->frametype == AST_FRAME_TEXT) {
        if (text_is(frame, "!!DISCONNECT!!")) {
            result = -1;
        }
        bool redundant = text_is(frame, "!NEWKEY!");
        if (redundant || text_is(frame, "!NEWKEY1!")) {
            atomic_store(&peer->voice_keying, !redundant);
            if (redundant && !peer->handshake_replied) {
                peer->handshake_replied = true;
                result = ast_sendtext(peer->channel, "!NEWKEY!") ? -1 : 0;
            }
        }
    } else if (frame->frametype == AST_FRAME_CONTROL) {
        if (frame->subclass.integer == AST_CONTROL_HANGUP) {
            result = -1;
        }
        if (frame->subclass.integer == AST_CONTROL_RADIO_KEY ||
            frame->subclass.integer == AST_CONTROL_RADIO_UNKEY) {
            if (!atomic_load(&peer->voice_keying)) {
                atomic_store(&peer->receiving, frame->subclass.integer == AST_CONTROL_RADIO_KEY);
                atomic_fetch_add(&peer->receive_epoch, 1);
            }
        }
    } else if (frame->frametype == AST_FRAME_VOICE) {
        struct ast_frame *audio = frame;
        const char *source_name = ast_format_get_name(frame->subclass.format);
        int source_samples = frame->samples;
        int source_bytes = frame->datalen;
        /* GCOVR_EXCL_START: compressed-frame translation requires a live Asterisk codec graph. */
        if (ast_format_cmp(frame->subclass.format, peer->linear) != AST_FORMAT_CMP_EQUAL) {
            if (!peer->decode || !peer->decode_format ||
                ast_format_cmp(peer->decode_format, frame->subclass.format) !=
                    AST_FORMAT_CMP_EQUAL) {
                ast_translator_free_path(peer->decode);
                peer->decode = ast_translator_build_path(peer->linear, frame->subclass.format);
                peer->decode_format = frame->subclass.format;
            }
            if (!peer->decode) {
                ast_log(LOG_WARNING, "rpt_advanced: no decoder from %s to %s\n",
                        ast_format_get_name(frame->subclass.format),
                        ast_format_get_name(peer->linear));
                result = -1;
                goto done;
            }
            /* A translator may buffer a codec packet and return no frame yet.
             * Keep the IAX session alive; the next packet completes it. */
            audio = ast_translate(peer->decode, frame, 1);
            if (!audio) {
                ast_log(LOG_WARNING, "rpt_advanced: decoder buffered %s frame\n",
                        ast_format_get_name(frame->subclass.format));
                return 0;
            }
        }
        /* GCOVR_EXCL_STOP */
        /* GCOVR_EXCL_START: translated frames are supplied by live codec modules. */
        /* IAX may send an empty timed voice frame while the peer is idle.
         * It carries no program audio and is not a transport failure. */
        if (!audio->data.ptr || audio->samples <= 0) {
            goto done;
        }
        if ((audio->datalen &&
             (size_t)audio->datalen != (size_t)audio->samples * sizeof(int16_t)) ||
            ast_format_cmp(audio->subclass.format, peer->linear) != AST_FORMAT_CMP_EQUAL) {
            ast_log(
                LOG_WARNING,
                "rpt_advanced: invalid decoded frame source=%s/%d/%d decoded=%s/%d/%d data=%p\n",
                source_name, source_samples, source_bytes,
                ast_format_get_name(audio->subclass.format), audio->samples, audio->datalen,
                audio->data.ptr);
            result = -1;
            goto done;
        }
        /* GCOVR_EXCL_STOP */
        if (atomic_load(&peer->voice_keying)) {
            atomic_fetch_add(&peer->receive_epoch, 1);
        }
        if (atomic_load(&peer->voice_keying) || atomic_load(&peer->receiving)) {
            ra_link_audio_write(&peer->received, audio->data.ptr, audio->samples);
        }
    done:
        ast_frfree(audio);
        return result;
    }
    ast_frfree(frame);
    return result;
}

/** @brief Send at most one queued DTMF digit from the channel-owning reader.
 * @param peer Reader-owned peer state.
 * @return Zero when no digit is pending or one was sent, minus one on IAX failure.
 */
static int send_digit(struct ra_link_peer *peer) {
    unsigned int tail = atomic_load_explicit(&peer->digit_tail, memory_order_relaxed);
    unsigned int head = atomic_load_explicit(&peer->digit_head, memory_order_acquire);
    if (tail == head) {
        return 0;
    }
    char digit = peer->digits[tail % sizeof(peer->digits)];
    if (ast_senddigit(peer->channel, digit, 0)) {
        return -1;
    }
    atomic_store_explicit(&peer->digit_tail, tail + 1, memory_order_release);
    return 0;
}

/** @brief Read until stop or hangup; only the lifecycle owner releases the channel.
 * @param context Stable peer state.
 * @return Null after publishing reader completion.
 */
static void *read_peer(void *context) {
    struct ra_link_peer *peer = context;
    while (!atomic_load(&peer->stop)) {
        if (send_digit(peer)) {
            break;
        }
        bool keyed = atomic_load(&peer->desired_key);
        uint64_t sent = atomic_load(&peer->sent_samples);
        if (keyed != peer->transmitting ||
            sent - peer->heartbeat_samples >= ast_format_get_sample_rate(peer->linear) * 2) {
            if (ast_indicate(peer->channel,
                             keyed ? AST_CONTROL_RADIO_KEY : AST_CONTROL_RADIO_UNKEY)) {
                break;
            }
            peer->transmitting = keyed;
            peer->heartbeat_samples = sent;
        }
        size_t count = ra_link_audio_available(&peer->outgoing);
        size_t block = ast_format_get_sample_rate(peer->linear) / 50;
        if (count > block) {
            count = block;
        }
        if (keyed && count) {
            ra_link_audio_read(&peer->outgoing, peer->send_buffer, count);
            struct ast_frame output = {.frametype = AST_FRAME_VOICE,
                                       .subclass.format = peer->linear,
                                       .data.ptr = peer->send_buffer,
                                       .datalen = count * sizeof(*peer->send_buffer),
                                       .samples = count};
            if (ast_write(peer->channel, &output)) {
                break;
            }
        }
        /* The reader is the sole Asterisk channel owner.  A one-millisecond
         * poll lets it service network input without delaying hardware ticks. */
        int ready = ast_waitfor(peer->channel, 1);
        if (ready < 0) {
            break;
        }
        if (!ready) {
            continue;
        }
        struct ast_frame *frame = ast_read(peer->channel);
        if (!frame) {
            break;
        }
        int result = accept_frame(peer, frame);
        if (result) {
            break;
        }
    }
    atomic_store(&peer->ended, true);
    return NULL;
}

int ra_link_peer_start(struct ra_link_peer *peer, struct ast_channel *channel,
                       struct ast_format *linear) {
    size_t capacity = ast_format_get_sample_rate(linear) / 5;
    if (!capacity) {
        return -1;
    }
    int16_t *storage = ast_calloc(capacity, sizeof(*storage));
    int16_t *outgoing = ast_calloc(capacity, sizeof(*outgoing));
    int16_t *send_buffer = ast_calloc(capacity / 10, sizeof(*send_buffer));
    if (!storage || !outgoing || !send_buffer) {
        ast_free(send_buffer);
        ast_free(outgoing);
        ast_free(storage);
        return -1;
    }
    if (ast_set_read_format(channel, linear) || ast_set_write_format(channel, linear) ||
        ast_sendtext(channel, "!NEWKEY!")) {
        ast_free(send_buffer);
        ast_free(outgoing);
        ast_free(storage);
        return -1;
    }
    peer->channel = channel;
    peer->linear = linear;
    peer->received = (struct ra_link_audio){.storage = storage, .capacity = capacity};
    peer->outgoing = (struct ra_link_audio){.storage = outgoing, .capacity = capacity};
    peer->outgoing_storage = outgoing;
    peer->send_buffer = send_buffer;
    atomic_init(&peer->stop, false);
    atomic_init(&peer->ended, false);
    atomic_init(&peer->receiving, false);
    atomic_init(&peer->voice_keying, false);
    atomic_init(&peer->receive_epoch, 0);
    atomic_init(&peer->desired_key, false);
    atomic_init(&peer->sent_samples, 0);
    atomic_init(&peer->digit_head, 0);
    atomic_init(&peer->digit_tail, 0);
    if (pthread_create(&peer->thread, NULL, read_peer, peer)) {
        ast_free(send_buffer);
        ast_free(outgoing);
        ast_free(storage);
        peer->channel = NULL;
        return -1;
    }
    return 0;
}

bool ra_link_peer_receive(struct ra_link_peer *peer, int16_t *audio, size_t samples) {
    bool voice_keying = atomic_load(&peer->voice_keying);
    uint64_t epoch = atomic_load(&peer->receive_epoch);
    if (epoch != peer->seen_epoch) {
        peer->seen_epoch = epoch;
        peer->receive_age = 0;
    } else if (voice_keying || atomic_load(&peer->receiving)) {
        peer->receive_age += samples;
    }
    bool signaled =
        voice_keying ? peer->receive_age < ast_format_get_sample_rate(peer->linear) / 20
                     : atomic_load(&peer->receiving) &&
                           peer->receive_age < (size_t)ast_format_get_sample_rate(peer->linear) * 4;
    if (!voice_keying && !signaled) {
        atomic_store(&peer->receiving, false);
    }
    bool receiving =
        (signaled || ra_link_audio_available(&peer->received)) && !atomic_load(&peer->ended);
    if (receiving) {
        ra_link_audio_read(&peer->received, audio, samples);
    } else {
        for (size_t i = 0; i < samples; ++i) {
            audio[i] = 0;
        }
    }
    return receiving;
}

int ra_link_peer_send(struct ra_link_peer *peer, bool keyed, const int16_t *audio, size_t samples) {
    if (atomic_load(&peer->ended)) {
        return -1;
    }
    atomic_store(&peer->desired_key, keyed);
    atomic_fetch_add(&peer->sent_samples, samples);
    if (!keyed || !samples) {
        return 0;
    }
    ra_link_audio_write(&peer->outgoing, audio, samples);
    return 0;
}

int ra_link_peer_send_digit(struct ra_link_peer *peer, char digit) {
    if (!strchr("0123456789ABCD*#", digit) || atomic_load(&peer->ended)) {
        return -1;
    }
    unsigned int head = atomic_load_explicit(&peer->digit_head, memory_order_relaxed);
    unsigned int tail = atomic_load_explicit(&peer->digit_tail, memory_order_acquire);
    if (head - tail >= sizeof(peer->digits)) {
        return -1;
    }
    peer->digits[head % sizeof(peer->digits)] = digit;
    atomic_store_explicit(&peer->digit_head, head + 1, memory_order_release);
    return 0;
}

void ra_link_peer_stop(struct ra_link_peer *peer) {
    atomic_store(&peer->stop, true);
    pthread_join(peer->thread, NULL);
    (void)ast_indicate(peer->channel, AST_CONTROL_RADIO_UNKEY);
    ast_hangup(peer->channel);
    ast_free(peer->received.storage);
    ast_free(peer->outgoing_storage);
    ast_free(peer->send_buffer);
    ast_translator_free_path(peer->decode);
    peer->channel = NULL;
}
