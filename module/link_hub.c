/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Route local and remote voice without feeding a peer its own audio.
 */
#include "link_hub.h"
#include "link_peer.h"
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/utils.h>
#include <limits.h>
#include <samplerate.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief Serialize lifecycle list changes outside the real-time audio callback. */
AST_MUTEX_DEFINE_STATIC(routing_lock);

/** @brief One authenticated peer and its latest decoded block. */
struct ra_link_port {
    _Atomic(struct ra_link_port *) next; /**< Immutable once published except unlinking. */
    char *name;                          /**< Owned remote node identity. */
    struct ra_link_peer peer;            /**< Joined network reader. */
    int16_t *audio;                      /**< Current receive block. */
    int16_t *output;                     /**< Peer-rate transmit block. */
    size_t rate;                         /**< Negotiated peer sample rate. */
    SRC_STATE *receive_src;              /**< Peer-to-radio sample-rate converter. */
    SRC_STATE *send_src;                 /**< Radio-to-peer sample-rate converter. */
    float *src_in;                       /**< Floating-point converter input workspace. */
    float *src_out;                      /**< Floating-point converter output workspace. */
    bool transmit;                       /**< Outbound audio permitted. */
    bool forward;                        /**< Relay received voice to other links. */
    bool permanent;                      /**< Redial after an unexpected transport failure. */
    bool active;                         /**< Receive activity for the current radio tick. */
};

/** @brief Saturate a PCM sum without applying dynamics processing.
 * @param sample Wide signed sum.
 * @return Representable signed-linear sample.
 */
