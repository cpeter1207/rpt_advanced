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
#include <string.h>
#include <time.h>

/** @brief One captured thread whose execution is controlled by this fixture. */
struct captured_thread {
    pthread_t identifier;   /**< Synthetic joinable thread identity. */
    void *(*entry)(void *); /**< Captured start entry. */
    void *context;          /**< Captured start context. */
    bool ran;               /**< Entry was explicitly completed. */
};
/** @brief Captured radio and DTMF dispatcher threads for one worker lifetime. */
static struct captured_thread threads[2];
/** @brief Number of successfully captured thread starts. */
static size_t thread_count;
/** @brief Number of captured threads already joined. */
static size_t joined_count;
/** @brief Captured worker passed to the thread entry. */
static struct ra_worker *active;
/** @brief Injected thread creation error. */
static int create_error;
/** @brief Fail only the second create call, after a DTMF dispatcher has started. */
static bool fail_second_create;
/** @brief Request dispatcher shutdown when its idle poll is reached. */
static bool stop_dispatch_on_sleep;
/** @brief Inject one radio-side event after dispatcher invalidates an overflow. */
static bool publish_after_drop;
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
/** @brief One digit delivered by the control-side dispatcher. */
struct delivered_digit {
    char digit;      /**< Digit, timeout, or drop marker. */
    uint64_t now_ms; /**< Original hardware timestamp. */
};
/** @brief Observed ordered control-side DTMF events. */
static struct delivered_digit delivered[RA_WORKER_DIGIT_QUEUE_DEPTH + 2];
/** @brief Number of observed control-side DTMF events. */
static size_t delivered_count;

/** @brief Verify digit delivery at the hardware event timestamp.
 * @param node Configured node identity.
 * @param digit Detected digit or timeout marker.
 * @param now_ms Hardware event timestamp.
 */
static void deliver_digit(const char *node, char digit, uint64_t now_ms) {
    assert(node && !strcmp(node, "alpha") &&
           delivered_count < sizeof(delivered) / sizeof(*delivered));
    delivered[delivered_count++] = (struct delivered_digit){.digit = digit, .now_ms = now_ms};
}

/** @brief Find one captured thread by the identity returned to the worker.
 * @param identifier Worker-owned thread identity.
 * @return Matching fixture record.
 */
static struct captured_thread *captured(pthread_t identifier) {
    for (size_t index = 0; index < thread_count; ++index) {
        if (pthread_equal(identifier, threads[index].identifier)) {
            return &threads[index];
        }
    }
    assert(false);
    return NULL;
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
    if (thread_count == joined_count) {
        thread_count = 0;
        joined_count = 0;
    }
    assert(thread_count < sizeof(threads) / sizeof(*threads));
    if (create_error && (!fail_second_create || thread_count == 1)) {
        return create_error;
    }
    *thread = (pthread_t)(thread_count + 1);
    threads[thread_count++] =
        (struct captured_thread){.identifier = *thread, .entry = start, .context = argument};
    active = argument;
    return 0;
}

/** @brief Complete a not-yet-scheduled worker after stop requests shutdown.
 * @param thread Fixture identity.
 * @param result Expected unused return value.
 * @return Successful join.
 */
