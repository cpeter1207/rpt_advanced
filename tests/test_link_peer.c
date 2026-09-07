/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Deterministic peer transport ownership and frame failure tests.
 */
#include "link_peer.h"
#include <assert.h>
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <stdlib.h>
#include <string.h>

/** @brief Minimal format fixture. */
struct ast_format {
    unsigned int rate; /**< PCM rate. */
};
struct ast_trans_pvt {};
/** @brief Captured reader. */
static void *(*reader)(void *);
/** @brief Active fixture peer. */
static struct ra_link_peer *active;
/** @brief Failure injection step. */
static int failure;
/** @brief One-based allocation call that must fail, or zero for no allocation failure. */
static unsigned int allocation_failure;
/** @brief Allocation calls made during the current start attempt. */
static unsigned int allocation_calls;
/** @brief Next input frame. */
static struct ast_frame *input;
/** @brief Readiness sequence. */
static int ready[4];
/** @brief Readiness cursor. */
static size_t next;
/** @brief Lock ownership balance. */
static int locked;
/** @brief Number of initialized fixture mutexes. */
static int mutex_inits;
/** @brief Released channels. */
static int hangups;
/** @brief Number of remote-command digits delivered by the reader. */
static unsigned int sent_digits;
/** @brief Number of radio-key indications sent to the fixture channel. */
static unsigned int key_indications;
/** @brief Number of radio-unkey indications sent to the fixture channel. */
static unsigned int unkey_indications;

/** @brief Real allocator for non-failing calls.
 * @param count Element count.
 * @param size Element size.
 * @return Allocated memory or null.
 */
void *__real_calloc(size_t count, size_t size);
/** @brief Inject allocation failure.
 * @param count Element count.
 * @param size Element size.
 * @return Allocation or injected null.
 */
void *__wrap_calloc(size_t count, size_t size) {
    ++allocation_calls;
    return allocation_failure == allocation_calls ? NULL : __real_calloc(count, size);
}
/** @brief Route Asterisk allocation through the same failure fixture.
 * @param count Element count.
 * @param size Element size.
 * @param file Unused source file.
 * @param line Unused source line.
 * @param function Unused calling function.
 * @return Allocated memory or injected null.
 */
void *__ast_calloc(size_t count, size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return __wrap_calloc(count, size);
}
/** @brief Release memory through Asterisk's public allocator boundary.
 * @param pointer Owned allocation.
 * @param file Unused source file.
 * @param line Unused source line.
 * @param function Unused calling function.
 */
void __ast_free(void *pointer, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    free(pointer);
}
/** @brief Capture reader without scheduling nondeterministic execution.
 * @param thread Receives fixture identity.
 * @param attr Unused attributes.
 * @param start Captured reader entry.
 * @param argument Captured peer.
 * @return Injected creation status.
 */
int __wrap_pthread_create(pthread_t *thread, const pthread_attr_t *attr, void *(*start)(void *),
                          void *argument) {
    (void)attr;
    *thread = pthread_self();
    reader = start;
    active = argument;
    return failure == 5;
}
/** @brief Verify shutdown was requested before joining.
 * @param thread Unused fixture identity.
 * @param result Unused result destination.
 * @return Zero after completing the stopped reader.
 */
int __wrap_pthread_join(pthread_t thread, void **result) {
    (void)thread;
    (void)result;
    assert(atomic_load(&active->stop));
    reader(active);
    return 0;
}
/** @brief Model Asterisk mutex initialization.
 * @param tracking Unused lock tracking switch.
 * @param file Unused call-site file.
 * @param line Unused call-site line.
 * @param func Unused call-site function.
 * @param name Unused mutex name.
 * @param lock Unused opaque mutex.
 * @return Injected initialization status.
 */
int __ast_pthread_mutex_init(int tracking, const char *file, int line, const char *func,
                             const char *name, ast_mutex_t *lock) {
    (void)tracking;
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    ++mutex_inits;
    return failure == mutex_inits + 1;
}
/** @brief Model balanced mutex disposal.
 * @param file Unused call-site file.
 * @param line Unused call-site line.
 * @param func Unused call-site function.
 * @param name Unused mutex name.
 * @param lock Unused opaque mutex.
 * @return Zero after checking no held lock remains.
 */
