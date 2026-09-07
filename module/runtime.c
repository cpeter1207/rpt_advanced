/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Bind named configuration, radio reservations, controllers, and workers.
 */
#include "runtime.h"
#include "assets.h"
#include "connection.h"
#include "link_access.h"
#include "link_directory.h"
#include "link_hub.h"
#include "media.h"
#include "schema.h"
#include "worker.h"
#include <asterisk.h>
#include <asterisk/astobj2.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief One stable-address controller and its exclusive radio resources. */
struct ra_runtime_node {
    struct ra_runtime_node *next;       /**< Next owned node. */
    struct ra_connection connection;    /**< Converter lifetime extends past worker join. */
    struct ra_controller controller;    /**< Audio and identifier state. */
    struct ra_worker worker;            /**< Hardware-clocked execution. */
    struct ra_link_hub links;           /**< Owned network peers and routing buffers. */
    struct ra_link_collector collector; /**< Local DTMF command state. */
    char last_node[64];                 /**< Destination used by the zero-node shorthand. */
    const char *name;                   /**< Borrowed local node name. */
    struct ra_node_settings settings; /**< Borrowed resolved settings retained by configuration. */
    struct ra_controller_id *ids;     /**< Resolved identifier array. */
    bool running;                     /**< Worker creation succeeded; it must be joined. */
};

void ra_runtime_stop(struct ra_runtime *runtime) {
    while (runtime->nodes) {
        struct ra_runtime_node *node = runtime->nodes;
        runtime->nodes = node->next;
        if (node->running) {
            ra_worker_stop(&node->worker);
        }
        ra_link_hub_close(&node->links);
        ra_connection_close(&node->connection);
        for (size_t index = 0; index < node->controller.count; ++index) {
            ast_free((void *)node->ids[index].audio);
        }
        ast_free(node->controller.states);
        ast_free(node->controller.rules);
        ast_free(node->ids);
        ast_free(node);
    }
}

/** @brief Allocate identifier state and resolve inherited settings.
 * @param node Reserved node.
 * @param document Validated configuration.
 * @param name Named node section.
 * @return Null on success or an allocation diagnostic.
 */
static const char *identifiers(struct ra_runtime_node *node, const struct ra_document *document,
                               const char *name) {
    size_t count = 0;
    while (ra_document_identifier(document, name, count)) {
        ++count;
    }
    if (count) {
        node->ids = ast_calloc(count, sizeof(*node->ids));
        node->controller.rules = ast_calloc(count, sizeof(*node->controller.rules));
        node->controller.states = ast_calloc(count, sizeof(*node->controller.states));
        if (!node->ids || !node->controller.rules || !node->controller.states) {
            return "cannot allocate identifier state";
        }
    }
    node->controller.ids = node->ids;
    node->controller.count = count;
    const char *defaults = ra_document_identifier_defaults(document, name);
    size_t usable = 0;
    for (size_t index = 0; index < count; ++index) {
        (void)ra_identifier_settings_resolve(document->entries, document->count, defaults,
                                             ra_document_identifier(document, name, index),
                                             &node->ids[usable].settings);
        int16_t *audio;
        ra_identifier_prepare(&node->ids[usable].settings, node->controller.rate, &audio,
                              &node->ids[usable].samples);
        node->ids[usable].audio = audio;
        /* An unavailable set must not repeatedly win and starve playable IDs. */
        if (audio || *node->ids[usable].settings.morse_text) {
            ++usable;
        }
    }
    node->controller.count = usable;
    return NULL;
}

/** @brief Reserve and activate one node without transferring partial ownership.
 * @param node List-owned zero-initialized state.
 * @param document Validated configuration.
 * @param name Named node section.
 * @param settings Resolved node settings.
 * @return Null once the worker owns the called channel, or a diagnostic.
 */
static const char *start_node(struct ra_runtime_node *node, const struct ra_document *document,
                              const char *name, const struct ra_node_settings *settings) {
    const char *error = ra_connection_open(&node->connection, settings->channel,
                                           settings->sample_rate, settings->codec);
    if (error) {
        return error;
    }
    node->controller.rate = ast_format_get_sample_rate(node->connection.radio.linear);
    error = identifiers(node, document, name);
    if (error) {
        return error;
    }
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) {
        return "cannot read monotonic clock";
    }
    node->controller.full_duplex = settings->full_duplex;
    node->controller.hang_ms = settings->hang_ms;
    if (!ra_controller_start(&node->controller,
                             (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000)) {
        return "identifier cannot render at negotiated sample rate";
    }
    if (ast_call(node->connection.channel, settings->channel, 0)) {
        return "cannot start radio channel";
    }
    node->worker.channel = node->connection.channel;
    node->worker.radio = node->connection.radio;
    node->worker.controller = &node->controller;
    node->worker.links = &node->links;
    node->worker.name = name;
    if (ra_worker_start(&node->worker)) {
        return "cannot start radio worker";
    }
    node->running = true;
    node->connection.channel = NULL;
    return NULL;
}

