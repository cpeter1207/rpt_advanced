/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Deterministic worker startup, readiness, timestamp, and cleanup failures.
 */
#include <asterisk.h>

#include "link_hub.h"
#include "worker.h"
#include "worker_dtmf_fixture.h"
#include <assert.h>
#include <asterisk/channel.h>
#include <errno.h>
#include <stdio.h>
#include <time.h>

/** @brief Captured thread entry; execution is controlled by this fixture. */
static void *(*entry)(void *);
/** @brief Captured worker passed to the thread entry. */
static struct ra_worker *active;
/** @brief Whether the captured thread has completed. */
static bool ran;
/** @brief Injected thread creation error. */
static int create_error;
/** @brief Injected monotonic clock error. */
static int clock_error;
/** @brief Injected frame exchange error. */
static int exchange_error;
/** @brief Ordered readiness results; 2 requests shutdown without an audio event. */
static int ready[4];
/** @brief Next readiness index. */
static size_t next;
/** @brief Released channel count. */
static unsigned int hung_up;
/** @brief Explicit unkey requests. */
static unsigned int unkeyed;
/** @brief Frame-exchange count, never increased by readiness timeouts. */
static unsigned int exchanged;
/** @brief Delivered digits and timeout markers. */
static unsigned int delivered;

/** @brief Verify digit delivery at the hardware event timestamp.
 * @param node Configured node identity.
 * @param digit Detected digit or timeout marker.
 * @param now_ms Hardware event timestamp.
 */
static void deliver_digit(const char *node, char digit, uint64_t now_ms) {
    assert(node && ((digit == '5' && now_ms == 5000) || (!digit && now_ms == 8000)));
    ++delivered;
}

/** @brief Capture a joinable thread or inject creation failure.
 * @param thread Receives fixture identity.
 * @param attributes Expected default attributes.
 * @param start Worker entry.
 * @param argument Worker context.
 * @return Injected pthread status.
 */
int __wrap_pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                          void *(*start)(void *), void *argument) {
    assert(!attributes);
    *thread = pthread_self();
    entry = start;
    active = argument;
    ran = false;
    return create_error;
}

/** @brief Complete a not-yet-scheduled worker after stop requests shutdown.
 * @param thread Fixture identity.
 * @param result Expected unused return value.
 * @return Successful join.
 */
int __wrap_pthread_join(pthread_t thread, void **result) {
    assert(pthread_equal(thread, pthread_self()) && !result);
    assert(atomic_load(&active->stop));
    if (!ran) {
        assert(entry(active) == NULL);
        ran = true;
    }
    return 0;
}

/** @brief Supply deterministic channel readiness and timeout events.
 * @param channel Owned channel.
 * @param milliseconds Bounded shutdown wait.
 * @return Readiness, timeout, or error.
 */
int ast_waitfor(struct ast_channel *channel, int milliseconds) {
    assert(channel == active->channel && milliseconds == 100 && next < 4);
    int result = ready[next++];
    if (result == 2) {
        atomic_store(&active->stop, true);
        return 0;
    }
    return result;
}

/** @brief Supply a monotonic timestamp or injected failure.
 * @param clock Expected monotonic clock.
 * @param now Receives time.
 * @return Injected result.
 */
int __wrap_clock_gettime(clockid_t clock, struct timespec *now) {
    assert(clock == CLOCK_MONOTONIC);
    now->tv_sec = 2;
    now->tv_nsec = 3000000;
    return clock_error;
}

/** @brief Exercise the real controller callback through the exchange interface.
 * @param state Worker-owned exchange state.
 * @param channel Owned channel.
 * @return Injected exchange result.
 */
int __wrap_ra_radio_exchange(struct ra_radio *state, struct ast_channel *channel) {
    assert(channel == active->channel && active->now_ms == 2003);
    int16_t audio[160] = {0};
    state->keyed = state->render(state->context, true, audio, 160);
    assert(state->keyed);
    ++exchanged;
    return exchange_error;
}

/** @brief Verify explicit transmitter release during shutdown.
 * @param channel Owned channel.
 * @param condition Required unkey indication.
 * @return Success.
 */
int ast_indicate(struct ast_channel *channel, int condition) {
    assert(channel == active->channel && condition == AST_CONTROL_RADIO_UNKEY);
    ++unkeyed;
    return 0;
}

/** @brief Record release of the channel owned by the worker.
 * @param channel Owned channel.
 */
void ast_hangup(struct ast_channel *channel) {
    assert(channel == active->channel);
    ++hung_up;
}

/** @brief Execute the captured worker before lifecycle shutdown.
 */
static void execute(void) {
    assert(entry(active) == NULL);
    ran = true;
    ra_worker_stop(active);
}

/** @brief Cover readiness pacing and every worker exit path.
 * @return Zero after all assertions.
 */
int main(void) {
    struct ra_controller controller = {.rate = 8000, .full_duplex = true};
    assert(ra_controller_start(&controller, 0));
    struct ra_worker worker = {.controller = &controller};
    create_error = EAGAIN;
    assert(ra_worker_start(&worker) == EAGAIN && hung_up == 0);
    create_error = 0;
    assert(ra_worker_start(&worker) == 0);
    ra_worker_stop(&worker);
    assert(hung_up == 1 && exchanged == 0 && worker.result == 0);
    assert(ra_worker_start(&worker) == 0);
    ready[0] = 0;
    ready[1] = 1;
    ready[2] = 2;
    execute();
    assert(exchanged == 1 && unkeyed == 1 && hung_up == 2 && worker.result == 0);
    assert(ra_worker_start(&worker) == 0);
    next = 0;
    ready[0] = -1;
    execute();
    assert(worker.result == -1 && hung_up == 3 && exchanged == 1);
    assert(ra_worker_start(&worker) == 0);
    next = 0;
    ready[0] = 1;
    clock_error = -1;
    execute();
    assert(worker.result == -1 && hung_up == 4 && exchanged == 1);
    assert(ra_worker_start(&worker) == 0);
    next = 0;
    clock_error = 0;
    exchange_error = -1;
    struct ra_link_hub hub = {0};
    worker.links = &hub;
    execute();
    assert(worker.result == -1 && hung_up == 5 && exchanged == 2 && unkeyed == 2);
    worker.name = "alpha";
    worker.digit = deliver_digit;
    ra_test_dtmf_fail = true;
    assert(ra_worker_start(&worker) == ENOMEM);
    ra_test_dtmf_fail = false;
    create_error = EAGAIN;
    assert(ra_worker_start(&worker) == EAGAIN && ra_test_dtmf_closed == 1);
    create_error = 0;
    assert(!ra_worker_start(&worker));
    int16_t audio[160] = {0};
    worker.now_ms = 5000;
    ra_test_dtmf_digit = '5';
    (void)worker.radio.render(&worker, true, audio, 160);
    ra_test_dtmf_digit = 0;
    worker.now_ms = 5100;
    (void)worker.radio.render(&worker, true, audio, 160);
    worker.now_ms = 8000;
    (void)worker.radio.render(&worker, true, audio, 160);
    (void)worker.radio.render(&worker, true, audio, 160);
    ra_worker_stop(&worker);
    assert(delivered == 2 && ra_test_dtmf_closed == 2);
    puts("joinable channel worker lifecycle tests passed");
    return 0;
}
