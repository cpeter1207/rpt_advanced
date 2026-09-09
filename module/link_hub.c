/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Route local and remote voice without feeding a peer its own audio.
 */
#include "link_hub.h"
#include "link_peer.h"
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/format_cache.h>
#include <asterisk/utils.h>
#include <limits.h>
#include <samplerate.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief Serialize lifecycle list changes outside the real-time audio callback. */
AST_MUTEX_DEFINE_STATIC(routing_lock);

/** @brief Require status atomics that never acquire a library lock in the radio callback. */
_Static_assert(ATOMIC_BOOL_LOCK_FREE == 2 && ATOMIC_CHAR_LOCK_FREE == 2 &&
                   ATOMIC_INT_LOCK_FREE == 2 && ATOMIC_POINTER_LOCK_FREE == 2,
               "link status requires lock-free atomics");

/** @brief Initialize all lifecycle and audio-list atomics before their first access.
 * @param hub Caller-owned storage with no active manager or attached peers.
 *
 * Close preserves this initialized state while clearing every owned resource, so the same hub can
 * be attached again without relying on zero-filled atomic representations.
 */
void ra_link_hub_init(struct ra_link_hub *hub) {
    hub->local = NULL;
    hub->remote = NULL;
    hub->outgoing = NULL;
    hub->capacity = 0;
    hub->rate = 0;
    hub->manager_started = false;
    hub->local_name = NULL;
    hub->reconnect = NULL;
    hub->reconnect_context = NULL;
    hub->digit = NULL;
    hub->digit_context = NULL;
    hub->event = NULL;
    hub->event_context = NULL;
    hub->retries = NULL;
    atomic_init(&hub->ports, NULL);
    atomic_init(&hub->readers, 0);
    atomic_init(&hub->stop, false);
    atomic_init(&hub->topology_generation, 0);
    atomic_init(&hub->last_keyed_sequence, 0);
    for (size_t index = 0; index < RA_LINK_PEER_NAME_MAX; ++index) {
        atomic_init(&hub->last_keyed[index], '\0');
    }
}

/** @brief Refresh legacy app_rpt linked-node advertisements often enough to clear stale state. */
#define RA_LINK_TOPOLOGY_POST_INTERVAL_MS UINT64_C(30000)
/** @brief app_rpt's conventional route-list marker for a safely truncated advertisement. */
#define RA_LINK_TOPOLOGY_TRUNCATION_ROUTE "R000000"

/** @brief One authenticated peer and its latest decoded block. */
struct ra_link_port {
    _Atomic(struct ra_link_port *) next; /**< Immutable once published except unlinking. */
    struct ra_link_hub *hub;             /**< Owning hub valid until the reader joins. */
    char *name;                          /**< Owned remote node identity. */
    struct ra_link_peer peer;            /**< Joined network reader. */
    int16_t *audio;                      /**< Current receive block. */
    int16_t *output;                     /**< Peer-rate transmit block. */
    size_t rate;                         /**< Negotiated peer sample rate. */
    size_t samples;                      /**< Peer samples scheduled for the current local block. */
    uint64_t remainder;     /**< Exact-rate scheduling remainder in local-rate units. */
    SRC_STATE *receive_src; /**< Peer-to-radio sample-rate converter. */
    SRC_STATE *send_src;    /**< Radio-to-peer sample-rate converter. */
    float *src_in;          /**< Floating-point converter input workspace. */
    float *src_out;         /**< Floating-point converter output workspace. */
    bool transmit;          /**< Outbound audio permitted. */
    bool forward;           /**< Relay received voice to other links. */
    bool permanent;         /**< Redial after an unexpected transport failure. */
    bool active;            /**< Receive activity for the current radio tick. */
};

/** @brief One retained recovery request, owned outside audio processing. */
struct ra_link_retry {
    struct ra_link_retry *next; /**< Next retained request. */
    char *name;                 /**< Owned remote node identity. */
    uint64_t due_ms;            /**< Monotonic time at which the next dial may begin. */
    uint64_t delay_ms;          /**< Delay selected after the last failed dial. */
    bool transmit;              /**< Original outbound-audio mode. */
    bool forward;               /**< Original peer-forwarding mode. */
    bool automatic;             /**< Retry unexpected loss only for permanent links. */
    atomic_bool paused;         /**< Disconnect-all retains this record until reconnect-all. */
    bool attempting;            /**< Manager owns this record while dialing outside the lock. */
    atomic_bool cancelled;      /**< Explicit permanent disconnect wins over a concurrent dial. */
};

/** @brief Saturate a PCM sum without applying dynamics processing.
 * @param sample Wide signed sum.
 * @return Representable signed-linear sample.
 */
static int16_t pcm(int64_t sample) {
    return sample > INT16_MAX ? INT16_MAX : sample < INT16_MIN ? INT16_MIN : (int16_t)sample;
}

/** @brief Publish a newly active peer identity without taking the lifecycle lock.
 * @param hub Routing hub retaining the fixed status storage.
 * @param name Stable peer identity copied before its port can be reclaimed.
 *
 * A sequence counter gives control-plane readers a coherent copy while the radio worker remains
 * entirely lock-free. One radio worker is the sole writer.
 */
static void remember_last_keyed(struct ra_link_hub *hub, const char *name) {
    atomic_fetch_add_explicit(&hub->last_keyed_sequence, 1, memory_order_release);
    size_t index = 0;
    while (index + 1 < RA_LINK_PEER_NAME_MAX && name[index]) {
        atomic_store_explicit(&hub->last_keyed[index], (unsigned char)name[index],
                              memory_order_relaxed);
        ++index;
    }
    atomic_store_explicit(&hub->last_keyed[index], '\0', memory_order_relaxed);
    atomic_fetch_add_explicit(&hub->last_keyed_sequence, 1, memory_order_release);
}