int __ast_pthread_mutex_destroy(const char *file, int line, const char *func, const char *name,
                                ast_mutex_t *lock) {
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    assert(!locked);
    return 0;
}
/** @brief Track queue exclusion.
 * @param file Unused call-site file.
 * @param line Unused call-site line.
 * @param func Unused call-site function.
 * @param name Unused mutex name.
 * @param lock Unused opaque mutex.
 * @return Zero after checking exclusive ownership.
 */
int __ast_pthread_mutex_lock(const char *file, int line, const char *func, const char *name,
                             ast_mutex_t *lock) {
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    assert(!locked++);
    return 0;
}
/** @brief Track balanced queue unlocks.
 * @param file Unused call-site file.
 * @param line Unused call-site line.
 * @param func Unused call-site function.
 * @param name Unused mutex name.
 * @param lock Unused opaque mutex.
 * @return Zero after releasing fixture ownership.
 */
int __ast_pthread_mutex_unlock(const char *file, int line, const char *func, const char *name,
                               ast_mutex_t *lock) {
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    assert(locked-- == 1);
    return 0;
}
/** @brief Return negotiated rate.
 * @param format Fixture format.
 * @return PCM sample rate.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) { return format->rate; }
/** @brief Return a fixture label for diagnostic-only format logging.
 * @param format Unused fixture format.
 * @return Stable test label.
 */
const char *ast_format_get_name(const struct ast_format *format) {
    (void)format;
    return "fixture";
}
/** @brief Discard diagnostic logging emitted only by live conversion failures.
 * @param level Unused Asterisk severity.
 * @param file Unused source path.
 * @param line Unused source line.
 * @param function Unused function name.
 * @param format Unused message format.
 */
void ast_log(int level, const char *file, int line, const char *function, const char *format, ...) {
    (void)level;
    (void)file;
    (void)line;
    (void)function;
    (void)format;
}
/** @brief Compare fixture formats by identity.
 * @param left First format.
 * @param right Second format.
 * @return Exact equality or mismatch.
 */
enum ast_format_cmp_res ast_format_cmp(const struct ast_format *left,
                                       const struct ast_format *right) {
    return left == right ? AST_FORMAT_CMP_EQUAL : AST_FORMAT_CMP_NOT_EQUAL;
}
/** @brief Fixture translator creation is unused by the signed-linear tests.
 * @param destination Unused destination format.
 * @param source Unused source format.
 * @return Null.
 */
struct ast_trans_pvt *ast_translator_build_path(struct ast_format *destination,
                                                struct ast_format *source) {
    (void)destination;
    (void)source;
    return NULL;
}
/** @brief Release a fixture translator.
 * @param translator Unused fixture translator.
 */
void ast_translator_free_path(struct ast_trans_pvt *translator) { (void)translator; }
/** @brief Fixture translation is unavailable.
 * @param translator Unused translator.
 * @param frame Unused input frame.
 * @param consume Unused ownership flag.
 * @return Null.
 */
struct ast_frame *ast_translate(struct ast_trans_pvt *translator, struct ast_frame *frame,
                                int consume) {
    (void)translator;
    (void)frame;
    (void)consume;
    return NULL;
}
/** @brief Inject read conversion failure.
 * @param channel Unused channel.
 * @param format Unused target format.
 * @return Injected read-format status.
 */
int ast_set_read_format(struct ast_channel *channel, struct ast_format *format) {
    (void)channel;
    (void)format;
    return failure == 2;
}
/** @brief Inject write conversion failure.
 * @param channel Unused channel.
 * @param format Unused target format.
 * @return Injected write-format status.
 */
int ast_set_write_format(struct ast_channel *channel, struct ast_format *format) {
    (void)channel;
    (void)format;
    return failure == 3;
}
/** @brief Verify explicit redundant-key handshake.
 * @param channel Unused channel.
 * @param text Handshake text.
 * @return Injected text-write status.
 */
int ast_sendtext(struct ast_channel *channel, const char *text) {
    (void)channel;
    assert(!strcmp(text, "!NEWKEY!"));
    return failure == 4;
}
/** @brief Accept queued remote-command DTMF in the fixture transport.
 * @param channel Unused channel fixture.
 * @param digit Valid DTMF digit queued by the peer.
 * @param duration Expected zero-duration IAX signal request.
 * @return Injected send failure status.
 */
int ast_senddigit(struct ast_channel *channel, char digit, unsigned int duration) {
    (void)channel;
    assert(strchr("0123456789ABCD*#", digit) && !duration);
    ++sent_digits;
    return failure == 10;
}
/** @brief Supply timeout, ready, and transport failure events.
 * @param channel Unused channel.
 * @param milliseconds Required bounded wait.
 * @return Next readiness result.
 */