const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document) {
    struct ra_runtime replacement = {.digit = runtime->digit};
    const char *error = NULL;
    const char *name;
    for (size_t index = 0; (name = ra_document_node(document, index)); ++index) {
        struct ra_node_settings settings;
        (void)ra_node_settings_resolve(document->entries, document->count, name, &settings);
        if (!settings.enabled) {
            continue;
        }
        struct ra_runtime_node *node = ast_calloc(1, sizeof(*node));
        if (!node) {
            error = "cannot allocate radio state";
            break;
        }
        node->next = replacement.nodes;
        node->name = name;
        node->settings = settings;
        node->worker.digit = runtime->digit;
        replacement.nodes = node;
        error = start_node(node, document, name, &settings);
        if (error) {
            break;
        }
    }
    if (error) {
        ra_runtime_stop(&replacement);
        return error;
    }
    *runtime = replacement;
    return NULL;
}

bool ra_runtime_digit(struct ra_runtime *runtime, const char *local, char digit, uint64_t now_ms,
                      struct ra_link_operation *operation) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (strcmp(node->name, local)) {
            continue;
        }
        char completed[128];
        struct ra_link_command command;
        if (!ra_link_collect(&node->collector, node->settings.link_commands, RA_LINK_ACTION_COUNT,
                             digit, now_ms, completed) ||
            !ra_link_command_parse(node->settings.link_commands, RA_LINK_ACTION_COUNT, completed,
                                   &command)) {
            return false;
        }
        const char *remote = command.node;
        if (!strcmp(remote, "0")) {
            if (!*node->last_node) {
                return false;
            }
            remote = node->last_node;
        }
        size_t length = strlen(remote);
        if (length >= sizeof(operation->remote)) {
            return false;
        }
        operation->action = command.action;
        for (size_t i = 0; i <= length; ++i) {
            operation->remote[i] = remote[i];
        }
        if (length) {
            for (size_t i = 0; i <= length; ++i) {
                node->last_node[i] = operation->remote[i];
            }
        }
        return true;
    }
    return false;
}

void ra_runtime_reset_digits(struct ra_runtime *runtime) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        node->collector = (struct ra_link_collector){0};
    }
}

int ra_runtime_accept(struct ra_runtime *runtime, const char *local, const char *remote,
                      struct ast_channel *channel, bool verified, bool same_server) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            if (!ra_link_access_allowed(node->settings.link_allow_nodes,
                                        node->settings.link_deny_nodes, remote, verified,
                                        same_server)) {
                return -1;
            }
            return ra_link_hub_attach(&node->links, remote, channel, node->connection.radio.linear,
                                      true, true);
        }
    }
    return -1;
}

int ra_runtime_prepare_link(struct ra_runtime *runtime, const char *local, const char *remote,
                            struct ra_link_dial *dial) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (strcmp(node->name, local)) {
            continue;
        }
        char *destination =
            ra_link_directory_lookup(remote, NULL, node->settings.link_directory_file);
        if (!destination) {
            return -1;
        }
        struct ast_format_cap *offer = ra_media_offer(node->connection.radio.linear);
        if (!offer) {
            ast_free(destination);
            return -1;
        }
        *dial = (struct ra_link_dial){destination, offer};
        return 0;
    }
    return -1;
}

struct ast_channel *ra_link_dial_run(struct ra_link_dial *dial, const char *local) {
    int reason = 0;
    struct ast_channel *channel = ast_request_and_dial(
        "IAX2", dial->offer, NULL, NULL, dial->destination, 20000, &reason, local, local);
    ao2_cleanup(dial->offer);
    ast_free(dial->destination);
    *dial = (struct ra_link_dial){0};
    if (channel && ast_channel_state(channel) != AST_STATE_UP) {
        ast_hangup(channel);
        return NULL;
    }
    return channel;
}

int ra_runtime_attach_link(struct ra_runtime *runtime, const char *local, const char *remote,
                           struct ast_channel *channel, bool transmit, bool forward) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            return ra_link_hub_attach(&node->links, remote, channel, node->connection.radio.linear,
                                      transmit, forward);
        }
    }
    return -1;
}

bool ra_runtime_disconnect(struct ra_runtime *runtime, const char *local, const char *remote) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            return ra_link_hub_disconnect(&node->links, remote);
        }
    }
    return false;
}

bool ra_runtime_authorize(struct ra_runtime *runtime, const char *local, const char *remote,
                          const char *peer_ip) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            char *verified =
                ra_link_directory_lookup(remote, peer_ip, node->settings.link_directory_file);
            bool allowed = ra_link_access_allowed(node->settings.link_allow_nodes,
                                                  node->settings.link_deny_nodes, remote,
                                                  verified != NULL, false);
            ast_free(verified);
            return allowed;
        }
    }
    return false;
}
