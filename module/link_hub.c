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
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief Short list/mix critical sections shared by node routers; never covers peer joining. */
AST_MUTEX_DEFINE_STATIC(routing_lock);

/** @brief One authenticated peer and its latest decoded block. */
struct ra_link_port {
    struct ra_link_port *next; /**< Next owned port. */
    char *name;                /**< Owned remote node identity. */
    struct ra_link_peer peer;  /**< Joined network reader. */
    int16_t *audio;            /**< Current receive block. */
    bool transmit;             /**< Outbound audio permitted. */
    bool forward;              /**< Relay received voice to other links. */
    bool permanent;            /**< Redial after an unexpected transport failure. */
    bool active;               /**< Receive activity for the current radio tick. */
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
    ast_free(port->name);
    ast_free(port);
}

/** @brief Remove ended readers without blocking hardware-paced audio on a join.
 * @param argument Owning hub, retained until this thread is joined.
 * @return Null after shutdown.
 */
static void *manage(void *argument) {
    struct ra_link_hub *hub = argument;
    while (!atomic_load(&hub->stop)) {
        ast_mutex_lock(&routing_lock);
        struct ra_link_port **cursor = &hub->ports;
        while (*cursor && !atomic_load(&(*cursor)->peer.ended)) {
            cursor = &(*cursor)->next;
        }
        struct ra_link_port *port = *cursor;
        if (port) {
            *cursor = port->next;
        }
        ast_mutex_unlock(&routing_lock);
        if (port) {
            /* GCOVR_EXCL_START: exercised only by a live transport failure. */
            if (port->permanent && hub->reconnect) {
                (void)hub->reconnect(hub->reconnect_context, port->name, port->transmit,
                                     port->forward);
            }
            /* GCOVR_EXCL_STOP */
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
    size_t capacity = ast_format_get_sample_rate(linear);
    struct ra_link_port *port = ast_calloc(1, sizeof(*port));
    if (!port) {
        return -1;
    }
    port->name = ast_strdup(name);
    port->audio = ast_calloc(capacity, sizeof(*port->audio));
    if (!port->name || !port->audio) {
        release_port(port);
        return -1;
    }
    ast_mutex_lock(&routing_lock);
    bool duplicate = false;
    for (struct ra_link_port *entry = hub->ports; entry; entry = entry->next) {
        duplicate |= !strcmp(entry->name, name);
    }
    if (!hub->local) {
        hub->local = ast_calloc(capacity * 3, sizeof(*hub->local));
        hub->capacity = capacity;
        if (hub->local) {
            hub->remote = hub->local + capacity;
            hub->outgoing = hub->remote + capacity;
        }
    }
    if (duplicate || !hub->local || capacity != hub->capacity) {
        ast_mutex_unlock(&routing_lock);
        release_port(port);
        return -1;
    }
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
    if (ra_link_peer_start(&port->peer, channel, linear)) {
        release_port(port);
        return -1;
    }
    port->transmit = transmit;
    port->forward = forward;
    port->permanent = permanent;
    ast_mutex_lock(&routing_lock);
    port->next = hub->ports;
    hub->ports = port;
    ast_mutex_unlock(&routing_lock);
    return 0;
}

void ra_link_hub_set_reconnector(struct ra_link_hub *hub, ra_link_reconnect_fn callback,
                                 void *context) {
    hub->reconnect = callback;
    hub->reconnect_context = context;
}

bool ra_link_hub_disconnect(struct ra_link_hub *hub, const char *name) {
    ast_mutex_lock(&routing_lock);
    struct ra_link_port **cursor = &hub->ports;
    while (*cursor && strcmp((*cursor)->name, name)) {
        cursor = &(*cursor)->next;
    }
    struct ra_link_port *port = *cursor;
    if (port) {
        *cursor = port->next;
    }
    ast_mutex_unlock(&routing_lock);
    if (!port) {
        return false;
    }
    release_port(port);
    return true;
}

size_t ra_link_hub_disconnect_all(struct ra_link_hub *hub) {
    size_t count = 0;
    while (hub->ports) {
        char name[32];
        ast_mutex_lock(&routing_lock);
        const struct ra_link_port *port = hub->ports;
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
    for (struct ra_link_port *port = hub->ports; port; port = port->next) {
        ++count;
    }
    ast_mutex_unlock(&routing_lock);
    return count;
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
    ast_mutex_lock(&routing_lock);
    if (samples || !hub->ports) {
        controller->link_active = false;
    }
    controller->link_audio = NULL;
    if (hub->ports && samples && samples <= hub->capacity) {
        for (size_t i = 0; i < samples; ++i) {
            hub->local[i] = receiving ? audio[i] : 0;
        }
        for (struct ra_link_port *port = hub->ports; port; port = port->next) {
            port->active = ra_link_peer_receive(&port->peer, port->audio, samples);
            controller->link_active |= port->active;
        }
        for (size_t i = 0; i < samples; ++i) {
            int64_t sum = 0;
            for (struct ra_link_port *port = hub->ports; port; port = port->next) {
                sum += port->audio[i];
            }
            hub->remote[i] = pcm(sum);
        }
        controller->link_audio = hub->remote;
        for (struct ra_link_port *destination = hub->ports; destination;
             destination = destination->next) {
            bool keyed = receiving;
            for (size_t i = 0; i < samples; ++i) {
                int64_t sum = hub->local[i];
                for (struct ra_link_port *source = hub->ports; source; source = source->next) {
                    if (source != destination && source->forward) {
                        sum += source->audio[i];
                        keyed |= source->active;
                    }
                }
                hub->outgoing[i] = pcm(sum);
            }
            if (ra_link_peer_send(&destination->peer, destination->transmit && keyed, hub->outgoing,
                                  samples)) {
                atomic_store(&destination->peer.stop, true);
            }
        }
    }
    bool keyed = ra_controller_process(controller, receiving, audio, samples, now_ms);
    controller->link_audio = NULL;
    ast_mutex_unlock(&routing_lock);
    return keyed;
}
