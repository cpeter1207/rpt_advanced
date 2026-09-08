/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Deterministic mix-minus, ownership, and asynchronous cleanup boundaries.
 */
#include "link_hub.h"
#include "link_peer.h"
#include <assert.h>
#include <asterisk.h>
#include <asterisk/format.h>
#include <asterisk/lock.h>
#include <samplerate.h>
#include <sched.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief Minimal cached-format fixture with an explicit sample rate. */
struct ast_format {
    unsigned int rate; /**< Public sample rate. */
};
/** @brief Concrete signed-linear formats available to the fixture. */
static struct ast_format format_8000 = {.rate = 8000};
/** @brief Concrete signed-linear formats available to the fixture. */
static struct ast_format format_16000 = {.rate = 16000};
/** @brief Concrete signed-linear formats available to the fixture. */
static struct ast_format format_48000 = {.rate = 48000};
/** @brief Concrete signed-linear format used to verify fractional-rate scheduling. */
static struct ast_format format_44100 = {.rate = 44100};
/** @brief Optional intentionally wrong cached linear format for rejection coverage. */
static struct ast_format *cached_format_override;

/** @brief Opaque transport represented by its observable audio and lifecycle. */
struct ast_channel {
    int16_t input;                       /**< Constant incoming sample. */
    int16_t output;                      /**< Most recent outgoing sample. */
    bool active;                         /**< Remote carrier. */
    bool keyed;                          /**< Outgoing carrier. */
    bool stopped;                        /**< Ownership was released. */
    struct ra_link_peer *peer;           /**< Reader state retained by the routing hub. */
    ra_link_peer_digit_fn inbound_digit; /**< Reader-to-control callback installed by the hub. */
    void *inbound_context;               /**< Context paired with inbound_digit. */
    const char *topology;                /**< Latest remote app_rpt `L` payload. */
    char *advertised;                    /**< Caller-owned captured outbound `L` payload. */
    size_t advertised_capacity;          /**< Captured payload capacity. */
    unsigned int advertisements;         /**< Number of reader-owned outbound list queues. */
    size_t received_samples;             /**< Total peer-rate samples consumed by the hub. */
    size_t sent_samples;                 /**< Total peer-rate samples scheduled by the hub. */
    struct ast_format *rawread;          /**< Negotiated inbound wire format. */
    struct ast_format *rawwrite;         /**< Negotiated outbound wire format. */
    bool missing_rawread;                /**< Simulate a transport without an inbound raw format. */
    bool missing_rawwrite; /**< Simulate a transport without an outbound raw format. */
};
/** @brief Return no negotiated format for the transport fixture.
 * @param channel Unused fixture channel.
 * @return Null; the fixture uses the supplied local format.
 */
struct ast_format *ast_channel_readformat(struct ast_channel *channel) {
    (void)channel;
    return NULL;
}
/** @brief Return the raw inbound format selected by IAX negotiation.
 * @param channel Fixture transport.
 * @return Explicit negotiated format or the legacy 8 kHz default.
 */
struct ast_format *ast_channel_rawreadformat(struct ast_channel *channel) {
    if (channel->missing_rawread) {
        return NULL;
    }
    return channel->rawread ? channel->rawread : &format_8000;
}
/** @brief Return the raw outbound format selected by IAX negotiation.
 * @param channel Fixture transport.
 * @return Explicit negotiated format or the legacy 8 kHz default.
 */
struct ast_format *ast_channel_rawwriteformat(struct ast_channel *channel) {
    if (channel->missing_rawwrite) {
        return NULL;
    }
    return channel->rawwrite ? channel->rawwrite : &format_8000;
}
/** @brief Return the matching cached linear format.
 * @param sample_rate Requested rate.
 * @return Cached signed-linear fixture or null.
 */
struct ast_format *ast_format_cache_get_slin_by_rate(unsigned int sample_rate) {
    if (cached_format_override) {
        return cached_format_override;
    }
    switch (sample_rate) {
    case 8000:
        return &format_8000;
    case 16000:
        return &format_16000;
    case 48000:
        return &format_48000;
    case 44100:
        return &format_44100;
    default:
        return NULL;
    }
}
/** @brief Release an optional cached-format fixture reference.
 * @param object Unused object.
 * @param tag Unused allocation tag.
 * @param file Unused source file.
 * @param line Unused source line.
 * @param function Unused source function.
 */
void __ao2_cleanup_debug(void *object, const char *tag, const char *file, int line,
                         const char *function) {
    (void)object;
    (void)tag;
    (void)file;
    (void)line;
    (void)function;
}
/** @brief Allocation sequence. */
static unsigned int allocations;
/** @brief Selected failed allocation. */
static unsigned int failed_allocation;
/** @brief Inject manager creation, peer startup, or write failure. */
static unsigned int failure;
/** @brief Number of sample-rate converter allocations requested by the current fixture case. */
static unsigned int src_new_calls;
/** @brief Converter allocation request that returns null, or zero when disabled. */
static unsigned int src_new_fail_call;
/** @brief Converter allocation request that reports a nonzero status, or zero when disabled. */
static unsigned int src_new_error_call;
/** @brief Fixture radio rate. */
static unsigned int rate = 8000;
/** @brief Held routing lock. */
static bool locked;
/** @brief Captured manager entry and context. */
static void *(*manager)(void *);
/** @brief Captured manager's hub. */
static struct ra_link_hub *managed;
/** @brief Reconnection attempts observed outside hardware routing. */
static unsigned int reconnect_calls;
/** @brief Result injected into a recovery attempt. */
static int reconnect_result;
/** @brief Select cancellation while the recovery callback is active. */
static bool cancel_reconnect;
/** @brief Select disconnect-all while the recovery callback is active. */
static bool pause_reconnect;
/** @brief Make one recovery callback attach the configured replacement peer. */
static bool attach_reconnect;
/** @brief Make the replacement peer end before the recovery callback returns. */
static bool attach_reconnect_ended;
/** @brief Callback result paired with the configured replacement attachment. */
static int attach_reconnect_result;
/** @brief Stable replacement channel used to model a concurrent recovery attachment. */
static struct ast_channel reconnect_attachment;
/** @brief Inject a monotonic-clock read failure. */
static bool fail_clock;
/** @brief Deterministic monotonic scheduling time in milliseconds. */
static uint64_t clock_ms = 1000;
/** @brief Number of 50-millisecond manager idle passes observed in the current fixture run. */
static unsigned int manager_idle_polls;
/** @brief Idle passes allowed before the fixture asks the manager to stop. */
static unsigned int manager_idle_limit = 1;
/** @brief Optional clock value installed after the first manager idle pass. */
static uint64_t clock_after_first_idle;
/** @brief Optional peer whose inbound linked-node list changes after the first idle pass. */
static struct ast_channel *topology_update_peer;
/** @brief Replacement inbound linked-node list used by the control-plane update fixture. */
static const char *topology_update_value;
/** @brief Optional peer whose reader ends after the first idle pass. */
static struct ast_channel *ended_after_first_idle;
/** @brief Optional peer that ends while the manager posts one outbound topology. */
static struct ast_channel *ended_during_advertisement;
/** @brief Opaque identity required by the inbound-IAX DTMF handler fixture. */
static int inbound_context;
/** @brief Number of inbound IAX DTMF events delivered through the hub. */
static unsigned int inbound_digits;
/** @brief Most recently delivered inbound IAX DTMF character. */
static char inbound_digit;
/** @brief Remote identity paired with the most recently delivered inbound DTMF event. */
static char inbound_remote[RA_LINK_PEER_NAME_MAX];
/** @brief Most recently delivered reader timestamp. */
static uint64_t inbound_now_ms;
/** @brief Number of direct-link lifecycle reports received by the control fixture. */
static unsigned int lifecycle_events;
/** @brief Most recently reported lifecycle remote identity. */
static char lifecycle_remote[RA_LINK_PEER_NAME_MAX];
/** @brief Whether the most recent lifecycle report was an attachment. */
static bool lifecycle_connected;
/** @brief Peer whose hardware-paced receive call is deliberately held for reclamation testing. */
static struct ra_link_peer *blocked_receive_peer;
/** @brief The held audio traversal reached its borrowed peer pointer. */
static atomic_bool blocked_receive_entered;
/** @brief Release the held audio traversal after detach has begun. */
static atomic_bool release_blocked_receive;
/** @brief A control thread reached the detached-port reader wait. */
static atomic_bool reclamation_waiting;
/** @brief The forced detach call returned after the audio traversal completed. */
static atomic_bool reclamation_finished;
/** @brief Route fixture thread calls to POSIX for the forced interleaving test. */
static bool real_threads;
/** @brief Preserve the real reader count instead of the fixture's legacy wait shortcut. */
static bool preserve_reader_wait;

/** @brief Initialize a fresh hub fixture and verify its audio publication state.
 * @param hub Caller-owned hub storage.
 */
