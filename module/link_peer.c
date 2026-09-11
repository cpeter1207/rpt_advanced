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
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief Require hardware-facing peer state to use native lock-free atomics. */
_Static_assert(ATOMIC_BOOL_LOCK_FREE == 2 && ATOMIC_CHAR_LOCK_FREE == 2 &&
                   ATOMIC_INT_LOCK_FREE == 2 && RA_ATOMIC_UINT_FAST64_LOCK_FREE,
               "peer audio state must not call libatomic");
_Static_assert(RA_LINK_PEER_KEY_TEXT_MAX >= 2U * (RA_LINK_PEER_NAME_MAX - 1U) + 27U,
               "keyed-source reply storage must retain two longest names and a uint64 age");

/** @brief Release the shared library's preallocated receiver playout state.
 * @param peer Peer owning the receiver ring.
 */
static void release_elastic(struct ra_link_peer *peer) { rpcr_destroy(&peer->received); }

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

/** @brief Expose bounded, non-embedded-NUL IAX text payload bytes.
 * @param frame Borrowed text frame.
 * @param text Receives its first payload byte.
 * @param length Receives payload bytes after one optional trailing terminator.
 * @return True when the frame contains a safe nonempty text payload.
 */
static bool text_payload(const struct ast_frame *frame, const char **text, size_t *length) {
    if (!frame->data.ptr || frame->datalen < 0) {
        return false;
    }
    *text = frame->data.ptr;
    *length = (size_t)frame->datalen;
    if (*length && (*text)[*length - 1] == '\0') {
        --*length;
    }
    return *length && !memchr(*text, '\0', *length);
}

/** @brief Advance across one nonempty space-separated bounded text token.
 * @param text Bounded protocol bytes with no embedded NUL.
 * @param length Exact number of readable bytes.
 * @param cursor In/out byte offset.
 * @param value Receives the token start.
 * @param value_length Receives the token length.
 * @return True when one token was found.
 */
static bool text_token(const char *text, size_t length, size_t *cursor, const char **value,
                       size_t *value_length) {
    while (*cursor < length && text[*cursor] == ' ') {
        ++*cursor;
    }
    if (*cursor == length) {
        return false;
    }
    size_t start = *cursor;
    while (*cursor < length && text[*cursor] != ' ') {
        ++*cursor;
    }
    *value = text + start;
    *value_length = *cursor - start;
    return true;
}

/** @brief Compare one bounded protocol token with a conventional C string.
 * @param value Token bytes.
 * @param length Exact token length.
 * @param expected NUL-terminated expected value.
 * @return True for exact equal bytes.
 */
static bool token_is(const char *value, size_t length, const char *expected) {
    size_t expected_length = strlen(expected);
    return length == expected_length && !memcmp(value, expected, length);
}

/** @brief Validate one decimal protocol token without converting it.
 * @param value Token bytes.
 * @param length Exact token length.
 * @return True for one or more ASCII decimal digits.
 */
static bool decimal_token(const char *value, size_t length) {
    for (size_t index = 0; index < length; ++index) {
        if (value[index] < '0' || value[index] > '9') {
            return false;
        }
    }
    return true;
}

/** @brief Convert one bounded decimal protocol token without integer overflow.
 * @param value Token bytes already known not to contain an embedded terminator.
 * @param length Exact token length.
 * @param result Receives the complete unsigned value.
 * @return True only for a complete representable unsigned decimal value.
 */
static bool decimal_value(const char *value, size_t length, uint64_t *result) {
    if (!decimal_token(value, length)) {
        return false;
    }
    uint64_t converted = 0;
    for (size_t index = 0; index < length; ++index) {
        uint64_t digit = (uint64_t)(value[index] - '0');
        if (converted > (UINT64_MAX - digit) / 10U) {
            return false;
        }
        converted = converted * 10U + digit;
    }
    *result = converted;
    return true;
}

/** @brief Check one app_rpt linked-node-list route marker.
 * @param value Untrusted route marker.
 * @return True when the marker is part of the documented app_rpt `L` payload.
 */
static bool topology_mode(unsigned char value) { return strchr("TRCL", value) != NULL; }

/** @brief Check one conservative remote node-name character.
 * @param value Untrusted node-name character.
 * @return True for an ASCII alphanumeric, underscore, or hyphen.
 */
static bool topology_name_character(unsigned char value) {
    return strchr("0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_-", value) !=
           NULL;
}

/** @brief Validate one conservative remote node identity.
 * @param value Untrusted identity bytes.
 * @param length Exact identity length.
 * @return True for one or more supported node-name characters.
 */