int ast_waitfor(struct ast_channel *channel, int milliseconds) {
    (void)channel;
    assert(milliseconds == 1 && next < 4);
    return ready[next++];
}
/** @brief Supply borrowed fixture frame or hangup.
 * @param channel Unused channel.
 * @return Current frame or null.
 */
struct ast_frame *ast_read(struct ast_channel *channel) {
    (void)channel;
    return input;
}
/** @brief Verify exactly the supplied frame is released.
 * @param frame Released input.
 * @param cache Expected cache flag.
 */
void ast_frame_free(struct ast_frame *frame, int cache) { assert(frame == input && cache == 1); }
/** @brief Verify voice output and inject failure.
 * @param channel Unused channel.
 * @param frame Outgoing PCM.
 * @return Injected write status.
 */
int ast_write(struct ast_channel *channel, struct ast_frame *frame) {
    (void)channel;
    assert(frame->frametype == AST_FRAME_VOICE && frame->samples > 0);
    return failure == 9;
}
/** @brief Verify key-only indications and inject failure.
 * @param channel Unused channel.
 * @param condition Radio key state.
 * @return Injected signaling status.
 */
int ast_indicate(struct ast_channel *channel, int condition) {
    (void)channel;
    assert(condition == AST_CONTROL_RADIO_KEY || condition == AST_CONTROL_RADIO_UNKEY);
    if (condition == AST_CONTROL_RADIO_KEY) {
        ++key_indications;
    } else {
        ++unkey_indications;
    }
    return failure == 8;
}
/** @brief Track transferred channel disposal.
 * @param channel Unused channel identity.
 */
void ast_hangup(struct ast_channel *channel) {
    (void)channel;
    ++hangups;
}

/** @brief Run one frame and terminate the reader on the following readiness result.
 * @param peer Started peer.
 * @param value Input frame or null for hangup.
 */
static void frame(struct ra_link_peer *peer, struct ast_frame *value) {
    input = value;
    next = 0;
    ready[0] = 0;
    ready[1] = 1;
    ready[2] = -1;
    atomic_store(&peer->ended, false);
    reader(peer);
    assert(atomic_load(&peer->ended));
    atomic_store(&peer->ended, false);
}

/** @brief Exercise peer lifetime, PCM gating/drain, malformed frames, and failed writes.
 * @return Zero after assertions.
 */
