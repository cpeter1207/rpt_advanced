/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Keep network readiness and Asterisk conversion independent of radio cadence.
 */
#include "link_peer.h"
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/frame.h>
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
 * @param frame Borrowed input frame.
 * @return Minus one on malformed PCM or hangup; zero otherwise.
 */
static int accept_frame(struct ra_link_peer *peer, const struct ast_frame *frame) {
    if (frame->frametype == AST_FRAME_TEXT) {
        if (text_is(frame, "!!DISCONNECT!!")) {
            return -1;
        }
        bool redundant = text_is(frame, "!NEWKEY!");
        if (redundant || text_is(frame, "!NEWKEY1!")) {
            ast_mutex_lock(&peer->lock);
            peer->voice_keying = !redundant;
            ast_mutex_unlock(&peer->lock);
            if (redundant && !peer->handshake_replied) {
                peer->handshake_replied = true;
                return ast_sendtext(peer->channel, "!NEWKEY!") ? -1 : 0;
            }
        }
    } else if (frame->frametype == AST_FRAME_CONTROL) {
        if (frame->subclass.integer == AST_CONTROL_HANGUP) {
            return -1;
        }
        if (frame->subclass.integer == AST_CONTROL_RADIO_KEY ||
            frame->subclass.integer == AST_CONTROL_RADIO_UNKEY) {
            ast_mutex_lock(&peer->lock);
            if (!peer->voice_keying) {
                peer->receiving = frame->subclass.integer == AST_CONTROL_RADIO_KEY;
                peer->receive_age = 0;
            }
            ast_mutex_unlock(&peer->lock);
        }
    } else if (frame->frametype == AST_FRAME_VOICE) {
        if (!frame->data.ptr || frame->samples <= 0 ||
            (size_t)frame->datalen != (size_t)frame->samples * sizeof(int16_t) ||
            ast_format_cmp(frame->subclass.format, peer->linear) != AST_FORMAT_CMP_EQUAL) {
            return -1;
        }
        ast_mutex_lock(&peer->lock);
        if (peer->voice_keying) {
            peer->receiving = true;
            peer->receive_age = 0;
        }
        if (peer->receiving) {
            ra_link_audio_write(&peer->received, frame->data.ptr, frame->samples);
        }
        ast_mutex_unlock(&peer->lock);
    }
    return 0;
}

/** @brief Read until stop or hangup; only the lifecycle owner releases the channel.
 * @param context Stable peer state.
 * @return Null after publishing reader completion.
 */
static void *read_peer(void *context) {
    struct ra_link_peer *peer = context;
    while (!atomic_load(&peer->stop)) {
        int ready = ast_waitfor(peer->channel, 100);
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
        ast_frfree(frame);
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
    if (!storage) {
        return -1;
    }
    if (ast_mutex_init(&peer->lock)) {
        ast_free(storage);
        return -1;
    }
    if (ast_set_read_format(channel, linear) || ast_set_write_format(channel, linear) ||
        ast_sendtext(channel, "!NEWKEY!")) {
        ast_mutex_destroy(&peer->lock);
        ast_free(storage);
        return -1;
    }
    peer->channel = channel;
    peer->linear = linear;
    peer->received = (struct ra_link_audio){.storage = storage, .capacity = capacity};
    atomic_init(&peer->stop, false);
    atomic_init(&peer->ended, false);
    if (pthread_create(&peer->thread, NULL, read_peer, peer)) {
        ast_mutex_destroy(&peer->lock);
        ast_free(storage);
        peer->channel = NULL;
        return -1;
    }
    return 0;
}

bool ra_link_peer_receive(struct ra_link_peer *peer, int16_t *audio, size_t samples) {
    ast_mutex_lock(&peer->lock);
    size_t limit = peer->voice_keying ? ast_format_get_sample_rate(peer->linear) / 20
                                      : (size_t)ast_format_get_sample_rate(peer->linear) * 4;
    /* Explicit key mode refreshes every two seconds; voice mode has a 50 ms tail. */
    if (peer->receiving) {
        peer->receive_age += samples < limit ? samples : limit;
        if (peer->receive_age >= limit) {
            peer->receiving = false;
        }
    }
    bool receiving = (peer->receiving || peer->received.count) && !atomic_load(&peer->ended);
    if (receiving) {
        ra_link_audio_read(&peer->received, audio, samples);
    } else {
        for (size_t i = 0; i < samples; ++i) {
            audio[i] = 0;
        }
    }
    ast_mutex_unlock(&peer->lock);
    return receiving;
}

int ra_link_peer_send(struct ra_link_peer *peer, bool keyed, const int16_t *audio, size_t samples) {
    if (atomic_load(&peer->ended)) {
        return -1;
    }
    peer->heartbeat_samples += samples;
    if (keyed != peer->transmitting ||
        peer->heartbeat_samples >= (size_t)ast_format_get_sample_rate(peer->linear) * 2) {
        if (ast_indicate(peer->channel, keyed ? AST_CONTROL_RADIO_KEY : AST_CONTROL_RADIO_UNKEY)) {
            return -1;
        }
        peer->transmitting = keyed;
        peer->heartbeat_samples = 0;
    }
    if (!keyed || !samples) {
        return 0;
    }
    struct ast_frame frame = {.frametype = AST_FRAME_VOICE,
                              .subclass.format = peer->linear,
                              .data.ptr = (void *)audio,
                              .datalen = samples * sizeof(*audio),
                              .samples = samples};
    return ast_write(peer->channel, &frame) ? -1 : 0;
}

void ra_link_peer_stop(struct ra_link_peer *peer) {
    atomic_store(&peer->stop, true);
    pthread_join(peer->thread, NULL);
    if (peer->transmitting) {
        (void)ast_indicate(peer->channel, AST_CONTROL_RADIO_UNKEY);
    }
    ast_hangup(peer->channel);
    ast_mutex_destroy(&peer->lock);
    ast_free(peer->received.storage);
    peer->channel = NULL;
}