/** @brief Make the hub manager publish a fresh linked-node list on its next control pass.
 * @param hub Hub whose externally visible route graph changed.
 *
 * This counter is only observed by the control manager. It never participates in hardware-paced
 * routing, so an atomic increment avoids coupling status propagation to the radio callback.
 */
static void topology_changed(struct ra_link_hub *hub) {
    atomic_fetch_add_explicit(&hub->topology_generation, 1, memory_order_release);
}

/** @brief Calculate the next bounded legacy linked-node-list refresh time.
 * @param now_ms Current monotonic control-clock time.
 * @return Thirty seconds later, saturated at the monotonic clock's maximum value.
 */
static uint64_t topology_next_post(uint64_t now_ms) {
    return now_ms > UINT64_MAX - RA_LINK_TOPOLOGY_POST_INTERVAL_MS
               ? UINT64_MAX
               : now_ms + RA_LINK_TOPOLOGY_POST_INTERVAL_MS;
}

/** @brief Release an unpublished port; a started reader owns its channel.
 * @param port Detached port.
 */
static void release_port(struct ra_link_port *port) {
    if (port->peer.channel) {
        ra_link_peer_stop(&port->peer);
    }
    ast_free(port->audio);
    ast_free(port->output);
    src_delete(port->receive_src);
    src_delete(port->send_src);
    ast_free(port->src_in);
    ast_free(port->src_out);
    ast_free(port->name);
    ast_free(port);
}

/** @brief Release a detached retained-link recovery request.
 * @param retry Request no longer reachable from its hub.
 */
static void release_retry(struct ra_link_retry *retry) {
    ast_free(retry->name);
    ast_free(retry);
}

/** @brief Read the monotonic scheduling clock without affecting real-time audio.
 * @return Milliseconds since an arbitrary monotonic epoch, or zero if unavailable.
 */
static uint64_t monotonic_ms(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) {
        return 0;
    }
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
}

/** @brief Hand one peer-identified reader DTMF event to the configured node control path.
 * @param context Stable port retained until its reader joins.
 * @param digit Validated conventional DTMF character.
 *
 * The reader stops and joins before its port is released. The callback therefore sees a live
 * hub and runs outside both the hardware callback and the routing lifecycle lock.
 */
static void receive_digit(void *context, char digit) {
    struct ra_link_port *port = context;
    struct ra_link_hub *hub = port->hub;
    if (hub->digit) {
        hub->digit(hub->digit_context, port->name, digit, monotonic_ms());
    }
}

/** @brief Hand one direct peer lifecycle change to the non-audio control plane.
 * @param hub Hub whose configured callback receives the event.
 * @param remote Stable peer identity retained until this call returns.
 * @param connected True for an attached port, false for a detached port.
 */
static void report_event(struct ra_link_hub *hub, const char *remote, bool connected) {
    if (hub->event) {
        hub->event(hub->event_context, remote, connected);
    }
}

uint64_t ra_link_hub_retry_delay(uint64_t previous_ms) {
    const uint64_t maximum = 300000;
    if (previous_ms < 1000) {
        return 1000;
    }
    return previous_ms >= maximum / 2 ? maximum : previous_ms * 2;
}

/** @brief Find an attached peer while the routing lifecycle lock is held.
 * @param hub Hub containing the immutable published list.
 * @param name Exact remote identity.
 * @return True when a live peer with that name is attached.
 */
static bool attached_locked(const struct ra_link_hub *hub, const char *name) {
    for (struct ra_link_port *port = atomic_load_explicit(&hub->ports, memory_order_seq_cst); port;
         port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
        if (!strcmp(port->name, name) && !atomic_load(&port->peer.ended)) {
            return true;
        }
    }
    return false;
}

/** @brief Retain link metadata for automatic recovery or an explicit reconnect-all.
 * @param hub Hub retaining the request.
 * @param name Stable remote node identity.
 * @param transmit Original outbound-audio mode.
 * @param forward Original peer-forwarding mode.
 * @param now_ms Current monotonic time.
 * @param automatic True for a permanent link that retries unexpected failures.
 * @param paused True when reconnect-all must explicitly resume this request.
 * @return New retained request, or null on allocation failure or duplicate recovery.
 */
static struct ra_link_retry *schedule_retry_locked(struct ra_link_hub *hub, const char *name,
                                                   bool transmit, bool forward, uint64_t now_ms,
                                                   bool automatic, bool paused) {
    for (const struct ra_link_retry *retry = hub->retries; retry; retry = retry->next) {
        if (!strcmp(retry->name, name)) {
            return NULL;
        }
    }
    struct ra_link_retry *retry = ast_calloc(1, sizeof(*retry));
    if (!retry) {
        return NULL;
    }
    retry->name = ast_strdup(name);
    if (!retry->name) {
        release_retry(retry);
        return NULL;
    }
    retry->transmit = transmit;
    retry->forward = forward;
    retry->automatic = automatic;
    atomic_init(&retry->paused, paused);
    retry->due_ms = now_ms;
    atomic_init(&retry->cancelled, false);
    retry->next = hub->retries;
    hub->retries = retry;
    return retry;
}

/** @brief Select one due recovery request without retaining the lifecycle lock while dialing.
 * @param hub Hub containing retry requests.
 * @param now_ms Current monotonic time.
 * @return Stable request owned by the manager, or null when none is due.
 */
static struct ra_link_retry *take_retry_locked(struct ra_link_hub *hub, uint64_t now_ms) {
    struct ra_link_retry **cursor = &hub->retries;
    while (*cursor) {
        struct ra_link_retry *retry = *cursor;
        if (attached_locked(hub, retry->name)) {
            *cursor = retry->next;
            release_retry(retry);
            continue;
        }
        if (!atomic_load(&retry->paused) && retry->due_ms <= now_ms) {
            retry->attempting = true;
            return retry;
        }
        cursor = &retry->next;
    }
    return NULL;
}