int __wrap_pthread_join(pthread_t thread, void **result) {
    assert(!result);
    struct captured_thread *record = captured(thread);
    assert(atomic_load(&active->stop) || atomic_load(&active->digit_stop));
    if (!record->ran) {
        assert(record->entry(record->context) == NULL);
        record->ran = true;
    }
    ++joined_count;
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

/** @brief End one deterministic dispatcher idle-poll interval without waiting.
 * @param interval Requested bounded sleep interval.
 * @param remaining Unused remaining-time destination.
 * @return Zero after optionally requesting dispatcher shutdown.
 */
int __wrap_nanosleep(const struct timespec *interval, struct timespec *remaining) {
    assert(interval && interval->tv_sec == 0 && interval->tv_nsec == 1000000 && !remaining);
    if (publish_after_drop) {
        int16_t audio[160] = {0};
        publish_after_drop = false;
        active->now_ms = 10000;
        ra_test_dtmf_digit = '6';
        (void)active->radio.render(active, true, audio, 160);
        ra_test_dtmf_digit = 0;
    } else if (stop_dispatch_on_sleep) {
        atomic_store(&active->digit_stop, true);
    }
    return 0;
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
 * @param worker Started fixture worker.
 */
static void execute(struct ra_worker *worker) {
    struct captured_thread *record = captured(worker->thread);
    assert(record->entry(record->context) == NULL);
    record->ran = true;
    ra_worker_stop(worker);
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
    execute(&worker);
    assert(exchanged == 1 && unkeyed == 1 && hung_up == 2 && worker.result == 0);
    assert(ra_worker_start(&worker) == 0);
    next = 0;
    ready[0] = -1;
    execute(&worker);
    assert(worker.result == -1 && hung_up == 3 && exchanged == 1);
    assert(ra_worker_start(&worker) == 0);
    next = 0;
    ready[0] = 1;
    clock_error = -1;
    execute(&worker);
    assert(worker.result == -1 && hung_up == 4 && exchanged == 1);
    assert(ra_worker_start(&worker) == 0);
    next = 0;
    clock_error = 0;
    exchange_error = -1;
    struct ra_link_hub hub = {0};
    worker.links = &hub;
    execute(&worker);
    assert(worker.result == -1 && hung_up == 5 && exchanged == 2 && unkeyed == 2);
    worker.name = "alpha";
    worker.digit = deliver_digit;
    ra_test_dtmf_fail = true;
    assert(ra_worker_start(&worker) == ENOMEM);
    ra_test_dtmf_fail = false;
    create_error = EAGAIN;
    assert(ra_worker_start(&worker) == EAGAIN && ra_test_dtmf_closed == 1);
    create_error = 0;
    create_error = EAGAIN;
    fail_second_create = true;
    assert(ra_worker_start(&worker) == EAGAIN && ra_test_dtmf_closed == 2);
    fail_second_create = false;
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
    assert(delivered_count == 2 && delivered[0].digit == '5' && delivered[0].now_ms == 5000 &&
           !delivered[1].digit && delivered[1].now_ms == 8000 && ra_test_dtmf_closed == 3);

    delivered_count = 0;
    assert(!ra_worker_start(&worker));
    worker.now_ms = 8100;
    ra_test_dtmf_digit = '5';
    (void)worker.radio.render(&worker, true, audio, 160);
    ra_test_dtmf_digit = 0;
    worker.now_ms = 8200;
    (void)worker.radio.render(&worker, false, NULL, 0);
    ra_worker_stop(&worker);
    assert(delivered_count == 2 && delivered[0].digit == '5' && delivered[0].now_ms == 8100 &&
           delivered[1].digit == '#' && delivered[1].now_ms == 8200 && ra_test_dtmf_closed == 4);

    /* Receiver unkey without a collected digit does not synthesize a terminator. */
    delivered_count = 0;
    assert(!ra_worker_start(&worker));
    worker.now_ms = 8300;
    (void)worker.radio.render(&worker, true, audio, 160);
    worker.now_ms = 8400;
    (void)worker.radio.render(&worker, false, NULL, 0);
    ra_worker_stop(&worker);
    assert(!delivered_count && ra_test_dtmf_closed == 5);

    assert(!ra_worker_start(&worker));
    stop_dispatch_on_sleep = true;
    struct captured_thread *dispatcher = captured(worker.digit_thread);
    assert(dispatcher->entry(dispatcher->context) == NULL);
    dispatcher->ran = true;
    stop_dispatch_on_sleep = false;
    ra_worker_stop(&worker);
    assert(ra_test_dtmf_closed == 6);

    delivered_count = 0;
    assert(!ra_worker_start(&worker));
    ra_test_dtmf_digit = '5';
    for (unsigned int index = 0; index < RA_WORKER_DIGIT_QUEUE_DEPTH + 2; ++index) {
        worker.now_ms = 9000 + index;
        (void)worker.radio.render(&worker, true, audio, 160);
    }
    ra_test_dtmf_digit = 0;
    publish_after_drop = true;
    stop_dispatch_on_sleep = true;
    dispatcher = captured(worker.digit_thread);
    assert(dispatcher->entry(dispatcher->context) == NULL);
    dispatcher->ran = true;
    publish_after_drop = false;
    stop_dispatch_on_sleep = false;
    ra_worker_stop(&worker);
    assert(delivered_count == 2 && delivered[0].digit == RA_WORKER_DIGIT_DROPPED &&
           !delivered[0].now_ms && delivered[1].digit == '6' && delivered[1].now_ms == 10000);
    assert(ra_test_dtmf_closed == 7);
    puts("joinable channel worker lifecycle tests passed");
    return 0;
}