static bool topology_name_valid(const char *value, size_t length) {
    if (!length) {
        return false;
    }
    for (size_t index = 0; index < length; ++index) {
        if (!topology_name_character((unsigned char)value[index])) {
            return false;
        }
    }
    return true;
}

/** @brief Copy one validated bounded node identity into fixed control-plane storage.
 * @param output Fixed destination identity storage.
 * @param value Valid token bytes.
 * @param length Exact source length, shorter than @ref RA_LINK_PEER_NAME_MAX.
 */
static void copy_name(char output[RA_LINK_PEER_NAME_MAX], const char *value, size_t length) {
    for (size_t index = 0; index < length; ++index) {
        output[index] = value[index];
    }
    output[length] = '\0';
}

/** @brief Validate an app_rpt comma-separated linked-node-list payload.
 * @param payload Untrusted bytes after `L `.
 * @param length Exact payload byte count.
 * @return True for an empty list or fully valid mode/name tokens.
 */
static bool topology_payload_valid(const char *payload, size_t length) {
    size_t index = 0;
    while (index < length) {
        if (!topology_mode((unsigned char)payload[index++])) {
            return false;
        }
        size_t name_start = index;
        while (index < length && payload[index] != ',') {
            if (!topology_name_character((unsigned char)payload[index])) {
                return false;
            }
            ++index;
        }
        if (index == name_start) {
            return false;
        }
        if (index == length) {
            return true;
        }
        ++index;
        if (index == length) {
            return false;
        }
    }
    return true;
}

/** @brief Decode one conventional DTMF end-frame value without accepting arbitrary bytes.
 * @param value IAX frame subclass integer.
 * @param digit Receives the validated DTMF character.
 * @return True when the frame contains a standard DTMF digit.
 */
static bool dtmf_end_digit(int value, char *digit) {
    if (value < 0 || value > UCHAR_MAX || !strchr("0123456789ABCD*#", value)) {
        return false;
    }
    *digit = (char)value;
    return true;
}

/** @brief Read the peer reader's scheduling clock in milliseconds.
 * @return Monotonic milliseconds; a clock failure yields the epoch.
 *
 * A system monotonic clock is always available on supported Asterisk hosts.  The epoch fallback
 * keeps a transient clock failure out of the audio path and merely postpones this control event.
 */
static uint64_t monotonic_ms(void) {
    struct timespec now = {0};
    (void)clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
}

/** @brief Refresh or cancel the peer-reader interdigit command deadline.
 * @param peer Reader-owned peer state.
 * @param digit Newly delivered conventional DTMF digit.
 *
 * Hash completes a command immediately, so it cancels the deferred terminator.  Other digits
 * retain one three-second deadline that the network reader, not a radio-paced callback, services.
 */
static void schedule_digit_timeout(struct ra_link_peer *peer, char digit) {
    if (digit == '#') {
        peer->inbound_timeout_pending = false;
        return;
    }
    peer->inbound_timeout_deadline_ms = monotonic_ms() + RA_LINK_PEER_DTMF_TIMEOUT_MS;
    peer->inbound_timeout_pending = true;
}

/** @brief Deliver a due peer IAX DTMF command terminator outside audio processing.
 * @param peer Reader-owned peer state.
 */
static void expire_digit_timeout(struct ra_link_peer *peer) {
    if (!peer->inbound_timeout_pending || monotonic_ms() < peer->inbound_timeout_deadline_ms) {
        return;
    }
    peer->inbound_timeout_pending = false;
    if (peer->inbound_digit) {
        peer->inbound_digit(peer->inbound_digit_context, '\0');
    }
}

/** @brief Validate and cache a peer's optional app_rpt `L` topology advertisement.
 * @param peer Reader-owned peer whose control-plane cache changes.
 * @param frame Borrowed untrusted IAX text frame.
 * @return True when a syntactically valid `L` message replaced the cached list.
 *
 * app_rpt normally sends `L ` for an empty list, but peers also use a bare `L` for that same
 * empty advertisement. An invalid message leaves the last known valid topology intact, which
 * prevents malformed IAX text from erasing a usable status report.
 */
static bool cache_topology(struct ra_link_peer *peer, const struct ast_frame *frame) {
    const char *text;
    size_t length;
    if (!text_payload(frame, &text, &length)) {
        return false;
    }
    if (text[0] != 'L') {
        return false;
    }
    const char *payload = text + 1;
    size_t payload_length = 0;
    if (length > 1) {
        if (text[1] != ' ') {
            return false;
        }
        payload = text + 2;
        payload_length = length - 2;
    }
    if (payload_length > RA_LINK_TOPOLOGY_TEXT_MAX ||
        !topology_payload_valid(payload, payload_length)) {
        return false;
    }
    ast_mutex_lock(&peer->topology_lock);
    bool changed =
        peer->topology_length != payload_length || memcmp(peer->topology, payload, payload_length);
    for (size_t index = 0; index < payload_length; ++index) {
        peer->topology[index] = payload[index];
    }
    peer->topology[payload_length] = '\0';
    peer->topology_length = payload_length;
    ast_mutex_unlock(&peer->topology_lock);
    if (changed && peer->topology_generation) {
        atomic_fetch_add_explicit(peer->topology_generation, 1, memory_order_release);
    }
    return true;
}