/** @brief Finish an unlocked redial and retain it only if a later retry is still needed.
 * @param hub Hub that owns the request.
 * @param retry Stable manager-owned request.
 * @param result Callback result.
 * @param now_ms Current monotonic time.
 */
static void finish_retry(struct ra_link_hub *hub, struct ra_link_retry *retry, int result,
                         uint64_t now_ms) {
    ast_mutex_lock(&routing_lock);
    struct ra_link_retry **cursor = &hub->retries;
    while (*cursor != retry) {
        cursor = &(*cursor)->next;
    }
    retry->attempting = false;
    if (atomic_load(&retry->paused)) {
        ast_mutex_unlock(&routing_lock);
        return;
    }
    if (atomic_load(&retry->cancelled) || !result || attached_locked(hub, retry->name) ||
        !retry->automatic) {
        *cursor = retry->next;
        ast_mutex_unlock(&routing_lock);
        release_retry(retry);
        return;
    }
    retry->delay_ms = ra_link_hub_retry_delay(retry->delay_ms);
    retry->due_ms = now_ms > UINT64_MAX - retry->delay_ms ? UINT64_MAX : now_ms + retry->delay_ms;
    ast_mutex_unlock(&routing_lock);
}

/** @brief Cancel one retained recovery request while the lifecycle lock is held.
 * @param hub Hub containing the request.
 * @param name Exact remote identity.
 * @return True when a recovery request existed.
 */
static bool cancel_retry_locked(struct ra_link_hub *hub, const char *name) {
    struct ra_link_retry **cursor = &hub->retries;
    while (*cursor) {
        struct ra_link_retry *retry = *cursor;
        if (strcmp(retry->name, name) || !retry->automatic) {
            cursor = &retry->next;
            continue;
        }
        atomic_store(&retry->cancelled, true);
        if (!retry->attempting) {
            *cursor = retry->next;
            release_retry(retry);
        }
        return true;
    }
    return false;
}

/** @brief Suspend all retained recovery requests while the lifecycle lock is held.
 * @param hub Hub whose reconnect-all state becomes dormant.
 */
static void pause_retries_locked(struct ra_link_hub *hub) {
    for (struct ra_link_retry *retry = hub->retries; retry; retry = retry->next) {
        atomic_store(&retry->paused, true);
    }
}

/** @brief Release every retained recovery request after the manager exits.
 * @param hub Hub whose retry list is no longer concurrently accessed.
 */
static void release_retries(struct ra_link_hub *hub) {
    while (hub->retries) {
        struct ra_link_retry *retry = hub->retries;
        hub->retries = retry->next;
        release_retry(retry);
    }
}

/** @brief Build one bounded app_rpt route list without allocating in the control manager.
 * @param text Caller-owned terminated destination.
 * @param capacity Destination size including its terminator.
 * @param length Number of route-list bytes already written.
 * @param truncated True after a complete next route would exceed the bound.
 */
struct ra_topology_writer {
    char *text;      /**< Caller-owned terminated route-list destination. */
    size_t capacity; /**< Destination capacity including its terminator. */
    size_t length;   /**< Number of route-list bytes already written. */
    bool truncated;  /**< True after a complete next route cannot fit. */
};

/** @brief Append one complete mode/name route while preserving a bounded wire payload.
 * @param writer Mutable route-list builder.
 * @param mode Route marker.
 * @param name Route identity bytes.
 * @param name_length Exact identity byte count.
 * @param terminal True only while appending the reserved truncation marker.
 *
 * No partial token is emitted. A later terminator identifies an intentionally truncated legacy
 * advertisement instead of presenting an invalid route list to a peer. Every private caller
 * supplies a terminated writer with `length < capacity`; successful appends preserve that
 * invariant.
 */
static void topology_append_route(struct ra_topology_writer *writer, char mode, const char *name,
                                  size_t name_length, bool terminal) {
    size_t separator = writer->length ? 1 : 0;
    size_t available = writer->capacity - writer->length - 1;
    size_t reserved = terminal ? 0 : sizeof("," RA_LINK_TOPOLOGY_TRUNCATION_ROUTE) - 1;
    size_t required = separator + 1 + name_length + reserved;
    if (required > available) {
        writer->truncated = true;
        return;
    }
    if (separator) {
        writer->text[writer->length++] = ',';
    }
    writer->text[writer->length++] = mode;
    for (size_t index = 0; index < name_length; ++index) {
        writer->text[writer->length++] = name[index];
    }
    writer->text[writer->length] = '\0';
}

/** @brief Append a cached peer list while applying monitor-route propagation.
 * @param writer Mutable route-list builder.
 * @param advertised Valid terminating-null app_rpt route list.
 * @param length Exact route-list byte count.
 * @param direct_mode Direct route mode that may downgrade downstream transceive entries.
 */
static void topology_append_cached(struct ra_topology_writer *writer, const char *advertised,
                                   size_t length, char direct_mode) {
    size_t start = 0;
    while (start < length && !writer->truncated) {
        size_t end = start;
        while (end < length && advertised[end] != ',') {
            ++end;
        }
        char mode = advertised[start];
        if (direct_mode == 'R' && mode == 'T') {
            mode = 'R';
        }
        topology_append_route(writer, mode, advertised + start + 1, end - start - 1, false);
        start = end + 1;
    }
}

/** @brief Finish a truncated app_rpt list with its conventional receive-only sentinel.
 * @param writer Mutable route-list builder.
 */
static void topology_finish(struct ra_topology_writer *writer) {
    if (!writer->truncated) {
        return;
    }
    writer->truncated = false;
    topology_append_route(writer, RA_LINK_TOPOLOGY_TRUNCATION_ROUTE[0],
                          &RA_LINK_TOPOLOGY_TRUNCATION_ROUTE[1],
                          sizeof(RA_LINK_TOPOLOGY_TRUNCATION_ROUTE) - 2, true);
}