static void initialize_hub(struct ra_link_hub *hub) {
    ra_link_hub_init(hub);
    assert(!atomic_load_explicit(&hub->ports, memory_order_seq_cst));
    assert(!atomic_load_explicit(&hub->readers, memory_order_seq_cst));
    assert(!atomic_load_explicit(&hub->stop, memory_order_seq_cst));
}

/** @brief Declare and initialize one isolated routing-hub fixture. */
#define RA_TEST_HUB(name)                                                                          \
    struct ra_link_hub name;                                                                       \
    initialize_hub(&(name))

/** @brief Capture a peer-reader DTMF event while verifying it bypasses routing exclusion.
 * @param context Expected control-plane callback identity.
 * @param remote Attached peer identity authenticated by the IAX admission path.
 * @param digit Validated IAX DTMF character.
 * @param now_ms Reader monotonic timestamp.
 */
static void receive_inbound_digit(void *context, const char *remote, char digit, uint64_t now_ms) {
    assert(context == &inbound_context && remote && !locked);
    ++inbound_digits;
    inbound_digit = digit;
    assert(strlen(remote) < sizeof(inbound_remote));
    memcpy(inbound_remote, remote, strlen(remote) + 1);
    inbound_now_ms = now_ms;
}

/** @brief Capture one hub event after it leaves routing lifecycle ownership.
 * @param context Expected control-plane callback identity.
 * @param remote Attached or detached direct-peer identity.
 * @param connected True after attach, false after detach.
 */
static void receive_lifecycle_event(void *context, const char *remote, bool connected) {
    assert(context == &inbound_context && remote && !locked);
    assert(strlen(remote) < sizeof(lifecycle_remote));
    ++lifecycle_events;
    memcpy(lifecycle_remote, remote, strlen(remote) + 1);
    lifecycle_connected = connected;
}

/** @brief Invoke the real libsamplerate state allocator behind a fixture wrapper.
 * @param converter_type Libsamplerate converter selection.
 * @param channels Number of PCM channels.
 * @param error Receives the libsamplerate status code.
 * @return Newly allocated converter state or null.
 */
SRC_STATE *__real_src_new(int converter_type, int channels, int *error);
/** @brief Inject a converter-allocation failure after normal hub setup coverage.
 * @param converter_type Libsamplerate converter selection.
 * @param channels Number of PCM channels.
 * @param error Receives the injected or libsamplerate status code.
 * @return Newly allocated converter state or null for the selected failure.
 */
SRC_STATE *__wrap_src_new(int converter_type, int channels, int *error) {
    ++src_new_calls;
    if (failure == 4 || src_new_calls == src_new_fail_call) {
        *error = 1;
        return NULL;
    }
    SRC_STATE *state = __real_src_new(converter_type, channels, error);
    if (src_new_calls == src_new_error_call) {
        *error = 1;
    }
    return state;
}
/** @brief Invoke libsamplerate's real process entry point behind a fixture wrapper.
 * @param state Converter state.
 * @param data Input and output sample description.
 * @return Libsamplerate status code.
 */
int __real_src_process(SRC_STATE *state, SRC_DATA *data);
/** @brief Inject one sample-rate conversion failure for zero-fill coverage.
 * @param state Converter state.
 * @param data Input and output sample description.
 * @return Injected nonzero failure or libsamplerate status.
 */
int __wrap_src_process(SRC_STATE *state, SRC_DATA *data) {
    return failure == 5 ? 1 : __real_src_process(state, data);
}

/** @brief Supply allocation failure at every ownership boundary.
 * @param count Element count.
 * @param size Element size.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Memory or null.
 */
void *__ast_calloc(size_t count, size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return ++allocations == failed_allocation ? NULL : calloc(count, size);
}

/** @brief Duplicate a node name with injectable failure.
 * @param text Node name.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Owned name or null.
 */
char *__ast_strdup(const char *text, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return ++allocations == failed_allocation ? NULL : strdup(text);
}

/** @brief Release a routing allocation.
 * @param pointer Owned allocation.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 */
void __ast_free(void *pointer, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    free(pointer);
}

/** @brief Track routing exclusion.
 * @param file Caller file.
 * @param line Caller line.
 * @param func Caller function.
 * @param name Lock name.
 * @param lock Opaque lock.
 * @return Zero.
 */
int __ast_pthread_mutex_lock(const char *file, int line, const char *func, const char *name,
                             ast_mutex_t *lock) {
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    assert(!locked);
    locked = true;
    return 0;
}

/** @brief Track balanced routing release.
 * @param file Caller file.
 * @param line Caller line.
 * @param func Caller function.
 * @param name Lock name.
 * @param lock Opaque lock.
 * @return Zero.
 */
int __ast_pthread_mutex_unlock(const char *file, int line, const char *func, const char *name,
                               ast_mutex_t *lock) {
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    assert(locked);
    locked = false;
    return 0;
}

/** @brief Return the fixture format rate.
 * @param format Cached format identity, or null for legacy fixture callers.
 * @return Samples per second.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) {
    return format ? format->rate : rate;
}

/** @brief Invoke POSIX thread creation for the forced reclamation interleaving.
 * @param thread Receives the created thread identity.
 * @param attributes Thread attributes, or null for defaults.
 * @param start Thread entry point.
 * @param argument Entry context.
 * @return POSIX thread-creation status.
 */
int __real_pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                          void *(*start)(void *), void *argument);

/** @brief Invoke POSIX thread joining for the forced reclamation interleaving.
 * @param thread Thread to join.
 * @param result Unused thread-result destination.
 * @return POSIX thread-join status.
 */
int __real_pthread_join(pthread_t thread, void **result);

/** @brief Capture manager startup without scheduling an uncontrolled thread.
 * @param thread Receives calling thread identity.
 * @param attributes Default attributes.
 * @param start Manager entry.
 * @param argument Hub context.
 * @return Injected startup status.
 */
int __wrap_pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                          void *(*start)(void *), void *argument) {
    if (real_threads) {
        return __real_pthread_create(thread, attributes, start, argument);
    }
    assert(!attributes && !locked);
    *thread = pthread_self();
    manager = start;
    managed = argument;
    return failure == 1;
}

/** @brief Complete manager shutdown outside the routing lock.
 * @param thread Calling thread identity.
 * @param result Unused return storage.
 * @return Zero.
 */
int __wrap_pthread_join(pthread_t thread, void **result) {
    if (real_threads) {
        return __real_pthread_join(thread, result);
    }
    assert(pthread_equal(thread, pthread_self()) && !result && !locked);
    assert(atomic_load(&managed->stop));
    assert(!manager(managed));
    return 0;
}

/** @brief Stop after the manager reaches its idle polling boundary.
 * @param delay Bounded poll delay.
 * @param remaining Unused remainder.
 * @return Zero.
 */
int __wrap_nanosleep(const struct timespec *delay, struct timespec *remaining) {
    assert(!remaining && !locked);
    if (delay->tv_nsec == 1000000) {
        if (preserve_reader_wait) {
            atomic_store_explicit(&reclamation_waiting, true, memory_order_seq_cst);
            (void)sched_yield();
        } else {
            atomic_store_explicit(&managed->readers, 0, memory_order_seq_cst);
        }
        return 0;
    }
    assert(delay->tv_nsec == 50000000);
    ++manager_idle_polls;
    if (manager_idle_polls == 1) {
        if (clock_after_first_idle) {
            clock_ms = clock_after_first_idle;
        }
        if (topology_update_peer) {
            topology_update_peer->topology = topology_update_value;
            atomic_fetch_add_explicit(topology_update_peer->peer->topology_generation, 1,
                                      memory_order_release);
            topology_update_peer = NULL;
        }
        if (ended_after_first_idle) {
            atomic_store(&ended_after_first_idle->peer->ended, true);
            ended_after_first_idle = NULL;
        }
    }
    if (manager_idle_polls >= manager_idle_limit) {
        atomic_store(&managed->stop, true);
    }
    return 0;
}

/** @brief Supply deterministic retry scheduling time.
 * @param clock Requested clock identifier.
 * @param value Receives the selected monotonic time.
 * @return Zero, or an injected failure.
 */
int __wrap_clock_gettime(clockid_t clock, struct timespec *value) {
    assert(clock == CLOCK_MONOTONIC && value);
    value->tv_sec = (time_t)(clock_ms / 1000);
    value->tv_nsec = (long)(clock_ms % 1000) * 1000000;
    return fail_clock ? -1 : 0;
}

int ra_link_peer_start(struct ra_link_peer *peer, struct ast_channel *channel,
                       struct ast_format *linear, ra_link_peer_digit_fn inbound_digit,
                       void *inbound_digit_context) {
    assert(!locked);
    if (failure == 2) {
        return -1;
    }
    peer->channel = channel;
    peer->linear = linear;
    channel->peer = peer;
    channel->inbound_digit = inbound_digit;
    channel->inbound_context = inbound_digit_context;
    atomic_init(&peer->ended, false);
    atomic_init(&peer->stop, false);
    return 0;
}

