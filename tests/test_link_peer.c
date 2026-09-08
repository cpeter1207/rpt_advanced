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
/** @brief Fixture result for a non-linear decoder request. */
enum fixture_translation {
    FIXTURE_TRANSLATOR_UNAVAILABLE, /**< No compatible decoder path exists. */
    FIXTURE_TRANSLATOR_BUFFERED,    /**< Decoder consumes an incomplete packet. */
    FIXTURE_TRANSLATOR_FRAME        /**< Decoder returns one signed-linear frame. */
};
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
/** @brief Select the shared playout-ring initialization failure path. */
static bool rpcr_init_failure;
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
/** @brief Number of queued topology advertisements delivered by the reader. */
static unsigned int sent_topologies;
/** @brief Most recent full IAX topology text sent by the channel owner. */
static char sent_topology[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 3];
/** @brief Number of initial voice-keyed IAX negotiation messages. */
static unsigned int sent_newkey1s;
/** @brief Number of IAX key-negotiation replies. */
static unsigned int sent_iaxkeys;
/** @brief Opaque identity required by the inbound-IAX DTMF callback fixture. */
static int inbound_context;
/** @brief Number of valid inbound IAX DTMF end events delivered by the reader. */
static unsigned int inbound_digits;
/** @brief Most recent valid inbound IAX DTMF character. */
static char inbound_digit;
/** @brief Selected result for the fixture's Asterisk translator. */
static enum fixture_translation translation_mode;
/** @brief Singleton translator returned for supported fixture conversions. */
static struct ast_trans_pvt translator;
/** @brief Required signed-linear destination format for fixture conversion requests. */
static struct ast_format *translation_destination;
/** @brief Format returned in the fixture translated frame. */
static struct ast_format *translated_format;
/** @brief PCM samples returned by the fixture translator. */
static int16_t translated_samples[] = {789, -321};
/** @brief Reusable translated frame. */
static struct ast_frame translated;

/** @brief Call the real shared playout-ring initializer behind the test wrapper.
 * @param ring Ring state to initialize.
 * @param capacity Ring capacity in PCM samples.
 * @param quality Requested libsamplerate quality.
 * @return Zero when the ring was initialized.
 */
int __real_rpcr_init(struct rpcr_ring *ring, size_t capacity, enum rpcr_quality quality);

/** @brief Inject an initialization failure at the consumer boundary.
 * @param ring Ring state to initialize.
 * @param capacity Ring capacity in PCM samples.
 * @param quality Requested libsamplerate quality.
 * @return A selected failure or the shared library result.
 */
int __wrap_rpcr_init(struct rpcr_ring *ring, size_t capacity, enum rpcr_quality quality) {
    return rpcr_init_failure ? -1 : __real_rpcr_init(ring, capacity, quality);
}
/** @brief Decoder paths requested by the peer. */
static unsigned int translator_builds;
/** @brief Decoder paths released by the peer. */
static unsigned int translator_frees;
/** @brief Input packets consumed by the fixture translator. */
static unsigned int translated_inputs;
/** @brief Signed-linear translated frames released by the peer. */
static unsigned int translated_frees;
/** @brief Capture one validated inbound IAX DTMF event outside the transport reader's locks.
 * @param context Expected callback identity.
 * @param digit Validated conventional DTMF character.
 */
static void receive_inbound_digit(void *context, char digit) {
    assert(context == &inbound_context && !locked);
    ++inbound_digits;
    inbound_digit = digit;
}

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
    return failure == 1;
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
/** @brief Build or reject a requested fixture decoder path.
 * @param destination Required signed-linear destination format.
 * @param source Non-linear source format.
 * @return A stable translator or null when conversion is unavailable.
 */
struct ast_trans_pvt *ast_translator_build_path(struct ast_format *destination,
                                                struct ast_format *source) {
    (void)source;
    assert(destination == translation_destination);
    ++translator_builds;
    return translation_mode == FIXTURE_TRANSLATOR_UNAVAILABLE ? NULL : &translator;
}
/** @brief Release a fixture translator.
 * @param value Null or the stable fixture translator.
 */
