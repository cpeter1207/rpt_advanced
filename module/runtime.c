/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Bind named configuration, radio reservations, controllers, and workers.
 */
#include "runtime.h"
#include "assets.h"
#include "connection.h"
#include "schema.h"
#include "worker.h"
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <stdlib.h>
#include <time.h>

/** @brief One stable-address controller and its exclusive radio resources. */
struct ra_runtime_node {
    struct ra_runtime_node *next;    /**< Next owned node. */
    struct ra_connection connection; /**< Converter lifetime extends past worker join. */
    struct ra_controller controller; /**< Audio and identifier state. */
    struct ra_worker worker;         /**< Hardware-clocked execution. */
    struct ra_controller_id *ids;    /**< Resolved identifier array. */
    bool running;                    /**< Worker creation succeeded; it must be joined. */
};

void ra_runtime_stop(struct ra_runtime *runtime) {
    while (runtime->nodes) {
        struct ra_runtime_node *node = runtime->nodes;
        runtime->nodes = node->next;
        if (node->running) {
            ra_worker_stop(&node->worker);
        }
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
    if (ra_worker_start(&node->worker)) {
        return "cannot start radio worker";
    }
    node->running = true;
    node->connection.channel = NULL;
    return NULL;
}

const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document) {
    struct ra_runtime replacement = {0};
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