void ra_link_peer_stop(struct ra_link_peer *peer) {
    assert(!locked);
    peer->channel->stopped = true;
    peer->channel->peer = NULL;
    peer->channel = NULL;
}

/* Fixture stub: copy the test channel's already validated app_rpt `L` payload.
 * @param peer Direct peer whose channel provides the fixture payload.
 * @param output Destination string, or null when only measuring.
 * @param capacity Destination capacity.
 * @return Full payload length.
 */
size_t ra_link_peer_topology(struct ra_link_peer *peer, char *output, size_t capacity) {
    assert(locked);
    const char *source = peer->channel->topology ? peer->channel->topology : "";
    size_t length = strlen(source);
    if (output && capacity) {
        size_t copied = length < capacity - 1 ? length : capacity - 1;
        memcpy(output, source, copied);
        output[copied] = '\0';
    }
    return length;
}

/* Fixture stub: capture a bounded outbound `L ` payload without taking channel ownership.
 * @param peer Direct peer selected by the hub manager while routing is locked.
 * @param topology Valid payload without the IAX `L ` prefix.
 * @return Zero unless the fixture peer has already ended.
 */
int ra_link_peer_queue_topology(struct ra_link_peer *peer, const char *topology) {
    assert(locked && topology);
    struct ast_channel *channel = peer->channel;
    assert(strlen(topology) <= RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX);
    ++channel->advertisements;
    if (channel->advertised) {
        assert(strlen(topology) < channel->advertised_capacity);
        memcpy(channel->advertised, topology, strlen(topology) + 1);
    }
    if (ended_during_advertisement) {
        atomic_store(&ended_during_advertisement->peer->ended, true);
        ended_during_advertisement = NULL;
    }
    return atomic_load(&peer->ended) ? -1 : 0;
}

bool ra_link_peer_receive(struct ra_link_peer *peer, int16_t *audio, size_t samples) {
    assert(!locked);
    if (peer == blocked_receive_peer) {
        atomic_store_explicit(&blocked_receive_entered, true, memory_order_seq_cst);
        while (!atomic_load_explicit(&release_blocked_receive, memory_order_seq_cst)) {
            (void)sched_yield();
        }
    }
    peer->channel->received_samples += samples;
    for (size_t i = 0; i < samples; ++i) {
        audio[i] = peer->channel->active ? peer->channel->input : 0;
    }
    return peer->channel->active;
}

int ra_link_peer_send(struct ra_link_peer *peer, bool keyed, const int16_t *audio, size_t samples) {
    assert(!locked);
    peer->channel->keyed = keyed;
    peer->channel->sent_samples += samples;
    if (samples) {
        peer->channel->output = audio[0];
    }
    return failure == 3 ? -1 : 0;
}

/** @cond TEST_FIXTURE */
int ra_link_peer_send_digit(struct ra_link_peer *peer, char digit) {
    assert(locked && strchr("0123456789ABCD*#", digit));
    return atomic_load(&peer->ended) ? -1 : 0;
}
/** @endcond */

/** @brief Observe a recovery callback and optionally model a concurrent replacement attachment.
 * @param context Unused callback context.
 * @param remote Remote peer identity.
 * @param transmit Preserved transmit mode.
 * @param forward Preserved forwarding mode.
 * @param permanent Preserved automatic-recovery setting.
 * @param cancelled Explicit permanent-disconnect flag.
 * @param paused Disconnect-all flag retaining the recovery request.
 * @return Configured recovery result after optional attachment.
 */
static int reconnect_stub(void *context, const char *remote, bool transmit, bool forward,
                          bool permanent, const atomic_bool *cancelled, const atomic_bool *paused) {
    (void)context;
    assert((!strcmp(remote, "3") ? permanent : !strcmp(remote, "4") && !permanent) &&
           !atomic_load(cancelled) && !atomic_load(paused));
    ++reconnect_calls;
    if (attach_reconnect) {
        attach_reconnect = false;
        memset(&reconnect_attachment, 0, sizeof(reconnect_attachment));
        assert(!ra_link_hub_attach(managed, remote, &reconnect_attachment, NULL, transmit, forward,
                                   permanent));
        if (attach_reconnect_ended) {
            atomic_store(&reconnect_attachment.peer->ended, true);
            ra_link_hub_set_reconnector(managed, NULL, NULL);
        }
        return attach_reconnect_result;
    }
    if (cancel_reconnect) {
        assert(ra_link_hub_disconnect_permanent(managed, remote));
        assert(!ra_link_hub_reconnect_all(managed));
    }
    if (pause_reconnect) {
        assert(!ra_link_hub_disconnect_all(managed));
        assert(atomic_load(paused));
    }
    return reconnect_result;
}

/** @brief One hardware-paced invocation held after it has borrowed a published port. */
struct reclamation_process {
    struct ra_link_hub *hub;          /**< Initialized hub whose list is traversed. */
    struct ra_controller *controller; /**< Started controller used by the process call. */
    int16_t *audio;                   /**< One local PCM sample. */
    bool keyed;                       /**< Key result returned by the traversal. */
};

/** @brief One control-plane detach request racing the held audio traversal. */
struct reclamation_disconnect {
    struct ra_link_hub *hub; /**< Initialized hub containing the selected peer. */
    bool detached;           /**< Result returned after safe port reclamation. */
};

/** @brief Process one block until the receive fixture releases its borrowed port.
 * @param argument Reclamation process request.
 * @return Null after the audio traversal exits its sequentially consistent reader section.
 */
static void *process_reclamation(void *argument) {
    struct reclamation_process *request = argument;
    request->keyed =
        ra_link_hub_process(request->hub, request->controller, true, request->audio, 1, 1000);
    return NULL;
}

/** @brief Detach one peer and record completion only after it is safe to release.
 * @param argument Reclamation detach request.
 * @return Null after the control-plane reclamation wait finishes.
 */
static void *disconnect_reclamation(void *argument) {
    struct reclamation_disconnect *request = argument;
    request->detached = ra_link_hub_disconnect(request->hub, "race");
    atomic_store_explicit(&reclamation_finished, true, memory_order_seq_cst);
    return NULL;
}

/** @brief Concurrent sequence publisher used to verify coherent keyed-peer snapshots. */
struct last_keyed_writer {
    struct ra_link_hub *hub; /**< Hub whose keyed-peer generation changes. */
    atomic_bool stop;        /**< Ends the bounded writer thread. */
    atomic_bool started;     /**< Confirms the writer can race a snapshot. */
};

/** @brief Continuously publish a later even generation without making the name empty.
 * @param argument Last-keyed writer request.
 * @return Null after the caller requests termination.
 */
static void *advance_last_keyed_generation(void *argument) {
    struct last_keyed_writer *writer = argument;
    atomic_store_explicit(&writer->started, true, memory_order_seq_cst);
    while (!atomic_load_explicit(&writer->stop, memory_order_seq_cst)) {
        atomic_fetch_add_explicit(&writer->hub->last_keyed_sequence, 2, memory_order_seq_cst);
    }
    return NULL;
}

/** @brief Cover failure cleanup, mix-minus, modes, saturation, and ended-peer collection.
 * @return Zero after assertions.
 */