/** @brief Publish a reader-owned keyed-source result for lock-free hub consumption.
 * @param peer Peer whose current source snapshot changes.
 * @param generation Query epoch paired with this result.
 * @param source First keyed responder bytes for the query.
 * @param source_length Exact responder length.
 *
 * One reader is the only writer. Its odd/even sequence makes a partial identity unavailable to
 * the hardware callback instead of requiring a mutex in the audio path.
 */
static void publish_key_source(struct ra_link_peer *peer, uint_fast64_t generation,
                               const char *source, size_t source_length) {
    atomic_fetch_add_explicit(&peer->key_source_sequence, 1, memory_order_release);
    atomic_store_explicit(&peer->key_source_generation, generation, memory_order_relaxed);
    for (size_t index = 0; index < RA_LINK_PEER_NAME_MAX; ++index) {
        char value = index < source_length ? source[index] : '\0';
        atomic_store_explicit(&peer->key_source[index], value, memory_order_relaxed);
    }
    atomic_fetch_add_explicit(&peer->key_source_sequence, 1, memory_order_release);
}

/** @brief One bounded, validated legacy keyed-source protocol message. */
struct key_message {
    enum ra_link_peer_key_kind kind;         /**< Query or reply class. */
    char destination[RA_LINK_PEER_NAME_MAX]; /**< Wildcard requester target or reply target. */
    char source[RA_LINK_PEER_NAME_MAX];      /**< Query requester or reply reporter. */
    bool keyed;                              /**< Reply carrier state, false for queries. */
    uint64_t age_seconds;                    /**< Reply age, zero for queries. */
};

/** @brief Parse one strictly bounded legacy keyed-source protocol message.
 * @param frame Borrowed untrusted IAX text frame.
 * @param message Receives a fully copied control message.
 * @return True only for a supported complete `K?` broadcast query or `K` reply.
 *
 * The parser deliberately accepts only the broadcast query emitted by this module and app_rpt.
 * It leaves directed query routing out of the radio hot path because source identification always
 * originates with that canonical query form.
 */
static bool parse_key_message(const struct ast_frame *frame, struct key_message *message) {
    const char *text;
    size_t length;
    if (!text_payload(frame, &text, &length)) {
        return false;
    }
    const char *token[5];
    size_t token_length[5];
    size_t cursor = 0;
    for (size_t index = 0; index < sizeof(token) / sizeof(*token); ++index) {
        if (!text_token(text, length, &cursor, &token[index], &token_length[index])) {
            return false;
        }
    }
    const char *extra;
    size_t extra_length;
    if (text_token(text, length, &cursor, &extra, &extra_length)) {
        return false;
    }
    if (token_is(token[0], token_length[0], "K?")) {
        if (!token_is(token[1], token_length[1], "*") || token_length[2] >= RA_LINK_PEER_NAME_MAX ||
            !topology_name_valid(token[2], token_length[2]) ||
            !token_is(token[3], token_length[3], "0") ||
            !token_is(token[4], token_length[4], "0")) {
            return false;
        }
        message->kind = RA_LINK_PEER_KEY_QUERY;
        copy_name(message->destination, token[1], token_length[1]);
        copy_name(message->source, token[2], token_length[2]);
        message->keyed = false;
        message->age_seconds = 0;
        return true;
    }
    if (!token_is(token[0], token_length[0], "K") || token_length[1] >= RA_LINK_PEER_NAME_MAX ||
        token_length[2] >= RA_LINK_PEER_NAME_MAX ||
        !topology_name_valid(token[1], token_length[1]) ||
        !topology_name_valid(token[2], token_length[2]) ||
        (!token_is(token[3], token_length[3], "0") && !token_is(token[3], token_length[3], "1")) ||
        !decimal_value(token[4], token_length[4], &message->age_seconds)) {
        return false;
    }
    message->kind = RA_LINK_PEER_KEY_REPLY;
    copy_name(message->destination, token[1], token_length[1]);
    copy_name(message->source, token[2], token_length[2]);
    message->keyed = token_is(token[3], token_length[3], "1");
    return true;
}

