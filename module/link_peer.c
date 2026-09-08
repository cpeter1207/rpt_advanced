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
_Static_assert(ATOMIC_BOOL_LOCK_FREE == 2 && RA_ATOMIC_UINT_FAST64_LOCK_FREE,
               "peer audio state must not call libatomic");

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
    if (!frame->data.ptr || frame->datalen < 0) {
        return false;
    }
    const char *text = frame->data.ptr;
    size_t length = (size_t)frame->datalen;
    if (length && text[length - 1] == '\0') {
        --length;
    }
    if (!length || memchr(text, '\0', length) || text[0] != 'L') {
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
            (void)cache_topology(peer, frame);
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
        atomic_fetch_add(&peer->receive_epoch, 1);
        rpcr_write(&peer->received, audio->data.ptr, audio->samples);
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
                       void *inbound_digit_context) {
    unsigned int linear_rate = ast_format_get_sample_rate(linear);
    size_t capacity = linear_rate / 5;
    if (!capacity) {
        return -1;
    }
    int16_t *outgoing = ast_calloc(capacity, sizeof(*outgoing));
    int16_t *send_buffer = ast_calloc(capacity / 10, sizeof(*send_buffer));
    if (!outgoing || !send_buffer || rpcr_init(&peer->received, capacity, RPCR_SINC_BEST)) {
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
    atomic_init(&peer->stop, false);
    atomic_init(&peer->ended, false);
    atomic_init(&peer->receive_epoch, 0);
    atomic_init(&peer->digit_head, 0);
    atomic_init(&peer->digit_tail, 0);
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
    size_t reserve = samples * RA_LINK_RECEIVE_RESERVE_BLOCKS;
    if (samples >= peer->received.capacity) {
        reserve = 0;
    } else if (reserve > peer->received.capacity - samples) {
        reserve = peer->received.capacity - samples;
    }
    atomic_store_explicit(&peer->received.reserve_samples, reserve, memory_order_relaxed);
    size_t available = rpcr_available(&peer->received);
    size_t target = peer->received.capacity > samples * 2U ? peer->received.capacity - samples * 2U
                                                           : reserve + samples;
    if (!peer->received.primed && available >= target) {
        peer->received.primed = true;
    }
    bool fresh = peer->receive_age < reserve;
    size_t protected_reserve = fresh ? reserve : 0;
    bool receiving = peer->received.primed &&
                     (available > protected_reserve || (fresh && available)) &&
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

void ra_link_peer_stop(struct ra_link_peer *peer) {
    atomic_store(&peer->stop, true);
    pthread_join(peer->thread, NULL);
    (void)ast_indicate(peer->channel, AST_CONTROL_RADIO_UNKEY);
    ast_hangup(peer->channel);
    ast_free(peer->outgoing_storage);
    ast_free(peer->send_buffer);
    release_elastic(peer);
    ast_translator_free_path(peer->decode);
    peer->channel = NULL;
    close_topology(peer);
}