int main(void) {
    RA_TEST_HUB(hub);
    assert(!ra_link_hub_has_retained_state(&hub));
    ra_link_hub_set_reconnector(&hub, reconnect_stub, NULL);
    assert(!ra_link_hub_disconnect_all(&hub));
    struct ast_channel first = {.active = true, .input = 100};
    struct ast_channel second = {.active = true, .input = 200};
    for (failed_allocation = 1; failed_allocation <= 7; ++failed_allocation) {
        allocations = 0;
        assert(ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false) == -1);
        ra_link_hub_close(&hub);
    }
    failed_allocation = 0;
    for (failure = 1; failure <= 2; ++failure) {
        assert(ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false) == -1);
        ra_link_hub_close(&hub);
    }
    failure = 0;
    ra_link_hub_set_digit_handler(&hub, receive_inbound_digit, &inbound_context);
    assert(!ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false));
    assert(ra_link_hub_has_retained_state(&hub));
    assert(first.inbound_digit && first.inbound_context);
    first.inbound_digit(first.inbound_context, '5');
    assert(inbound_digits == 1 && inbound_digit == '5' && !strcmp(inbound_remote, "1") &&
           inbound_now_ms == clock_ms);
    assert(ra_link_hub_snapshot(&hub, NULL, 0) == 1);
    assert(ra_link_hub_connected(&hub, "1"));
    assert(!ra_link_hub_send_digit(&hub, "1", '1'));
    assert(ra_link_hub_send_digit(&hub, "missing", '1') == -1);
    assert(!ra_link_hub_connected(&hub, "missing"));
    assert(ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false) == -1);
    assert(!ra_link_hub_attach(&hub, "2", &second, NULL, true, true, false));
    first.topology = "T10,R11,C12";
    second.topology = "T20";
    allocations = 0;
    failed_allocation = 1;
    assert(!ra_link_hub_topology(&hub));
    failed_allocation = 0;
    char *topology = ra_link_hub_topology(&hub);
    assert(topology && !strcmp(topology, "T2,T20,T1,T10,R11,C12"));
    ast_free(topology);
    first.topology = NULL;
    second.topology = NULL;
    topology = ra_link_hub_topology(&hub);
    assert(topology && !strcmp(topology, "T2,T1"));
    ast_free(topology);
    char oversized_topology[RA_LINK_TOPOLOGY_TEXT_MAX + 2];
    memset(oversized_topology, 'T', sizeof(oversized_topology) - 1);
    oversized_topology[sizeof(oversized_topology) - 1] = '\0';
    first.topology = oversized_topology;
    topology = ra_link_hub_topology(&hub);
    assert(topology && !strcmp(topology, "T2,T1"));
    ast_free(topology);
    first.topology = "T10,R11,C12";
    second.topology = "T20";
    struct ra_link_peer_status peers[2];
    assert(ra_link_hub_snapshot(&hub, NULL, 0) == 2);
    assert(ra_link_hub_snapshot(&hub, peers, 1) == 2 && !strcmp(peers[0].name, "2") &&
           peers[0].transmit && peers[0].forward && !peers[0].permanent);
    assert(ra_link_hub_snapshot(&hub, peers, 2) == 2 && !strcmp(peers[1].name, "1"));
    /* A disconnected/unfinished peer has no rate basis for a reserve duration. */
    atomic_store(&first.peer->received.reserve_samples, 160);
    first.peer->linear_rate = 0;
    assert(ra_link_hub_snapshot(&hub, peers, 2) == 2 && !peers[1].receive_reserve_ms);
    first.peer->linear_rate = 8000;
    atomic_store(&second.peer->ended, true);
    assert(ra_link_hub_snapshot(&hub, peers, 2) == 1 && !strcmp(peers[0].name, "1"));
    topology = ra_link_hub_topology(&hub);
    assert(topology && !strcmp(topology, "T1,T10,R11,C12"));
    ast_free(topology);
    atomic_store(&second.peer->ended, false);
    char last_keyed[RA_LINK_PEER_NAME_MAX];
    assert(!ra_link_hub_last_keyed(&hub, last_keyed, sizeof(last_keyed)) && !last_keyed[0]);
    assert(!ra_link_hub_last_keyed(&hub, NULL, sizeof(last_keyed)));
    assert(!ra_link_hub_last_keyed(&hub, last_keyed, 1));
    struct ra_controller controller = {.rate = 8000, .full_duplex = true};
    assert(ra_controller_start(&controller, 0));
    int16_t audio[8001] = {50};
    assert(ra_link_hub_process(&hub, &controller, true, audio, 1, 20));
    assert(audio[0] == 350 && first.output == 250 && second.output == 150);
    assert(first.keyed && second.keyed);
    assert(ra_link_hub_last_keyed(&hub, last_keyed, sizeof(last_keyed)) &&
           !strcmp(last_keyed, "1"));
    atomic_store(&hub.last_keyed_sequence, 1);
    assert(!ra_link_hub_last_keyed(&hub, last_keyed, sizeof(last_keyed)));
    atomic_store(&hub.last_keyed_sequence, 2);
    first.active = second.active = false;
    assert(!ra_link_hub_process(&hub, &controller, false, audio, 1, 21));
    second.active = true;
    assert(ra_link_hub_process(&hub, &controller, false, audio, 1, 22));
    assert(ra_link_hub_last_keyed(&hub, last_keyed, sizeof(last_keyed)) &&
           !strcmp(last_keyed, "2"));
    first.active = second.active = true;
    assert(ra_link_hub_process(&hub, &controller, false, NULL, 0, 21));
    assert(!ra_link_hub_process(&hub, &controller, false, audio, 8001, 22));
    first.input = second.input = INT16_MAX;
    audio[0] = INT16_MAX;
    assert(ra_link_hub_process(&hub, &controller, true, audio, 1, 40));
    assert(audio[0] == INT16_MAX && first.output == INT16_MAX);
    first.input = second.input = INT16_MIN;
    audio[0] = INT16_MIN;
    assert(ra_link_hub_process(&hub, &controller, true, audio, 1, 60));
    assert(audio[0] == INT16_MIN && second.output == INT16_MIN);
    assert(!ra_link_hub_disconnect(&hub, "missing"));
    atomic_store(&hub.readers, 1);
    assert(ra_link_hub_disconnect(&hub, "1") && first.stopped);
    first.input = 100;
    second.input = 200;
    assert(!ra_link_hub_attach(&hub, "1", &first, NULL, false, false, false));
    topology = ra_link_hub_topology(&hub);
    assert(topology && !strcmp(topology, "T2,T20"));
    ast_free(topology);
    assert(ra_link_hub_process(&hub, &controller, false, audio, 1, 80));
    assert(audio[0] == 300 && !first.keyed && !second.keyed && !second.output);
    first.active = second.active = false;
    assert(!ra_link_hub_process(&hub, &controller, false, audio, 1, 100));
    failure = 3;
    (void)ra_link_hub_process(&hub, &controller, false, audio, 1, 120);
    assert(atomic_load(&first.peer->stop) && atomic_load(&second.peer->stop));
    atomic_store(&second.peer->ended, true);
    assert(!manager(managed));
    assert(second.stopped && first.peer);
    atomic_store(&first.peer->ended, true);
    atomic_store(&hub.stop, false);
    assert(!manager(managed));
    assert(!hub.ports);
    assert(!ra_link_hub_process(&hub, &controller, false, audio, 1, 140));
    assert(!ra_link_hub_process(&hub, &controller, false, NULL, 0, 141));
    failure = 0;
    assert(!ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false));
    assert(ra_link_hub_disconnect_all(&hub) == 1);
    struct ra_link_peer_status retry;
    /* Snapshot callers may count retained state without storage or with no free slot. */
    assert(ra_link_hub_snapshot(&hub, NULL, 0) == 1);
    assert(ra_link_hub_snapshot(&hub, &retry, 0) == 1);
    assert(ra_link_hub_snapshot(&hub, &retry, 1) == 1 && !strcmp(retry.name, "1") &&
           retry.transmit && retry.forward && !retry.permanent && retry.retrying && retry.paused);
    assert(ra_link_hub_reconnect_all(&hub) == 1);
    assert(ra_link_hub_snapshot(&hub, &retry, 1) == 1 && retry.retrying && !retry.paused);
    /* A just-attached replacement suppresses its obsolete retained retry from the snapshot. */
    assert(!ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false));
    assert(ra_link_hub_snapshot(&hub, &retry, 1) == 1 && !retry.retrying && !retry.paused);
    ra_link_hub_close(&hub);
    assert(!ra_link_hub_has_retained_state(&hub));
    ra_link_hub_close(&hub);

    RA_TEST_HUB(monitor);
    struct ast_channel monitor_peer = {.topology = "T30,R31,C32"};
    assert(!ra_link_hub_attach(&monitor, "3", &monitor_peer, NULL, false, true, false));
    topology = ra_link_hub_topology(&monitor);
    assert(topology && !strcmp(topology, "R3,R30,R31,C32"));
    ast_free(topology);
    ra_link_hub_close(&monitor);

    char first_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    char second_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    RA_TEST_HUB(advertisement);
    struct ast_channel advertised_first = {.topology = "T10",
                                           .advertised = first_advertisement,
                                           .advertised_capacity = sizeof(first_advertisement)};
    struct ast_channel advertised_second = {.topology = "T20",
                                            .advertised = second_advertisement,
                                            .advertised_capacity = sizeof(second_advertisement)};
    assert(!ra_link_hub_attach(&advertisement, "1", &advertised_first, NULL, true, true, false));
    assert(!ra_link_hub_attach(&advertisement, "2", &advertised_second, NULL, true, true, false));
    manager_idle_polls = 0;
    manager_idle_limit = 2;
    clock_ms = 1000;
    atomic_store(&advertisement.stop, false);
    assert(!manager(managed));
    assert(advertised_first.advertisements == 1 && !strcmp(first_advertisement, "T2,T20"));
    assert(advertised_second.advertisements == 1 && !strcmp(second_advertisement, "T1,T10"));
    manager_idle_limit = 1;
    ra_link_hub_close(&advertisement);

    RA_TEST_HUB(periodic);
    struct ast_channel periodic_first = {0};
    struct ast_channel periodic_second = {0};
    assert(!ra_link_hub_attach(&periodic, "1", &periodic_first, NULL, true, true, false));
    assert(!ra_link_hub_attach(&periodic, "2", &periodic_second, NULL, true, true, false));
    manager_idle_polls = 0;
    manager_idle_limit = 2;
    clock_ms = 1000;
    clock_after_first_idle = 31000;
    atomic_store(&periodic.stop, false);
    assert(!manager(managed));
    assert(periodic_first.advertisements == 2 && periodic_second.advertisements == 2);
    clock_after_first_idle = 0;
    manager_idle_limit = 1;
    clock_ms = 1000;
    ra_link_hub_close(&periodic);

    RA_TEST_HUB(updated);
    char update_first_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    char update_second_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    struct ast_channel updated_first = {.topology = "T10",
                                        .advertised = update_first_advertisement,
                                        .advertised_capacity = sizeof(update_first_advertisement)};
    struct ast_channel updated_second = {.advertised = update_second_advertisement,
                                         .advertised_capacity =
                                             sizeof(update_second_advertisement)};
    assert(!ra_link_hub_attach(&updated, "1", &updated_first, NULL, true, true, false));
    assert(!ra_link_hub_attach(&updated, "2", &updated_second, NULL, true, true, false));
    manager_idle_polls = 0;
    manager_idle_limit = 2;
    topology_update_peer = &updated_first;
    topology_update_value = "T99";
    atomic_store(&updated.stop, false);
    assert(!manager(managed));
    assert(updated_first.advertisements == 2 && updated_second.advertisements == 2 &&
           !strcmp(update_second_advertisement, "T1,T99"));
    topology_update_value = NULL;
    manager_idle_limit = 1;
    ra_link_hub_close(&updated);

    RA_TEST_HUB(detached);
    char detached_second_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    struct ast_channel detached_first = {.topology = "T10"};
    struct ast_channel detached_second = {.advertised = detached_second_advertisement,
                                          .advertised_capacity =
                                              sizeof(detached_second_advertisement)};
    assert(!ra_link_hub_attach(&detached, "1", &detached_first, NULL, true, true, false));
    assert(!ra_link_hub_attach(&detached, "2", &detached_second, NULL, true, true, false));
    manager_idle_polls = 0;
    manager_idle_limit = 2;
    ended_after_first_idle = &detached_first;
    atomic_store(&detached.stop, false);
    assert(!manager(managed));
    assert(detached_second.advertisements == 2 && !detached_second_advertisement[0]);
    manager_idle_limit = 1;
    ra_link_hub_close(&detached);

    RA_TEST_HUB(modes);
    char monitor_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    char transceive_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    char local_monitor_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    struct ast_channel monitored = {.topology = "T10",
                                    .advertised = monitor_advertisement,
                                    .advertised_capacity = sizeof(monitor_advertisement)};
    struct ast_channel transceive = {.topology = "T20",
                                     .advertised = transceive_advertisement,
                                     .advertised_capacity = sizeof(transceive_advertisement)};
    struct ast_channel local_monitor = {.advertised = local_monitor_advertisement,
                                        .advertised_capacity = sizeof(local_monitor_advertisement)};
    assert(!ra_link_hub_attach(&modes, "1", &monitored, NULL, false, true, false));
    assert(!ra_link_hub_attach(&modes, "2", &transceive, NULL, true, true, false));
    assert(!ra_link_hub_attach(&modes, "3", &local_monitor, NULL, false, false, false));
    manager_idle_polls = 0;
    atomic_store(&modes.stop, false);
    assert(!manager(managed));
    assert(!strcmp(monitor_advertisement, "T2,T20"));
    assert(!strcmp(transceive_advertisement, "R1,R10"));
    assert(!strcmp(local_monitor_advertisement, "T2,T20,R1,R10"));
    ra_link_hub_close(&modes);

    enum { truncation_peer_count = 160 };
    RA_TEST_HUB(truncated_hub);
    struct ast_channel truncated_peers[truncation_peer_count] = {{0}};
    char truncated_names[truncation_peer_count][RA_LINK_PEER_NAME_MAX];
    char truncated_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    truncated_peers[0].advertised = truncated_advertisement;
    truncated_peers[0].advertised_capacity = sizeof(truncated_advertisement);
    for (size_t index = 0; index < truncation_peer_count; ++index) {
        memset(truncated_names[index], 'a', sizeof(truncated_names[index]) - 1);
        truncated_names[index][0] = 'n';
        truncated_names[index][60] = (char)('0' + index / 100);
        truncated_names[index][61] = (char)('0' + index / 10 % 10);
        truncated_names[index][62] = (char)('0' + index % 10);
        truncated_names[index][63] = '\0';
        assert(!ra_link_hub_attach(&truncated_hub, truncated_names[index], &truncated_peers[index],
                                   NULL, true, true, false));
    }
    manager_idle_polls = 0;
    atomic_store(&truncated_hub.stop, false);
    assert(!manager(managed));
    size_t truncated_length = strlen(truncated_advertisement);
    assert(
        truncated_length <= RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX &&
        truncated_length >= sizeof("R000000") - 1 &&
        !strcmp(truncated_advertisement + truncated_length - (sizeof("R000000") - 1), "R000000"));
    ra_link_hub_close(&truncated_hub);

    RA_TEST_HUB(cached_truncated_hub);
    char cached_truncation[RA_LINK_TOPOLOGY_TEXT_MAX + 1];
    size_t cached_length = 0;
    cached_truncation[cached_length++] = 'T';
    memset(cached_truncation + cached_length, 'a', 4996);
    cached_length += 4996;
    cached_truncation[cached_length++] = ',';
    cached_truncation[cached_length++] = 'T';
    memset(cached_truncation + cached_length, 'b', 4996);
    cached_length += 4996;
    cached_truncation[cached_length++] = ',';
    cached_truncation[cached_length++] = 'T';
    cached_truncation[cached_length++] = '3';
    cached_truncation[cached_length] = '\0';
    assert(cached_length == 9998);
    char cached_truncated_advertisement[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
    struct ast_channel cached_source = {.topology = cached_truncation};
    struct ast_channel cached_recipient = {
        .advertised = cached_truncated_advertisement,
        .advertised_capacity = sizeof(cached_truncated_advertisement),
    };
    assert(
        !ra_link_hub_attach(&cached_truncated_hub, "1", &cached_source, NULL, true, true, false));
    assert(!ra_link_hub_attach(&cached_truncated_hub, "2", &cached_recipient, NULL, true, true,
                               false));
    manager_idle_polls = 0;
    atomic_store(&cached_truncated_hub.stop, false);
    assert(!manager(managed));
    size_t cached_advertised_length = strlen(cached_truncated_advertisement);
    assert(
        cached_advertised_length <= RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX &&
        cached_advertised_length >= sizeof("R000000") - 1 &&
        !strcmp(cached_truncated_advertisement + cached_advertised_length - (sizeof("R000000") - 1),
                "R000000"));
    ra_link_hub_close(&cached_truncated_hub);

    RA_TEST_HUB(ended_hub);
    struct ast_channel skipped_ended = {0};
    struct ast_channel advertised_before_end = {0};
    assert(!ra_link_hub_attach(&ended_hub, "1", &skipped_ended, NULL, true, true, false));
    assert(!ra_link_hub_attach(&ended_hub, "2", &advertised_before_end, NULL, true, true, false));
    ended_during_advertisement = &skipped_ended;
    manager_idle_polls = 0;
    atomic_store(&ended_hub.stop, false);
    assert(!manager(managed));
    assert(advertised_before_end.advertisements == 1 && !skipped_ended.advertisements);
    ra_link_hub_close(&ended_hub);

    RA_TEST_HUB(saturated_topology);
    struct ast_channel saturated_peer = {0};
    assert(!ra_link_hub_attach(&saturated_topology, "1", &saturated_peer, NULL, true, true, false));
    manager_idle_polls = 0;
    manager_idle_limit = 2;
    clock_ms = UINT64_MAX - 1;
    atomic_store(&saturated_topology.stop, false);
    assert(!manager(managed));
    assert(saturated_peer.advertisements == 1);
    clock_ms = 1000;
    manager_idle_limit = 1;
    ra_link_hub_close(&saturated_topology);

    assert(ra_link_hub_retry_delay(0) == 1000);
    assert(ra_link_hub_retry_delay(1) == 1000);
    assert(ra_link_hub_retry_delay(1000) == 2000);
    assert(ra_link_hub_retry_delay(150000) == 300000);
    assert(ra_link_hub_retry_delay(300000) == 300000);

    /* A permanent command may fail before it has a port. It still records an automatic retry,
     * runs at once, then uses the same one-second exponential schedule as an ended peer. */
    RA_TEST_HUB(initial_unavailable);
    assert(!ra_link_hub_retain_permanent(&initial_unavailable, "3", true, true));
    ra_link_hub_close(&initial_unavailable);

    RA_TEST_HUB(initial_allocation_failure);
    ra_link_hub_set_reconnector(&initial_allocation_failure, reconnect_stub, NULL);
    allocations = 0;
    failed_allocation = 1;
    assert(!ra_link_hub_retain_permanent(&initial_allocation_failure, "3", true, true));
    failed_allocation = 0;
    ra_link_hub_close(&initial_allocation_failure);

    RA_TEST_HUB(initial_manager_failure);
    ra_link_hub_set_reconnector(&initial_manager_failure, reconnect_stub, NULL);
    failure = 1;
    assert(!ra_link_hub_retain_permanent(&initial_manager_failure, "3", true, true));
    failure = 0;
    assert(!ra_link_hub_snapshot(&initial_manager_failure, NULL, 0));
    ra_link_hub_close(&initial_manager_failure);

    RA_TEST_HUB(initial_duplicate);
    struct ast_channel initial_duplicate_peer = {.active = true, .input = 225};
    ra_link_hub_set_reconnector(&initial_duplicate, reconnect_stub, NULL);
    assert(!ra_link_hub_attach(&initial_duplicate, "3", &initial_duplicate_peer, NULL, true, true,
                               false));
    assert(!ra_link_hub_retain_permanent(&initial_duplicate, "3", true, true));
    ra_link_hub_close(&initial_duplicate);

    RA_TEST_HUB(initial_retry);
    struct ra_link_peer_status initial_status = {0};
    ra_link_hub_set_reconnector(&initial_retry, reconnect_stub, NULL);
    reconnect_calls = 0;
    reconnect_result = -1;
    cancel_reconnect = false;
    pause_reconnect = false;
    attach_reconnect = false;
    attach_reconnect_ended = false;
    clock_ms = 1000;
    assert(ra_link_hub_retain_permanent(&initial_retry, "3", true, true));
    assert(!ra_link_hub_retain_permanent(&initial_retry, "3", true, true));
    assert(ra_link_hub_snapshot(&initial_retry, &initial_status, 1) == 1 &&
           !strcmp(initial_status.name, "3") && initial_status.transmit && initial_status.forward &&
           initial_status.permanent && initial_status.retrying && !initial_status.paused);
    manager_idle_polls = 0;
    manager_idle_limit = 2;
    clock_after_first_idle = 2000;
    atomic_store(&initial_retry.stop, false);
    assert(!manager(managed));
    assert(reconnect_calls == 2);
    assert(ra_link_hub_snapshot(&initial_retry, &initial_status, 1) == 1 &&
           initial_status.retrying && !initial_status.paused);
    assert(ra_link_hub_disconnect_permanent(&initial_retry, "3"));
    assert(!ra_link_hub_snapshot(&initial_retry, NULL, 0));
    clock_after_first_idle = 0;
    manager_idle_limit = 1;
    ra_link_hub_close(&initial_retry);

    /* A control-plane callback may be removed after a retry is retained. The manager must
     * consume that request safely instead of dereferencing a stale callback. */
    RA_TEST_HUB(callback_removed);
    ra_link_hub_set_reconnector(&callback_removed, reconnect_stub, NULL);
    assert(ra_link_hub_retain_permanent(&callback_removed, "3", true, true));
    ra_link_hub_set_reconnector(&callback_removed, NULL, NULL);
    manager_idle_polls = 0;
    atomic_store(&callback_removed.stop, false);
    assert(!manager(managed));
    ra_link_hub_close(&callback_removed);

    /* A permanent peer can end before a runtime installs recovery support. The manager must
     * release it without retaining a retry record. */
    RA_TEST_HUB(no_recovery);
    struct ast_channel no_recovery_peer = {.active = true, .input = 250};
    assert(!ra_link_hub_attach(&no_recovery, "3", &no_recovery_peer, NULL, true, true, true));
    atomic_store(&no_recovery_peer.peer->ended, true);
    manager_idle_polls = 0;
    atomic_store(&no_recovery.stop, false);
    assert(!manager(managed));
    assert(!ra_link_hub_has_retained_state(&no_recovery));
    ra_link_hub_close(&no_recovery);

    /* A replacement may arrive while its failed dial callback is unwinding. A live replacement
     * consumes the stale retry; an ended one is collected without dereferencing it as live. */
    RA_TEST_HUB(callback_replacement);
    struct ast_channel callback_peer = {.active = true, .input = 260};
    ra_link_hub_set_reconnector(&callback_replacement, reconnect_stub, NULL);
    reconnect_calls = 0;
    reconnect_result = -1;
    attach_reconnect = true;
    attach_reconnect_result = 0;
    attach_reconnect_ended = false;
    assert(!ra_link_hub_attach(&callback_replacement, "3", &callback_peer, NULL, true, true, true));
    atomic_store(&callback_peer.peer->ended, true);
    manager_idle_polls = 0;
    atomic_store(&callback_replacement.stop, false);
    assert(!manager(managed));
    assert(reconnect_calls == 1 && ra_link_hub_connected(&callback_replacement, "3"));
    ra_link_hub_close(&callback_replacement);

    RA_TEST_HUB(callback_race);
    struct ast_channel callback_race_peer = {.active = true, .input = 270};
    ra_link_hub_set_reconnector(&callback_race, reconnect_stub, NULL);
    reconnect_calls = 0;
    attach_reconnect = true;
    attach_reconnect_result = -1;
    attach_reconnect_ended = false;
    assert(!ra_link_hub_attach(&callback_race, "3", &callback_race_peer, NULL, true, true, true));
    atomic_store(&callback_race_peer.peer->ended, true);
    manager_idle_polls = 0;
    atomic_store(&callback_race.stop, false);
    assert(!manager(managed));
    assert(reconnect_calls == 1 && ra_link_hub_connected(&callback_race, "3"));
    ra_link_hub_close(&callback_race);

    RA_TEST_HUB(callback_ended);
    struct ast_channel callback_ended_peer = {.active = true, .input = 280};
    ra_link_hub_set_reconnector(&callback_ended, reconnect_stub, NULL);
    reconnect_calls = 0;
    attach_reconnect = true;
    attach_reconnect_result = -1;
    attach_reconnect_ended = true;
    assert(!ra_link_hub_attach(&callback_ended, "3", &callback_ended_peer, NULL, true, true, true));
    atomic_store(&callback_ended_peer.peer->ended, true);
    manager_idle_polls = 0;
    manager_idle_limit = 2;
    clock_after_first_idle = 2000;
    atomic_store(&callback_ended.stop, false);
    assert(!manager(managed));
    assert(reconnect_calls == 1 && !ra_link_hub_connected(&callback_ended, "3") &&
           ra_link_hub_has_retained_state(&callback_ended));
    ra_link_hub_close(&callback_ended);
    attach_reconnect_ended = false;
    clock_after_first_idle = 0;
    manager_idle_limit = 1;

    RA_TEST_HUB(recovery);
    struct ast_channel third = {.active = true, .input = 300};
    ra_link_hub_set_reconnector(&recovery, reconnect_stub, NULL);
    reconnect_calls = 0;
    reconnect_result = -1;
    cancel_reconnect = false;
    assert(!ra_link_hub_attach(&recovery, "3", &third, NULL, true, true, true));
    atomic_store(&third.peer->ended, true);
    assert(!manager(managed));
    assert(ra_link_hub_has_retained_state(&recovery));
    assert(reconnect_calls == 1 && ra_link_hub_reconnect_all(&recovery) == 1);
    struct ast_channel replacement = {.active = true, .input = 301};
    struct ast_channel unrelated = {.active = true, .input = 304};
    assert(!ra_link_hub_attach(&recovery, "3", &replacement, NULL, true, true, true));
    assert(!ra_link_hub_attach(&recovery, "4", &unrelated, NULL, false, false, false));
    atomic_store(&recovery.stop, false);
    assert(!manager(managed));
    assert(!ra_link_hub_reconnect_all(&recovery));
    assert(ra_link_hub_disconnect_permanent(&recovery, "3"));
    assert(!ra_link_hub_reconnect_all(&recovery));
    ra_link_hub_close(&recovery);

    RA_TEST_HUB(duplicate_retry);
    struct ast_channel duplicate_first = {.active = true, .input = 302};
    struct ast_channel duplicate_second = {.active = true, .input = 303};
    ra_link_hub_set_reconnector(&duplicate_retry, reconnect_stub, NULL);
    reconnect_calls = 0;
    assert(!ra_link_hub_attach(&duplicate_retry, "3", &duplicate_first, NULL, true, true, true));
    atomic_store(&duplicate_first.peer->ended, true);
    assert(!manager(managed));
    assert(!ra_link_hub_attach(&duplicate_retry, "3", &duplicate_second, NULL, true, true, true));
    assert(ra_link_hub_disconnect_all(&duplicate_retry) == 1);
    assert(ra_link_hub_reconnect_all(&duplicate_retry) == 1);
    assert(!ra_link_hub_disconnect_permanent(&duplicate_retry, "4"));
    ra_link_hub_close(&duplicate_retry);

    RA_TEST_HUB(reconnect_all);
    struct ast_channel fourth = {.active = true, .input = 400};
    struct ast_channel fifth = {.active = true, .input = 500};
    ra_link_hub_set_reconnector(&reconnect_all, reconnect_stub, NULL);
    reconnect_calls = 0;
    assert(!ra_link_hub_attach(&reconnect_all, "3", &fourth, NULL, true, true, true));
    assert(!ra_link_hub_attach(&reconnect_all, "4", &fifth, NULL, false, false, false));
    assert(!ra_link_hub_disconnect(&reconnect_all, "3"));
    assert(ra_link_hub_disconnect_all(&reconnect_all) == 2);
    assert(!ra_link_hub_disconnect_permanent(&reconnect_all, "4"));
    assert(!manager(managed));
    assert(!reconnect_calls);
    atomic_store(&reconnect_all.stop, false);
    assert(ra_link_hub_reconnect_all(&reconnect_all) == 2);
    assert(!manager(managed));
    assert(reconnect_calls == 2 && ra_link_hub_reconnect_all(&reconnect_all) == 1);
    assert(ra_link_hub_disconnect_permanent(&reconnect_all, "3"));
    assert(!ra_link_hub_reconnect_all(&reconnect_all));
    ra_link_hub_close(&reconnect_all);

    RA_TEST_HUB(cancelled);
    struct ast_channel sixth = {.active = true, .input = 600};
    ra_link_hub_set_reconnector(&cancelled, reconnect_stub, NULL);
    reconnect_calls = 0;
    cancel_reconnect = true;
    assert(!ra_link_hub_attach(&cancelled, "3", &sixth, NULL, true, true, true));
    atomic_store(&sixth.peer->ended, true);
    assert(!manager(managed));
    assert(reconnect_calls == 1 && !ra_link_hub_reconnect_all(&cancelled));
    cancel_reconnect = false;
    ra_link_hub_close(&cancelled);

    RA_TEST_HUB(paused_retry);
    struct ast_channel paused_channel = {.active = true, .input = 650};
    ra_link_hub_set_reconnector(&paused_retry, reconnect_stub, NULL);
    reconnect_calls = 0;
    pause_reconnect = true;
    assert(!ra_link_hub_attach(&paused_retry, "3", &paused_channel, NULL, true, true, true));
    atomic_store(&paused_channel.peer->ended, true);
    assert(!manager(managed));
    assert(reconnect_calls == 1);
    pause_reconnect = false;
    atomic_store(&paused_retry.stop, false);
    assert(ra_link_hub_reconnect_all(&paused_retry) == 1);
    assert(!manager(managed));
    assert(ra_link_hub_disconnect_permanent(&paused_retry, "3"));
    ra_link_hub_close(&paused_retry);

    RA_TEST_HUB(active_permanent);
    struct ast_channel seventh = {.active = true, .input = 700};
    ra_link_hub_set_reconnector(&active_permanent, reconnect_stub, NULL);
    assert(!ra_link_hub_attach(&active_permanent, "3", &seventh, NULL, true, true, true));
    assert(ra_link_hub_disconnect_permanent(&active_permanent, "3") && seventh.stopped);
    ra_link_hub_close(&active_permanent);

    RA_TEST_HUB(retry_allocation_failure);
    struct ast_channel eighth = {.active = true, .input = 800};
    allocations = 0;
    failed_allocation = 0;
    ra_link_hub_set_reconnector(&retry_allocation_failure, reconnect_stub, NULL);
    assert(!ra_link_hub_attach(&retry_allocation_failure, "3", &eighth, NULL, true, true, true));
    failed_allocation = allocations + 1;
    atomic_store(&eighth.peer->ended, true);
    assert(!manager(managed));
    assert(!ra_link_hub_reconnect_all(&retry_allocation_failure));
    failed_allocation = 0;
    ra_link_hub_close(&retry_allocation_failure);

    RA_TEST_HUB(retry_name_failure);
    struct ast_channel ninth = {.active = true, .input = 900};
    allocations = 0;
    ra_link_hub_set_reconnector(&retry_name_failure, reconnect_stub, NULL);
    assert(!ra_link_hub_attach(&retry_name_failure, "3", &ninth, NULL, true, true, true));
    failed_allocation = allocations + 2;
    atomic_store(&ninth.peer->ended, true);
    assert(!manager(managed));
    assert(!ra_link_hub_reconnect_all(&retry_name_failure));
    failed_allocation = 0;
    ra_link_hub_close(&retry_name_failure);

    RA_TEST_HUB(clock_failure);
    struct ast_channel tenth = {.active = true, .input = 1000};
    ra_link_hub_set_reconnector(&clock_failure, reconnect_stub, NULL);
    assert(!ra_link_hub_attach(&clock_failure, "3", &tenth, NULL, true, true, true));
    fail_clock = true;
    atomic_store(&tenth.peer->ended, true);
    assert(!manager(managed));
    fail_clock = false;
    assert(ra_link_hub_disconnect_permanent(&clock_failure, "3"));
    ra_link_hub_close(&clock_failure);

    RA_TEST_HUB(saturated_delay);
    struct ast_channel eleventh = {.active = true, .input = 1100};
    ra_link_hub_set_reconnector(&saturated_delay, reconnect_stub, NULL);
    assert(!ra_link_hub_attach(&saturated_delay, "3", &eleventh, NULL, true, true, true));
    clock_ms = UINT64_MAX - 500;
    atomic_store(&eleventh.peer->ended, true);
    assert(!manager(managed));
    clock_ms = 1000;
    assert(ra_link_hub_disconnect_permanent(&saturated_delay, "3"));
    ra_link_hub_close(&saturated_delay);

    /* A negotiated legacy peer remains at 8 kHz while the radio controller
     * stays at its native 48 kHz rate. A second 16 kHz peer covers both rate
     * directions through the same shared boundary converter. */
    struct ast_format zero_format = {0};
    struct ast_format unavailable_format = {.rate = 12345};
    struct ast_channel asymmetric = {.rawread = &format_8000, .rawwrite = &format_16000};
    struct ast_channel unavailable = {.rawread = &unavailable_format,
                                      .rawwrite = &unavailable_format};
    struct ast_channel zero_wire = {.rawread = &zero_format, .rawwrite = &zero_format};
    struct ast_channel missing_read = {.missing_rawread = true};
    struct ast_channel missing_write = {.missing_rawwrite = true};
    struct ast_channel cache_mismatch = {.rawread = &format_16000, .rawwrite = &format_16000};
    RA_TEST_HUB(invalid_hub);
    assert(ra_link_hub_attach(&invalid_hub, "bad", &asymmetric, &format_48000, true, true, false) ==
           -1);
    assert(ra_link_hub_attach(&invalid_hub, "bad", &unavailable, &format_48000, true, true,
                              false) == -1);
    assert(ra_link_hub_attach(&invalid_hub, "bad", &unavailable, &zero_format, true, true, false) ==
           -1);
    assert(ra_link_hub_attach(&invalid_hub, "bad", &zero_wire, &format_48000, true, true, false) ==
           -1);
    assert(ra_link_hub_attach(&invalid_hub, "bad", &missing_read, &format_48000, true, true,
                              false) == -1);
    assert(ra_link_hub_attach(&invalid_hub, "bad", &missing_write, &format_48000, true, true,
                              false) == -1);
    cached_format_override = &format_8000;
    assert(ra_link_hub_attach(&invalid_hub, "bad", &cache_mismatch, &format_48000, true, true,
                              false) == -1);
    cached_format_override = NULL;

    RA_TEST_HUB(rates);
    struct ast_channel legacy = {
        .active = true, .input = 12000, .rawread = &format_8000, .rawwrite = &format_8000};
    struct ast_channel wide = {
        .active = true, .input = 4000, .rawread = &format_16000, .rawwrite = &format_16000};
    src_new_calls = 0;
    failure = 4;
    assert(ra_link_hub_attach(&rates, "8", &legacy, &format_48000, true, true, false) == -1);
    failure = 0;
    src_new_calls = 0;
    src_new_fail_call = 2;
    assert(ra_link_hub_attach(&rates, "8", &legacy, &format_48000, true, true, false) == -1);
    src_new_fail_call = 0;
    src_new_calls = 0;
    src_new_error_call = 2;
    assert(ra_link_hub_attach(&rates, "8", &legacy, &format_48000, true, true, false) == -1);
    src_new_error_call = 0;
    assert(!ra_link_hub_attach(&rates, "8", &legacy, &format_48000, true, true, false));
    assert(!ra_link_hub_attach(&rates, "16", &wide, &format_48000, true, true, false));
    assert(rates.rate == 48000 && legacy.peer->linear == &format_8000 &&
           wide.peer->linear == &format_16000);
    struct ast_channel incompatible = {.rawread = &format_8000, .rawwrite = &format_8000};
    assert(ra_link_hub_attach(&rates, "bad", &incompatible, &format_16000, true, true, false) ==
           -1);
    struct ra_controller rate_controller = {.rate = 48000, .full_duplex = true};
    assert(ra_controller_start(&rate_controller, 0));
    int16_t rate_audio[1920] = {0};
    (void)ra_link_hub_process(&rates, &rate_controller, false, rate_audio, 1, 199);
    for (size_t iteration = 0; iteration < 10; ++iteration) {
        memset(rate_audio, 0, sizeof(rate_audio));
        (void)ra_link_hub_process(&rates, &rate_controller, false, rate_audio, 960,
                                  200 + iteration);
    }
    assert(abs(legacy.output - wide.input) < 512 && abs(wide.output - legacy.input) < 512);
    memset(rate_audio, 0, sizeof(rate_audio));
    (void)ra_link_hub_process(&rates, &rate_controller, false, rate_audio,
                              sizeof(rate_audio) / sizeof(*rate_audio), 220);
    failure = 5;
    memset(rate_audio, 0, sizeof(rate_audio));
    (void)ra_link_hub_process(&rates, &rate_controller, false, rate_audio, 960, 240);
    assert(!legacy.output && !wide.output);
    failure = 0;
    ra_link_hub_close(&rates);

    /* Fractional 44.1 kHz scheduling must remain exact instead of rounding
     * every 1 ms local block upward and gradually running the peer too fast. */
    RA_TEST_HUB(fractional);
    struct ast_channel peer_44100 = {.rawread = &format_44100, .rawwrite = &format_44100};
    assert(!ra_link_hub_attach(&fractional, "441", &peer_44100, &format_48000, true, true, false));
    struct ra_controller fractional_controller = {.rate = 48000, .full_duplex = true};
    assert(ra_controller_start(&fractional_controller, 0));
    int16_t fractional_audio[48] = {0};
    for (size_t iteration = 0; iteration < 10; ++iteration) {
        (void)ra_link_hub_process(&fractional, &fractional_controller, false, fractional_audio,
                                  sizeof(fractional_audio) / sizeof(*fractional_audio),
                                  300 + iteration);
    }
    assert(peer_44100.received_samples == 441 && peer_44100.sent_samples == 441);
    ra_link_hub_close(&fractional);

    /* A maximum-length peer identity must remain terminated in the lock-free keyed-peer report. */
    RA_TEST_HUB(max_keyed);
    char maximum_name[RA_LINK_PEER_NAME_MAX];
    memset(maximum_name, 'k', sizeof(maximum_name) - 1);
    maximum_name[sizeof(maximum_name) - 1] = '\0';
    struct ast_channel maximum_peer = {.active = true, .input = 700};
    struct ra_controller maximum_controller = {.rate = 8000, .full_duplex = true};
    int16_t maximum_audio[] = {0};
    assert(ra_controller_start(&maximum_controller, 0));
    assert(!ra_link_hub_attach(&max_keyed, maximum_name, &maximum_peer, &format_8000, true, true,
                               false));
    assert(ra_link_hub_process(&max_keyed, &maximum_controller, false, maximum_audio,
                               sizeof(maximum_audio) / sizeof(*maximum_audio), 250));
    assert(ra_link_hub_last_keyed(&max_keyed, last_keyed, sizeof(last_keyed)) &&
           !strcmp(last_keyed, maximum_name));
    struct last_keyed_writer keyed_writer = {.hub = &max_keyed};
    atomic_init(&keyed_writer.stop, false);
    atomic_init(&keyed_writer.started, false);
    real_threads = true;
    pthread_t keyed_writer_thread;
    assert(
        !pthread_create(&keyed_writer_thread, NULL, advance_last_keyed_generation, &keyed_writer));
    while (!atomic_load_explicit(&keyed_writer.started, memory_order_seq_cst)) {
        (void)sched_yield();
    }
    bool generation_changed = false;
    for (size_t attempt = 0; attempt < 100000 && !generation_changed; ++attempt) {
        (void)sched_yield();
        generation_changed = !ra_link_hub_last_keyed(&max_keyed, last_keyed, sizeof(last_keyed));
    }
    atomic_store_explicit(&keyed_writer.stop, true, memory_order_seq_cst);
    assert(!pthread_join(keyed_writer_thread, NULL));
    real_threads = false;
    assert(generation_changed);
    ra_link_hub_close(&max_keyed);

    RA_TEST_HUB(detached_reconnect);
    struct ast_channel reconnect_peer = {0};
    assert(!ra_link_hub_attach(&detached_reconnect, "reconnect", &reconnect_peer, &format_8000,
                               true, true, true));
    assert(!ra_link_hub_detach_reconnect(&detached_reconnect, "missing", true));
    assert(!ra_link_hub_detach_reconnect(&detached_reconnect, "reconnect", false));
    assert(ra_link_hub_detach_reconnect(&detached_reconnect, "reconnect", true));
    assert(reconnect_peer.stopped);
    ra_link_hub_close(&detached_reconnect);

    /* Lifecycle reports leave the routing lock and identify both attach and detach. */
    RA_TEST_HUB(lifecycle);
    struct ast_channel lifecycle_peer = {0};
    lifecycle_events = 0;
    ra_link_hub_set_event_handler(&lifecycle, receive_lifecycle_event, &inbound_context);
    assert(!ra_link_hub_attach(&lifecycle, "events", &lifecycle_peer, &format_8000, true, true,
                               false));
    assert(lifecycle_events == 1 && !strcmp(lifecycle_remote, "events") && lifecycle_connected);
    assert(ra_link_hub_disconnect(&lifecycle, "events"));
    assert(lifecycle_events == 2 && !lifecycle_connected);
    ra_link_hub_close(&lifecycle);

    /* A hub without a configured runtime control recipient must safely discard peer DTMF. */
    RA_TEST_HUB(no_digit);
    struct ast_channel no_digit_peer = {0};
    assert(
        !ra_link_hub_attach(&no_digit, "discard", &no_digit_peer, &format_8000, true, true, false));
    assert(no_digit_peer.inbound_digit && no_digit_peer.inbound_context);
    no_digit_peer.inbound_digit(no_digit_peer.inbound_context, '8');
    assert(inbound_digits == 1);
    ra_link_hub_close(&no_digit);

    /* The audio thread enters its sequentially consistent reader section, then blocks after
     * borrowing the port. Detach must unlink it but cannot free it until that traversal exits. */
    RA_TEST_HUB(reclamation);
    struct ast_channel reclamation_peer = {.active = true, .input = 1200};
    struct ra_controller reclamation_controller = {.rate = 8000, .full_duplex = true};
    assert(ra_controller_start(&reclamation_controller, 0));
    assert(!ra_link_hub_attach(&reclamation, "race", &reclamation_peer, &format_8000, true, true,
                               false));
    int16_t reclamation_audio[] = {100};
    struct reclamation_process process = {
        .hub = &reclamation, .controller = &reclamation_controller, .audio = reclamation_audio};
    struct reclamation_disconnect disconnect = {.hub = &reclamation};
    blocked_receive_peer = reclamation_peer.peer;
    atomic_store_explicit(&blocked_receive_entered, false, memory_order_seq_cst);
    atomic_store_explicit(&release_blocked_receive, false, memory_order_seq_cst);
    atomic_store_explicit(&reclamation_waiting, false, memory_order_seq_cst);
    atomic_store_explicit(&reclamation_finished, false, memory_order_seq_cst);
    preserve_reader_wait = true;
    real_threads = true;
    pthread_t audio_thread;
    pthread_t detach_thread;
    assert(!pthread_create(&audio_thread, NULL, process_reclamation, &process));
    while (!atomic_load_explicit(&blocked_receive_entered, memory_order_seq_cst)) {
        (void)sched_yield();
    }
    assert(!pthread_create(&detach_thread, NULL, disconnect_reclamation, &disconnect));
    while (!atomic_load_explicit(&reclamation_waiting, memory_order_seq_cst)) {
        (void)sched_yield();
    }
    assert(!atomic_load_explicit(&reclamation_finished, memory_order_seq_cst));
    assert(!reclamation_peer.stopped);
    atomic_store_explicit(&release_blocked_receive, true, memory_order_seq_cst);
    assert(!pthread_join(audio_thread, NULL));
    assert(!pthread_join(detach_thread, NULL));
    assert(process.keyed && disconnect.detached && reclamation_peer.stopped);
    real_threads = false;
    preserve_reader_wait = false;
    blocked_receive_peer = NULL;
    ra_link_hub_close(&reclamation);
    return 0;
}