int main(void) {
    struct ast_format linear = {8000}, other = {8000}, invalid = {0};
    struct ra_link_peer peer = {0};
    assert(ra_link_peer_start(&peer, NULL, &invalid) == -1);
    for (allocation_failure = 1; allocation_failure <= 3; ++allocation_failure) {
        allocation_calls = 0;
        assert(ra_link_peer_start(&peer, NULL, &linear) == -1);
    }
    allocation_failure = 0;
    for (failure = 2; failure <= 5; ++failure) {
        mutex_inits = 0;
        allocation_calls = 0;
        assert(ra_link_peer_start(&peer, NULL, &linear) == -1);
    }
    failure = 0;
    mutex_inits = 0;
    allocation_calls = 0;
    assert(!ra_link_peer_start(&peer, NULL, &linear));
    int16_t samples[] = {123, -456}, output[2];
    struct ast_frame voice = {.frametype = AST_FRAME_VOICE,
                              .subclass.format = &linear,
                              .data.ptr = samples,
                              .samples = 2,
                              .datalen = 4};
    struct ast_frame control = {.frametype = AST_FRAME_CONTROL,
                                .subclass.integer = AST_CONTROL_RADIO_KEY};
    struct ast_frame ignored = {.frametype = AST_FRAME_NULL};
    frame(&peer, &ignored);
    atomic_store(&peer.sent_samples, 16000);
    frame(&peer, &ignored);
    assert(unkey_indications == 1);
    atomic_store(&peer.sent_samples, 0);
    peer.heartbeat_samples = 0;
    struct ast_frame text = {.frametype = AST_FRAME_TEXT};
    frame(&peer, &text);
    text.data.ptr = "!NEWKEY!x";
    text.datalen = 9;
    frame(&peer, &text);
    text.data.ptr = "unknown";
    text.datalen = 7;
    frame(&peer, &text);
    text.data.ptr = "!NEWKEY1!";
    text.datalen = 10;
    frame(&peer, &text);
    assert(atomic_load(&peer.voice_keying));
    frame(&peer, &voice);
    frame(&peer, &control);
    assert(ra_link_peer_receive(&peer, output, 2) && output[0] == 123);
    int16_t quiet[32000];
    bool expired_voice = ra_link_peer_receive(&peer, quiet, 400);
    assert(peer.receive_age == 400 && !expired_voice);
    text.data.ptr = "!NEWKEY!";
    text.datalen = 8;
    failure = 4;
    frame(&peer, &text);
    failure = 0;
    peer.handshake_replied = false;
    frame(&peer, &text);
    frame(&peer, &text);
    assert(!atomic_load(&peer.voice_keying) && peer.handshake_replied);
    control.subclass.integer = AST_CONTROL_RADIO_KEY;
    frame(&peer, &control);
    assert(ra_link_peer_receive(&peer, output, 2));
    assert(!ra_link_peer_receive(&peer, quiet, 32000));
    text.data.ptr = "!!DISCONNECT!!";
    text.datalen = 14;
    frame(&peer, &text);
    frame(&peer, NULL);
    frame(&peer, &voice);
    assert(!ra_link_peer_receive(&peer, output, 2) && output[0] == 0);
    frame(&peer, &control);
    frame(&peer, &voice);
    control.subclass.integer = AST_CONTROL_RADIO_UNKEY;
    frame(&peer, &control);
    assert(ra_link_peer_receive(&peer, output, 2) && output[0] == 123 && output[1] == -456);
    assert(!ra_link_peer_receive(&peer, output, 2));
    control.subclass.integer = AST_CONTROL_ANSWER;
    frame(&peer, &control);
    control.subclass.integer = AST_CONTROL_HANGUP;
    frame(&peer, &control);
    voice.data.ptr = NULL;
    frame(&peer, &voice);
    voice.data.ptr = samples;
    voice.samples = 0;
    frame(&peer, &voice);
    voice.samples = 2;
    voice.datalen = 3;
    frame(&peer, &voice);
    voice.datalen = 0;
    frame(&peer, &voice);
    voice.datalen = 4;
    voice.subclass.format = &other;
    frame(&peer, &voice);
    assert(!ra_link_peer_send(&peer, false, samples, 2));
    failure = 0;
    assert(!ra_link_peer_send(&peer, true, samples, 2));
    assert(!ra_link_peer_send(&peer, true, NULL, 0));
    assert(!ra_link_peer_send(&peer, true, samples, 2));
    assert(!ra_link_peer_send(&peer, false, NULL, 0));
    atomic_store(&peer.desired_key, true);
    for (size_t index = 0; index < 100; ++index) {
        assert(!ra_link_peer_send(&peer, true, samples, 2));
    }
    frame(&peer, &ignored);
    peer.transmitting = false;
    failure = 8;
    frame(&peer, &ignored);
    failure = 0;
    assert(!ra_link_peer_send(&peer, true, samples, 2));
    failure = 9;
    frame(&peer, &ignored);
    failure = 0;
    assert(!ra_link_peer_send_digit(&peer, '1'));
    assert(ra_link_peer_send_digit(&peer, 'x') == -1);
    atomic_store(&peer.digit_tail, atomic_load(&peer.digit_head));
    atomic_store(&peer.digit_head, sizeof(peer.digits));
    atomic_store(&peer.digit_tail, 0);
    assert(ra_link_peer_send_digit(&peer, '1') == -1);
    atomic_store(&peer.digit_tail, atomic_load(&peer.digit_head));
    assert(!ra_link_peer_send_digit(&peer, '2'));
    frame(&peer, &ignored);
    assert(sent_digits == 1);
    assert(!ra_link_peer_send_digit(&peer, '3'));
    failure = 10;
    frame(&peer, &ignored);
    failure = 0;
    atomic_store(&peer.ended, true);
    assert(ra_link_peer_send_digit(&peer, '1') == -1);
    assert(ra_link_peer_send(&peer, false, NULL, 0) == -1);
    atomic_store(&peer.receiving, true);
    assert(!ra_link_peer_receive(&peer, output, 2));
    ra_link_peer_stop(&peer);
    peer = (struct ra_link_peer){0};
    assert(!ra_link_peer_start(&peer, NULL, &linear));
    assert(!ra_link_peer_send(&peer, true, samples, 2));
    ra_link_peer_stop(&peer);
    assert(hangups == 2);
    return 0;
}
