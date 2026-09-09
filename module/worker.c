/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Channel-readiness-driven controller execution and orderly shutdown.
 */
#include <asterisk.h>

#include "dtmf.h"
#include "link_hub.h"
#include "worker.h"
#include <asterisk/channel.h>
#include <errno.h>
#include <time.h>

/** @brief Require the digit handoff to use native atomics instead of a library lock. */
_Static_assert(ATOMIC_BOOL_LOCK_FREE == 2 && ATOMIC_INT_LOCK_FREE == 2,
               "worker digit handoff requires lock-free atomics");

/** @brief Publish one DTMF event without allocating, locking, or waiting.
 * @param worker Radio worker that is the ring's sole producer.
 * @param digit Decoded digit or interdigit timeout marker.
 * @param now_ms Hardware event timestamp.
 *
 * A full ring increments a loss generation that the dispatcher reports before later queued
 * events. This invalidates partial commands without making the radio callback wait for control.
 */
static void queue_digit(struct ra_worker *worker, char digit, uint64_t now_ms) {
    unsigned int write = atomic_load_explicit(&worker->digit_write, memory_order_relaxed);
    unsigned int read = atomic_load_explicit(&worker->digit_read, memory_order_acquire);
    if (write - read >= RA_WORKER_DIGIT_QUEUE_DEPTH) {
        atomic_fetch_add_explicit(&worker->digit_dropped, 1, memory_order_release);
        return;
    }
    worker->digit_events[write % RA_WORKER_DIGIT_QUEUE_DEPTH] =
        (struct ra_worker_digit_event){.now_ms = now_ms, .digit = digit};
    atomic_store_explicit(&worker->digit_write, write + 1, memory_order_release);
}

/** @brief Consume one DTMF event from the worker's lock-free handoff.
 * @param worker Worker whose dispatcher is the sole consumer.
 * @param event Receives the next event when one is available.
 * @return True when an event was consumed.
 */
static bool take_digit(struct ra_worker *worker, struct ra_worker_digit_event *event) {
    unsigned int read = atomic_load_explicit(&worker->digit_read, memory_order_relaxed);
    unsigned int write = atomic_load_explicit(&worker->digit_write, memory_order_acquire);
    if (read == write) {
        return false;
    }
    *event = worker->digit_events[read % RA_WORKER_DIGIT_QUEUE_DEPTH];
    atomic_store_explicit(&worker->digit_read, read + 1, memory_order_release);
    return true;
}

/** @brief Discard events published before an observed queue overflow.
 * @param worker Worker whose dispatcher is the ring's sole consumer.
 *
 * The acquire snapshot is the exclusive boundary: entries published before it can no longer form
 * part of a valid DTMF command, while a producer that publishes after it remains available to the
 * next consume operation. This prevents stale prefixes from becoming a new command after loss.
 */
static void discard_overflowed_digits(struct ra_worker *worker) {
    unsigned int write = atomic_load_explicit(&worker->digit_write, memory_order_acquire);
    atomic_store_explicit(&worker->digit_read, write, memory_order_release);
}

/** @brief Deliver DTMF events outside the hardware-paced radio callback.
 * @param context Worker owning the SPSC handoff and stable node name.
 * @return Null after draining events accepted before shutdown.
 */
static void *dispatch_digits(void *context) {
    struct ra_worker *worker = context;
    unsigned int reported_drops = 0;
    for (;;) {
        unsigned int dropped = atomic_load_explicit(&worker->digit_dropped, memory_order_acquire);
        if (reported_drops != dropped) {
            /* A loss invalidates every queued prefix published before this write snapshot. */
            discard_overflowed_digits(worker);
            reported_drops = dropped;
            worker->digit(worker->name, RA_WORKER_DIGIT_DROPPED, 0);
        }
        struct ra_worker_digit_event event;
        if (take_digit(worker, &event)) {
            worker->digit(worker->name, event.digit, event.now_ms);
            continue;
        }
        if (atomic_load_explicit(&worker->digit_stop, memory_order_acquire)) {
            return NULL;
        }
        const struct timespec interval = {.tv_nsec = 1000000};
        (void)nanosleep(&interval, NULL);
    }
}

/** @brief Stop and drain an already-created DTMF dispatcher.
 * @param worker Worker whose dispatcher may require joining.
 */