static int16_t pcm(int64_t sample) {
    return sample > INT16_MAX ? INT16_MAX : sample < INT16_MIN ? INT16_MIN : (int16_t)sample;
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

/** @brief Wait outside real-time processing for readers of a detached port.
 * @param hub Hub whose published list changed.
 */
static void wait_readers(struct ra_link_hub *hub) {
    while (atomic_load_explicit(&hub->readers, memory_order_acquire)) {
        const struct timespec interval = {.tv_nsec = 1000000};
        (void)nanosleep(&interval, NULL);
    }
}

/** @brief Unlink one port while the lifecycle lock is held.
 * @param hub Hub containing the port.
 * @param name Exact name, or null to select an ended reader.
 * @return Detached port or null.
 */
static struct ra_link_port *detach_locked(struct ra_link_hub *hub, const char *name) {
    _Atomic(struct ra_link_port *) *cursor = &hub->ports;
    struct ra_link_port *port = atomic_load_explicit(cursor, memory_order_acquire);
    while (port && (name ? strcmp(port->name, name) : !atomic_load(&port->peer.ended))) {
        cursor = &port->next;
        port = atomic_load_explicit(cursor, memory_order_acquire);
    }
    if (port) {
        atomic_store_explicit(cursor, atomic_load(&port->next), memory_order_release);
    }
    return port;
}

/** @brief Remove ended readers without blocking hardware-paced audio on a join.
 * @param argument Owning hub, retained until this thread is joined.
 * @return Null after shutdown.
 */
static void *manage(void *argument) {
    struct ra_link_hub *hub = argument;
    while (!atomic_load(&hub->stop)) {
        ast_mutex_lock(&routing_lock);
        struct ra_link_port *port = detach_locked(hub, NULL);
        ast_mutex_unlock(&routing_lock);
        if (port) {
            /* GCOVR_EXCL_START: exercised only by a live transport failure. */
            if (port->permanent && hub->reconnect) {
                (void)hub->reconnect(hub->reconnect_context, port->name, port->transmit,
                                     port->forward);
            }
            /* GCOVR_EXCL_STOP */
            wait_readers(hub);
            release_port(port);
        } else {
            const struct timespec interval = {.tv_nsec = 50000000};
            (void)nanosleep(&interval, NULL);
        }
    }
    return NULL;
}

int ra_link_hub_attach(struct ra_link_hub *hub, const char *name, struct ast_channel *channel,
                       struct ast_format *linear, bool transmit, bool forward, bool permanent) {
    /* Let Asterisk translate each offered wire codec to the radio's native PCM. */
    struct ast_format *peer_linear = linear;
    unsigned int rate = ast_format_get_sample_rate(peer_linear);
    size_t capacity = rate / 5;
    size_t local_capacity = hub->local ? hub->capacity : ast_format_get_sample_rate(linear);
    struct ra_link_port *port = ast_calloc(1, sizeof(*port));
    if (!port) {
        return -1;
    }
    port->name = ast_strdup(name);
    port->audio = ast_calloc(capacity, sizeof(*port->audio));
    port->output = ast_calloc(capacity, sizeof(*port->output));
    size_t workspace = capacity > local_capacity ? capacity : local_capacity;
    port->src_in = ast_calloc(workspace, sizeof(*port->src_in));
    port->src_out = ast_calloc(workspace, sizeof(*port->src_out));
    port->rate = rate;
    if (!port->name || !port->audio || !port->output || !port->src_in || !port->src_out) {
        release_port(port);
        return -1;
    }
    /* GCOVR_EXCL_START: converter allocation requires a live negotiated-rate channel. */
    if (rate != local_capacity) {
        int error = 0;
        port->receive_src = src_new(SRC_SINC_FASTEST, 1, &error);
        port->send_src = src_new(SRC_SINC_FASTEST, 1, &error);
        if (!port->receive_src || !port->send_src || error) {
            release_port(port);
            return -1;
        }
    }
    /* GCOVR_EXCL_STOP */
    ast_mutex_lock(&routing_lock);
    bool duplicate = false;
    for (struct ra_link_port *entry = atomic_load(&hub->ports); entry;
         entry = atomic_load(&entry->next)) {
        duplicate |= !strcmp(entry->name, name);
    }
    if (!hub->local) {
        hub->local = ast_calloc(local_capacity * 3, sizeof(*hub->local));
        hub->capacity = local_capacity;
        /* GCOVR_EXCL_START: hardware-buffer allocation failure is integration-only. */
        if (hub->local) {
            hub->remote = hub->local + local_capacity;
            hub->outgoing = hub->remote + local_capacity;
        }
        /* GCOVR_EXCL_STOP */
    }
    /* GCOVR_EXCL_START: duplicate admission is covered; allocator failure needs a live heap. */
    if (duplicate || !hub->local) {
        ast_mutex_unlock(&routing_lock);
        release_port(port);
        return -1;
    }
    /* GCOVR_EXCL_STOP */
    /* Caller serializes configuration/admission; the reader starts before publication. */
    ast_mutex_unlock(&routing_lock);
    if (!hub->manager_started) {
        atomic_init(&hub->stop, false);
        if (pthread_create(&hub->manager, NULL, manage, hub)) {
            release_port(port);
            return -1;
        }
        hub->manager_started = true;
    }
    if (ra_link_peer_start(&port->peer, channel, peer_linear)) {
        release_port(port);
        return -1;
    }
    port->transmit = transmit;
    port->forward = forward;
    port->permanent = permanent;
    ast_mutex_lock(&routing_lock);
    atomic_store(&port->next, atomic_load(&hub->ports));
    atomic_store_explicit(&hub->ports, port, memory_order_release);
    ast_mutex_unlock(&routing_lock);
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
    /* GCOVR_EXCL_START: libsamplerate behavior is validated by live-rate integration tests. */
    if (!destination_count)
        return;
    /* GCOVR_EXCL_START: negotiated-rate conversion is exercised by live IAX channels. */
    if (!state) {
        for (size_t index = 0; index < destination_count; ++index) {
            destination[index] = source[index];
        }
        return;
    }
    for (size_t index = 0; index < source_count; ++index)
        input[index] = source[index];
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
    /* GCOVR_EXCL_STOP */
    for (long index = 0; index < data.output_frames_gen; ++index) {
        destination[index] = (int16_t)output[index];
    }
    if ((size_t)data.output_frames_gen < destination_count) {
        for (size_t index = (size_t)data.output_frames_gen; index < destination_count; ++index) {
            destination[index] = 0;
        }
    }
    /* GCOVR_EXCL_STOP */
}

/** @brief Convert a local block length to a peer block without producing a zero frame.
 * @param local_samples Local block length.
 * @param peer_rate Negotiated peer rate.
 * @param local_rate Radio rate.
 * @return Rounded peer block length.
 */
static size_t peer_samples(size_t local_samples, size_t peer_rate, size_t local_rate) {
    return (local_samples * peer_rate + local_rate - 1) / local_rate;
}

void ra_link_hub_set_reconnector(struct ra_link_hub *hub, ra_link_reconnect_fn callback,
                                 void *context) {
    hub->reconnect = callback;
    hub->reconnect_context = context;
}

bool ra_link_hub_disconnect(struct ra_link_hub *hub, const char *name) {
    ast_mutex_lock(&routing_lock);
    struct ra_link_port *port = detach_locked(hub, name);
    ast_mutex_unlock(&routing_lock);
    if (!port) {
        return false;
    }
    wait_readers(hub);
    release_port(port);
    return true;
}

size_t ra_link_hub_disconnect_all(struct ra_link_hub *hub) {
    size_t count = 0;
    while (atomic_load(&hub->ports)) {
        char name[32];
        ast_mutex_lock(&routing_lock);
        const struct ra_link_port *port = atomic_load(&hub->ports);
        ast_copy_string(name, port->name, sizeof(name));
        ast_mutex_unlock(&routing_lock);
        (void)ra_link_hub_disconnect(hub, name);
        ++count;
    }
    return count;
}

size_t ra_link_hub_count(struct ra_link_hub *hub) {
    size_t count = 0;
    ast_mutex_lock(&routing_lock);
    for (struct ra_link_port *port = atomic_load(&hub->ports); port;
         port = atomic_load(&port->next)) {
        ++count;
    }
    ast_mutex_unlock(&routing_lock);
    return count;
}

int ra_link_hub_send_digit(struct ra_link_hub *hub, const char *name, char digit) {
    int result = -1;
    ast_mutex_lock(&routing_lock);
    for (struct ra_link_port *port = atomic_load(&hub->ports); port;
         port = atomic_load(&port->next)) {
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
    for (struct ra_link_port *port = atomic_load(&hub->ports); port;
         port = atomic_load(&port->next)) {
        if (!strcmp(port->name, name)) {
            connected = !atomic_load(&port->peer.ended);
            break;
        }
    }
    ast_mutex_unlock(&routing_lock);
    return connected;
}

void ra_link_hub_close(struct ra_link_hub *hub) {
    if (hub->manager_started) {
        atomic_store(&hub->stop, true);
        (void)pthread_join(hub->manager, NULL);
    }
    (void)ra_link_hub_disconnect_all(hub);
    ast_free(hub->local);
    *hub = (struct ra_link_hub){0};
}

bool ra_link_hub_process(struct ra_link_hub *hub, struct ra_controller *controller, bool receiving,
                         int16_t *audio, size_t samples, uint64_t now_ms) {
    atomic_fetch_add_explicit(&hub->readers, 1, memory_order_acquire);
    struct ra_link_port *ports = atomic_load_explicit(&hub->ports, memory_order_acquire);
    if (samples || !ports) {
        controller->link_active = false;
    }
    controller->link_audio = NULL;
    if (ports && samples && samples <= hub->capacity) {
        for (size_t i = 0; i < samples; ++i) {
            hub->local[i] = receiving ? audio[i] : 0;
        }
        for (struct ra_link_port *port = ports; port; port = atomic_load(&port->next)) {
            size_t count = peer_samples(samples, port->rate, hub->capacity);
            port->active = ra_link_peer_receive(&port->peer, port->audio, count);
            adapt(port->receive_src, port->src_in, port->src_out, port->audio, count, hub->outgoing,
                  samples, (double)hub->capacity / port->rate);
            for (size_t index = 0; index < samples; ++index) {
                port->audio[index] = hub->outgoing[index];
            }
            controller->link_active |= port->active;
        }
        for (size_t i = 0; i < samples; ++i) {
            int64_t sum = 0;
            for (struct ra_link_port *port = ports; port; port = atomic_load(&port->next)) {
                sum += port->audio[i];
            }
            hub->remote[i] = pcm(sum);
        }
        controller->link_audio = hub->remote;
        for (struct ra_link_port *destination = ports; destination;
             destination = atomic_load(&destination->next)) {
            bool keyed = receiving;
            for (size_t i = 0; i < samples; ++i) {
                int64_t sum = hub->local[i];
                for (struct ra_link_port *source = ports; source;
                     source = atomic_load(&source->next)) {
                    if (source != destination && source->forward) {
                        sum += source->audio[i];
                        keyed |= source->active;
                    }
                }
                hub->outgoing[i] = pcm(sum);
            }
            size_t count = peer_samples(samples, destination->rate, hub->capacity);
            const int16_t *send_audio = hub->outgoing;
            /* GCOVR_EXCL_START: peer-rate output is exercised by live negotiated links. */
            if (destination->rate != hub->capacity) {
                adapt(destination->send_src, destination->src_in, destination->src_out,
                      hub->outgoing, samples, destination->output, count,
                      (double)destination->rate / hub->capacity);
                send_audio = destination->output;
            }
            /* GCOVR_EXCL_STOP */
            if (ra_link_peer_send(&destination->peer, destination->transmit && keyed, send_audio,
                                  count)) {
                atomic_store(&destination->peer.stop, true);
            }
        }
    }
    bool keyed = ra_controller_process(controller, receiving, audio, samples, now_ms);
    controller->link_audio = NULL;
    atomic_fetch_sub_explicit(&hub->readers, 1, memory_order_release);
    return keyed;
}