void ast_translator_free_path(struct ast_trans_pvt *value) {
    assert(!value || value == &translator);
    if (value) {
        ++translator_frees;
    }
}
/** @brief Consume one fixture packet and optionally return signed-linear PCM.
 * @param value Stable fixture translator.
 * @param frame Owned non-linear input frame.
 * @param consume Required input-ownership transfer flag.
 * @return Null for a buffered packet or a signed-linear fixture frame.
 */
struct ast_frame *ast_translate(struct ast_trans_pvt *value, struct ast_frame *frame, int consume) {
    assert(value == &translator && frame == input && consume == 1);
    ++translated_inputs;
    if (translation_mode == FIXTURE_TRANSLATOR_BUFFERED) {
        return NULL;
    }
    assert(translation_mode == FIXTURE_TRANSLATOR_FRAME);
    translated = (struct ast_frame){.frametype = AST_FRAME_VOICE,
                                    .subclass.format = translated_format,
                                    .data.ptr = translated_samples,
                                    .samples = 2,
                                    .datalen = sizeof(translated_samples)};
    return &translated;
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
/** @brief Verify reader-owned IAX control replies and linked-node-list text.
 * @param channel Unused channel.
 * @param text IAX control reply or linked-node-list text.
 * @return Injected text-write status for the selected message class.
 */
int ast_sendtext(struct ast_channel *channel, const char *text) {
    (void)channel;
    if (!strcmp(text, "!NEWKEY1!")) {
        ++sent_newkey1s;
        return failure == 4;
    }
    if (!strcmp(text, "!IAXKEY! 1 1 0 0")) {
        ++sent_iaxkeys;
        return failure == 13;
    }
    assert(!strncmp(text, "L ", 2));
    assert(strlen(text) <= RA_LINK_TOPOLOGY_TEXT_MAX);
    memcpy(sent_topology, text, strlen(text) + 1);
    ++sent_topologies;
    return failure == 11;
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
/** @brief Verify exactly the supplied or translated frame is released.
 * @param frame Released input.
 * @param cache Expected cache flag.
 */
void ast_frame_free(struct ast_frame *frame, int cache) {
    assert(cache == 1);
    if (frame == &translated) {
        ++translated_frees;
        return;
    }
    assert(frame == input);
}
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
/** @brief Accept the teardown's explicit unkey indication.
 * @param channel Unused channel.
 * @param condition Radio control indication.
 * @return Always succeeds for this fixture.
 */
int ast_indicate(struct ast_channel *channel, int condition) {
    (void)channel;
    assert(condition == AST_CONTROL_RADIO_UNKEY);
    return 0;
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
    struct ast_format linear = {8000}, other = {8000}, another = {8000}, invalid = {0};
    translation_destination = &linear;
    translated_format = &linear;
    struct ra_link_peer peer = {0};
    char topology[64] = "not-empty";
    assert(!ra_link_peer_topology(&peer, topology, sizeof(topology)) && !topology[0]);
    assert(!ra_link_peer_topology(&peer, topology, 0) && !topology[0]);
    assert(ra_link_peer_queue_topology(&peer, "T1") == -1);
    assert(ra_link_peer_start(&peer, NULL, &invalid, NULL, NULL) == -1);
    for (allocation_failure = 1; allocation_failure <= 2; ++allocation_failure) {
        allocation_calls = 0;
        assert(ra_link_peer_start(&peer, NULL, &linear, NULL, NULL) == -1);
    }
    allocation_failure = 0;
    struct ra_link_peer ring_failure = {0};
    rpcr_init_failure = true;
    assert(ra_link_peer_start(&ring_failure, NULL, &linear, NULL, NULL) == -1);
    rpcr_init_failure = false;
    for (failure = 1; failure <= 5; ++failure) {
        mutex_inits = 0;
        allocation_calls = 0;
        assert(ra_link_peer_start(&peer, NULL, &linear, NULL, NULL) == -1);
    }
    failure = 0;
    mutex_inits = 0;
    allocation_calls = 0;
    sent_newkey1s = sent_iaxkeys = 0;
    assert(!ra_link_peer_start(&peer, NULL, &linear, receive_inbound_digit, &inbound_context));
    assert(sent_newkey1s == 1);
    atomic_uint generation;
    atomic_init(&generation, 0);
    peer.topology_generation = &generation;
    assert(ra_link_peer_queue_topology(&peer, NULL) == -1);
    assert(ra_link_peer_queue_topology(&peer, "X1") == -1);
    char overlength[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 2];
    memset(overlength, 'a', sizeof(overlength));
    overlength[0] = 'T';
    overlength[sizeof(overlength) - 1] = '\0';
    assert(ra_link_peer_queue_topology(&peer, overlength) == -1);
    assert(!ra_link_peer_queue_topology(&peer, ""));
    int16_t samples[1600], output[2];
    for (size_t index = 0; index < sizeof(samples) / sizeof(*samples); ++index) {
        samples[index] = index % 2 ? -456 : 123;
    }
    struct ast_frame voice = {.frametype = AST_FRAME_VOICE,
                              .subclass.format = &linear,
                              .data.ptr = samples,
                              .samples = (int)(sizeof(samples) / sizeof(*samples)),
                              .datalen = (int)sizeof(samples)};
    struct ast_frame control = {.frametype = AST_FRAME_CONTROL,
                                .subclass.integer = AST_CONTROL_RADIO_KEY};
    struct ast_frame ignored = {.frametype = AST_FRAME_NULL};
    struct ast_frame dtmf_begin = {.frametype = AST_FRAME_DTMF_BEGIN, .subclass.integer = '5'};
    struct ast_frame dtmf_end = {.frametype = AST_FRAME_DTMF_END, .subclass.integer = '5'};
    /* Before the protected reserve fills, playout remains silent without
     * consuming a partial network callback. */
    assert(!ra_link_peer_receive(&peer, output, sizeof(output) / sizeof(*output)) && !output[0] &&
           !output[1]);
    /* A fresh voice indication keeps one short received fragment active until
     * its next hardware callback, even though the normal reserve is not full. */
    rpcr_write(&peer.received, samples, 1);
    peer.received.primed = true;
    assert(ra_link_peer_receive(&peer, output, sizeof(output) / sizeof(*output)));
    peer.received.primed = false;
    frame(&peer, &dtmf_begin);
    assert(!inbound_digits);
    frame(&peer, &dtmf_end);
    assert(inbound_digits == 1 && inbound_digit == '5');
    assert(peer.inbound_timeout_pending);
    frame(&peer, &ignored);
    assert(inbound_digits == 1 && peer.inbound_timeout_pending);
    peer.inbound_timeout_deadline_ms = 1;
    frame(&peer, &ignored);
    assert(inbound_digits == 2 && !inbound_digit && !peer.inbound_timeout_pending);
    dtmf_end.subclass.integer = '6';
    frame(&peer, &dtmf_end);
    assert(inbound_digits == 3 && inbound_digit == '6' && peer.inbound_timeout_pending);
    dtmf_end.subclass.integer = '#';
    frame(&peer, &dtmf_end);
    assert(inbound_digits == 4 && inbound_digit == '#' && !peer.inbound_timeout_pending);
    peer.inbound_timeout_deadline_ms = 1;
    frame(&peer, &ignored);
    assert(inbound_digits == 4);
    dtmf_end.subclass.integer = 'x';
    frame(&peer, &dtmf_end);
    dtmf_end.subclass.integer = -1;
    frame(&peer, &dtmf_end);
    dtmf_end.subclass.integer = 256;
    frame(&peer, &dtmf_end);
    assert(inbound_digits == 4);
    peer.inbound_digit = NULL;
    dtmf_end.subclass.integer = '*';
    frame(&peer, &dtmf_end);
    assert(inbound_digits == 4 && !peer.inbound_timeout_pending);
    peer.inbound_timeout_pending = true;
    peer.inbound_timeout_deadline_ms = 1;
    frame(&peer, &ignored);
    assert(inbound_digits == 4 && !peer.inbound_timeout_pending);
    peer.inbound_digit = receive_inbound_digit;
    frame(&peer, &ignored);
    assert(sent_topologies == 1 && !strcmp(sent_topology, "L "));
    assert(!ra_link_peer_queue_topology(&peer, "T1"));
    assert(!ra_link_peer_queue_topology(&peer, "R2"));
    frame(&peer, &ignored);
    assert(sent_topologies == 2 && !strcmp(sent_topology, "L R2"));
    assert(!ra_link_peer_queue_topology(&peer, "T3"));
    failure = 11;
    frame(&peer, &ignored);
    failure = 0;
    assert(sent_topologies == 3 && !strcmp(sent_topology, "L T3"));
    frame(&peer, &ignored);
    struct ast_frame text = {.frametype = AST_FRAME_TEXT};
    frame(&peer, &text);
    text.data.ptr = "";
    text.datalen = -1;
    frame(&peer, &text);
    text.datalen = 0;
    frame(&peer, &text);
    text.data.ptr = "X ";
    text.datalen = 2;
    frame(&peer, &text);
    text.data.ptr = "Lx";
    text.datalen = 2;
    frame(&peer, &text);
    text.data.ptr = "!NEWKEY!x";
    text.datalen = 9;
    frame(&peer, &text);
    text.data.ptr = "unknown";
    text.datalen = 7;
    frame(&peer, &text);
    static const char advertised[] = "L T1,RWH6GJL-P,C3,L4";
    text.data.ptr = (char *)advertised;
    text.datalen = sizeof(advertised);
    frame(&peer, &text);
    assert(atomic_load(&generation) == 1);
    frame(&peer, &text);
    assert(atomic_load(&generation) == 1);
    assert(ra_link_peer_topology(&peer, topology, sizeof(topology)) ==
           strlen("T1,RWH6GJL-P,C3,L4"));
    assert(!strcmp(topology, "T1,RWH6GJL-P,C3,L4"));
    char truncated[5] = "xxx";
    assert(ra_link_peer_topology(&peer, truncated, sizeof(truncated)) ==
           strlen("T1,RWH6GJL-P,C3,L4"));
    assert(!strcmp(truncated, "T1,R"));
    assert(ra_link_peer_topology(&peer, NULL, 0) == strlen("T1,RWH6GJL-P,C3,L4"));
    char no_capacity = 'x';
    assert(ra_link_peer_topology(&peer, &no_capacity, 0) == strlen("T1,RWH6GJL-P,C3,L4"));
    assert(no_capacity == 'x');
    char one_byte[1] = {'x'};
    assert(ra_link_peer_topology(&peer, one_byte, sizeof(one_byte)) ==
           strlen("T1,RWH6GJL-P,C3,L4"));
    assert(!one_byte[0]);
    text.data.ptr = "L X1";
    text.datalen = 4;
    frame(&peer, &text);
    text.data.ptr = "L T!";
    text.datalen = 4;
    frame(&peer, &text);
    text.data.ptr = "L R";
    text.datalen = 3;
    frame(&peer, &text);
    text.data.ptr = "L T1,";
    text.datalen = 5;
    frame(&peer, &text);
    text.data.ptr = "L T1, R2";
    text.datalen = 8;
    frame(&peer, &text);
    static const char embedded_nul[] = {'L', ' ', 'T', '1', '\0', 'R', '2'};
    text.data.ptr = (char *)embedded_nul;
    text.datalen = sizeof(embedded_nul);
    frame(&peer, &text);
    char oversized[RA_LINK_TOPOLOGY_TEXT_MAX + 3];
    memset(oversized, 'a', sizeof(oversized));
    oversized[0] = 'L';
    oversized[1] = ' ';
    oversized[2] = 'T';
    text.data.ptr = oversized;
    text.datalen = sizeof(oversized);
    frame(&peer, &text);
    assert(ra_link_peer_topology(&peer, topology, sizeof(topology)) ==
           strlen("T1,RWH6GJL-P,C3,L4"));
    assert(!strcmp(topology, "T1,RWH6GJL-P,C3,L4"));
    text.data.ptr = "L";
    text.datalen = 1;
    frame(&peer, &text);
    assert(!ra_link_peer_topology(&peer, topology, sizeof(topology)) && !topology[0]);
    assert(atomic_load(&generation) == 2);
    text.data.ptr = "L ";
    text.datalen = 2;
    frame(&peer, &text);
    assert(!ra_link_peer_topology(&peer, topology, sizeof(topology)) && !topology[0]);
    assert(atomic_load(&generation) == 2);
    static const char bare_with_nul[] = {'L', '\0'};
    text.data.ptr = (char *)bare_with_nul;
    text.datalen = sizeof(bare_with_nul);
    frame(&peer, &text);
    assert(!ra_link_peer_topology(&peer, topology, sizeof(topology)) && !topology[0]);
    assert(atomic_load(&generation) == 2);
    peer.topology_generation = NULL;
    text.data.ptr = "L T9";
    text.datalen = 4;
    frame(&peer, &text);
    assert(ra_link_peer_topology(&peer, topology, sizeof(topology)) == 2 &&
           !strcmp(topology, "T9"));
    text.data.ptr = "L R8";
    frame(&peer, &text);
    assert(ra_link_peer_topology(&peer, topology, sizeof(topology)) == 2 &&
           !strcmp(topology, "R8"));
    text.data.ptr = "L\t";
    text.datalen = 2;
    frame(&peer, &text);
    assert(ra_link_peer_topology(&peer, topology, sizeof(topology)) == 2 &&
           !strcmp(topology, "R8"));
    assert(atomic_load(&generation) == 2);
    peer.topology_generation = &generation;
    text.data.ptr = "L T10";
    text.datalen = 5;
    frame(&peer, &text);
    assert(atomic_load(&generation) == 3);
    text.data.ptr = "!NEWKEY1!";
    text.datalen = 10;
    frame(&peer, &text);
    frame(&peer, &text);
    assert(sent_newkey1s == 1);
    text.datalen = 9;
    frame(&peer, &text);
    assert(sent_newkey1s == 1);
    text.data.ptr = "!IAXKEY!";
    text.datalen = 8;
    failure = 13;
    frame(&peer, &text);
    failure = 0;
    frame(&peer, &text);
    assert(sent_iaxkeys == 2);
    frame(&peer, &voice);
    frame(&peer, &control);
    /* The persistent sinc converter needs history after the protected reserve.
     * It may initially render silence, but must reach steady playout without
     * advancing the producer-owned receive cursor beyond available PCM. */
    bool rendered = false;
    for (size_t callback = 0; callback < 8; ++callback) {
        assert(ra_link_peer_receive(&peer, output, 2));
        rendered = rendered || output[0] || output[1];
    }
    assert(rendered);
    /* A callback larger than the reserve target must still use only the
     * preallocated converter workspace and retain one safe playout block. */
    int16_t wide[1000] = {0};
    assert(ra_link_peer_receive(&peer, wide, sizeof(wide) / sizeof(*wide)));
    assert(atomic_load(&peer.received.reserve_samples) ==
           peer.received.capacity - sizeof(wide) / sizeof(*wide));
    while (ra_link_peer_receive(&peer, output, 2)) {
    }
    int16_t quiet[32000];
    peer.receive_age = 0;
    peer.seen_epoch = atomic_load(&peer.receive_epoch);
    bool expired_voice = ra_link_peer_receive(&peer, quiet, 400);
    assert(peer.receive_age == 400 && !expired_voice);
    rpcr_write(&peer.received, samples, 2);
    peer.receive_age = linear.rate / 20;
    bool restored = false;
    for (size_t callback = 0; callback < 8; ++callback) {
        if (!ra_link_peer_receive(&peer, output, 2)) {
            break;
        }
        restored = restored || output[0] || output[1];
    }
    assert(restored);
    text.data.ptr = "!NEWKEY!";
    text.datalen = 8;
    frame(&peer, &text);
    frame(&peer, &text);
    assert(sent_newkey1s == 1);
    control.subclass.integer = AST_CONTROL_RADIO_KEY;
    frame(&peer, &control);
    frame(&peer, &voice);
    assert(ra_link_peer_receive(&peer, output, 2));
    linear.rate = 16000;
    assert(ra_link_peer_receive(&peer, quiet, 32000));
    linear.rate = 8000;
    text.data.ptr = "!!DISCONNECT!!";
    text.datalen = 14;
    frame(&peer, &text);
    frame(&peer, NULL);
    frame(&peer, &voice);
    atomic_store(&peer.ended, true);
    assert(!ra_link_peer_receive(&peer, output, 2) && output[0] == 0);
    atomic_store(&peer.ended, false);
    frame(&peer, &control);
    frame(&peer, &voice);
    control.subclass.integer = AST_CONTROL_RADIO_UNKEY;
    frame(&peer, &control);
    assert(ra_link_peer_receive(&peer, output, 2));
    control.subclass.integer = AST_CONTROL_ANSWER;
    frame(&peer, &control);
    control.subclass.integer = AST_CONTROL_HANGUP;
    frame(&peer, &control);
    voice.data.ptr = NULL;
    frame(&peer, &voice);
    voice.data.ptr = samples;
    voice.samples = 0;
    frame(&peer, &voice);
    voice.samples = 4;
    voice.datalen = 3;
    frame(&peer, &voice);
    voice.datalen = 0;
    frame(&peer, &voice);
    voice.datalen = 8;
    voice.subclass.format = &other;
    translation_mode = FIXTURE_TRANSLATOR_UNAVAILABLE;
    frame(&peer, &voice);
    assert(translator_builds == 1 && !translated_inputs && !translated_frees);
    translation_mode = FIXTURE_TRANSLATOR_BUFFERED;
    frame(&peer, &voice);
    assert(translator_builds == 2 && translated_inputs == 1 && !translated_frees);
    translation_mode = FIXTURE_TRANSLATOR_FRAME;
    frame(&peer, &voice);
    assert(translator_builds == 2 && translated_inputs == 2 && translated_frees == 1);
    voice.subclass.format = &another;
    frame(&peer, &voice);
    assert(translator_builds == 3 && translator_frees == 1 && translated_inputs == 3 &&
           translated_frees == 2);
    translated_format = &invalid;
    frame(&peer, &voice);
    assert(translator_builds == 3 && translator_frees == 1 && translated_inputs == 4 &&
           translated_frees == 3);
    translated_format = &linear;
    peer.decode_format = NULL;
    frame(&peer, &voice);
    assert(translator_builds == 4 && translator_frees == 2 && translated_inputs == 5 &&
           translated_frees == 4);
    assert(!ra_link_peer_send(&peer, false, samples, 2));
    failure = 0;
    assert(!ra_link_peer_send(&peer, true, samples, 2));
    assert(!ra_link_peer_send(&peer, true, NULL, 0));
    assert(!ra_link_peer_send(&peer, true, samples, 2));
    assert(!ra_link_peer_send(&peer, false, NULL, 0));
    for (size_t index = 0; index < 100; ++index) {
        assert(!ra_link_peer_send(&peer, true, samples, 2));
    }
    frame(&peer, &ignored);
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
    assert(ra_link_peer_queue_topology(&peer, "T1") == -1);
    assert(ra_link_peer_send(&peer, false, NULL, 0) == -1);
    assert(!ra_link_peer_receive(&peer, output, 2));
    unsigned int inbound_before_no_handler = inbound_digits;
    ra_link_peer_stop(&peer);
    peer = (struct ra_link_peer){0};
    assert(!ra_link_peer_start(&peer, NULL, &linear, NULL, NULL));
    dtmf_end.subclass.integer = '8';
    frame(&peer, &dtmf_end);
    assert(inbound_digits == inbound_before_no_handler);
    assert(!ra_link_peer_send(&peer, true, samples, 2));
    ra_link_peer_stop(&peer);
    assert(hangups == 2);
    return 0;
}