static void stop_digit_dispatcher(struct ra_worker *worker) {
    if (worker->digit_thread_started) {
        atomic_store_explicit(&worker->digit_stop, true, memory_order_release);
        (void)pthread_join(worker->digit_thread, NULL);
        worker->digit_thread_started = false;
    }
}

/** @brief Render using the event timestamp captured by the channel worker.
 * @param context Worker-owned state.
 * @param receiving Qualified receiver indication.
 * @param audio In-place PCM, null for carrier events.
 * @param samples Hardware-paced sample count.
 * @return Requested transmitter state.
 */
static bool render(void *context, bool receiving, int16_t *audio, size_t samples) {
    struct ra_worker *worker = context;
    if (worker->detector) {
        char digit = ra_dtmf_process(worker->detector, receiving, audio, samples);
        if (digit) {
            queue_digit(worker, digit, worker->now_ms);
            worker->last_digit_ms = worker->now_ms;
            worker->digit_timeout = true;
        } else if (worker->digit_timeout && worker->now_ms - worker->last_digit_ms >= 3000) {
            queue_digit(worker, 0, worker->now_ms);
            worker->digit_timeout = false;
        }
        if (worker->was_receiving && !receiving && worker->digit_timeout) {
            /* The receiver transition is an unambiguous local end-of-command marker. */
            queue_digit(worker, '#', worker->now_ms);
            worker->digit_timeout = false;
        }
        worker->was_receiving = receiving;
    }
    if (worker->links) {
        return ra_link_hub_process(worker->links, worker->controller, receiving, audio, samples,
                                   worker->now_ms);
    }
    return ra_controller_process(worker->controller, receiving, audio, samples, worker->now_ms);
}

/** @brief Own channel reads, controller execution, and channel release.
 * @param context Worker retained by the joining lifecycle owner.
 * @return Null after channel cleanup.
 */
static void *run(void *context) {
    struct ra_worker *worker = context;
    while (!atomic_load(&worker->stop)) {
        /* The timeout bounds shutdown latency only; it never generates audio. */
        int ready = ast_waitfor(worker->channel, 100);
        if (ready < 0) {
            worker->result = -1;
            break;
        }
        if (!ready) {
            continue;
        }
        struct timespec now;
        if (clock_gettime(CLOCK_MONOTONIC, &now)) {
            worker->result = -1;
            break;
        }
        worker->now_ms = (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
        if (ra_radio_exchange(&worker->radio, worker->channel)) {
            worker->result = -1;
            break;
        }
    }
    if (worker->radio.keyed) {
        (void)ast_indicate(worker->channel, AST_CONTROL_RADIO_UNKEY);
    }
    ast_hangup(worker->channel);
    return NULL;
}

int ra_worker_start(struct ra_worker *worker) {
    if (worker->digit) {
        worker->detector = ra_dtmf_open(worker->controller->rate);
        if (!worker->detector) {
            return ENOMEM;
        }
        ra_dtmf_set_muting(worker->detector, worker->dtmf_muting);
    }
    atomic_init(&worker->stop, false);
    atomic_init(&worker->digit_write, 0);
    atomic_init(&worker->digit_read, 0);
    atomic_init(&worker->digit_dropped, 0);
    atomic_init(&worker->digit_stop, false);
    worker->digit_thread_started = false;
    worker->result = 0;
    worker->digit_timeout = false;
    worker->was_receiving = false;
    worker->radio.receiving = false;
    worker->radio.keyed = false;
    worker->radio.render = render;
    worker->radio.context = worker;
    if (worker->digit) {
        int result = pthread_create(&worker->digit_thread, NULL, dispatch_digits, worker);
        if (result) {
            ra_dtmf_close(worker->detector);
            worker->detector = NULL;
            return result;
        }
        worker->digit_thread_started = true;
    }
    int result = pthread_create(&worker->thread, NULL, run, worker);
    if (result) {
        stop_digit_dispatcher(worker);
        if (worker->detector) {
            ra_dtmf_close(worker->detector);
            worker->detector = NULL;
        }
    }
    return result;
}

void ra_worker_stop(struct ra_worker *worker) {
    atomic_store(&worker->stop, true);
    (void)pthread_join(worker->thread, NULL);
    stop_digit_dispatcher(worker);
    if (worker->detector) {
        ra_dtmf_close(worker->detector);
        worker->detector = NULL;
    }
}