/** @brief Build the status visible through one direct IAX link while routing is stable.
 * @param hub Hub containing direct peers and their cached linked-node lists.
 * @param recipient Direct recipient excluded from its own advertised topology.
 * @param writer Mutable bounded route-list builder.
 *
 * app_rpt omits local-monitor peers from linked-node advertisements and does not report the
 * recipient back to itself. A monitor direct route converts downstream transceive routes to
 * receive-only routes, matching app_rpt's `__mklinklist()` propagation rule.
 */
static void topology_build_locked(struct ra_link_hub *hub, const struct ra_link_port *recipient,
                                  struct ra_topology_writer *writer) {
    for (struct ra_link_port *port = atomic_load_explicit(&hub->ports, memory_order_seq_cst);
         port && !writer->truncated;
         port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
        if (port == recipient || atomic_load(&port->peer.ended) || !port->forward) {
            continue;
        }
        char mode = port->transmit ? 'T' : 'R';
        topology_append_route(writer, mode, port->name, strlen(port->name), false);
        if (writer->truncated) {
            continue;
        }
        char advertised[RA_LINK_TOPOLOGY_TEXT_MAX + 1];
        size_t length = ra_link_peer_topology(&port->peer, advertised, sizeof(advertised));
        if (length && length < sizeof(advertised)) {
            topology_append_cached(writer, advertised, length, mode);
        }
    }
    topology_finish(writer);
}

/** @brief Queue the latest complete topology for every attached IAX peer while locked.
 * @param hub Hub whose direct peers receive independent recipient-excluded lists.
 */
static void topology_advertise_locked(struct ra_link_hub *hub) {
    for (struct ra_link_port *port = atomic_load_explicit(&hub->ports, memory_order_seq_cst); port;
         port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
        if (atomic_load(&port->peer.ended)) {
            continue;
        }
        char advertised[RA_LINK_TOPOLOGY_ADVERTISEMENT_MAX + 1] = "";
        struct ra_topology_writer writer = {.text = advertised, .capacity = sizeof(advertised)};
        topology_build_locked(hub, port, &writer);
        (void)ra_link_peer_queue_topology(&port->peer, advertised);
    }
}

/** @brief Check whether a validated remote route list reaches this local node.
 * @param topology Comma-separated app_rpt route payload.
 * @param local_name Local node identity to find.
 * @return True when an advertised route names the local node.
 *
 * Both inputs are nonempty validated strings supplied by `detach_topology_loop_locked`. A remote
 * `L` list containing this node proves that retaining the direct peer would close an RF/IP
 * topology loop. Route mode is irrelevant: any reachable copy of the local node loops.
 */
static bool topology_contains_local(const char *topology, const char *local_name) {
    size_t local_length = strlen(local_name);
    for (size_t start = 0; topology[start];) {
        size_t end = start;
        while (topology[end] && topology[end] != ',') {
            ++end;
        }
        if (end - start - 1 == local_length &&
            !memcmp(topology + start + 1, local_name, local_length)) {
            return true;
        }
        start = topology[end] ? end + 1 : end;
    }
    return false;
}

/** @brief Detach one peer whose advertised route graph has looped back to this node.
 * @param hub Hub whose control-plane peer list is locked.
 * @return Detached loop-forming peer, or null.
 */
static struct ra_link_port *detach_topology_loop_locked(struct ra_link_hub *hub) {
    if (!hub->local_name || !*hub->local_name) {
        return NULL;
    }
    _Atomic(struct ra_link_port *) *cursor = &hub->ports;
    struct ra_link_port *port = atomic_load_explicit(cursor, memory_order_seq_cst);
    while (port) {
        char topology[RA_LINK_TOPOLOGY_TEXT_MAX + 1];
        size_t length = ra_link_peer_topology(&port->peer, topology, sizeof(topology));
        if (length < sizeof(topology) && topology_contains_local(topology, hub->local_name)) {
            atomic_store_explicit(cursor, atomic_load_explicit(&port->next, memory_order_seq_cst),
                                  memory_order_seq_cst);
            return port;
        }
        cursor = &port->next;
        port = atomic_load_explicit(cursor, memory_order_seq_cst);
    }
    return NULL;
}

/** @brief Wait outside real-time processing for readers of a detached port.
 * @param hub Hub whose published list changed.
 *
 * The sequentially consistent reader entry, list publication, and reclaim check form one total
 * order. A reader that can still observe an unlinked port entered before its unlink and must
 * decrement before this load can read zero; a later reader cannot observe that port.
 */
static void wait_readers(struct ra_link_hub *hub) {
    while (atomic_load_explicit(&hub->readers, memory_order_seq_cst)) {
        const struct timespec interval = {.tv_nsec = 1000000};
        (void)nanosleep(&interval, NULL);
    }
}

/** @brief Unlink one port while the lifecycle lock is held.
 * @param hub Hub containing the port.
 * @param name Exact name, or null to select an ended reader.
 * @param permanent Exact permanence required for a named selection; ignored for an ended reader.
 * @return Detached port or null.
 */
static struct ra_link_port *detach_locked(struct ra_link_hub *hub, const char *name,
                                          int permanent) {
    _Atomic(struct ra_link_port *) *cursor = &hub->ports;
    struct ra_link_port *port = atomic_load_explicit(cursor, memory_order_seq_cst);
    while (port && (name             ? strcmp(port->name, name) || port->permanent != permanent
                    : permanent >= 0 ? port->permanent != permanent
                                     : !atomic_load(&port->peer.ended))) {
        cursor = &port->next;
        port = atomic_load_explicit(cursor, memory_order_seq_cst);
    }
    if (port) {
        atomic_store_explicit(cursor, atomic_load_explicit(&port->next, memory_order_seq_cst),
                              memory_order_seq_cst);
    }
    return port;
}

