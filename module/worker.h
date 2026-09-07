/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Joinable hardware-paced channel worker ownership.
 */
#ifndef RPT_ADVANCED_WORKER_H
#define RPT_ADVANCED_WORKER_H
#include "controller.h"
#include "radio.h"
#include "runtime.h"
struct ra_dtmf_detector;
struct ra_link_hub;
#include <pthread.h>
#include <stdatomic.h>

/** @brief Worker state retained by the lifecycle owner until stop has returned. */
struct ra_worker {
    struct ast_channel *channel;      /**< Open channel; ownership transfers on successful start. */
    struct ra_controller *controller; /**< Initialized controller retained until stop returns. */
    struct ra_link_hub *links;        /**< Optional network router retained until stop returns. */
    const char *name;                 /**< Borrowed configured node name. */
    ra_digit_handler digit;           /**< Nonblocking digit delivery to the control queue. */
    struct ra_dtmf_detector *detector; /**< Worker-owned normalized DTMF detector. */
    uint64_t last_digit_ms;            /**< Last emitted digit time. */
    bool digit_timeout;                /**< Interdigit timeout still needs to be emitted. */
    struct ra_radio radio;             /**< PCM format binding and frame-exchange state. */
    pthread_t thread;                  /**< Joinable worker, valid after successful start. */
    atomic_bool stop;                  /**< Shutdown requested by the lifecycle owner. */
    uint64_t now_ms;                   /**< Current monotonic event timestamp, worker-owned. */
    int result;                        /**< Terminal status, readable after stop joins. */
};

/** @brief Start exchanging an already connected channel at its hardware cadence.
 * @param worker State with channel, controller, and radio.linear set; borrowed data stays valid.
 * @return Zero on success; pthread error otherwise, leaving channel ownership with caller.
 */
int ra_worker_start(struct ra_worker *worker);

/** @brief Request shutdown and join a successfully started worker exactly once.
 * @param worker Started worker. Its channel is released before this function returns.
 * No cancellation is used; controller/media can be freed safely after the join.
 */
void ra_worker_stop(struct ra_worker *worker);
#endif