/** @brief Consume one matching legacy app_rpt keyed-source reply.
 * @param peer Reader-owned peer transport.
 * @param message Valid parsed keyed-source protocol message.
 *
 * `K` replies identify a currently keyed responder but do not prove PCM provenance. The direct
 * peer's own reply is deliberately ignored: it is the fallback, not evidence of a downstream
 * source. The first remaining valid keyed reply to each one-second query wins, which supplies a
 * useful best-effort choice during doubles without delaying or suppressing the eventual courtesy
 * tone.
 */
static void cache_key_source(struct ra_link_peer *peer, const struct key_message *message) {
    if (message->kind != RA_LINK_PEER_KEY_REPLY || !message->keyed ||
        strcmp(message->destination, peer->key_query_requester)) {
        return;
    }
    uint_fast64_t generation =
        atomic_load_explicit(&peer->key_query_generation, memory_order_acquire);
    if (!generation || generation != peer->key_query_sent ||
        generation !=
            atomic_load_explicit(&peer->key_query_active_generation, memory_order_acquire)) {
        return;
    }
    if ((peer->key_query_direct[0] && !strcmp(message->source, peer->key_query_direct)) ||
        peer->key_query_responded) {
        return;
    }
    peer->key_query_responded = true;
    publish_key_source(peer, generation, message->source, strlen(message->source));
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
        if (text_is(frame, "!NEWKEY!")) {
            /* NEWKEY is a legacy compatibility probe.  This endpoint starts
             * with NEWKEY1, where ordinary voice frames carry carrier state;
             * replying would make an otherwise modern peer switch away from
             * that mode and discard its unaccompanied voice frames. */
        } else if (text_is(frame, "!NEWKEY1!")) {
            /* The selected transport already uses voice-frame keying. */
        } else if (text_is(frame, "!IAXKEY!")) {
            result = ast_sendtext(peer->channel, "!IAXKEY! 1 1 0 0") ? -1 : 0;
        } else {
            struct key_message message;
            if (parse_key_message(frame, &message)) {
                cache_key_source(peer, &message);
                if (peer->inbound_key) {
                    peer->inbound_key(peer->inbound_key_context, message.kind, message.destination,
                                      message.source, message.keyed, message.age_seconds);
                }
            } else {
                (void)cache_topology(peer, frame);
            }
        }
    } else if (frame->frametype == AST_FRAME_CONTROL) {
        if (frame->subclass.integer == AST_CONTROL_HANGUP) {
            result = -1;
        }
    } else if (frame->frametype == AST_FRAME_DTMF_END) {
        char digit;
        if (peer->inbound_digit && dtmf_end_digit(frame->subclass.integer, &digit)) {
            /* The IAX reader is a control thread. It never invokes audio routing or takes its
             * locks; the runtime callback independently queues this event for command handling.
             * The same reader schedules the documented three-second NUL terminator. */
            peer->inbound_digit(peer->inbound_digit_context, digit);
            schedule_digit_timeout(peer, digit);
        }
    } else if (frame->frametype == AST_FRAME_VOICE) {
        struct ast_frame *audio = frame;
        const char *source_name = ast_format_get_name(frame->subclass.format);
        int source_samples = frame->samples;
        int source_bytes = frame->datalen;
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
        rpcr_write(&peer->received, audio->data.ptr, audio->samples);
        /* Publish the activity edge after its PCM is visible to the radio worker. */
        atomic_fetch_add_explicit(&peer->receive_epoch, 1, memory_order_release);
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

/** @brief Send one requested legacy keyed-source query from the channel-owning reader.
 * @param peer Reader-owned peer transport.
 *
 * Query delivery is advisory. A failed IAX text write must not drop a healthy media link, because
 * ordinary peers may not implement the legacy `K?` control exchange at all.
 */
static void send_key_query(struct ra_link_peer *peer) {
    uint_fast64_t generation =
        atomic_load_explicit(&peer->key_query_generation, memory_order_acquire);
    if (!generation || generation == peer->key_query_attempted) {
        return;
    }
    static const char prefix[] = "K? * ";
    static const char suffix[] = " 0 0";
    char text[RA_LINK_PEER_NAME_MAX + sizeof(prefix) + sizeof(suffix)];
    size_t position = 0;
    for (size_t index = 0; index + 1 < sizeof(prefix); ++index) {
        text[position++] = prefix[index];
    }
    for (size_t index = 0;
         index + 1 < sizeof(peer->key_query_requester) && peer->key_query_requester[index];
         ++index) {
        text[position++] = peer->key_query_requester[index];
    }
    for (size_t index = 0; index + 1 < sizeof(suffix); ++index) {
        text[position++] = suffix[index];
    }
    text[position] = '\0';
    /* The falling edge invalidates an unsent request. Recheck after construction so a queued
     * control iteration cannot start a K? exchange after the radio worker has unkeyed. */
    if (atomic_load_explicit(&peer->key_query_active_generation, memory_order_acquire) !=
        generation) {
        return;
    }
    peer->key_query_attempted = generation;
    peer->key_query_responded = false;
    /* A `K` reply carries no request serial, so only a delivered current query is eligible.
     * Later delayed replies remain inherently advisory and may be attributed to a newer query. */
    peer->key_query_sent = ast_sendtext(peer->channel, text) ? 0 : generation;
    if (atomic_load_explicit(&peer->key_query_active_generation, memory_order_acquire) !=
        generation) {
        peer->key_query_sent = 0;
    }
}

/** @brief Send one relayed legacy keyed-source text from the channel-owning reader.
 * @param peer Reader-owned peer transport.
 *
 * Relay text is advisory just like the locally originated `K?` message.  A failed IAX text
 * write is discarded instead of tearing down voice media, and a bounded FIFO preserves arrival
 * order when more than one remote responder is keyed during a double.
 */
static void send_key_message(struct ra_link_peer *peer) {
    char text[RA_LINK_PEER_KEY_TEXT_MAX];
    ast_mutex_lock(&peer->topology_lock);
    if (peer->key_message_tail == peer->key_message_head) {
        ast_mutex_unlock(&peer->topology_lock);
        return;
    }
    unsigned int slot = peer->key_message_tail % RA_LINK_PEER_KEY_QUEUE_CAPACITY;
    for (size_t index = 0; index < sizeof(text); ++index) {
        text[index] = peer->key_messages[slot][index];
    }
    ++peer->key_message_tail;
    ast_mutex_unlock(&peer->topology_lock);
    (void)ast_sendtext(peer->channel, text);
}

/** @brief Send the latest queued linked-node list from the channel-owning reader.
 * @param peer Reader-owned peer state.
 * @return Zero when no list is pending or its IAX text was sent, minus one on IAX failure.
 *
 * The control producer and reader share the queue under the topology lock, but the potentially
 * blocking channel operation happens after copying the bounded payload to reader-owned stack
 * storage. This preserves channel ownership and keeps every radio callback lock-free.
 */
static int send_topology(struct ra_link_peer *peer) {
    char payload[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1];
    ast_mutex_lock(&peer->topology_lock);
    if (!peer->advertised_pending) {
        ast_mutex_unlock(&peer->topology_lock);
        return 0;
    }
    size_t length = peer->advertised_length;
    for (size_t index = 0; index < length; ++index) {
        payload[index] = peer->advertised[index];
    }
    payload[length] = '\0';
    peer->advertised_pending = false;
    ast_mutex_unlock(&peer->topology_lock);
    char text[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 3] = "L ";
    for (size_t index = 0; index < length; ++index) {
        text[index + 2] = payload[index];
    }
    return ast_sendtext(peer->channel, text) ? -1 : 0;
}

/** @brief Read until stop or hangup; only the lifecycle owner releases the channel.
 * @param context Stable peer state.
 * @return Null after publishing reader completion.
 */
static void *read_peer(void *context) {
    struct ra_link_peer *peer = context;
    while (!atomic_load(&peer->stop)) {
        expire_digit_timeout(peer);
        if (send_digit(peer)) {
            break;
        }
        send_key_query(peer);
        send_key_message(peer);
        if (send_topology(peer)) {
            break;
        }
        size_t count = ra_link_audio_available(&peer->outgoing);
        size_t block = peer->linear_rate / 50;
        if (count > block) {
            count = block;
        }
        if (count) {
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

/** @brief Release the control-plane lock after a failed or stopped peer lifetime.
 * @param peer Peer that may have initialized its topology lock.
 */
static void close_topology(struct ra_link_peer *peer) {
    ast_mutex_destroy(&peer->topology_lock);
    peer->topology_lock_initialized = false;
}

int ra_link_peer_start(struct ra_link_peer *peer, struct ast_channel *channel,
                       struct ast_format *linear, ra_link_peer_digit_fn inbound_digit,
                       void *inbound_digit_context, ra_link_peer_key_fn inbound_key,
                       void *inbound_key_context) {
    unsigned int linear_rate = ast_format_get_sample_rate(linear);
    /* The protected reserve needs enough PCM above it for sinc conversion and
     * slow clock recovery.  Keep a fixed time budget across negotiated rates. */
    size_t capacity = (size_t)linear_rate * RA_LINK_RECEIVE_CAPACITY_MS / 1000U;
    if (!capacity) {
        return -1;
    }
    int16_t *outgoing = ast_calloc(capacity, sizeof(*outgoing));
    int16_t *send_buffer = ast_calloc(capacity / 10, sizeof(*send_buffer));
    if (!outgoing || !send_buffer || rpcr_init(&peer->received, capacity, RPCR_SINC_BEST) ||
        rpcr_set_sample_rate(&peer->received, linear_rate)) {
        release_elastic(peer);
        ast_free(send_buffer);
        ast_free(outgoing);
        return -1;
    }
    if (ast_mutex_init(&peer->topology_lock)) {
        release_elastic(peer);
        ast_free(send_buffer);
        ast_free(outgoing);
        return -1;
    }
    peer->topology_lock_initialized = true;
    peer->topology_length = 0;
    peer->topology[0] = '\0';
    peer->advertised_length = 0;
    peer->advertised_pending = false;
    peer->advertised[0] = '\0';
    peer->key_message_head = 0;
    peer->key_message_tail = 0;
    if (ast_set_read_format(channel, linear) || ast_set_write_format(channel, linear) ||
        ast_sendtext(channel, "!NEWKEY1!")) {
        close_topology(peer);
        release_elastic(peer);
        ast_free(send_buffer);
        ast_free(outgoing);
        return -1;
    }
    peer->channel = channel;
    peer->linear = linear;
    peer->linear_rate = linear_rate;
    ra_link_audio_init(&peer->outgoing, outgoing, capacity);
    peer->outgoing_storage = outgoing;
    peer->send_buffer = send_buffer;
    peer->inbound_digit = inbound_digit;
    peer->inbound_digit_context = inbound_digit_context;
    peer->inbound_key = inbound_key;
    peer->inbound_key_context = inbound_key_context;
    atomic_init(&peer->stop, false);
    atomic_init(&peer->ended, false);
    atomic_init(&peer->receive_epoch, 0);
    peer->receive_primed = false;
    atomic_init(&peer->digit_head, 0);
    atomic_init(&peer->digit_tail, 0);
    atomic_init(&peer->key_query_active_generation, 0);
    atomic_init(&peer->key_query_generation, 0);
    atomic_init(&peer->key_query_start_generation, 0);
    peer->key_query_attempted = 0;
    peer->key_query_sent = 0;
    peer->key_query_responded = false;
    atomic_init(&peer->key_source_sequence, 0);
    atomic_init(&peer->key_source_generation, 0);
    for (size_t index = 0; index < RA_LINK_PEER_NAME_MAX; ++index) {
        atomic_init(&peer->key_source[index], '\0');
    }
    if (pthread_create(&peer->thread, NULL, read_peer, peer)) {
        close_topology(peer);
        release_elastic(peer);
        ast_free(send_buffer);
        ast_free(outgoing);
        peer->channel = NULL;
        return -1;
    }
    return 0;
}

bool ra_link_peer_receive(struct ra_link_peer *peer, int16_t *audio, size_t samples) {
    uint64_t epoch = atomic_load(&peer->receive_epoch);
    if (epoch != peer->seen_epoch) {
        peer->seen_epoch = epoch;
        peer->receive_age = 0;
    } else {
        peer->receive_age += samples;
    }
    size_t reserve = (size_t)peer->linear_rate * RA_LINK_RECEIVE_RESERVE_MS / 1000U;
    if (samples >= peer->received.capacity) {
        reserve = 0;
    } else if (reserve > peer->received.capacity - samples) {
        reserve = peer->received.capacity - samples;
    }
    atomic_store_explicit(&peer->received.reserve_samples, reserve, memory_order_relaxed);
    size_t available = rpcr_available(&peer->received);
    size_t target = (size_t)peer->linear_rate * RA_LINK_RECEIVE_TARGET_MS / 1000U;
    size_t maximum_target =
        samples < peer->received.capacity ? peer->received.capacity - samples : 0;
    if (target > maximum_target) {
        target = maximum_target;
    }
    if (!peer->receive_primed && available >= target) {
        peer->receive_primed = true;
    }
    bool fresh = peer->receive_age < reserve;
    size_t protected_reserve = fresh ? reserve : 0;
    /* Keep the source active through one protected-reserve interval so the
     * shared PCM ring can conceal a brief network shortage. A real end of
     * stream still wins immediately and never synthesizes media. */
    bool receiving = peer->receive_primed && (available > protected_reserve || fresh) &&
                     !atomic_load(&peer->ended);
    if (!receiving) {
        for (size_t i = 0; i < samples; ++i) {
            audio[i] = 0;
        }
        return false;
    }
    size_t rendered = rpcr_render(&peer->received, audio, samples, protected_reserve, target);
    rpcr_record_shortfall(&peer->received, samples - rendered, samples, peer->linear_rate);
    return true;
}

int ra_link_peer_send(struct ra_link_peer *peer, bool keyed, const int16_t *audio, size_t samples) {
    if (atomic_load(&peer->ended)) {
        return -1;
    }
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

/** @brief Advance one advisory source-query epoch without using the peer's channel or locks.
 * @param peer Started direct peer.
 * @param begins_receive True when this is the direct peer's receive rising edge.
 */
static void request_key_query(struct ra_link_peer *peer, bool begins_receive) {
    if (!peer || !peer->key_query_requester[0] ||
        atomic_load_explicit(&peer->ended, memory_order_acquire)) {
        return;
    }
    if (!begins_receive &&
        !atomic_load_explicit(&peer->key_query_active_generation, memory_order_acquire)) {
        return;
    }
    uint_fast64_t generation =
        atomic_fetch_add_explicit(&peer->key_query_generation, 1, memory_order_release) + 1;
    if (begins_receive) {
        atomic_store_explicit(&peer->key_query_start_generation, generation, memory_order_release);
    }
    atomic_store_explicit(&peer->key_query_active_generation, generation, memory_order_release);
}

void ra_link_peer_begin_keyed_source(struct ra_link_peer *peer) { request_key_query(peer, true); }

void ra_link_peer_end_keyed_source(struct ra_link_peer *peer) {
    if (peer) {
        atomic_store_explicit(&peer->key_query_active_generation, 0, memory_order_release);
    }
}

void ra_link_peer_request_key_query(struct ra_link_peer *peer) { request_key_query(peer, false); }

bool ra_link_peer_keyed_source(const struct ra_link_peer *peer, char *output, size_t capacity) {
    if (output && capacity) {
        output[0] = '\0';
    }
    if (!peer || !output || capacity < RA_LINK_PEER_NAME_MAX) {
        return false;
    }
    uint_fast64_t start =
        atomic_load_explicit(&peer->key_query_start_generation, memory_order_acquire);
    uint_fast64_t current = atomic_load_explicit(&peer->key_query_generation, memory_order_acquire);
    if (!start || start > current) {
        return false;
    }
    for (unsigned int attempt = 0; attempt < 2; ++attempt) {
        uint_fast64_t before =
            atomic_load_explicit(&peer->key_source_sequence, memory_order_acquire);
        uint_fast64_t generation =
            atomic_load_explicit(&peer->key_source_generation, memory_order_relaxed);
        for (size_t index = 0; index < RA_LINK_PEER_NAME_MAX; ++index) {
            output[index] =
                (char)atomic_load_explicit(&peer->key_source[index], memory_order_relaxed);
        }
        uint_fast64_t after =
            atomic_load_explicit(&peer->key_source_sequence, memory_order_acquire);
        /* Both checks must be evaluated: an in-progress writer and a completed publication
         * during the copy are equally unsafe. */
        bool stable = ((before & 1U) == 0U) & (before == after);
        if (!stable) {
            continue;
        }
        if (!output[0] || generation < start || generation > current) {
            output[0] = '\0';
            return false;
        }
        return true;
    }
    output[0] = '\0';
    return false;
}

size_t ra_link_peer_topology(struct ra_link_peer *peer, char *output, size_t capacity) {
    if (output && capacity) {
        output[0] = '\0';
    }
    if (!peer->topology_lock_initialized) {
        return 0;
    }
    ast_mutex_lock(&peer->topology_lock);
    size_t length = peer->topology_length;
    if (output && capacity) {
        size_t copied = length < capacity - 1 ? length : capacity - 1;
        for (size_t index = 0; index < copied; ++index) {
            output[index] = peer->topology[index];
        }
        output[copied] = '\0';
    }
    ast_mutex_unlock(&peer->topology_lock);
    return length;
}

int ra_link_peer_queue_topology(struct ra_link_peer *peer, const char *topology) {
    if (!peer->topology_lock_initialized || !topology || atomic_load(&peer->ended)) {
        return -1;
    }
    size_t length = strnlen(topology, RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1);
    if (length > RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX || !topology_payload_valid(topology, length)) {
        return -1;
    }
    ast_mutex_lock(&peer->topology_lock);
    for (size_t index = 0; index < length; ++index) {
        peer->advertised[index] = topology[index];
    }
    peer->advertised[length] = '\0';
    peer->advertised_length = length;
    peer->advertised_pending = true;
    ast_mutex_unlock(&peer->topology_lock);
    return 0;
}

/** @brief Validate one NUL-terminated remote identity for outbound keyed-source text.
 * @param value Candidate node identity.
 * @return True only for a complete supported identity shorter than the fixed message field.
 */
static bool key_name_valid(const char *value) {
    if (!value) {
        return false;
    }
    size_t length = strnlen(value, RA_LINK_PEER_NAME_MAX);
    return length < RA_LINK_PEER_NAME_MAX && topology_name_valid(value, length);
}

/** @brief Retain one bounded keyed-source IAX text message for its peer reader.
 * @param peer Started destination peer.
 * @param text Complete canonical IAX text message.
 * @param length Text length without its terminating NUL.
 * @return Zero when queued, minus one when no control-plane slot is available.
 */
static int queue_key_message(struct ra_link_peer *peer, const char text[RA_LINK_PEER_KEY_TEXT_MAX],
                             size_t length) {
    if (!peer || !peer->topology_lock_initialized || atomic_load(&peer->ended)) {
        return -1;
    }
    ast_mutex_lock(&peer->topology_lock);
    if (peer->key_message_head - peer->key_message_tail >= RA_LINK_PEER_KEY_QUEUE_CAPACITY) {
        ast_mutex_unlock(&peer->topology_lock);
        return -1;
    }
    unsigned int slot = peer->key_message_head % RA_LINK_PEER_KEY_QUEUE_CAPACITY;
    for (size_t index = 0; index <= length; ++index) {
        peer->key_messages[slot][index] = text[index];
    }
    ++peer->key_message_head;
    ast_mutex_unlock(&peer->topology_lock);
    return 0;
}

/** @brief Append a known complete text field to a bounded keyed-source message.
 * @param output Fixed outbound message storage.
 * @param position First unwritten byte in @p output.
 * @param value Complete field terminated by NUL.
 * @return First unwritten byte after the appended field.
 *
 * Callers validate names and use compile-time-sized storage before appending. The static reply
 * capacity assertion above proves every supported composition fits without truncation.
 */
static size_t append_key_text(char output[RA_LINK_PEER_KEY_TEXT_MAX], size_t position,
                              const char *value) {
    for (size_t index = 0; value[index]; ++index) {
        output[position++] = value[index];
    }
    return position;
}

/** @brief Append one unsigned decimal reply age without library formatting.
 * @param output Fixed outbound message storage.
 * @param position First unwritten byte in @p output.
 * @param value Nonnegative age in seconds.
 * @return First unwritten byte after the decimal digits.
 */
static size_t append_key_age(char output[RA_LINK_PEER_KEY_TEXT_MAX], size_t position,
                             uint64_t value) {
    char digits[sizeof(value) * CHAR_BIT];
    size_t count = 0;
    do {
        digits[count++] = (char)('0' + value % 10U);
        value /= 10U;
    } while (value);
    while (count) {
        output[position++] = digits[--count];
    }
    return position;
}

int ra_link_peer_queue_key_query(struct ra_link_peer *peer, const char *requester) {
    if (!key_name_valid(requester)) {
        return -1;
    }
    char text[RA_LINK_PEER_KEY_TEXT_MAX];
    size_t length = append_key_text(text, 0, "K? * ");
    length = append_key_text(text, length, requester);
    length = append_key_text(text, length, " 0 0");
    text[length] = '\0';
    return queue_key_message(peer, text, length);
}

int ra_link_peer_queue_key_reply(struct ra_link_peer *peer, const char *destination,
                                 const char *source, bool keyed, uint64_t age_seconds) {
    if (!key_name_valid(destination) || !key_name_valid(source)) {
        return -1;
    }
    char text[RA_LINK_PEER_KEY_TEXT_MAX];
    size_t length = append_key_text(text, 0, "K ");
    length = append_key_text(text, length, destination);
    length = append_key_text(text, length, " ");
    length = append_key_text(text, length, source);
    length = append_key_text(text, length, keyed ? " 1 " : " 0 ");
    length = append_key_age(text, length, age_seconds);
    text[length] = '\0';
    return queue_key_message(peer, text, length);
}

/** @brief Stop one started reader and release its private storage.
 * @param peer Started peer, called once and with no concurrent sender/consumer.
 * @param hangup_channel True when the peer owns the answered channel.
 */
static void stop_peer(struct ra_link_peer *peer, bool hangup_channel) {
    atomic_store(&peer->stop, true);
    pthread_join(peer->thread, NULL);
    (void)ast_indicate(peer->channel, AST_CONTROL_RADIO_UNKEY);
    if (hangup_channel) {
        ast_hangup(peer->channel);
    }
    ast_free(peer->outgoing_storage);
    ast_free(peer->send_buffer);
    release_elastic(peer);
    ast_translator_free_path(peer->decode);
    peer->channel = NULL;
    close_topology(peer);
}

void ra_link_peer_stop(struct ra_link_peer *peer) { stop_peer(peer, true); }

void ra_link_peer_stop_preserve_channel(struct ra_link_peer *peer) { stop_peer(peer, false); }