/** @brief Reap ended readers and retry permanent links outside hardware-paced audio.
 * @param argument Owning hub, retained until this thread is joined.
 * @return Null after shutdown.
 */
static void *manage(void *argument) {
    struct ra_link_hub *hub = argument;
    unsigned int observed_topology = UINT_MAX;
    uint64_t topology_due_ms = 0;
    while (!atomic_load(&hub->stop)) {
        uint64_t now_ms = monotonic_ms();
        ast_mutex_lock(&routing_lock);
        struct ra_link_port *port = detach_locked(hub, NULL, -1);
        bool topology_loop = false;
        if (!port) {
            port = detach_topology_loop_locked(hub);
            topology_loop = port != NULL;
        }
        if (port && !topology_loop && port->permanent && hub->reconnect) {
            (void)schedule_retry_locked(hub, port->name, port->transmit, port->forward, now_ms,
                                        true, false);
        }
        if (port) {
            topology_changed(hub);
        }
        struct ra_link_retry *retry = port ? NULL : take_retry_locked(hub, now_ms);
        unsigned int generation =
            atomic_load_explicit(&hub->topology_generation, memory_order_acquire);
        if (generation != observed_topology || now_ms >= topology_due_ms) {
            topology_advertise_locked(hub);
            observed_topology = generation;
            topology_due_ms = topology_next_post(now_ms);
        }
        ast_mutex_unlock(&routing_lock);
        if (port) {
            wait_readers(hub);
            report_event(hub, port->name, false);
            release_port(port);
        } else if (retry) {
            int result = hub->reconnect
                             ? hub->reconnect(hub->reconnect_context, retry->name, retry->transmit,
                                              retry->forward, retry->automatic, &retry->cancelled,
                                              &retry->paused)
                             : -1;
            finish_retry(hub, retry, result, monotonic_ms());
        } else {
            const struct timespec interval = {.tv_nsec = 50000000};
            (void)nanosleep(&interval, NULL);
        }
    }
    return NULL;
}

/** @brief Start the one control-plane manager for a hub when its first peer or retry needs it.
 * @param hub Hub whose caller serializes lifecycle setup.
 * @return True when a manager is already running or was created.
 *
 * The manager is intentionally independent of routing-buffer allocation: an initial failed
 * permanent call has no attached port, but still needs its retry callback to run.
 */
static bool start_manager(struct ra_link_hub *hub) {
    if (hub->manager_started) {
        return true;
    }
    atomic_store_explicit(&hub->stop, false, memory_order_seq_cst);
    if (pthread_create(&hub->manager, NULL, manage, hub)) {
        return false;
    }
    hub->manager_started = true;
    return true;
}

int ra_link_hub_attach(struct ra_link_hub *hub, const char *name, struct ast_channel *channel,
                       struct ast_format *linear, bool transmit, bool forward, bool permanent) {
    unsigned int local_rate = ast_format_get_sample_rate(linear);
    struct ast_format *read_format = ast_channel_rawreadformat(channel);
    struct ast_format *write_format = ast_channel_rawwriteformat(channel);
    unsigned int read_rate = read_format ? ast_format_get_sample_rate(read_format) : 0;
    unsigned int write_rate = write_format ? ast_format_get_sample_rate(write_format) : 0;
    if (!local_rate || !read_rate || read_rate != write_rate) {
        return -1;
    }
    /* Raw formats describe the negotiated IAX wire codec before this function
     * asks Asterisk for matching-rate SLIN. The radio rate is deliberately
     * independent and libsamplerate joins the two clock domains below. */
    struct ast_format *peer_linear = ast_format_cache_get_slin_by_rate(read_rate);
    if (!peer_linear || ast_format_get_sample_rate(peer_linear) != read_rate) {
        return -1;
    }
    size_t local_capacity = hub->local ? hub->capacity : local_rate;
    if (hub->local && hub->rate != local_rate) {
        return -1;
    }
    /* The hub retains exactly one local-rate second of scratch space. A full
     * local block therefore maps to at most one peer-rate second. */
    size_t peer_capacity = read_rate;
    size_t audio_capacity = peer_capacity > local_capacity ? peer_capacity : local_capacity;
    struct ra_link_port *port = ast_calloc(1, sizeof(*port));
    if (!port) {
        return -1;
    }
    atomic_init(&port->next, NULL);
    port->name = ast_strdup(name);
    /* audio is reused after peer-to-local conversion, so it must hold either
     * rate's largest block. output remains peer-rate-only. */
    port->audio = ast_calloc(audio_capacity, sizeof(*port->audio));
    port->output = ast_calloc(peer_capacity, sizeof(*port->output));
    size_t workspace = audio_capacity;
    port->src_in = ast_calloc(workspace, sizeof(*port->src_in));
    port->src_out = ast_calloc(workspace, sizeof(*port->src_out));
    port->rate = read_rate;
    if (!port->name || !port->audio || !port->output || !port->src_in || !port->src_out) {
        release_port(port);
        return -1;
    }
    if (read_rate != local_rate) {
        int error = 0;
        port->receive_src = src_new(SRC_SINC_FASTEST, 1, &error);
        port->send_src = src_new(SRC_SINC_FASTEST, 1, &error);
        if (!port->receive_src || !port->send_src || error) {
            release_port(port);
            return -1;
        }
    }
    ast_mutex_lock(&routing_lock);
    bool duplicate = false;
    for (struct ra_link_port *entry = atomic_load_explicit(&hub->ports, memory_order_seq_cst);
         entry; entry = atomic_load_explicit(&entry->next, memory_order_seq_cst)) {
        duplicate |= !strcmp(entry->name, name);
    }
    if (!hub->local) {
        hub->local = ast_calloc(local_capacity * 3, sizeof(*hub->local));
        hub->capacity = local_capacity;
        hub->rate = local_rate;
        if (hub->local) {
            hub->remote = hub->local + local_capacity;
            hub->outgoing = hub->remote + local_capacity;
        }
    }
    if (duplicate || !hub->local) {
        ast_mutex_unlock(&routing_lock);
        release_port(port);
        return -1;
    }
    /* Caller serializes configuration/admission; the reader starts before publication. */
    ast_mutex_unlock(&routing_lock);
    if (!start_manager(hub)) {
        release_port(port);
        return -1;
    }
    port->hub = hub;
    port->peer.topology_generation = &hub->topology_generation;
    if (ra_link_peer_start(&port->peer, channel, peer_linear, receive_digit, port)) {
        release_port(port);
        return -1;
    }
    port->transmit = transmit;
    port->forward = forward;
    port->permanent = permanent;
    ast_mutex_lock(&routing_lock);
    atomic_store_explicit(&port->next, atomic_load_explicit(&hub->ports, memory_order_seq_cst),
                          memory_order_seq_cst);
    atomic_store_explicit(&hub->ports, port, memory_order_seq_cst);
    topology_changed(hub);
    ast_mutex_unlock(&routing_lock);
    report_event(hub, port->name, true);
    return 0;
}

