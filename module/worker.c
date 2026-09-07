/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Channel-readiness-driven controller execution and orderly shutdown.
 */
#include <asterisk.h>

#include "worker.h"
#include <asterisk/channel.h>
#include <time.h>

/** @brief Render using the event timestamp captured by the channel worker.
 * @param context Worker-owned state.
 * @param receiving Qualified receiver indication.
 * @param audio In-place PCM, null for carrier events.
 * @param samples Hardware-paced sample count.
 * @return Requested transmitter state.
 */
static bool render(void *context, bool receiving, int16_t *audio, size_t samples) {
    struct ra_worker *worker = context;
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
    atomic_init(&worker->stop, false);
    worker->result = 0;
    worker->radio.receiving = false;
    worker->radio.keyed = false;
    worker->radio.render = render;
    worker->radio.context = worker;
    return pthread_create(&worker->thread, NULL, run, worker);
}

void ra_worker_stop(struct ra_worker *worker) {
    atomic_store(&worker->stop, true);
    (void)pthread_join(worker->thread, NULL);
}
