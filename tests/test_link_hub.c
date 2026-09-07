/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Deterministic mix-minus, ownership, and asynchronous cleanup boundaries.
 */
#include "link_hub.h"
#include "link_peer.h"
#include <assert.h>
#include <asterisk.h>
#include <asterisk/format.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief Opaque transport represented by its observable audio and lifecycle. */
struct ast_channel {
    int16_t input;             /**< Constant incoming sample. */
    int16_t output;            /**< Most recent outgoing sample. */
    bool active;               /**< Remote carrier. */
    bool keyed;                /**< Outgoing carrier. */
    bool stopped;              /**< Ownership was released. */
    struct ra_link_peer *peer; /**< Reader state retained by the routing hub. */
};
/** @brief Allocation sequence. */
static unsigned int allocations;
/** @brief Selected failed allocation. */
static unsigned int failed_allocation;
/** @brief Inject manager creation, peer startup, or write failure. */
static unsigned int failure;
/** @brief Fixture radio rate. */
static unsigned int rate = 8000;
/** @brief Held routing lock. */
static bool locked;
/** @brief Captured manager entry and context. */
static void *(*manager)(void *);
/** @brief Captured manager's hub. */
static struct ra_link_hub *managed;

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

/** @brief Return the fixture radio rate.
 * @param format Unused cache identity.
 * @return Samples per second.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) {
    (void)format;
    return rate;
}

/** @brief Capture manager startup without scheduling an uncontrolled thread.
 * @param thread Receives calling thread identity.
 * @param attributes Default attributes.
 * @param start Manager entry.
 * @param argument Hub context.
 * @return Injected startup status.
 */
int __wrap_pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                          void *(*start)(void *), void *argument) {
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
    assert(delay->tv_nsec == 50000000 && !remaining && !locked);
    atomic_store(&managed->stop, true);
    return 0;
}

int ra_link_peer_start(struct ra_link_peer *peer, struct ast_channel *channel,
                       struct ast_format *linear) {
    (void)linear;
    assert(!locked);
    if (failure == 2) {
        return -1;
    }
    peer->channel = channel;
    channel->peer = peer;
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

bool ra_link_peer_receive(struct ra_link_peer *peer, int16_t *audio, size_t samples) {
    assert(locked);
    for (size_t i = 0; i < samples; ++i) {
        audio[i] = peer->channel->active ? peer->channel->input : 0;
    }
    return peer->channel->active;
}

int ra_link_peer_send(struct ra_link_peer *peer, bool keyed, const int16_t *audio, size_t samples) {
    assert(locked && samples);
    peer->channel->keyed = keyed;
    peer->channel->output = audio[0];
    return failure == 3 ? -1 : 0;
}

/** @brief Accept a recovery callback without creating a transport.
 * @param context Unused callback context.
 * @param remote Unused peer identity.
 * @param transmit Unused transmit mode.
 * @param forward Unused forwarding mode.
 * @return Zero.
 */
static int reconnect_stub(void *context, const char *remote, bool transmit, bool forward) {
    (void)context;
    (void)remote;
    (void)transmit;
    (void)forward;
    return 0;
}

/** @brief Cover failure cleanup, mix-minus, modes, saturation, and ended-peer collection.
 * @return Zero after assertions.
 */
int main(void) {
    struct ra_link_hub hub = {0};
    ra_link_hub_set_reconnector(&hub, reconnect_stub, NULL);
    struct ast_channel first = {.active = true, .input = 100};
    struct ast_channel second = {.active = true, .input = 200};
    for (failed_allocation = 1; failed_allocation <= 4; ++failed_allocation) {
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
    assert(!ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false));
    assert(ra_link_hub_attach(&hub, "1", &first, NULL, true, true, false) == -1);
    rate = 16000;
    assert(ra_link_hub_attach(&hub, "2", &second, NULL, true, true, false) == -1);
    rate = 8000;
    assert(!ra_link_hub_attach(&hub, "2", &second, NULL, true, true, false));
    struct ra_controller controller = {.rate = 8000, .full_duplex = true};
    assert(ra_controller_start(&controller, 0));
    int16_t audio[8001] = {50};
    assert(ra_link_hub_process(&hub, &controller, true, audio, 1, 20));
    assert(audio[0] == 350 && first.output == 250 && second.output == 150);
    assert(first.keyed && second.keyed);
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
    assert(ra_link_hub_disconnect(&hub, "1") && first.stopped);
    first.input = 100;
    second.input = 200;
    assert(!ra_link_hub_attach(&hub, "1", &first, NULL, false, false, false));
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
    ra_link_hub_close(&hub);
    ra_link_hub_close(&hub);
    return 0;
}