/** @brief Resample one bounded block with libsamplerate at the link boundary.
 * @param state Stateful converter, or null for same-rate copying.
 * @param input Floating-point input workspace.
 * @param output Floating-point output workspace.
 * @param source Input PCM samples.
 * @param source_count Number of input samples.
 * @param destination Output PCM samples.
 * @param destination_count Number of output samples.
 * @param ratio Output/input sample-rate ratio.
 */
static void adapt(SRC_STATE *state, float *input, float *output, const int16_t *source,
                  size_t source_count, int16_t *destination, size_t destination_count,
                  double ratio) {
    if (!destination_count) {
        return;
    }
    if (!state) {
        /* A null converter is selected only when the exact-rate scheduler has
         * made equally sized peer and local blocks. */
        for (size_t index = 0; index < destination_count; ++index) {
            destination[index] = source[index];
        }
        return;
    }
    src_short_to_float_array(source, input, (int)source_count);
    SRC_DATA data = {.data_in = input,
                     .data_out = output,
                     .input_frames = (long)source_count,
                     .output_frames = (long)destination_count,
                     .src_ratio = ratio,
                     .end_of_input = 0};
    if (src_process(state, &data)) {
        for (size_t index = 0; index < destination_count; ++index) {
            destination[index] = 0;
        }
        return;
    }
    src_float_to_short_array(output, destination, (int)data.output_frames_gen);
    if ((size_t)data.output_frames_gen < destination_count) {
        for (size_t index = (size_t)data.output_frames_gen; index < destination_count; ++index) {
            destination[index] = 0;
        }
    }
}

/** @brief Schedule an exact peer block length without accumulating rate drift.
 * @param port Peer retaining its fractional-rate remainder.
 * @param local_samples Local block length.
 * @param local_rate Radio rate.
 * @return Peer block length for this local interval.
 */
static size_t peer_samples(struct ra_link_port *port, size_t local_samples, size_t local_rate) {
    uint64_t total = port->remainder + (uint64_t)local_samples * port->rate;
    port->remainder = total % local_rate;
    return (size_t)(total / local_rate);
}

void ra_link_hub_set_reconnector(struct ra_link_hub *hub, ra_link_reconnect_fn callback,
                                 void *context) {
    hub->reconnect = callback;
    hub->reconnect_context = context;
}

bool ra_link_hub_retain_permanent(struct ra_link_hub *hub, const char *name, bool transmit,
                                  bool forward) {
    if (!hub->reconnect) {
        return false;
    }
    ast_mutex_lock(&routing_lock);
    bool retained =
        !attached_locked(hub, name) &&
        schedule_retry_locked(hub, name, transmit, forward, monotonic_ms(), true, false) != NULL;
    ast_mutex_unlock(&routing_lock);
    if (retained && !start_manager(hub)) {
        ast_mutex_lock(&routing_lock);
        (void)cancel_retry_locked(hub, name);
        ast_mutex_unlock(&routing_lock);
        return false;
    }
    return retained;
}

void ra_link_hub_set_digit_handler(struct ra_link_hub *hub, ra_link_hub_digit_fn callback,
                                   void *context) {
    hub->digit = callback;
    hub->digit_context = context;
}

void ra_link_hub_set_event_handler(struct ra_link_hub *hub, ra_link_hub_event_fn callback,
                                   void *context) {
    hub->event = callback;
    hub->event_context = context;
}

bool ra_link_hub_disconnect(struct ra_link_hub *hub, const char *name) {
    ast_mutex_lock(&routing_lock);
    struct ra_link_port *port = detach_locked(hub, name, 0);
    if (port) {
        topology_changed(hub);
    }
    ast_mutex_unlock(&routing_lock);
    if (!port) {
        return false;
    }
    wait_readers(hub);
    report_event(hub, port->name, false);
    release_port(port);
    return true;
}

bool ra_link_hub_disconnect_permanent(struct ra_link_hub *hub, const char *name) {
    ast_mutex_lock(&routing_lock);
    struct ra_link_port *port = detach_locked(hub, name, 1);
    bool cancelled = cancel_retry_locked(hub, name);
    if (port) {
        topology_changed(hub);
    }
    ast_mutex_unlock(&routing_lock);
    if (port) {
        wait_readers(hub);
        report_event(hub, port->name, false);
        release_port(port);
    }
    return port || cancelled;
}

bool ra_link_hub_detach_reconnect(struct ra_link_hub *hub, const char *name, bool permanent) {
    ast_mutex_lock(&routing_lock);
    struct ra_link_port *port = detach_locked(hub, name, permanent);
    if (port) {
        topology_changed(hub);
    }
    ast_mutex_unlock(&routing_lock);
    if (!port) {
        return false;
    }
    wait_readers(hub);
    report_event(hub, port->name, false);
    release_port(port);
    return true;
}

size_t ra_link_hub_disconnect_all(struct ra_link_hub *hub) {
    size_t count = 0;
    ast_mutex_lock(&routing_lock);
    pause_retries_locked(hub);
    ast_mutex_unlock(&routing_lock);
    for (;;) {
        ast_mutex_lock(&routing_lock);
        struct ra_link_port *port = atomic_load_explicit(&hub->ports, memory_order_seq_cst);
        if (port) {
            atomic_store_explicit(&hub->ports,
                                  atomic_load_explicit(&port->next, memory_order_seq_cst),
                                  memory_order_seq_cst);
            (void)schedule_retry_locked(hub, port->name, port->transmit, port->forward,
                                        monotonic_ms(), port->permanent, true);
            topology_changed(hub);
        }
        ast_mutex_unlock(&routing_lock);
        if (!port) {
            return count;
        }
        wait_readers(hub);
        report_event(hub, port->name, false);
        release_port(port);
        ++count;
    }
}

size_t ra_link_hub_disconnect_nonpermanent_all(struct ra_link_hub *hub) {
    size_t count = 0;
    for (;;) {
        ast_mutex_lock(&routing_lock);
        struct ra_link_port *port = detach_locked(hub, NULL, 0);
        if (port) {
            topology_changed(hub);
        }
        ast_mutex_unlock(&routing_lock);
        if (!port) {
            return count;
        }
        wait_readers(hub);
        report_event(hub, port->name, false);
        release_port(port);
        ++count;
    }
}

size_t ra_link_hub_reconnect_all(struct ra_link_hub *hub) {
    size_t count = 0;
    uint64_t now_ms = monotonic_ms();
    ast_mutex_lock(&routing_lock);
    for (struct ra_link_retry *retry = hub->retries; retry; retry = retry->next) {
        if (!atomic_load(&retry->cancelled)) {
            atomic_store(&retry->paused, false);
            retry->delay_ms = 0;
            retry->due_ms = now_ms;
            ++count;
        }
    }
    ast_mutex_unlock(&routing_lock);
    return count;
}

bool ra_link_hub_has_retained_state(struct ra_link_hub *hub) {
    ast_mutex_lock(&routing_lock);
    bool retained = atomic_load_explicit(&hub->ports, memory_order_seq_cst) || hub->retries;
    ast_mutex_unlock(&routing_lock);
    return retained;
}

size_t ra_link_hub_snapshot(struct ra_link_hub *hub, struct ra_link_peer_status *entries,
                            size_t capacity) {
    size_t count = 0;
    ast_mutex_lock(&routing_lock);
    for (struct ra_link_port *port = atomic_load_explicit(&hub->ports, memory_order_seq_cst); port;
         port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
        if (atomic_load(&port->peer.ended)) {
            continue;
        }
        if (entries && count < capacity) {
            struct ra_link_peer_status *entry = &entries[count];
            ast_copy_string(entry->name, port->name, sizeof(entry->name));
            entry->transmit = port->transmit;
            entry->forward = port->forward;
            entry->permanent = port->permanent;
            entry->retrying = false;
            entry->paused = false;
            entry->receive_missing = atomic_load(&port->peer.received.missing);
            entry->consecutive_underruns = atomic_load(&port->peer.received.consecutive_underruns);
            entry->underrun_average_milli =
                atomic_load(&port->peer.received.underrun_average_milli);
            struct rpcr_observation observation;
            rpcr_observe(&port->peer.received, &observation);
            if (port->peer.linear_rate) {
                entry->receive_reserve_ms =
                    (unsigned int)(observation.reserve_samples * 1000U / port->peer.linear_rate);
                entry->receive_capacity_ms =
                    (unsigned int)(observation.capacity_samples * 1000U / port->peer.linear_rate);
                entry->receive_occupancy_ms =
                    (unsigned int)(observation.available_samples * 1000U / port->peer.linear_rate);
                entry->receive_filtered_occupancy_ms =
                    (unsigned int)(observation.filtered_occupancy_samples * 1000U /
                                   port->peer.linear_rate);
                entry->receive_target_ms =
                    (unsigned int)(observation.target_samples * 1000U / port->peer.linear_rate);
            }
            entry->receive_ratio_correction_ppm = observation.ratio_correction_ppm;
        }
        ++count;
    }
    for (struct ra_link_retry *retry = hub->retries; retry; retry = retry->next) {
        /* A manager-created replacement can coexist briefly with its old retry record. */
        if (attached_locked(hub, retry->name)) {
            continue;
        }
        if (entries && count < capacity) {
            struct ra_link_peer_status *entry = &entries[count];
            ast_copy_string(entry->name, retry->name, sizeof(entry->name));
            entry->transmit = retry->transmit;
            entry->forward = retry->forward;
            entry->permanent = retry->automatic;
            entry->retrying = true;
            entry->paused = atomic_load(&retry->paused);
        }
        ++count;
    }
    ast_mutex_unlock(&routing_lock);
    return count;
}

char *ra_link_hub_topology(struct ra_link_hub *hub) {
    char *topology = ast_calloc(RA_LINK_TOPOLOGY_TEXT_MAX + 1, sizeof(*topology));
    if (!topology) {
        return NULL;
    }
    struct ra_topology_writer writer = {.text = topology,
                                        .capacity = RA_LINK_TOPOLOGY_TEXT_MAX + 1};
    ast_mutex_lock(&routing_lock);
    topology_build_locked(hub, NULL, &writer);
    ast_mutex_unlock(&routing_lock);
    return topology;
}

bool ra_link_hub_last_keyed(const struct ra_link_hub *hub, char *name, size_t capacity) {
    if (!name || capacity < RA_LINK_PEER_NAME_MAX) {
        return false;
    }
    unsigned int before = atomic_load_explicit(&hub->last_keyed_sequence, memory_order_acquire);
    if (before & 1U) {
        return false;
    }
    for (size_t index = 0; index < RA_LINK_PEER_NAME_MAX; ++index) {
        name[index] = (char)atomic_load_explicit(&hub->last_keyed[index], memory_order_relaxed);
    }
    unsigned int after = atomic_load_explicit(&hub->last_keyed_sequence, memory_order_acquire);
    return before == after && name[0] != '\0';
}

int ra_link_hub_send_digit(struct ra_link_hub *hub, const char *name, char digit) {
    int result = -1;
    ast_mutex_lock(&routing_lock);
    for (struct ra_link_port *port = atomic_load_explicit(&hub->ports, memory_order_seq_cst); port;
         port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
        if (!strcmp(port->name, name)) {
            result = ra_link_peer_send_digit(&port->peer, digit);
            break;
        }
    }
    ast_mutex_unlock(&routing_lock);
    return result;
}

bool ra_link_hub_connected(struct ra_link_hub *hub, const char *name) {
    bool connected = false;
    ast_mutex_lock(&routing_lock);
    for (struct ra_link_port *port = atomic_load_explicit(&hub->ports, memory_order_seq_cst); port;
         port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
        if (!strcmp(port->name, name)) {
            connected = !atomic_load(&port->peer.ended);
            break;
        }
    }
    ast_mutex_unlock(&routing_lock);
    return connected;
}

void ra_link_hub_close(struct ra_link_hub *hub) {
    /* Teardown has no live radio controller to report to. */
    hub->event = NULL;
    hub->event_context = NULL;
    if (hub->manager_started) {
        atomic_store_explicit(&hub->stop, true, memory_order_seq_cst);
        (void)pthread_join(hub->manager, NULL);
    }
    (void)ra_link_hub_disconnect_all(hub);
    release_retries(hub);
    ast_free(hub->local);
    hub->local = NULL;
    hub->remote = NULL;
    hub->outgoing = NULL;
    hub->capacity = 0;
    hub->rate = 0;
    hub->manager_started = false;
    hub->reconnect = NULL;
    hub->reconnect_context = NULL;
    hub->digit = NULL;
    hub->digit_context = NULL;
    hub->retries = NULL;
    atomic_store_explicit(&hub->ports, NULL, memory_order_seq_cst);
    atomic_store_explicit(&hub->readers, 0, memory_order_seq_cst);
    atomic_store_explicit(&hub->stop, false, memory_order_seq_cst);
    atomic_store_explicit(&hub->topology_generation, 0, memory_order_seq_cst);
    atomic_store_explicit(&hub->last_keyed_sequence, 0, memory_order_seq_cst);
    for (size_t index = 0; index < RA_LINK_PEER_NAME_MAX; ++index) {
        atomic_store_explicit(&hub->last_keyed[index], '\0', memory_order_seq_cst);
    }
}

bool ra_link_hub_process(struct ra_link_hub *hub, struct ra_controller *controller, bool receiving,
                         int16_t *audio, size_t samples, uint64_t now_ms) {
    atomic_fetch_add_explicit(&hub->readers, 1, memory_order_seq_cst);
    struct ra_link_port *ports = atomic_load_explicit(&hub->ports, memory_order_seq_cst);
    if (samples || !ports) {
        controller->link_active = false;
    }
    controller->link_audio = NULL;
    if (ports && samples && samples <= hub->capacity) {
        for (size_t i = 0; i < samples; ++i) {
            hub->local[i] = receiving ? audio[i] : 0;
        }
        for (struct ra_link_port *port = ports; port;
             port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
            port->samples = peer_samples(port, samples, hub->rate);
            bool active = ra_link_peer_receive(&port->peer, port->audio, port->samples);
            if (active && !port->active) {
                remember_last_keyed(hub, port->name);
            }
            port->active = active;
            adapt(port->receive_src, port->src_in, port->src_out, port->audio, port->samples,
                  hub->outgoing, samples, (double)hub->rate / port->rate);
            for (size_t index = 0; index < samples; ++index) {
                port->audio[index] = hub->outgoing[index];
            }
            controller->link_active |= port->active;
        }
        for (size_t i = 0; i < samples; ++i) {
            int64_t sum = 0;
            for (struct ra_link_port *port = ports; port;
                 port = atomic_load_explicit(&port->next, memory_order_seq_cst)) {
                sum += port->audio[i];
            }
            hub->remote[i] = pcm(sum);
        }
        controller->link_audio = hub->remote;
        for (struct ra_link_port *destination = ports; destination;
             destination = atomic_load_explicit(&destination->next, memory_order_seq_cst)) {
            bool keyed = receiving;
            for (size_t i = 0; i < samples; ++i) {
                int64_t sum = hub->local[i];
                for (struct ra_link_port *source = ports; source;
                     source = atomic_load_explicit(&source->next, memory_order_seq_cst)) {
                    if (source != destination && source->forward) {
                        sum += source->audio[i];
                        keyed |= source->active;
                    }
                }
                hub->outgoing[i] = pcm(sum);
            }
            size_t count = destination->samples;
            const int16_t *send_audio = hub->outgoing;
            if (destination->rate != hub->rate) {
                adapt(destination->send_src, destination->src_in, destination->src_out,
                      hub->outgoing, samples, destination->output, count,
                      (double)destination->rate / hub->rate);
                send_audio = destination->output;
            }
            if (ra_link_peer_send(&destination->peer, destination->transmit && keyed, send_audio,
                                  count)) {
                atomic_store(&destination->peer.stop, true);
            }
        }
    }
    bool keyed = ra_controller_process(controller, receiving, audio, samples, now_ms);
    controller->link_audio = NULL;
    atomic_fetch_sub_explicit(&hub->readers, 1, memory_order_seq_cst);
    return keyed;
}
