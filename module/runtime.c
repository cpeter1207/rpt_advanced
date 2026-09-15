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
#include "message_template.h"
#include "schema.h"
#include "time_announcement.h"
#include "tone_sequence.h"
#include "worker.h"
#include <asterisk.h>
#include <asterisk/astobj2.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/localtime.h>
#include <asterisk/lock.h>
#include <ctype.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

/** @brief Apply a configured courtesy level before the real-time worker sees PCM.
 * @param audio Writable prepared sound-file or speech PCM.
 * @param samples PCM sample count.
 * @param level_db Configured non-positive level relative to full scale.
 */
static void apply_courtesy_gain(int16_t *audio, size_t samples, int64_t level_db) {
    double gain = pow(10.0, level_db / 20.0);
    for (size_t index = 0; index < samples; ++index) {
        audio[index] = (int16_t)lround(audio[index] * gain);
    }
}

/** @brief Total outbound IAX dialing budget shared by ordered codec attempts. */
#define RA_LINK_DIAL_TIMEOUT_MS 20000

/** @brief One stable-address controller and its exclusive radio resources. */
struct ra_runtime_node {
    struct ra_runtime_node *next;       /**< Next owned node. */
    struct ra_connection connection;    /**< Converter lifetime extends past worker join. */
    struct ra_controller controller;    /**< Audio and identifier state. */
    struct ra_worker worker;            /**< Hardware-clocked execution. */
    struct ra_link_hub links;           /**< Owned network peers and routing buffers. */
    ast_mutex_t callback_lock;          /**< Serializes non-audio hub callbacks with reconfigure. */
    struct ra_link_collector collector; /**< Local DTMF command state. */
    char last_node[RA_NODE_NAME_MAX];   /**< Destination used by the zero-node shorthand. */
    char remote_node[RA_NODE_NAME_MAX]; /**< Direct peer receiving remote-command DTMF. */
    const char *name;                   /**< Borrowed local node name. */
    struct ra_runtime *owner;           /**< Runtime retaining this stable node. */
    struct ra_node_settings settings; /**< Borrowed resolved settings retained by configuration. */
    struct ra_identifier_settings
        status_settings; /**< Resolved speech and Morse defaults for RF telemetry. */
    struct ra_time_settings time_settings; /**< Resolved local clock-announcement format. */
    struct ra_controller_id *ids;          /**< Resolved identifier array. */
    struct ra_controller_announcement
        *announcements; /**< Resolved announcement media in configuration order. */
    struct ra_controller_id
        *courtesies; /**< Prepared named courtesy media in configuration order. */
    struct ra_controller_peer_courtesy
        *peer_courtesies;    /**< Prepared exact-direct-peer courtesy overrides. */
    size_t courtesy_count;   /**< Number of owned courtesy media records. */
    const char *reload_name; /**< Previous document-owned name while a replacement is pending. */
    struct ra_node_settings
        reload_settings;          /**< Previous resolved settings while a replacement is pending. */
    bool running;                 /**< Worker creation succeeded; it must be joined. */
    bool reload_reconfigured;     /**< Current replacement must restore this node on failure. */
    bool reload_new;              /**< Current replacement owns this node until commit. */
    bool configured_links_paused; /**< Operator `*806` hold on configuration-owned links. */
};

/** @brief One configuration-order event plus the control-plane state for its current occurrence. */
struct ra_runtime_schedule_event {
    struct ra_runtime_node *node; /**< Stable active node selected by the event's section scope. */
    struct ra_event_settings
        settings; /**< Resolved strings and parsed trigger owned by configuration. */
    uint64_t completed_occurrence; /**< Last completed local calendar-minute key. */
    uint64_t pending_occurrence;   /**< Reserved calendar-minute key awaiting telemetry enqueue. */
    struct tm pending_local;       /**< Civil time retained for an unqueued retry. */
    uint64_t ready_occurrence;     /**< Due calendar-minute key retained behind an earlier event. */
    struct tm ready_local;         /**< Civil time retained with @c ready_occurrence. */
    bool pending;                  /**< A dispatch was returned but not yet completed. */
    bool message_queued; /**< The reserved dispatch's message is on the telemetry queue. */
    bool ready;          /**< A due occurrence awaits its turn in configuration order. */
};

/** @brief One configuration-managed direct link and its issued control-plane intent. */
struct ra_runtime_configured_link {
    char local[RA_NODE_NAME_MAX];  /**< Copied local node identity. */
    char remote[RA_NODE_NAME_MAX]; /**< Copied remote direct-peer identity. */
    const char
        *label;     /**< Borrowed permanent-link label; null for schedule routes or retirement. */
    bool permanent; /**< True only for an always-desired configured permanent link. */
    bool desired;   /**< Current scheduler policy requests this route. */
    bool issued;    /**< The hub owns this route or its retained permanent retry intent. */
    bool pending;   /**< A copied transition is outside the runtime lock awaiting settlement. */
    uint64_t reservation; /**< Nonzero nonce for the copied transition while @c pending. */
    bool retiring; /**< A removed reload-time route awaits one explicit permanent disconnect. */
};

/** @brief Runtime state for one configured local-time permanent-link replacement window. */
struct ra_runtime_link_window {
    const char *name; /**< Borrowed unique schedule label retained by the active document. */
    char local[RA_NODE_NAME_MAX];      /**< Copied local node identity. */
    struct ra_scheduled_window window; /**< Parsed local civil-time membership. */
    uint64_t end_inactivity_ms;        /**< Quiet interval required after the window ends. */
    size_t route_index;                /**< Configuration-managed replacement link in @c links. */
    size_t replaced_index;             /**< Configured permanent link suspended by this window. */
    uint64_t last_activity_ms;    /**< Last observed qualifying receive activity across reload. */
    uint64_t initial_deadline_ms; /**< Cold-start remaining quiet-period deadline. */
    bool was_active;       /**< The window was previously active and may now drain activity. */
    bool waiting_for_idle; /**< End passed while qualifying receive activity may still be recent. */
    bool initialized; /**< Initial policy evaluation has applied cold-start grace when required. */
    bool initial_grace; /**< True while a post-restart remaining quiet period is still running. */
};

/** @brief Runtime-owned event ordering and configured permanent-link control state. */
struct ra_runtime_schedule {
    const struct ra_document *document; /**< Immutable configuration retained by the runtime. */
    struct ra_runtime_schedule_event *events; /**< Owned global configuration-order event array. */
    size_t count;                             /**< Number of active-node events in @p events. */
    struct ra_runtime_configured_link *links; /**< Owned configured direct-link intent records. */
    size_t link_count;                        /**< Number of current and retiring route records. */
    struct ra_runtime_link_window *windows; /**< Owned configured local-time replacement windows. */
    size_t window_count;                    /**< Number of active-node replacement windows. */
    uint64_t generation;                    /**< Invalidates copied dispatches after a reload. */
    uint64_t
        next_link_reservation; /**< Advances copied link-operation nonces within this instance. */
    time_t
        last_tick; /**< Latest control-task wall-clock instant permitted to create occurrences. */
    bool has_last_tick; /**< True after @c last_tick has been initialized by a scheduler task. */
};

/** @brief Find a running node by its exact configuration name. */
static struct ra_runtime_node *runtime_node(struct ra_runtime *runtime, const char *name);

/** @brief Allocate a replacement global event schedule after all selected nodes start.
 * @param runtime Active replacement-node owner whose private generation is advanced on commit.
 * @param document Valid immutable configuration supplying global event order and definitions.
 * @param previous Prior schedule retained only to preserve matching occurrence state.
 * @param result Receives owned schedule state, or null when no enabled node has an event.
 * @return Null on success or an allocation/configuration diagnostic.
 *
 * Every enabled scoped configuration has a running node before this helper builds its schedule.
 */
static const char *schedule_create(struct ra_runtime *runtime, const struct ra_document *document,
                                   const struct ra_runtime_schedule *previous,
                                   struct ra_runtime_schedule **result);

/** @brief Convert a captured wall-clock instant to validated local civil time. */
static bool schedule_local_time(time_t now, struct tm *local);

/** @brief Recompute configuration-owned direct-link policy under the runtime control lock. */
static void schedule_link_desires(struct ra_runtime *runtime, struct ra_runtime_schedule *schedule,
                                  const struct tm *local, uint64_t now_ms);

/** @brief Clear stale configuration-issued state after an exact hub route has disappeared. */
static void schedule_refresh_link_ownership(struct ra_runtime *runtime,
                                            struct ra_runtime_schedule *schedule);

/** @brief Return the current monotonic control timestamp, or zero after a clock failure. */
static uint64_t runtime_monotonic_ms(void);

/** @brief Withdraw configuration-owned routes the current schedule no longer requests.
 * @param runtime Active runtime that owns every selected routing hub.
 * @param schedule Candidate or active configuration-owned route state.
 * @param local Valid current local civil time.
 * @param now_ms Current monotonic control timestamp.
 * @param selected_local Optional node restriction, or null for every active node.
 *
 * This control-plane reconciliation runs before resuming held retries and before a reload
 * publishes replacement configuration. Cancelling first prevents an old permanent retry from
 * attaching after its replacement window or configuration removal has taken effect.
 * Its static callers have already selected a non-null schedule, validated @p local, and started
 * every enabled route owner.
 */
static void reconcile_scheduled_links(struct ra_runtime *runtime,
                                      struct ra_runtime_schedule *schedule, const struct tm *local,
                                      uint64_t now_ms, const char *selected_local);

/** @brief Discard copied configured-link reservations for one node or every node. */
static void clear_scheduled_link_reservations(struct ra_runtime *runtime, const char *local);

/** @brief Advance a nonzero copied-dispatch generation without ever publishing zero.
 * @param generation Previous private runtime generation.
 * @return Next nonzero generation, restarting at one only after unsigned wrap.
 */
static uint64_t schedule_next_generation(uint64_t generation) {
    return generation == UINT64_MAX ? 1 : generation + 1;
}

/** @brief Release one runtime-owned schedule without touching its borrowed configuration. */
static void schedule_release(struct ra_runtime_schedule *schedule);

/** @brief Resolve one peer using the selected node's inherited directory policy.
 * @param node Local runtime node whose settings own the policy strings.
 * @param remote Decimal remote node identity.
 * @param peer_ip Numeric incoming address, or null for an outbound lookup.
 * @return Owned IAX destination, or null when no selected source verifies the peer.
 *
 * Resolution is called only by dialing, admission, recovery, and remote-command control paths;
 * no hardware-paced audio callback accesses a directory source.
 */
static char *resolve_link_node(const struct ra_runtime_node *node, const char *remote,
                               const char *peer_ip) {
    const struct ra_link_directory_policy policy = {
        .static_file = node->settings.link_static_directory_file,
        .external_file = node->settings.link_directory_file,
        .method = node->settings.link_lookup_method};
    return ra_link_directory_lookup(remote, peer_ip, &policy);
}

/** @brief Release prepared dialing state that has not transferred a channel.
 * @param dial Prepared state, or null.
 */
static void discard_link_dial(struct ra_link_dial *dial) {
    if (!dial) {
        return;
    }
    ast_free(dial->destination);
    ra_media_candidates_release(dial->candidates, dial->candidate_count);
    *dial = (struct ra_link_dial){0};
}

/** @brief Bind a resolved destination to owned codec candidates before releasing runtime state.
 * @param dial Empty output state.
 * @param destination Owned resolved IAX address.
 * @param linear Borrowed local signed-linear format.
 * @return Zero on success or minus one after releasing destination on failure.
 */
static int prepare_link_dial(struct ra_link_dial *dial, char *destination,
                             struct ast_format *linear) {
    if (!dial || ra_media_candidates_collect(linear, &dial->candidates, &dial->candidate_count)) {
        ast_free(destination);
        return -1;
    }
    dial->destination = destination;
    return 0;
}

/** @brief Calculate a remaining shared IAX dialing budget.
 * @param started Monotonic start time recorded before the first codec attempt.
 * @param reliable True when the clock was readable at dialing start.
 * @return Positive caller timeout in milliseconds, or zero after the budget expires.
 *
 * A clock failure does not make a link unusable: its one attempted format retains Asterisk's
 * normal bounded timeout. A reliable clock ensures rejected high-rate candidates cannot multiply
 * the configured 20-second link-attempt budget.
 */
static int remaining_dial_timeout(const struct timespec *started, bool reliable) {
    if (!reliable) {
        return RA_LINK_DIAL_TIMEOUT_MS;
    }
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) {
        return 0;
    }
    uint64_t start_ms = (uint64_t)started->tv_sec * 1000 + (uint64_t)started->tv_nsec / 1000000;
    uint64_t now_ms = (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
    if (now_ms < start_ms || now_ms - start_ms >= RA_LINK_DIAL_TIMEOUT_MS) {
        return 0;
    }
    return (int)(RA_LINK_DIAL_TIMEOUT_MS - (now_ms - start_ms));
}

/** @brief Deliver policy-authorized IAX DTMF through the same module control queue as local DTMF.
 * @param context Runtime node retained until its routing hub closes every peer reader.
 * @param remote Stable identity of the attached peer that emitted this digit.
 * @param digit Completed conventional IAX DTMF character.
 * @param now_ms Peer-reader monotonic timestamp.
 *
 * Every attached port was directory-verified before its reader started. The current deny-first
 * policy is deliberately applied here rather than at attach time so a successful reload takes
 * effect for an already connected peer. This is intentionally called only from a network reader,
 * never from link routing or the hardware-paced audio callback.
 */
static void receive_link_digit(void *context, const char *remote, char digit, uint64_t now_ms) {
    struct ra_runtime_node *node = context;
    /* Peer readers are outside the radio callback, so this lifecycle lock cannot affect PCM. */
    ast_mutex_lock(&node->callback_lock);
    if (node->running && node->worker.digit &&
        ra_link_access_allowed(node->settings.link_allow_nodes, node->settings.link_deny_nodes,
                               remote, true)) {
        node->worker.digit(node->name, digit, now_ms);
    }
    ast_mutex_unlock(&node->callback_lock);
}

/** @brief Forward a hub lifecycle event to the module's nonblocking control handoff.
 * @param context Stable runtime node that owns the direct hub.
 * @param remote Stable direct-peer identity.
 * @param connected True after attachment, false after detachment.
 */
static void receive_link_event(void *context, const char *remote, bool connected) {
    struct ra_runtime_node *node = context;
    if (node->owner->event) {
        node->owner->event(node->name, remote, connected);
    }
}

/** @brief Redial one permanent peer after its reader reports transport failure.
 * @param context Runtime node that owns the failed peer.
 * @param remote Decimal remote node identity.
 * @param transmit Preserve outbound audio mode.
 * @param forward Preserve forwarding mode.
 * @param permanent Preserve automatic recovery after a reconnect-all attempt.
 * @param cancelled Explicit permanent-disconnect indicator.
 * @param paused Disconnect-all indicator retaining this request for reconnect-all.
 * @return Zero when a replacement peer was attached.
 */
static int reconnect_node(void *context, const char *remote, bool transmit, bool forward,
                          bool permanent, const atomic_bool *cancelled, const atomic_bool *paused) {
    struct ra_runtime_node *node = context;
    int result = -1;
    /* Keep the stable node and its borrowed configuration alive through the whole dial. */
    ast_mutex_lock(&node->callback_lock);
    if (atomic_load(cancelled) || atomic_load(paused)) {
        goto done;
    }
    char *destination = resolve_link_node(node, remote, NULL);
    if (!destination) {
        goto done;
    }
    struct ra_link_dial dial = {0};
    if (prepare_link_dial(&dial, destination, node->connection.radio.linear)) {
        goto done;
    }
    if (atomic_load(cancelled) || atomic_load(paused)) {
        discard_link_dial(&dial);
        goto done;
    }
    struct ast_channel *channel = ra_link_dial_run(&dial, node->name);
    if (!channel || atomic_load(cancelled) || atomic_load(paused)) {
        if (channel) {
            ast_hangup(channel);
        }
        goto done;
    }
    if (ra_link_hub_attach_gated(&node->links, remote, channel, node->connection.radio.linear,
                                 transmit, forward, permanent, cancelled, paused, NULL, NULL)) {
        ast_hangup(channel);
        goto done;
    }
    if (atomic_load(cancelled)) {
        (void)ra_link_hub_disconnect_permanent(&node->links, remote);
        goto done;
    }
    if (atomic_load(paused)) {
        (void)ra_link_hub_detach_reconnect(&node->links, remote, permanent);
        goto done;
    }
    result = 0;
done:
    ast_mutex_unlock(&node->callback_lock);
    return result;
}

/** @brief Stop one node's hardware-paced worker before changing its controller state.
 * @param node Node whose worker has completed every audio callback on return.
 */
static void stop_node_worker(struct ra_runtime_node *node) {
    if (node->running) {
        ra_worker_stop(&node->worker);
        node->running = false;
    }
}

/** @brief Release one stopped node's radio resources while retaining its stable link hub.
 * @param node Node whose worker and controller are no longer used by any radio callback.
 *
 * The caller excludes peer-reader and manager callbacks before changing configuration pointers.
 * It deliberately does not close `links`: active ports and retry records remain valid across a
 * successful replacement and while the previous configuration is restored after a failure.
 */
static void stop_node_resources(struct ra_runtime_node *node) {
    ra_digit_handler digit = node->worker.digit;
    ra_connection_close(&node->connection);
    for (size_t index = 0; index < node->controller.count; ++index) {
        ast_free((void *)node->ids[index].audio);
    }
    for (size_t index = 0; index < node->controller.announcement_count; ++index) {
        ast_free((void *)node->announcements[index].media.audio);
    }
    for (size_t index = 0; index < node->courtesy_count; ++index) {
        ast_free((void *)node->courtesies[index].audio);
    }
    for (size_t index = 0; index < RA_CONTROLLER_STATUS_QUEUE_DEPTH; ++index) {
        ast_free(node->controller.status_queue[index].audio);
    }
    ast_free(node->controller.states);
    ast_free(node->controller.rules);
    ast_free(node->controller.announcement_states);
    ast_free(node->ids);
    ast_free(node->announcements);
    ast_free(node->courtesies);
    ast_free(node->peer_courtesies);
    node->controller = (struct ra_controller){0};
    node->worker = (struct ra_worker){.digit = digit};
    node->ids = NULL;
    node->announcements = NULL;
    node->courtesies = NULL;
    node->peer_courtesies = NULL;
    node->courtesy_count = 0;
}

/** @brief Stop and release radio resources while retaining the node's routing hub.
 * @param node Node whose hub remains available to peer readers and recovery management.
 */
static void reset_node_resources(struct ra_runtime_node *node) {
    stop_node_worker(node);
    stop_node_resources(node);
}

/** @brief Release one detached node after its peer manager and readers have stopped.
 * @param node Detached node.
 */
static void release_node(struct ra_runtime_node *node) {
    /* Match reader callbacks before changing `running`; do not hold this while joining the hub. */
    ast_mutex_lock(&node->callback_lock);
    stop_node_worker(node);
    ast_mutex_unlock(&node->callback_lock);
    /* The manager may redial through connection and peer readers may submit through worker. */
    ra_link_hub_close(&node->links);
    stop_node_resources(node);
    ast_mutex_destroy(&node->callback_lock);
    ast_free(node);
}

/** @brief Initialize a node's empty stable routing hub and its stable callback context.
 * @param node Node that owns the empty hub.
 */
static void initialize_node_links(struct ra_runtime_node *node) {
    ra_link_hub_init(&node->links);
    ra_link_hub_set_reconnector(&node->links, reconnect_node, node);
    ra_link_hub_set_digit_handler(&node->links, receive_link_digit, node);
    ra_link_hub_set_event_handler(&node->links, receive_link_event, node);
}

/** @brief Allocate stable callback and routing ownership for one new node.
 * @param runtime Owning runtime and its module control callbacks.
 * @return Initialized node, or null on allocation or lock initialization failure.
 */
static struct ra_runtime_node *new_node(struct ra_runtime *runtime) {
    struct ra_runtime_node *node = ast_calloc(1, sizeof(*node));
    if (!node) {
        return NULL;
    }
    if (ast_mutex_init(&node->callback_lock)) {
        ast_free(node);
        return NULL;
    }
    node->owner = runtime;
    node->worker.digit = runtime->digit;
    initialize_node_links(node);
    return node;
}

/** @brief Stop every node after releasing scheduled runtime state.
 * @param runtime Owned runtime, safe when empty.
 */
void ra_runtime_stop(struct ra_runtime *runtime) {
    schedule_release(runtime->schedule);
    runtime->schedule = NULL;
    while (runtime->nodes) {
        struct ra_runtime_node *node = runtime->nodes;
        runtime->nodes = node->next;
        release_node(node);
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
    size_t usable = 0;
    for (size_t index = 0; index < count; ++index) {
        (void)ra_identifier_settings_resolve(document->entries, document->count, name,
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

/** @brief Allocate prepared announcement state and resolve its inherited media settings.
 * @param node Reserved runtime node that owns the resulting arrays.
 * @param document Valid configuration document.
 * @param name Exact configured node name.
 * @return Null on success, or an allocation diagnostic.
 *
 * An unrenderable set is omitted rather than being allowed to win repeatedly.  This mirrors
 * identifier preparation and leaves the controller with only media that can reach RF.
 */
static const char *announcements(struct ra_runtime_node *node, const struct ra_document *document,
                                 const char *name) {
    size_t count = 0;
    while (ra_document_announcement(document, name, count)) {
        ++count;
    }
    if (!count) {
        return NULL;
    }
    node->announcements = ast_calloc(count, sizeof(*node->announcements));
    node->controller.announcement_states =
        ast_calloc(count, sizeof(*node->controller.announcement_states));
    if (!node->announcements || !node->controller.announcement_states) {
        return "cannot allocate announcement state";
    }
    node->controller.announcements = node->announcements;
    size_t usable = 0;
    for (size_t index = 0; index < count; ++index) {
        struct ra_announcement_settings settings;
        struct ra_controller_announcement *announcement = &node->announcements[usable];
        (void)ra_announcement_settings_resolve(document->entries, document->count, name,
                                               ra_document_announcement(document, name, index),
                                               &settings);
        announcement->interval_ms = settings.interval_ms;
        announcement->media.settings = settings.media;
        int16_t *audio;
        ra_identifier_prepare(&announcement->media.settings, node->controller.rate, &audio,
                              &announcement->media.samples);
        announcement->media.audio = audio;
        if (audio || *announcement->media.settings.morse_text) {
            ++usable;
        }
    }
    node->controller.announcement_count = usable;
    return NULL;
}

/** @brief Prepare one courtesy medium using file, speech, generated tones, then Morse fallback.
 * @param settings Fully inherited source-specific courtesy settings.
 * @param rate Negotiated node PCM rate.
 * @param media Receives node-owned prepared media.
 * @return Null on success, or a precise tone-sequence preparation diagnostic.
 *
 * File and speech use the established asset helper. A configured tone sequence is always parsed
 * so reload reports invalid syntax even when a higher-priority file or speech source succeeds.
 * Its independent allocator is copied into Asterisk-owned memory before the worker can see it.
 */
static const char *prepare_courtesy(const struct ra_courtesy_settings *settings, unsigned int rate,
                                    struct ra_controller_id *media) {
    media->settings = settings->media;
    int16_t *audio;
    size_t samples;
    ra_identifier_prepare(&media->settings, rate, &audio, &samples);
    bool file_or_speech = audio != NULL;
    if (*settings->tone_sequence) {
        int16_t *tones = NULL;
        size_t tone_samples = 0;
        const char *error = ra_tone_sequence_prepare(
            settings->tone_sequence, rate, (int)settings->level_db, &tones, &tone_samples);
        if (error) {
            ast_free(audio);
            return error;
        }
        if (!audio && tones) {
            audio = ast_calloc(tone_samples, sizeof(*audio));
            if (!audio) {
                ra_tone_sequence_free(tones);
                return "cannot allocate courtesy media";
            }
            /* Both arrays contain exactly tone_samples signed PCM values. */
            for (size_t sample = 0; sample < tone_samples; ++sample) {
                audio[sample] = tones[sample];
            }
            samples = tone_samples;
        }
        ra_tone_sequence_free(tones);
    }
    if (file_or_speech) {
        apply_courtesy_gain(audio, samples, settings->level_db);
    }
    media->audio = audio;
    media->samples = samples;
    return NULL;
}

/** @brief Resolve, prepare, and publish all named courtesy inputs for one node.
 * @param node Runtime node retaining media through the worker lifetime.
 * @param document Valid configuration document.
 * @param name Exact local node name.
 * @return Null on success or an allocation/preparation diagnostic.
 */
static const char *courtesies(struct ra_runtime_node *node, const struct ra_document *document,
                              const char *name) {
    size_t count = 0;
    while (ra_document_courtesy(document, name, count)) {
        ++count;
    }
    if (!count) {
        return NULL;
    }
    node->courtesies = ast_calloc(count, sizeof(*node->courtesies));
    node->peer_courtesies = ast_calloc(count, sizeof(*node->peer_courtesies));
    if (!node->courtesies || !node->peer_courtesies) {
        return "cannot allocate courtesy media";
    }
    node->courtesy_count = count;
    size_t peers = 0;
    for (size_t index = 0; index < count; ++index) {
        const char *set = ra_document_courtesy(document, name, index);
        struct ra_courtesy_settings settings;
        const char *error =
            ra_courtesy_settings_resolve(document->entries, document->count, name, set, &settings);
        if (!error) {
            error = prepare_courtesy(&settings, node->controller.rate, &node->courtesies[index]);
        }
        if (error) {
            return error;
        }
        struct ra_controller_id *media = &node->courtesies[index];
        if (!media->audio && !*media->settings.morse_text) {
            continue;
        }
        if (settings.input == RA_COURTESY_INPUT_RECEIVER) {
            /* Schema validation guarantees this is the only receiver assignment. */
            node->controller.receiver_courtesy = media;
        } else if (!*settings.remote_node) {
            /* Schema validation guarantees this is the only generic-link assignment. */
            node->controller.link_courtesy = media;
        } else {
            node->peer_courtesies[peers++] = (struct ra_controller_peer_courtesy){
                .remote = settings.remote_node, .media = media};
        }
    }
    node->controller.peer_courtesies = node->peer_courtesies;
    node->controller.peer_courtesy_count = peers;
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
    const char *error = ra_connection_open(&node->connection, settings->channel);
    if (error) {
        return error;
    }
    node->controller.rate = ast_format_get_sample_rate(node->connection.radio.linear);
    if (node->links.rate && node->links.rate != node->controller.rate) {
        if (ra_link_hub_has_retained_state(&node->links)) {
            return "cannot change the sample rate of a node with retained link routing";
        }
        /* An empty hub has no peer callback or retry ownership tied to its old rate. */
        ra_link_hub_close(&node->links);
        initialize_node_links(node);
    }
    node->links.local_name = name;
    (void)ra_identifier_settings_resolve(document->entries, document->count, name, NULL,
                                         &node->status_settings);
    (void)ra_time_settings_resolve(document->entries, document->count, name, &node->time_settings);
    node->controller.status_speed_wpm = (unsigned int)node->status_settings.morse_speed_wpm;
    node->controller.status_frequency_hz = (unsigned int)node->status_settings.morse_frequency_hz;
    node->controller.status_level_db = (int)node->status_settings.morse_level_db;
    node->controller.courtesy_delay_ms = settings->courtesy_delay_ms;
    error = courtesies(node, document, name);
    if (error) {
        return error;
    }
    error = identifiers(node, document, name);
    if (error) {
        return error;
    }
    error = announcements(node, document, name);
    if (error) {
        return error;
    }
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) {
        return "cannot read monotonic clock";
    }
    node->controller.full_duplex = settings->full_duplex;
    node->controller.hang_ms = settings->hang_ms;
    node->controller.transmit_timeout_ms = settings->transmit_timeout_ms;
    node->controller.timeout_lockout_ms = settings->timeout_lockout_ms;
    node->controller.kerchunk_max_ms = settings->kerchunk_max_ms;
    node->controller.telemetry_duck_db = (int)settings->telemetry_duck_db;
    if (!ra_controller_start(&node->controller,
                             (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000)) {
        return "scheduled media cannot render at negotiated sample rate";
    }
    if (ast_call(node->connection.channel, settings->channel, 0)) {
        return "cannot start radio channel";
    }
    node->worker.channel = node->connection.channel;
    node->worker.radio = node->connection.radio;
    node->worker.controller = &node->controller;
    node->worker.links = &node->links;
    node->worker.name = name;
    node->worker.dtmf_muting = settings->dtmf_muting;
    if (ra_worker_start(&node->worker)) {
        return "cannot start radio worker";
    }
    node->running = true;
    node->connection.channel = NULL;
    return NULL;
}

const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document) {
    struct ra_runtime replacement = {.digit = runtime->digit,
                                     .event = runtime->event,
                                     .schedule_generation = runtime->schedule_generation};
    const char *error = NULL;
    const char *name;
    for (size_t index = 0; (name = ra_document_node(document, index)); ++index) {
        struct ra_node_settings settings;
        (void)ra_node_settings_resolve(document->entries, document->count, name, &settings);
        if (!settings.enabled) {
            continue;
        }
        struct ra_runtime_node *node = new_node(runtime);
        if (!node) {
            error = "cannot allocate radio state";
            break;
        }
        node->next = replacement.nodes;
        node->name = name;
        node->settings = settings;
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
    error = schedule_create(&replacement, document, NULL, &replacement.schedule);
    if (error) {
        ra_runtime_stop(&replacement);
        return error;
    }
    replacement.schedule_generation = schedule_next_generation(replacement.schedule_generation);
    *runtime = replacement;
    return NULL;
}

/** @brief Find a document-owned node name matching a stable runtime identity.
 * @param document Valid configuration to search.
 * @param name Exact node identity.
 * @return Borrowed section name, or null when the document does not define the node.
 */
static const char *document_node_name(const struct ra_document *document, const char *name) {
    for (size_t index = 0;; ++index) {
        const char *candidate = ra_document_node(document, index);
        if (!candidate || !strcmp(candidate, name)) {
            return candidate;
        }
    }
}

/** @brief Resolve whether one named node is present in a replacement document.
 * @param document Valid configuration to search.
 * @param name Exact node name.
 * @param settings Receives resolved settings when the node is present.
 * @return True when the document defines the requested node.
 */
static bool document_node_settings(const struct ra_document *document, const char *name,
                                   struct ra_node_settings *settings) {
    /* Static callers hold the active validated document and provide local result storage. */
    if (!name) {
        return false;
    }
    const char *candidate = document_node_name(document, name);
    if (!candidate) {
        return false;
    }
    (void)ra_node_settings_resolve(document->entries, document->count, candidate, settings);
    return true;
}

/** @brief Find one stable runtime node by its configuration name.
 * @param runtime Runtime whose caller serializes lifecycle changes.
 * @param name Exact configured node name.
 * @return Stable node or null when it is not currently enabled.
 *
 * Lifecycle code assigns every node name before linking that node into @p runtime, and callers
 * pass resolved document or copied route identities.
 */
static struct ra_runtime_node *runtime_node(struct ra_runtime *runtime, const char *name) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, name)) {
            return node;
        }
    }
    return NULL;
}

/** @brief Forget one direct peer selected for remote DTMF control.
 * @param node Stable node that owns the selection.
 * @param remote Direct peer being withdrawn.
 *
 * A peer can disappear through either operator control or scheduler reconciliation.  Clearing the
 * selection in both paths prevents the next local digit from being sent to a withdrawn peer.
 */
static void clear_remote_selection(struct ra_runtime_node *node, const char *remote) {
    if (!strcmp(node->remote_node, remote)) {
        node->remote_node[0] = '\0';
    }
}

/** @brief Pack one parsed trigger so equality never depends on structure padding.
 * @param trigger Valid parsed trigger.
 * @return Unique bitwise key for every documented trigger field.
 */
static uint64_t scheduled_trigger_key(const struct ra_scheduled_event_time *trigger) {
    return (uint64_t)trigger->kind << 39 | (uint64_t)trigger->year << 23 |
           (uint64_t)trigger->month << 19 | (uint64_t)trigger->day << 14 |
           (uint64_t)trigger->weekday << 11 | (uint64_t)trigger->hour << 6 | trigger->minute;
}

/** @brief Compare two parsed event triggers without depending on structure padding.
 * @param first First parsed trigger.
 * @param second Second parsed trigger.
 * @return True only when every scheduling field is equal.
 */
static bool same_event_trigger(const struct ra_scheduled_event_time *first,
                               const struct ra_scheduled_event_time *second) {
    return scheduled_trigger_key(first) == scheduled_trigger_key(second);
}

/** @brief Find the prior state for one unchanged event across a successful reload.
 * @param previous Prior schedule, or null at initial startup.
 * @param node Stable running node selected by the candidate event.
 * @param settings Resolved candidate event whose name and trigger identify its state.
 * @return Matching previous event, or null when the event was added or changed.
 */
static const struct ra_runtime_schedule_event *
prior_schedule_event(const struct ra_runtime_schedule *previous, const struct ra_runtime_node *node,
                     const struct ra_event_settings *settings) {
    if (!previous) {
        return NULL;
    }
    for (size_t index = 0; index < previous->count; ++index) {
        const struct ra_runtime_schedule_event *event = &previous->events[index];
        if (!strcmp(event->node->name, node->name) &&
            !strcmp(event->settings.name, settings->name) &&
            same_event_trigger(&event->settings.trigger, &settings->trigger)) {
            return event;
        }
    }
    return NULL;
}

/** @brief Test exact copied endpoint identity equality without relying on configuration pointers.
 * @param link Configured route record.
 * @param local Complete local node identity.
 * @param remote Complete remote node identity.
 * @return True only for the same directed endpoint pair.
 */
static bool same_configured_link(const struct ra_runtime_configured_link *link, const char *local,
                                 const char *remote) {
    return !strcmp(link->local, local) && !strcmp(link->remote, remote);
}

/** @brief Locate the exact pending configured-link reservation copied to the control task.
 * @param runtime Active runtime protected by the module control lock.
 * @param operation Copied scheduler operation to validate.
 * @return The matching pending route, or null after a reload, hold, re-reservation, or mismatch.
 *
 * A route endpoint alone is not an identity: `*806` can cancel an in-flight operation and `*816`
 * can reserve that same endpoint again before the old dial returns. The schedule generation and
 * per-reservation nonce make the stale operation unable to affect the new route.
 */
static struct ra_runtime_configured_link *
scheduled_link_reservation(struct ra_runtime *runtime,
                           const struct ra_scheduled_link_operation *operation) {
    if (!runtime || !operation || !operation->reservation || !runtime->schedule ||
        operation->schedule_generation != runtime->schedule->generation ||
        operation->link_index >= runtime->schedule->link_count) {
        return NULL;
    }
    struct ra_runtime_configured_link *link = &runtime->schedule->links[operation->link_index];
    if (!link->pending || link->reservation != operation->reservation ||
        !same_configured_link(link, operation->local, operation->operation.remote)) {
        return NULL;
    }
    return link;
}

/** @brief Test whether a copied reservation still authorizes its physical link transition.
 * @param runtime Active runtime protected by the module control lock.
 * @param operation Copied scheduler operation to validate.
 * @return True only while the matching route remains pending and requested by current policy.
 */
static bool scheduled_link_operation_current(struct ra_runtime *runtime,
                                             const struct ra_scheduled_link_operation *operation) {
    const struct ra_runtime_configured_link *link = scheduled_link_reservation(runtime, operation);
    if (!link) {
        return false;
    }
    switch (operation->operation.action) {
    case RA_LINK_PERMANENT_TRANSCEIVE:
        /* Reservation creation already proved unissued, unheld, nonretiring attachment state. */
        return link->desired;
    case RA_LINK_DISCONNECT_PERMANENT:
        /* A pending withdrawal was issued only for an owned route; policy can still change. */
        return !link->desired;
    default:
        return false;
    }
}

/** @brief Recheck one pending configured-link transition against current civil-time policy.
 * @param runtime Active runtime protected by its serialized control lock.
 * @param operation Copied configured-link reservation.
 * @return True only when the reservation remains current under the present civil-time policy.
 *
 * Dialing and queued withdrawals are intentionally outside the control lock and can span a window
 * boundary. The ticker cannot update @c link->desired while the operation is in flight, so a
 * cached policy could attach a primary after its replacement begins or detach it after that window
 * ends. Refreshing before each scheduler-owned physical transition prevents either stale action.
 * A schedule with no replacement windows has no civil-time policy, so its ordinary permanent
 * route remains transitionable if the wall clock is temporarily unavailable.
 */
static bool scheduled_link_policy_current_now(struct ra_runtime *runtime,
                                              const struct ra_scheduled_link_operation *operation) {
    /* Public callers have already dereferenced @p runtime to locate their local node. */
    if (!runtime->schedule) {
        return false;
    }
    if (runtime->schedule->window_count) {
        struct tm local;
        if (!schedule_local_time(time(NULL), &local)) {
            return false;
        }
        schedule_link_desires(runtime, runtime->schedule, &local, runtime_monotonic_ms());
    }
    return scheduled_link_operation_current(runtime, operation);
}

/** @brief Find prior issued state for one unchanged configured direct link.
 * @param previous Prior schedule, or null on first startup.
 * @param local Complete local node identity.
 * @param remote Complete remote node identity.
 * @return Prior route record, or null when it was newly configured.
 */
static const struct ra_runtime_configured_link *
prior_configured_link(const struct ra_runtime_schedule *previous, const char *local,
                      const char *remote) {
    if (!previous) {
        return NULL;
    }
    for (size_t index = 0; index < previous->link_count; ++index) {
        const struct ra_runtime_configured_link *link = &previous->links[index];
        if (!link->retiring && same_configured_link(link, local, remote)) {
            return link;
        }
    }
    return NULL;
}

/** @brief Compare two parsed local civil-time windows without depending on padding bytes.
 * @param first First parsed window.
 * @param second Second parsed window.
 * @return True when every scheduling selector and bound is equal.
 */
static bool same_scheduled_window(const struct ra_scheduled_window *first,
                                  const struct ra_scheduled_window *second) {
    if (first->start_minute != second->start_minute || first->end_minute != second->end_minute ||
        first->weekday_mask != second->weekday_mask || first->date_count != second->date_count) {
        return false;
    }
    for (size_t index = 0; index < first->date_count; ++index) {
        if (first->dates[index].year != second->dates[index].year ||
            first->dates[index].month != second->dates[index].month ||
            first->dates[index].day != second->dates[index].day) {
            return false;
        }
    }
    return true;
}

/** @brief Find prior receive activity for one same-identity configured link-replacement window.
 * @param previous Prior schedule, or null on first startup.
 * @param local Complete local node identity.
 * @param settings Newly resolved window settings.
 * @return Prior matching window, or null when its label or routing identity changed.
 *
 * A reload may deliberately change the window selector, time bounds, or quiet interval. Its most
 * recent local/link receive time remains meaningful, so the new policy must start from it rather
 * than treating the reload as a cold start.
 */
static const struct ra_runtime_link_window *
prior_link_window(const struct ra_runtime_schedule *previous, const char *local,
                  const struct ra_link_schedule_settings *settings) {
    if (!previous) {
        return NULL;
    }
    for (size_t index = 0; index < previous->window_count; ++index) {
        const struct ra_runtime_link_window *window = &previous->windows[index];
        const struct ra_runtime_configured_link *route = &previous->links[window->route_index];
        const struct ra_runtime_configured_link *replaced =
            &previous->links[window->replaced_index];
        if (!strcmp(window->local, local) && !strcmp(window->name, settings->name) &&
            !strcmp(route->remote, settings->remote_node) &&
            !strcmp(replaced->label, settings->replace_permanent)) {
            return window;
        }
    }
    return NULL;
}

/** @brief Copy a bounded configured route into preallocated schedule storage.
 * @param schedule Destination schedule with unused route capacity.
 * @param local Complete local node identity.
 * @param remote Complete remote node identity.
 * @param label Permanent-link label, or null for a schedule route.
 * @param permanent True for an always-desired configured link.
 * @param issued Prior attach intent to retain across reload.
 *
 * Callers validate the local identity before reaching this helper. The settings resolver already
 * bounds the decimal remote identity, and a retiring route was copied through this same helper.
 */
static void schedule_add_link(struct ra_runtime_schedule *schedule, const char *local,
                              const char *remote, const char *label, bool permanent, bool issued) {
    struct ra_runtime_configured_link *link = &schedule->links[schedule->link_count++];
    ast_copy_string(link->local, local, sizeof(link->local));
    ast_copy_string(link->remote, remote, sizeof(link->remote));
    link->label = label;
    link->permanent = permanent;
    link->issued = issued;
}

/** @brief Locate a current configuration-managed permanent route by its same-node label.
 * @param schedule New schedule under construction.
 * @param local Complete local node identity.
 * @param label Required permanent-link label.
 * @return Route index, or SIZE_MAX when no matching configured permanent link exists.
 */
static size_t schedule_permanent_index(const struct ra_runtime_schedule *schedule,
                                       const char *local, const char *label) {
    for (size_t index = 0; index < schedule->link_count; ++index) {
        const struct ra_runtime_configured_link *link = &schedule->links[index];
        /* Prior window routes have no label, so ignore them while resolving another window. */
        if (link->permanent && !strcmp(link->local, local) && !strcmp(link->label, label)) {
            return index;
        }
    }
    return SIZE_MAX;
}

/** @brief Release one runtime-owned schedule without touching its borrowed configuration.
 * @param schedule Owned schedule, or null.
 */
static void schedule_release(struct ra_runtime_schedule *schedule) {
    if (!schedule) {
        return;
    }
    ast_free(schedule->windows);
    ast_free(schedule->links);
    ast_free(schedule->events);
    ast_free(schedule);
}

/** @brief Retain the newest receive timestamp for every configured link window.
 * @param runtime Active nodes whose controllers publish lock-free receive timestamps.
 * @param schedule Configuration-owned windows updated before a reload or policy evaluation.
 *
 * The audio worker writes only its controller atomic.  This control-plane snapshot preserves the
 * most recent value through a live configuration reload, whose worker restart otherwise resets
 * the replacement controller's atomic state.
 */
static void schedule_capture_activity(struct ra_runtime *runtime,
                                      struct ra_runtime_schedule *schedule) {
    if (!schedule) {
        return;
    }
    for (size_t index = 0; index < schedule->window_count; ++index) {
        struct ra_runtime_link_window *window = &schedule->windows[index];
        struct ra_runtime_node *node = runtime_node(runtime, window->local);
        /* Window creation requires this enabled owner, which outlives the schedule. */
        uint64_t activity = ra_controller_qualifying_activity_ms(&node->controller);
        if (activity > window->last_activity_ms) {
            window->last_activity_ms = activity;
            /* Real receive activity supersedes a cold-start estimate of the quiet deadline. */
            window->initial_grace = false;
        }
    }
}

static const char *schedule_create(struct ra_runtime *runtime, const struct ra_document *document,
                                   const struct ra_runtime_schedule *previous,
                                   struct ra_runtime_schedule **result) {
    *result = NULL;
    size_t declared_events = 0;
    size_t declared_permanents = 0;
    size_t declared_windows = 0;
    while (ra_document_event(document, declared_events, NULL)) {
        ++declared_events;
    }
    while (ra_document_permanent(document, declared_permanents, NULL)) {
        ++declared_permanents;
    }
    while (ra_document_schedule(document, declared_windows, NULL)) {
        ++declared_windows;
    }
    size_t link_capacity =
        declared_permanents + declared_windows + (previous ? previous->link_count : 0U);
    /* Avoid allocations and fixture churn when neither event nor configured-link state exists. */
    if (!declared_events && !link_capacity) {
        return NULL;
    }
    struct ra_runtime_schedule *schedule = ast_calloc(1, sizeof(*schedule));
    if (!schedule) {
        return "cannot allocate scheduled event state";
    }
    if (declared_events) {
        schedule->events = ast_calloc(declared_events, sizeof(*schedule->events));
    }
    if (declared_events && !schedule->events) {
        schedule_release(schedule);
        return "cannot allocate scheduled event state";
    }
    if (link_capacity) {
        schedule->links = ast_calloc(link_capacity, sizeof(*schedule->links));
    }
    if (link_capacity && !schedule->links) {
        schedule_release(schedule);
        return "cannot allocate configured link state";
    }
    if (declared_windows) {
        schedule->windows = ast_calloc(declared_windows, sizeof(*schedule->windows));
    }
    if (declared_windows && !schedule->windows) {
        schedule_release(schedule);
        return "cannot allocate configured link state";
    }
    schedule->document = document;
    schedule->generation = schedule_next_generation(runtime->schedule_generation);
    /* Retain the chronological task boundary across reload so an old queued task cannot create a
     * new occurrence after the replacement has already processed a newer minute. */
    schedule->last_tick = previous ? previous->last_tick : (time_t)0;
    schedule->has_last_tick = previous && previous->has_last_tick;
    for (size_t index = 0; index < declared_events; ++index) {
        const char *node_name = NULL;
        const char *set = ra_document_event(document, index, &node_name);
        struct ra_runtime_node *node = node_name ? runtime_node(runtime, node_name) : NULL;
        struct ra_node_settings node_settings;
        const char *node_error = node_name
                                     ? ra_node_settings_resolve(document->entries, document->count,
                                                                node_name, &node_settings)
                                     : "event references an unknown node";
        if (node_error) {
            schedule_release(schedule);
            return node_error;
        }
        /* A disabled node will be released at commit and has no controller to accept events. */
        if (!node_settings.enabled) {
            continue;
        }
        struct ra_event_settings settings;
        const char *error =
            ra_event_settings_resolve(document->entries, document->count, set, &settings);
        if (error) {
            schedule_release(schedule);
            return error;
        }
        if (strnlen(node->name, RA_NODE_NAME_MAX) == RA_NODE_NAME_MAX) {
            schedule_release(schedule);
            return "scheduled event node name is too long";
        }
        if (*settings.macro_name) {
            const char *macro_set =
                ra_document_macro_named(document, node->name, settings.macro_name);
            struct ra_macro_settings macro;
            const char *macro_error =
                macro_set ? ra_macro_settings_resolve(document->entries, document->count,
                                                      node->name, macro_set, &macro)
                          : "event references an unknown macro";
            if (macro_error) {
                schedule_release(schedule);
                return macro_error;
            }
        }
        const struct ra_runtime_schedule_event *prior =
            prior_schedule_event(previous, node, &settings);
        bool retry_pending = prior && prior->pending && !prior->message_queued;
        /* A queued message may outlive a reload, but its stale dispatch cannot safely run a
         * macro afterward.  Settle that occurrence rather than emitting it twice. */
        bool settle_queued = prior && prior->pending && prior->message_queued;
        schedule->events[schedule->count] = (struct ra_runtime_schedule_event){
            .node = node,
            .settings = settings,
            .completed_occurrence = settle_queued ? prior->pending_occurrence
                                    : prior       ? prior->completed_occurrence
                                                  : UINT64_MAX,
            .pending_occurrence = retry_pending ? prior->pending_occurrence : 0,
            .pending_local = retry_pending ? prior->pending_local : (struct tm){0},
            .ready_occurrence = prior && prior->ready ? prior->ready_occurrence : 0,
            .ready_local = prior && prior->ready ? prior->ready_local : (struct tm){0},
            .pending = retry_pending,
            .ready = prior && prior->ready,
        };
        ++schedule->count;
    }

    for (size_t index = 0; index < declared_permanents; ++index) {
        const char *node_name = NULL;
        const char *set = ra_document_permanent(document, index, &node_name);
        struct ra_node_settings node_settings;
        if (!document_node_settings(document, node_name, &node_settings) ||
            !node_settings.enabled) {
            continue;
        }
        struct ra_runtime_node *node = runtime_node(runtime, node_name);
        struct ra_permanent_link_settings settings;
        const char *error =
            ra_permanent_link_settings_resolve(document->entries, document->count, set, &settings);
        if (error) {
            schedule_release(schedule);
            return error;
        }
        if (strnlen(node->name, RA_NODE_NAME_MAX) == RA_NODE_NAME_MAX) {
            schedule_release(schedule);
            return "configured link node name is too long";
        }
        const struct ra_runtime_configured_link *prior =
            prior_configured_link(previous, node->name, settings.remote_node);
        schedule_add_link(schedule, node->name, settings.remote_node, settings.name, true,
                          prior && prior->issued);
    }

    for (size_t index = 0; index < declared_windows; ++index) {
        const char *node_name = NULL;
        const char *set = ra_document_schedule(document, index, &node_name);
        struct ra_node_settings node_settings;
        if (!document_node_settings(document, node_name, &node_settings) ||
            !node_settings.enabled) {
            continue;
        }
        struct ra_runtime_node *node = runtime_node(runtime, node_name);
        struct ra_link_schedule_settings settings;
        const char *error =
            ra_link_schedule_settings_resolve(document->entries, document->count, set, &settings);
        if (error) {
            schedule_release(schedule);
            return error;
        }
        if (strnlen(node->name, RA_NODE_NAME_MAX) == RA_NODE_NAME_MAX) {
            schedule_release(schedule);
            return "configured link node name is too long";
        }
        size_t replaced_index =
            schedule_permanent_index(schedule, node->name, settings.replace_permanent);
        if (replaced_index == SIZE_MAX) {
            schedule_release(schedule);
            return "schedule references an unknown permanent link";
        }
        const struct ra_runtime_configured_link *prior =
            prior_configured_link(previous, node->name, settings.remote_node);
        size_t route_index = schedule->link_count;
        schedule_add_link(schedule, node->name, settings.remote_node, NULL, false,
                          prior && prior->issued);
        const struct ra_runtime_link_window *previous_window =
            prior_link_window(previous, node->name, &settings);
        bool same_window_policy =
            previous_window && previous_window->end_inactivity_ms == settings.end_inactivity_ms &&
            same_scheduled_window(&previous_window->window, &settings.window);
        schedule->windows[schedule->window_count++] = (struct ra_runtime_link_window){
            .name = settings.name,
            .local = {0},
            .window = settings.window,
            .end_inactivity_ms = settings.end_inactivity_ms,
            .route_index = route_index,
            .replaced_index = replaced_index,
            .last_activity_ms = previous_window ? previous_window->last_activity_ms : 0,
            .initial_deadline_ms = same_window_policy ? previous_window->initial_deadline_ms : 0,
            .was_active = same_window_policy && previous_window->was_active,
            .waiting_for_idle = same_window_policy && previous_window->waiting_for_idle,
            .initialized = same_window_policy && previous_window->initialized,
            .initial_grace = same_window_policy && previous_window->initial_grace,
        };
        ast_copy_string(schedule->windows[schedule->window_count - 1U].local, node->name,
                        sizeof(schedule->windows[schedule->window_count - 1U].local));
    }

    if (previous) {
        for (size_t prior_index = 0; prior_index < previous->link_count; ++prior_index) {
            const struct ra_runtime_configured_link *prior = &previous->links[prior_index];
            bool retained = false;
            for (size_t current_index = 0; current_index < schedule->link_count; ++current_index) {
                if (same_configured_link(&schedule->links[current_index], prior->local,
                                         prior->remote)) {
                    retained = true;
                    break;
                }
            }
            struct ra_node_settings node_settings;
            if (prior->issued && !retained &&
                document_node_settings(document, prior->local, &node_settings) &&
                node_settings.enabled) {
                schedule_add_link(schedule, prior->local, prior->remote, NULL, false, true);
                schedule->links[schedule->link_count - 1U].retiring = true;
            }
        }
    }
    if (!schedule->count && !schedule->link_count) {
        schedule_release(schedule);
        return NULL;
    }
    *result = schedule;
    return NULL;
}

static void reconcile_scheduled_links(struct ra_runtime *runtime,
                                      struct ra_runtime_schedule *schedule, const struct tm *local,
                                      uint64_t now_ms, const char *selected_local) {
    schedule_link_desires(runtime, schedule, local, now_ms);
    schedule_refresh_link_ownership(runtime, schedule);
    for (size_t index = 0; index < schedule->link_count; ++index) {
        struct ra_runtime_configured_link *link = &schedule->links[index];
        if (!link->issued || link->desired ||
            (selected_local && strcmp(link->local, selected_local))) {
            continue;
        }
        struct ra_runtime_node *node = runtime_node(runtime, link->local);
        link->pending = false;
        clear_remote_selection(node, link->remote);
        (void)ra_link_hub_disconnect_permanent(&node->links, link->remote);
        link->issued = false;
    }
}

static int queue_status_speech(struct ra_runtime_node *node, const char *text, const char *node_one,
                               const char *node_two);

/** @brief Announce rejection of a duplicate or loop-forming link request.
 * @param node Running local node that owns the RF-status queue.
 */
static void announce_link_loop(struct ra_runtime_node *node) {
    (void)queue_status_speech(node, "LINK REJECTED TOPOLOGY LOOP", NULL, NULL);
}

/** @brief Restart one stable node while retaining its peer manager and routing ownership.
 * @param node Stable node whose callbacks use the same address before and after replacement.
 * @param document Valid configuration that owns `name` and resolved-string storage.
 * @param name Exact node section in `document`.
 * @param settings Resolved node settings borrowed from `document`.
 * @return Null when the radio resources are running, or a startup diagnostic.
 */
static const char *restart_node(struct ra_runtime_node *node, const struct ra_document *document,
                                const char *name, const struct ra_node_settings *settings) {
    /* Peer readers and the manager can call only through this stable node address. */
    ast_mutex_lock(&node->callback_lock);
    reset_node_resources(node);
    node->name = name;
    node->settings = *settings;
    const char *error = start_node(node, document, name, settings);
    ast_mutex_unlock(&node->callback_lock);
    return error;
}

/** @brief Remember a stable node's old document-owned settings before replacement.
 * @param node Running node whose radio resources will be restarted.
 *
 * The copied structure contains only pointers borrowed from the current document, which remains
 * alive until this reload commits.  Keeping it on the stable node makes rollback independent of a
 * second document lookup and therefore guarantees initialized settings for restart.
 */
static void snapshot_node_configuration(struct ra_runtime_node *node) {
    node->reload_name = node->name;
    node->reload_settings = node->settings;
    node->reload_reconfigured = true;
}

/** @brief Discard a completed reload snapshot before its document may be released.
 * @param node Stable node whose snapshot is no longer needed.
 */
static void clear_node_snapshot(struct ra_runtime_node *node) {
    node->reload_name = NULL;
    node->reload_settings = (struct ra_node_settings){0};
    node->reload_reconfigured = false;
}

/** @brief Remove newly created nodes and restore every reconfigured node after a failed reload.
 * @param runtime Runtime being restored.
 * @param current Valid document that still owns the previous node configuration.
 * @return Null when every former node restarted, or the first restoration diagnostic.
 */
static const char *rollback_reload(struct ra_runtime *runtime, const struct ra_document *current) {
    struct ra_runtime_node **cursor = &runtime->nodes;
    while (*cursor) {
        struct ra_runtime_node *node = *cursor;
        if (!node->reload_new) {
            cursor = &node->next;
            continue;
        }
        *cursor = node->next;
        release_node(node);
    }
    const char *failure = NULL;
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!node->reload_reconfigured) {
            continue;
        }
        const char *error = restart_node(node, current, node->reload_name, &node->reload_settings);
        if (error && !failure) {
            failure = error;
        }
        clear_node_snapshot(node);
    }
    /* The module invalidates the current tick on every reload attempt. Release copied work so
     * the retained schedule can issue a fresh nonce on the next ticker pass. */
    clear_scheduled_link_reservations(runtime, NULL);
    return failure;
}

/** @brief Release nodes omitted or disabled by a successful replacement.
 * @param runtime Runtime whose retained nodes now use the replacement configuration.
 * @param replacement Valid configuration that selects the kept nodes.
 */
static void release_removed_nodes(struct ra_runtime *runtime,
                                  const struct ra_document *replacement) {
    struct ra_runtime_node **cursor = &runtime->nodes;
    while (*cursor) {
        struct ra_runtime_node *node = *cursor;
        struct ra_node_settings settings;
        if (document_node_settings(replacement, node->name, &settings) && settings.enabled) {
            clear_node_snapshot(node);
            node->reload_new = false;
            cursor = &node->next;
            continue;
        }
        *cursor = node->next;
        release_node(node);
    }
}

const char *ra_runtime_reload(struct ra_runtime *runtime, const struct ra_document *current,
                              const struct ra_document *replacement) {
    schedule_capture_activity(runtime, runtime->schedule);
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        struct ra_node_settings settings;
        if (!document_node_settings(replacement, node->name, &settings) || !settings.enabled) {
            continue;
        }
        const char *name = document_node_name(replacement, node->name);
        snapshot_node_configuration(node);
        const char *error = restart_node(node, replacement, name, &settings);
        if (error) {
            const char *restoration = rollback_reload(runtime, current);
            return restoration ? restoration : error;
        }
    }
    for (size_t index = 0;; ++index) {
        const char *name = ra_document_node(replacement, index);
        if (!name) {
            break;
        }
        struct ra_node_settings settings;
        (void)ra_node_settings_resolve(replacement->entries, replacement->count, name, &settings);
        if (!settings.enabled || runtime_node(runtime, name)) {
            continue;
        }
        struct ra_runtime_node *node = new_node(runtime);
        if (!node) {
            const char *restoration = rollback_reload(runtime, current);
            return restoration ? restoration : "cannot allocate radio state";
        }
        node->name = name;
        node->settings = settings;
        const char *error = start_node(node, replacement, name, &settings);
        if (error) {
            release_node(node);
            const char *restoration = rollback_reload(runtime, current);
            return restoration ? restoration : error;
        }
        node->reload_new = true;
        node->next = runtime->nodes;
        runtime->nodes = node;
    }
    struct ra_runtime_schedule *schedule = NULL;
    const char *schedule_error =
        schedule_create(runtime, replacement, runtime->schedule, &schedule);
    if (schedule_error) {
        const char *restoration = rollback_reload(runtime, current);
        return restoration ? restoration : schedule_error;
    }
    if (schedule && schedule->link_count) {
        struct tm local;
        if (!schedule_local_time(time(NULL), &local)) {
            schedule_release(schedule);
            const char *restoration = rollback_reload(runtime, current);
            return restoration ? restoration
                               : "cannot read local time to reconcile configured links";
        }
        reconcile_scheduled_links(runtime, schedule, &local, runtime_monotonic_ms(), NULL);
    }
    release_removed_nodes(runtime, replacement);
    schedule_release(runtime->schedule);
    runtime->schedule = schedule;
    runtime->schedule_generation = schedule_next_generation(runtime->schedule_generation);
    return NULL;
}

bool ra_runtime_digit(struct ra_runtime *runtime, const char *local, char digit, uint64_t now_ms,
                      struct ra_link_operation *operation) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (strcmp(node->name, local)) {
            continue;
        }
        if (*node->remote_node) {
            if (digit == '#') {
                node->remote_node[0] = '\0';
                return false;
            }
            if (digit && strchr("0123456789ABCD*", digit)) {
                *operation = (struct ra_link_operation){.action = RA_LINK_COMMAND, .digit = digit};
                ast_copy_string(operation->remote, node->remote_node, sizeof(operation->remote));
                return true;
            }
            return false;
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
        *operation = (struct ra_link_operation){.action = command.action};
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
                      struct ast_channel *channel, bool verified) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            if (!strcmp(local, remote) || ra_link_hub_reaches(&node->links, remote)) {
                announce_link_loop(node);
                return -1;
            }
            if (!ra_link_access_allowed(node->settings.link_allow_nodes,
                                        node->settings.link_deny_nodes, remote, verified)) {
                return -1;
            }
            return ra_link_hub_attach(&node->links, remote, channel, node->connection.radio.linear,
                                      true, true, false);
        }
    }
    return -1;
}

/** @brief Test direct-link admission while honoring the operator disconnect-all hold.
 * @param node Local route owner.
 * @param remote Requested direct peer identity.
 * @param scheduled Nullable scheduler reservation, or null for a manual operation.
 * @return True when a live direct peer, topology advertisement, or eligible retry reaches remote.
 */
static bool runtime_link_reaches(const struct ra_runtime_node *node, const char *remote,
                                 const struct ra_scheduled_link_operation *scheduled) {
    return !scheduled && node->configured_links_paused
               ? ra_link_hub_reaches_live(&node->links, remote)
               : ra_link_hub_reaches(&node->links, remote);
}

int ra_runtime_prepare_link(struct ra_runtime *runtime, const char *local, const char *remote,
                            struct ra_link_dial *dial,
                            const struct ra_scheduled_link_operation *scheduled) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (strcmp(node->name, local)) {
            continue;
        }
        if (scheduled &&
            (scheduled->operation.action != RA_LINK_PERMANENT_TRANSCEIVE ||
             !scheduled_link_policy_current_now(runtime, scheduled) ||
             strcmp(scheduled->local, local) || strcmp(scheduled->operation.remote, remote))) {
            return -1;
        }
        /* `*806` retains dormant retries for `*816`, but an operator may replace them manually. */
        if (!strcmp(local, remote) || runtime_link_reaches(node, remote, scheduled)) {
            if (!scheduled) {
                announce_link_loop(node);
            }
            return -1;
        }
        char *destination = resolve_link_node(node, remote, NULL);
        if (!destination) {
            return -1;
        }
        return prepare_link_dial(dial, destination, node->connection.radio.linear);
    }
    return -1;
}

/** @brief Settle one reserved configured-link operation while the runtime lock is held.
 * @param runtime Active runtime owning the current schedule.
 * @param operation Exact copied scheduler reservation to settle.
 * @param accepted True when the link hub accepted the requested resulting state.
 * @return True only when a current, policy-authorized reservation was cleared.
 *
 * An attachment or retry is settled before the runtime lock is released. This prevents a reload
 * from observing an attached route as unissued. A stale operation still clears only its exact
 * pending reservation; it can never mark a newly reserved route issued.
 */
static bool settle_scheduled_link_operation(struct ra_runtime *runtime,
                                            const struct ra_scheduled_link_operation *operation,
                                            bool accepted) {
    struct ra_runtime_configured_link *link = scheduled_link_reservation(runtime, operation);
    if (!link) {
        return false;
    }
    bool current = scheduled_link_operation_current(runtime, operation);
    link->pending = false;
    if (!current) {
        return false;
    }
    if (accepted) {
        link->issued = operation->operation.action == RA_LINK_PERMANENT_TRANSCEIVE;
    }
    return true;
}

bool ra_runtime_complete_scheduled_link_operation(
    struct ra_runtime *runtime, const struct ra_scheduled_link_operation *operation,
    bool accepted) {
    return settle_scheduled_link_operation(runtime, operation, accepted);
}

struct ast_channel *ra_link_dial_run(struct ra_link_dial *dial, const char *local) {
    if (!dial || !dial->destination || !dial->candidates || !dial->candidate_count) {
        discard_link_dial(dial);
        return NULL;
    }
    struct timespec started;
    bool reliable_clock = !clock_gettime(CLOCK_MONOTONIC, &started);
    struct ast_channel *channel = NULL;
    for (size_t index = 0; index < dial->candidate_count; ++index) {
        int timeout = remaining_dial_timeout(&started, reliable_clock);
        if (!timeout) {
            break;
        }
        struct ast_format_cap *offer = ra_media_offer_create(dial->candidates[index]);
        if (!offer) {
            break;
        }
        int reason = 0;
        channel = ast_request_and_dial("IAX2", offer, NULL, NULL, dial->destination, timeout,
                                       &reason, local, local);
        ao2_cleanup(offer);
        if (channel && ast_channel_state(channel) == AST_STATE_UP) {
            break;
        }
        if (channel) {
            ast_hangup(channel);
            channel = NULL;
        }
    }
    discard_link_dial(dial);
    return channel;
}

/** @brief Caller-owned state for an exact scheduled attachment at final hub publication. */
struct scheduled_attachment_gate {
    struct ra_runtime *runtime; /**< Runtime holding the current schedule under its control lock. */
    const struct ra_scheduled_link_operation
        *operation; /**< Copied reservation that must remain current at publication. */
};

/** @brief Revalidate a scheduled attachment immediately before the hub publishes its peer.
 * @param context Pointer to one active @ref scheduled_attachment_gate.
 * @return True only while the copied reservation remains selected by current civil-time policy.
 *
 * This runs under the hub routing lock but the caller already owns the runtime control lock. It
 * reads only that runtime's scheduler state and lock-free receive activity, so it cannot block or
 * take the routing lock recursively.
 */
static bool scheduled_attachment_current(void *context) {
    const struct scheduled_attachment_gate *gate = context;
    return scheduled_link_policy_current_now(gate->runtime, gate->operation);
}

int ra_runtime_attach_link(struct ra_runtime *runtime, const char *local, const char *remote,
                           struct ast_channel *channel, bool transmit, bool forward, bool permanent,
                           const struct ra_scheduled_link_operation *scheduled) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            if (scheduled &&
                (scheduled->operation.action != RA_LINK_PERMANENT_TRANSCEIVE ||
                 !scheduled_link_policy_current_now(runtime, scheduled) ||
                 strcmp(scheduled->local, local) || strcmp(scheduled->operation.remote, remote))) {
                return -1;
            }
            if (!strcmp(local, remote) || runtime_link_reaches(node, remote, scheduled)) {
                if (!scheduled) {
                    announce_link_loop(node);
                }
                return -1;
            }
            struct scheduled_attachment_gate gate = {.runtime = runtime, .operation = scheduled};
            int result = ra_link_hub_attach_gated(
                &node->links, remote, channel, node->connection.radio.linear, transmit, forward,
                permanent, NULL, NULL, scheduled ? scheduled_attachment_current : NULL,
                scheduled ? &gate : NULL);
            if (!result && scheduled) {
                (void)settle_scheduled_link_operation(runtime, scheduled, true);
            }
            return result;
        }
    }
    return -1;
}

bool ra_runtime_retain_permanent_link(struct ra_runtime *runtime, const char *local,
                                      const char *remote, bool transmit, bool forward,
                                      const struct ra_scheduled_link_operation *scheduled) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            if (scheduled &&
                (scheduled->operation.action != RA_LINK_PERMANENT_TRANSCEIVE ||
                 !scheduled_link_policy_current_now(runtime, scheduled) ||
                 strcmp(scheduled->local, local) || strcmp(scheduled->operation.remote, remote))) {
                return false;
            }
            if (!strcmp(local, remote) || runtime_link_reaches(node, remote, scheduled)) {
                if (!scheduled) {
                    announce_link_loop(node);
                }
                return false;
            }
            bool retained = ra_link_hub_retain_permanent(&node->links, remote, transmit, forward);
            if (retained && scheduled) {
                (void)settle_scheduled_link_operation(runtime, scheduled, true);
            }
            return retained;
        }
    }
    return false;
}

bool ra_runtime_disconnect(struct ra_runtime *runtime, const char *local, const char *remote) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            bool disconnected = ra_link_hub_disconnect(&node->links, remote);
            if (disconnected) {
                clear_remote_selection(node, remote);
            }
            return disconnected;
        }
    }
    return false;
}

bool ra_runtime_disconnect_permanent(struct ra_runtime *runtime, const char *local,
                                     const char *remote,
                                     const struct ra_scheduled_link_operation *scheduled) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            if (scheduled &&
                (scheduled->operation.action != RA_LINK_DISCONNECT_PERMANENT ||
                 !scheduled_link_policy_current_now(runtime, scheduled) ||
                 strcmp(scheduled->local, local) || strcmp(scheduled->operation.remote, remote))) {
                return false;
            }
            bool disconnected = ra_link_hub_disconnect_permanent(&node->links, remote);
            if (disconnected || scheduled) {
                clear_remote_selection(node, remote);
            }
            if (scheduled) {
                /* A route already withdrawn by transport teardown has reached the requested state.
                 */
                (void)settle_scheduled_link_operation(runtime, scheduled, true);
                return true;
            }
            return disconnected;
        }
    }
    return false;
}

/** @brief Discard copied scheduler reservations for one node or after a failed reload.
 * @param runtime Active runtime owning the schedule.
 * @param local Exact selected local node identity, or null for every configured node.
 */
static void clear_scheduled_link_reservations(struct ra_runtime *runtime, const char *local) {
    if (!runtime->schedule) {
        return;
    }
    for (size_t index = 0; index < runtime->schedule->link_count; ++index) {
        struct ra_runtime_configured_link *link = &runtime->schedule->links[index];
        if (!local || !strcmp(link->local, local)) {
            link->pending = false;
        }
    }
}

/** @brief Read a bounded monotonic timestamp for an operator reconnect reconciliation.
 * @return Milliseconds since the monotonic epoch, or zero when the host clock is unavailable.
 */
static uint64_t runtime_monotonic_ms(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value)) {
        return 0;
    }
    return (uint64_t)value.tv_sec * 1000U + (uint64_t)value.tv_nsec / 1000000U;
}

/** @brief Test whether a local node owns any configuration-managed direct route.
 * @param schedule Active schedule, or null.
 * @param local Exact local node identity.
 * @return True when a permanent, replacement, or retiring route belongs to @p local.
 */
static bool schedule_has_links_for_node(const struct ra_runtime_schedule *schedule,
                                        const char *local) {
    if (!schedule) {
        return false;
    }
    for (size_t index = 0; index < schedule->link_count; ++index) {
        if (!strcmp(schedule->links[index].local, local)) {
            return true;
        }
    }
    return false;
}

/** @brief Cancel paused retries that no longer match the current configured schedule.
 * @param runtime Active runtime whose control caller serializes link state.
 * @param node Local node leaving its operator disconnect-all hold.
 * @return True after policy reconciliation, or false when required local time is unavailable.
 *
 * Scheduler policy is recalculated before the hub resumes its retained retries. This prevents a
 * formerly permanent route from briefly reconnecting during a replacement window and preserves
 * detach-before-attach ordering when `*816` restores configuration-owned links.
 */
static bool reconcile_scheduled_links_before_reconnect(struct ra_runtime *runtime,
                                                       const struct ra_runtime_node *node) {
    struct ra_runtime_schedule *schedule = runtime->schedule;
    struct tm local;
    if (!schedule_has_links_for_node(schedule, node->name)) {
        return true;
    }
    if (!schedule_local_time(time(NULL), &local)) {
        return false;
    }
    reconcile_scheduled_links(runtime, schedule, &local, runtime_monotonic_ms(), node->name);
    return true;
}

size_t ra_runtime_disconnect_all(struct ra_runtime *runtime, const char *local) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            node->configured_links_paused = true;
            clear_scheduled_link_reservations(runtime, local);
            size_t count = ra_link_hub_disconnect_all(&node->links);
            node->remote_node[0] = '\0';
            return count;
        }
    }
    return 0;
}

size_t ra_runtime_disconnect_nonpermanent_all(struct ra_runtime *runtime, const char *local) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            size_t count = ra_link_hub_disconnect_nonpermanent_all(&node->links);
            node->remote_node[0] = '\0';
            return count;
        }
    }
    return 0;
}

size_t ra_runtime_reconnect_all(struct ra_runtime *runtime, const char *local) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            clear_scheduled_link_reservations(runtime, local);
            if (!reconcile_scheduled_links_before_reconnect(runtime, node)) {
                return 0;
            }
            node->configured_links_paused = false;
            return ra_link_hub_reconnect_all(&node->links);
        }
    }
    return 0;
}

/** @brief Name the routing behavior of one direct peer for an operator report.
 * @param peer Direct-peer status snapshot.
 * @return Static Morse-safe mode label.
 */
static const char *peer_mode(const struct ra_link_peer_status *peer) {
    if (peer->transmit) {
        return "TRANSCEIVE";
    }
    /* RF status must remain brief enough for the controller's bounded telemetry queue. */
    return peer->forward ? "MONITOR" : "LOCAL";
}

/* A formatted status has a decimal count, one bounded peer name, and the longest fixed labels.
 * The worker's controller queue always provides this fixed storage, so no test-only caller-sized
 * formatting interface is needed. */
_Static_assert(sizeof(size_t) <= 8 && sizeof(size_t) * 3 + sizeof(" LINKS ") - 1 +
                                              RA_LINK_PEER_NAME_MAX - 1 + sizeof(" ") - 1 +
                                              sizeof("TRANSCEIVE") - 1 <=
                                          RA_CONTROLLER_STATUS_TEXT_MAX,
               "controller status buffer must hold every direct-peer report");

/** @brief Append a fixed status fragment to the compile-time-bounded status buffer.
 * @param text Destination controller-status buffer.
 * @param length Current text length, updated after the append.
 * @param fragment Null-terminated static fragment.
 */
static void status_append(char *text, size_t *length, const char *fragment) {
    size_t added = 0;
    while (fragment[added] != '\0') {
        ++added;
    }
    for (size_t index = 0; index <= added; ++index) {
        text[*length + index] = fragment[index];
    }
    *length += added;
}

/** @brief Append one bounded peer name without trusting a terminator beyond its protocol limit.
 * @param text Destination controller-status buffer.
 * @param length Current text length, updated after a successful append.
 * @param name Peer identity with RA_LINK_PEER_NAME_MAX bytes available.
 * @return True only for a complete, terminated peer identity.
 */
static bool status_append_peer_name(char *text, size_t *length,
                                    const char name[RA_LINK_PEER_NAME_MAX]) {
    size_t added = 0;
    while (added < RA_LINK_PEER_NAME_MAX && name[added] != '\0') {
        ++added;
    }
    if (added == RA_LINK_PEER_NAME_MAX) {
        return false;
    }
    for (size_t index = 0; index <= added; ++index) {
        text[*length + index] = name[index];
    }
    *length += added;
    return true;
}

/** @brief Append a decimal direct-peer count without formatted I/O.
 * @param text Destination controller-status buffer.
 * @param length Current text length, updated after a successful append.
 * @param value Unsigned count to append.
 */
static void status_append_count(char *text, size_t *length, size_t value) {
    char digits[sizeof(value) * 3];
    size_t digit_count = 0;
    do {
        digits[digit_count++] = (char)('0' + value % 10);
        value /= 10;
    } while (value);
    while (digit_count) {
        const char digit[] = {digits[--digit_count], '\0'};
        status_append(text, length, digit);
    }
}

/** @brief Format a bounded direct-peer status response for one resolved node.
 * @param node Resolved runtime node.
 * @param last_keyed Select the remembered direct peer instead of current-link status.
 * @param text Output status text.
 * @param identity Output peer identity for speech formatting.
 * @return True when a complete status was written.
 */
static bool node_link_status_text(struct ra_runtime_node *node, bool last_keyed, char *text,
                                  char identity[RA_LINK_PEER_NAME_MAX]) {
    text[0] = '\0';
    identity[0] = '\0';
    size_t length = 0;
    if (last_keyed) {
        char name[RA_LINK_PEER_NAME_MAX];
        if (!ra_link_hub_last_keyed(&node->links, name, sizeof(name))) {
            status_append(text, &length, "NO LAST KEYED");
            return true;
        }
        ast_copy_string(identity, name, RA_LINK_PEER_NAME_MAX);
        status_append(text, &length, "LAST KEYED ");
        return status_append_peer_name(text, &length, name);
    }
    struct ra_link_peer_status peer;
    size_t count = ra_link_hub_snapshot(&node->links, &peer, 1);
    if (!count) {
        status_append(text, &length, "NO LINKS");
        return true;
    }
    if (count == 1) {
        ast_copy_string(identity, peer.name, RA_LINK_PEER_NAME_MAX);
        status_append(text, &length, "LINK ");
        if (!status_append_peer_name(text, &length, peer.name)) {
            return false;
        }
        status_append(text, &length, " ");
        status_append(text, &length, peer_mode(&peer));
        return true;
    }
    ast_copy_string(identity, peer.name, RA_LINK_PEER_NAME_MAX);
    status_append_count(text, &length, count);
    status_append(text, &length, " LINKS ");
    if (!status_append_peer_name(text, &length, peer.name)) {
        return false;
    }
    status_append(text, &length, " ");
    status_append(text, &length, peer_mode(&peer));
    return true;
}

/** @brief Release status speech PCM after the radio worker has completed its slot.
 * @param node Runtime node whose serial control executor owns status-PCM destruction.
 *
 * The controller publishes completed slots with release/acquire ordering. This control-plane
 * reaper deliberately performs all deallocation outside the hardware-paced audio callback.
 */
static void reclaim_status_audio(struct ra_runtime_node *node) {
    int16_t *audio[RA_CONTROLLER_STATUS_QUEUE_DEPTH];
    size_t count =
        ra_controller_reclaim_status(&node->controller, audio, RA_CONTROLLER_STATUS_QUEUE_DEPTH);
    for (size_t index = 0; index < count; ++index) {
        ast_free(audio[index]);
    }
}

bool ra_runtime_telemetry_speech_text(const char *source, const char *node_one,
                                      const char *node_two, char *speech, size_t capacity) {
    size_t used = 0;
    while (*source) {
        while (*source == ' ') {
            if (used + 1 >= capacity)
                return false;
            speech[used++] = *source++;
        }
        const char *word = source;
        bool letters = false, has_digit = false;
        size_t length = 0;
        while (source[length] && source[length] != ' ') {
            char c = source[length++];
            letters |= isalpha((unsigned char)c) != 0;
            has_digit |= isdigit((unsigned char)c) != 0;
        }
        bool node = (node_one && strlen(node_one) == length && !strncmp(word, node_one, length)) ||
                    (node_two && strlen(node_two) == length && !strncmp(word, node_two, length));
        bool callsign = !node && letters && has_digit;
        if (node) {
            static const char prefix[] = "node,";
            if (used + sizeof(prefix) - 1 >= capacity)
                return false;
            for (size_t index = 0; index < sizeof(prefix) - 1; ++index)
                speech[used++] = prefix[index];
        }
        for (size_t index = 0; index < length; ++index) {
            if (used + 1 + (node || callsign ? 1 : 0) >= capacity)
                return false;
            speech[used++] = word[index];
            if (node || callsign)
                speech[used++] = ',';
        }
        if (node || callsign)
            --used;
        source += length;
    }
    if (used >= capacity)
        return false;
    speech[used] = '\0';
    return true;
}

/** @brief Prepare and queue one RF telemetry reply with its Morse fallback.
 * @param node Running node whose inherited speech and Morse settings apply.
 * @param speech Prepared speech text.
 * @param morse Morse-safe fallback text.
 * @return Zero when the reply is queued, minus one if its bounded queue is full or invalid.
 */
static int queue_status(struct ra_runtime_node *node, const char *speech, const char *morse) {
    reclaim_status_audio(node);
    struct ra_identifier_settings settings = node->status_settings;
    settings.file = "";
    settings.speech_text = speech;
    settings.morse_text = morse;
    int16_t *audio = NULL;
    size_t samples = 0;
    /* The serial control executor may wait for Piper; the real-time worker only reads PCM. */
    ra_identifier_prepare(&settings, node->controller.rate, &audio, &samples);
    if (!ra_controller_queue_status(&node->controller, morse, audio, samples)) {
        ast_free(audio);
        return -1;
    }
    return 0;
}

/** @brief Prepare a normal RF-status reply from its Morse-safe text.
 * @param node Running node whose inherited speech and Morse settings apply.
 * @param text Bounded Morse-safe status text.
 * @param node_one First known numeric node identity, if any.
 * @param node_two Second known numeric node identity, if any.
 * @return Zero when queued, otherwise minus one.
 */
static int queue_status_speech(struct ra_runtime_node *node, const char *text, const char *node_one,
                               const char *node_two) {
    char speech[RA_CONTROLLER_STATUS_TEXT_MAX * 2];
    (void)ra_runtime_telemetry_speech_text(text, node_one, node_two, speech, sizeof(speech));
    return queue_status(node, speech, text);
}

/* The public declaration documents this control-plane operation. */
int ra_runtime_queue_time(struct ra_runtime *runtime, const char *local) {
    time_t now = time(NULL);
    struct timeval when;
    struct ast_tm ast_time;
    struct tm local_time;
    char speech[RA_CONTROLLER_STATUS_TEXT_MAX];
    char morse[RA_CONTROLLER_STATUS_TEXT_MAX];
    when = (struct timeval){.tv_sec = now, .tv_usec = 0};
    if (now == (time_t)-1 || !ast_localtime(&when, &ast_time, NULL)) {
        return -1;
    }
    local_time = (struct tm){.tm_hour = ast_time.tm_hour, .tm_min = ast_time.tm_min};
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            if (!ra_time_announcement_format(&local_time, node->time_settings.format, speech,
                                             sizeof(speech), morse, sizeof(morse))) {
                return -1;
            }
            return queue_status(node, speech, morse);
        }
    }
    return -1;
}

int ra_runtime_queue_link_status(struct ra_runtime *runtime, const char *local, bool last_keyed) {
    char text[RA_CONTROLLER_STATUS_TEXT_MAX];
    char identity[RA_LINK_PEER_NAME_MAX];
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            if (!node_link_status_text(node, last_keyed, text, identity)) {
                return -1;
            }
            return queue_status_speech(node, text, identity, NULL);
        }
    }
    return -1;
}

/** @brief Return the fixed local-time greeting selected by a validated civil clock hour.
 * @param hour Local hour from zero through 23.
 * @return Static greeting selected for the supplied hour.
 */
static const char *scheduled_greeting(int hour) {
    if (hour < 12) {
        return "Good Morning";
    }
    if (hour < 17) {
        return "Good Afternoon";
    }
    return "Good Evening";
}

/** @brief Copy a scheduled message into a bounded Morse-safe fallback without changing speech.
 * @param source Complete rendered message from a validated template.
 * @param destination Fixed output buffer receiving supported Morse characters or spaces.
 * @return True only when the fallback contains at least one audible Morse character.
 */
static bool scheduled_morse_text(const char *source,
                                 char destination[RA_MESSAGE_TEMPLATE_OUTPUT_MAX]) {
    static const char allowed[] =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789/.,?-=+@()'!\":;_$& \t\r\n";
    size_t index = 0;
    bool audible = false;
    /* The renderer owns this input and proves it has at most 127 payload bytes. */
    while (source[index]) {
        destination[index] = strchr(allowed, source[index]) ? source[index] : ' ';
        audible |= !isspace((unsigned char)destination[index]);
        ++index;
    }
    destination[index] = '\0';
    if (!audible) {
        destination[0] = '\0';
    }
    return audible;
}

/** @brief Append one proven-bounded fragment while retaining one destination terminator.
 * @param destination Initialized scheduled-speech output buffer.
 * @param used Current destination length, updated after a complete append.
 * @param source Source bytes to append.
 * @param length Source byte count excluding a terminator.
 *
 * Template validation bounds rendered source text to 127 bytes. A scheduled node expands at most
 * six-fold, and `RA_RUNTIME_SCHEDULED_SPEECH_MAX` is that conservative bound. Valid settings
 * therefore cannot overrun the fixed output while they are expanded.
 */
static void scheduled_speech_append(char *destination, size_t *used, const char *source,
                                    size_t length) {
    for (size_t index = 0; index < length; ++index) {
        destination[*used + index] = source[index];
    }
    *used += length;
    destination[*used] = '\0';
}

/** @brief Expand one non-node scheduled-message fragment using ordinary telemetry speech rules.
 * @param source Complete rendered message containing the fragment.
 * @param offset First fragment byte in @p source.
 * @param length Fragment byte count.
 * @param destination Initialized scheduled-speech output buffer.
 * @param used Current destination length, updated after a complete append.
 */
static void scheduled_speech_fragment(const char *source, size_t offset, size_t length,
                                      char *destination, size_t *used) {
    char fragment[RA_MESSAGE_TEMPLATE_OUTPUT_MAX];
    char spoken[RA_RUNTIME_SCHEDULED_SPEECH_MAX];
    for (size_t index = 0; index < length; ++index) {
        fragment[index] = source[offset + index];
    }
    fragment[length] = '\0';
    (void)ra_runtime_telemetry_speech_text(fragment, NULL, NULL, spoken, sizeof(spoken));
    scheduled_speech_append(destination, used, spoken, strlen(spoken));
}

/** @brief Append a local-node identity as a speech-safe node prefix and individually spaced digits.
 * @param node Valid nonempty local node identity.
 * @param destination Initialized scheduled-speech output buffer.
 * @param used Current destination length, updated after a complete append.
 */
static void scheduled_speech_node(const char *node, char *destination, size_t *used) {
    static const char prefix[] = "node,";
    scheduled_speech_append(destination, used, prefix, sizeof(prefix) - 1);
    size_t length = strlen(node);
    for (size_t index = 0; index < length; ++index) {
        scheduled_speech_append(destination, used, node + index, 1);
        if (index + 1 < length) {
            scheduled_speech_append(destination, used, ",", 1);
        }
    }
}

/** @brief Append an amateur callsign one character at a time for offline speech synthesis.
 * @param callsign Valid nonempty configured callsign.
 * @param destination Initialized scheduled-speech output buffer.
 * @param used Current destination length, updated after a complete append.
 */
static void scheduled_speech_callsign(const char *callsign, char *destination, size_t *used) {
    size_t length = strlen(callsign);
    for (size_t index = 0; index < length; ++index) {
        scheduled_speech_append(destination, used, callsign + index, 1);
        if (index + 1 < length) {
            scheduled_speech_append(destination, used, ",", 1);
        }
    }
}

/** @brief Match one configured identity only at punctuation-safe word boundaries.
 * @param source Complete scheduled message text.
 * @param index Candidate identity offset in @p source.
 * @param identity Nonempty configured node or callsign text.
 * @return True when the complete identity is present without adjacent alphanumeric text.
 */
static bool scheduled_speech_identity_at(const char *source, size_t index, const char *identity) {
    size_t length = strlen(identity);
    if (length > strlen(source + index)) {
        return false;
    }
    if (index && isalnum((unsigned char)source[index - 1])) {
        return false;
    }
    if (strncmp(source + index, identity, length)) {
        return false;
    }
    return !isalnum((unsigned char)source[index + length]);
}

/** @brief Render scheduled speech while recognizing configured node, peer, and callsign text
 * beside punctuation.
 * @param source Complete bounded message expanded from an event template.
 * @param node Bounded local node identity to say as a node number.
 * @param callsign Configured station callsign to say character-by-character.
 * @param link_identity Current direct-peer identity selected by `${link_status}`, if any.
 * @param destination Speech-synthesizer output destination.
 *
 * Ordinary numbers retain existing telemetry pronunciation.
 *
 * The general telemetry formatter intentionally treats only whitespace-separated identities as
 * node numbers.  Scheduled templates commonly place `${node}` before sentence punctuation, so
 * this small adapter recognizes configured node and callsign identities at non-alphanumeric
 * boundaries before delegating all remaining text to that general formatter.
 */
static void scheduled_speech_text(const char *source, const char *node, const char *callsign,
                                  const char *link_identity, char *destination) {
    destination[0] = '\0';
    size_t used = 0;
    size_t fragment_start = 0;
    size_t index = 0;
    while (source[index]) {
        if (scheduled_speech_identity_at(source, index, node)) {
            scheduled_speech_fragment(source, fragment_start, index - fragment_start, destination,
                                      &used);
            scheduled_speech_node(node, destination, &used);
            index += strlen(node);
            fragment_start = index;
            continue;
        }
        if (*link_identity && scheduled_speech_identity_at(source, index, link_identity)) {
            scheduled_speech_fragment(source, fragment_start, index - fragment_start, destination,
                                      &used);
            scheduled_speech_node(link_identity, destination, &used);
            index += strlen(link_identity);
            fragment_start = index;
            continue;
        }
        if (*callsign && scheduled_speech_identity_at(source, index, callsign)) {
            scheduled_speech_fragment(source, fragment_start, index - fragment_start, destination,
                                      &used);
            scheduled_speech_callsign(callsign, destination, &used);
            index += strlen(callsign);
            fragment_start = index;
            continue;
        }
        ++index;
    }
    scheduled_speech_fragment(source, fragment_start, index - fragment_start, destination, &used);
}

/** @brief Format a scheduler-validated local date as a fixed ISO calendar string.
 * @param local Valid local civil time.
 * @param date Fixed output buffer receiving `YYYY-MM-DD`.
 */
static void scheduled_date_text(const struct tm *local, char date[16]) {
    unsigned int value = (unsigned int)(local->tm_year + 1900);
    date[0] = (char)('0' + value / 1000);
    date[1] = (char)('0' + value / 100 % 10);
    date[2] = (char)('0' + value / 10 % 10);
    date[3] = (char)('0' + value % 10);
    date[4] = '-';
    date[5] = (char)('0' + (local->tm_mon + 1) / 10);
    date[6] = (char)('0' + (local->tm_mon + 1) % 10);
    date[7] = '-';
    date[8] = (char)('0' + local->tm_mday / 10);
    date[9] = (char)('0' + local->tm_mday % 10);
    date[10] = '\0';
}

/** @brief Format an already validated local time using one configured clock format.
 * @param hour Local hour from zero through 23.
 * @param minute Local minute from zero through 59.
 * @param format Configured 12- or 24-hour format.
 * @param time_text Fixed output buffer receiving the local clock text.
 */
static void scheduled_time_text(int hour, int minute, uint64_t format, char time_text[16]) {
    if (format == 24) {
        time_text[0] = (char)('0' + hour / 10);
        time_text[1] = (char)('0' + hour % 10);
        time_text[2] = ':';
        time_text[3] = (char)('0' + minute / 10);
        time_text[4] = (char)('0' + minute % 10);
        time_text[5] = '\0';
        return;
    }
    /* The settings resolver admits only 12 or 24; retain a useful 12-hour fallback defensively. */
    int display_hour = hour % 12;
    if (!display_hour) {
        display_hour = 12;
    }
    size_t index = 0;
    if (display_hour >= 10) {
        time_text[index++] = (char)('0' + display_hour / 10);
    }
    time_text[index++] = (char)('0' + display_hour % 10);
    time_text[index++] = ':';
    time_text[index++] = (char)('0' + minute / 10);
    time_text[index++] = (char)('0' + minute % 10);
    time_text[index++] = ' ';
    time_text[index++] = hour < 12 ? 'A' : 'P';
    time_text[index++] = 'M';
    time_text[index] = '\0';
}

/** @brief Format the fixed time-dependent values available to one due scheduled event.
 * @param node Active event owner.
 * @param local Current validated local civil time.
 * @param values Receives pointers to the bounded generated values.
 * @param day_of_week Receives the local weekday name.
 * @param date Receives the ISO local date.
 * @param time_text Receives the configured local clock text.
 * @param link_status Receives the current bounded direct-peer summary.
 * @param link_identity Receives the direct peer that appears in @p link_status, if any.
 * @return True when every value is valid and complete.
 */
static bool scheduled_template_values(struct ra_runtime_node *node, const struct tm *local,
                                      struct ra_message_template_values *values,
                                      char day_of_week[16], char date[16], char time_text[16],
                                      char link_status[RA_CONTROLLER_STATUS_TEXT_MAX],
                                      char link_identity[RA_LINK_PEER_NAME_MAX]) {
    static const char *const weekdays[] = {"Sunday",   "Monday", "Tuesday", "Wednesday",
                                           "Thursday", "Friday", "Saturday"};
    scheduled_date_text(local, date);
    ast_copy_string(day_of_week, weekdays[local->tm_wday], 16);
    scheduled_time_text(local->tm_hour, local->tm_min, node->time_settings.format, time_text);
    if (!node_link_status_text(node, false, link_status, link_identity)) {
        return false;
    }
    *values = (struct ra_message_template_values){
        .day_of_week = day_of_week,
        .date = date,
        .time = time_text,
        .greeting = scheduled_greeting(local->tm_hour),
        .link_status = link_status,
        .node = node->name,
        .callsign = node->settings.callsign,
    };
    return true;
}

/** @brief Resolve and render an event's direct message or same-label inherited template.
 * @param schedule Active schedule retaining the immutable configuration document.
 * @param event Event selected for dispatch.
 * @param local Current local civil time used by substitutions.
 * @param dispatch Copied output receiving optional telemetry strings.
 * @return True when no message is requested or the complete message renders.
 */
static bool scheduled_message(const struct ra_runtime_schedule *schedule,
                              const struct ra_runtime_schedule_event *event, const struct tm *local,
                              struct ra_scheduled_dispatch *dispatch) {
    const char *template_text = event->settings.message;
    if (!*template_text) {
        if (!*event->settings.template_name) {
            /* A macro-only event deliberately reserves no telemetry message. */
            return true;
        }
        const char *set = ra_document_template_named(schedule->document, event->node->name,
                                                     event->settings.template_name);
        struct ra_template_settings template_settings;
        if (!set ||
            ra_template_settings_resolve(schedule->document->entries, schedule->document->count,
                                         event->node->name, set, &template_settings)) {
            return false;
        }
        template_text = template_settings.text;
    }
    char day_of_week[16];
    char date[16];
    char time_text[16];
    char link_status[RA_CONTROLLER_STATUS_TEXT_MAX];
    char link_identity[RA_LINK_PEER_NAME_MAX];
    struct ra_message_template_values values;
    char rendered[RA_MESSAGE_TEMPLATE_OUTPUT_MAX];
    if (!scheduled_template_values(event->node, local, &values, day_of_week, date, time_text,
                                   link_status, link_identity) ||
        !ra_message_template_render(template_text, &values, rendered, sizeof(rendered))) {
        return false;
    }
    /* An optional empty callsign can legitimately render an otherwise-empty template. */
    if (!*rendered) {
        return true;
    }
    if (!scheduled_morse_text(rendered, dispatch->morse)) {
        /* A status without either speech-independent fallback or Morse would only key silently
         * when the synthesizer is unavailable. */
        return true;
    }
    scheduled_speech_text(rendered, event->node->name, event->node->settings.callsign,
                          link_identity, dispatch->speech);
    dispatch->has_message = true;
    return true;
}

/** @brief Resolve an approved named macro into the existing app-facing link operation type.
 * @param schedule Active schedule retaining the immutable configuration document.
 * @param event Event selected for dispatch.
 * @param dispatch Copied output receiving an optional validated operation.
 * @return True when no macro is requested or its complete operation is copied.
 */
static bool scheduled_operation(const struct ra_runtime_schedule *schedule,
                                const struct ra_runtime_schedule_event *event,
                                struct ra_scheduled_dispatch *dispatch) {
    if (!*event->settings.macro_name) {
        return true;
    }
    const char *set =
        ra_document_macro_named(schedule->document, event->node->name, event->settings.macro_name);
    struct ra_macro_settings macro;
    if (!set || ra_macro_settings_resolve(schedule->document->entries, schedule->document->count,
                                          event->node->name, set, &macro)) {
        return false;
    }
    static const enum ra_link_action actions[] = {
        [RA_SCHEDULED_ACTION_CONNECT] = RA_LINK_TRANSCEIVE,
        [RA_SCHEDULED_ACTION_DISCONNECT] = RA_LINK_DISCONNECT,
        [RA_SCHEDULED_ACTION_DISCONNECT_ALL] = RA_LINK_DISCONNECT_ALL,
        [RA_SCHEDULED_ACTION_RECONNECT_ALL] = RA_LINK_RECONNECT_ALL,
    };
    _Static_assert(sizeof(actions) / sizeof(*actions) == RA_SCHEDULED_ACTION_RECONNECT_ALL + 1,
                   "every validated scheduled action needs one link operation");
    dispatch->operation.action = actions[macro.action];
    /* The resolver validates the target against RA_NODE_NAME_MAX before every copied dispatch. */
    ast_copy_string(dispatch->operation.remote, macro.target_node,
                    sizeof(dispatch->operation.remote));
    dispatch->has_operation = true;
    return true;
}

/** @brief Validate a copied dispatch against the one reserved runtime event that created it.
 * @param runtime Active runtime owning the current schedule.
 * @param dispatch Copied candidate dispatch.
 * @return Reserved event only when its current generation and occurrence still match.
 */
static struct ra_runtime_schedule_event *
scheduled_dispatch_event(struct ra_runtime *runtime, const struct ra_scheduled_dispatch *dispatch) {
    if (!runtime || !runtime->schedule || !dispatch ||
        dispatch->generation != runtime->schedule->generation ||
        dispatch->event_index >= runtime->schedule->count) {
        return NULL;
    }
    struct ra_runtime_schedule_event *event = &runtime->schedule->events[dispatch->event_index];
    if (!event->pending || event->pending_occurrence != dispatch->occurrence ||
        strcmp(event->node->name, dispatch->local)) {
        return NULL;
    }
    return event;
}

/** @brief Fill one copied dispatch from an already due event without retaining configuration
 * pointers.
 * @param schedule Active schedule owning the selected event.
 * @param index Configuration-order event index.
 * @param local Current local civil time used by substitutions.
 * @param occurrence Due civil calendar-minute key.
 * @param dispatch Receives completely copied control-plane data.
 * @return True when all optional message and macro data could be copied.
 */
static bool scheduled_dispatch_fill(struct ra_runtime_schedule *schedule, size_t index,
                                    const struct tm *local, uint64_t occurrence,
                                    struct ra_scheduled_dispatch *dispatch) {
    struct ra_runtime_schedule_event *event = &schedule->events[index];
    *dispatch = (struct ra_scheduled_dispatch){
        .generation = schedule->generation, .event_index = index, .occurrence = occurrence};
    ast_copy_string(dispatch->local, event->node->name, sizeof(dispatch->local));
    if (!scheduled_message(schedule, event, local, dispatch)) {
        return false;
    }
    return scheduled_operation(schedule, event, dispatch);
}

/** @brief Convert one captured wall-clock instant to a validated local civil time.
 * @param now Captured wall-clock instant.
 * @param local Receives the host-local civil time.
 * @return True when the result has usable calendar and clock fields.
 *
 * Both zero-time events and configured replacement windows use this one conversion so their
 * local-time and daylight-saving behavior cannot drift apart.
 */
static bool schedule_local_time(time_t now, struct tm *local) {
    if (now == (time_t)-1) {
        return false;
    }
    struct timeval when = {.tv_sec = now, .tv_usec = 0};
    struct ast_tm ast_time = {0};
    if (!ast_localtime(&when, &ast_time, NULL)) {
        return false;
    }
    *local = (struct tm){.tm_sec = ast_time.tm_sec,
                         .tm_min = ast_time.tm_min,
                         .tm_hour = ast_time.tm_hour,
                         .tm_mday = ast_time.tm_mday,
                         .tm_mon = ast_time.tm_mon,
                         .tm_year = ast_time.tm_year,
                         .tm_wday = ast_time.tm_wday,
                         .tm_yday = ast_time.tm_yday,
                         .tm_isdst = ast_time.tm_isdst};
    return local->tm_wday >= 0 && local->tm_wday <= 6 && local->tm_year >= -1900 &&
           local->tm_year <= 8099 && local->tm_hour >= 0 && local->tm_hour < 24 &&
           local->tm_min >= 0 && local->tm_min < 60 && local->tm_mon >= 0 && local->tm_mon < 12 &&
           local->tm_mday > 0 && local->tm_mday <= 31;
}

/** @brief Test whether a current civil date selects a configured window regardless of its time.
 * @param window Valid same-day configured window.
 * @param local Valid current local civil time.
 * @return True when the date selector accepts @p local's date.
 */
static bool schedule_window_date_matches(const struct ra_scheduled_window *window,
                                         const struct tm *local) {
    struct tm inside = *local;
    unsigned int minute = window->end_minute - 1U;
    inside.tm_hour = (int)(minute / 60U);
    inside.tm_min = (int)(minute % 60U);
    return ra_scheduled_window_matches(window, &inside);
}

/** @brief Recompute configured-link intent from the current local window and receive activity.
 * @param runtime Active runtime whose nodes own the published receive timestamps.
 * @param schedule Active schedule to update under the runtime control lock.
 * @param local Valid current local civil time.
 * @param now_ms Monotonic control timestamp used only for the inactivity deadline.
 */
static void schedule_link_desires(struct ra_runtime *runtime, struct ra_runtime_schedule *schedule,
                                  const struct tm *local, uint64_t now_ms) {
    schedule_capture_activity(runtime, schedule);
    for (size_t index = 0; index < schedule->link_count; ++index) {
        struct ra_runtime_configured_link *link = &schedule->links[index];
        link->desired = link->permanent;
    }
    for (size_t index = 0; index < schedule->window_count; ++index) {
        struct ra_runtime_link_window *window = &schedule->windows[index];
        bool active = ra_scheduled_window_matches(&window->window, local);
        if (!window->initialized) {
            window->initialized = true;
            unsigned int minute = (unsigned int)local->tm_hour * 60U + (unsigned int)local->tm_min;
            if (!active && window->end_inactivity_ms && minute >= window->window.end_minute &&
                schedule_window_date_matches(&window->window, local)) {
                if (window->last_activity_ms) {
                    /* Receive observed before this first control tick gets a full quiet period,
                     * not the shorter wall-clock grace intended only for a true cold start. */
                    window->waiting_for_idle = true;
                } else {
                    uint64_t elapsed_ms = ((uint64_t)(minute - window->window.end_minute) * 60U +
                                           (uint64_t)local->tm_sec) *
                                          1000U;
                    if (elapsed_ms < window->end_inactivity_ms) {
                        uint64_t remaining_ms = window->end_inactivity_ms - elapsed_ms;
                        window->waiting_for_idle = true;
                        window->initial_grace = true;
                        window->initial_deadline_ms =
                            UINT64_MAX - now_ms < remaining_ms ? UINT64_MAX : now_ms + remaining_ms;
                    }
                }
            }
        }
        if (active) {
            window->was_active = true;
            window->waiting_for_idle = false;
            window->initial_grace = false;
        } else if (window->was_active) {
            window->was_active = false;
            window->waiting_for_idle = true;
        }
        bool requested = active;
        if (window->waiting_for_idle) {
            uint64_t activity = window->last_activity_ms;
            bool quiet =
                window->initial_grace
                    ? now_ms >= window->initial_deadline_ms
                    : !window->end_inactivity_ms || !activity ||
                          (now_ms >= activity && now_ms - activity >= window->end_inactivity_ms);
            if (quiet) {
                window->waiting_for_idle = false;
                window->initial_grace = false;
            } else {
                requested = true;
            }
        }
        if (requested) {
            schedule->links[window->route_index].desired = true;
            schedule->links[window->replaced_index].desired = false;
        }
    }
}

/** @brief Reconcile scheduler-issued state with exact permanent hub ownership.
 * @param runtime Active runtime whose hubs expose direct-route ownership.
 * @param schedule Current schedule whose issued records may need reattachment.
 *
 * A hub normally turns a failed permanent port into a retry immediately. If that allocation
 * fails, no direct route remains. Clearing only that stale issued bit lets the next scheduler
 * tick retry the configured route without treating an advertised transitive topology as a peer.
 */
static void schedule_refresh_link_ownership(struct ra_runtime *runtime,
                                            struct ra_runtime_schedule *schedule) {
    for (size_t index = 0; index < schedule->link_count; ++index) {
        struct ra_runtime_configured_link *link = &schedule->links[index];
        struct ra_runtime_node *node = runtime_node(runtime, link->local);
        if (link->issued && !ra_link_hub_has_permanent_route(&node->links, link->remote)) {
            link->issued = false;
        }
    }
}

/** @brief Copy one exact configured-link reservation for unlocked control work.
 * @param schedule Active schedule that owns @p index.
 * @param index Configuration-route index selected by current policy.
 * @param action Permanent attach or permanent-detach transition.
 * @param operation Caller-owned output cleared by the scheduler before selection.
 *
 * The nonce changes on every reservation, including after an operator clears a pending request.
 * A late dial therefore cannot become valid merely because the same endpoint is selected again.
 */
static void reserve_scheduled_link_operation(struct ra_runtime_schedule *schedule, size_t index,
                                             enum ra_link_action action,
                                             struct ra_scheduled_link_operation *operation) {
    struct ra_runtime_configured_link *link = &schedule->links[index];
    schedule->next_link_reservation = schedule_next_generation(schedule->next_link_reservation);
    link->pending = true;
    link->reservation = schedule->next_link_reservation;
    operation->schedule_generation = schedule->generation;
    operation->link_index = index;
    operation->reservation = link->reservation;
    ast_copy_string(operation->local, link->local, sizeof(operation->local));
    operation->operation.action = action;
    ast_copy_string(operation->operation.remote, link->remote, sizeof(operation->operation.remote));
}

int ra_runtime_next_scheduled_link_operation(struct ra_runtime *runtime, time_t now,
                                             uint64_t now_ms,
                                             struct ra_scheduled_link_operation *operation) {
    if (!runtime) {
        return -1;
    }
    if (!operation) {
        return -1;
    }
    *operation = (struct ra_scheduled_link_operation){0};
    struct ra_runtime_schedule *schedule = runtime->schedule;
    if (!schedule) {
        return 0;
    }
    if (!schedule->link_count) {
        return 0;
    }
    struct tm local;
    if (!schedule_local_time(now, &local)) {
        return -1;
    }
    schedule_link_desires(runtime, schedule, &local, now_ms);
    schedule_refresh_link_ownership(runtime, schedule);
    /* Always withdraw routes first. A replacement cannot briefly overlap the permanent peer it
     * suppresses, which prevents a configuration-driven topology loop during handoff. */
    for (size_t index = 0; index < schedule->link_count; ++index) {
        const struct ra_runtime_configured_link *link = &schedule->links[index];
        const struct ra_runtime_node *node = runtime_node(runtime, link->local);
        /* Reload reconciliation withdraws retiring routes before publishing this schedule, so an
         * operator hold suppresses every remaining scheduler-owned route. */
        if (node->configured_links_paused || link->pending) {
            continue;
        }
        if (link->issued && !link->desired) {
            reserve_scheduled_link_operation(schedule, index, RA_LINK_DISCONNECT_PERMANENT,
                                             operation);
            return 1;
        }
    }
    for (size_t index = 0; index < schedule->link_count; ++index) {
        const struct ra_runtime_configured_link *link = &schedule->links[index];
        const struct ra_runtime_node *node = runtime_node(runtime, link->local);
        if (node->configured_links_paused || link->pending) {
            continue;
        }
        if (!link->issued && link->desired) {
            reserve_scheduled_link_operation(schedule, index, RA_LINK_PERMANENT_TRANSCEIVE,
                                             operation);
            return 1;
        }
    }
    return 0;
}

int ra_runtime_next_scheduled_dispatch(struct ra_runtime *runtime, time_t now,
                                       struct ra_scheduled_dispatch *dispatch) {
    if (!runtime || !dispatch) {
        return -1;
    }
    *dispatch = (struct ra_scheduled_dispatch){0};
    struct ra_runtime_schedule *schedule = runtime->schedule;
    if (!schedule) {
        return 0;
    }
    struct tm local;
    if (!schedule_local_time(now, &local)) {
        return -1;
    }
    /* A ready occurrence retains this civil clock for delayed config-order dispatch. The app
     * bridge queues one task each second in FIFO order; an older task observed after a newer task
     * may still drain reserved work, but must not create an occurrence from the past after the
     * scheduler has advanced. */
    if (!schedule->has_last_tick || now >= schedule->last_tick) {
        schedule->last_tick = now;
        schedule->has_last_tick = true;
        /* Snapshot every due event before returning the first.  A slow macro therefore cannot
         * age later same-minute events out of their local-time trigger. */
        for (size_t index = 0; index < schedule->count; ++index) {
            struct ra_runtime_schedule_event *event = &schedule->events[index];
            uint64_t occurrence;
            if (event->pending || event->ready ||
                !ra_scheduled_event_due(&event->settings.trigger, &local, &occurrence) ||
                event->completed_occurrence == occurrence) {
                continue;
            }
            event->ready = true;
            event->ready_occurrence = occurrence;
            event->ready_local = local;
        }
    }
    size_t selected = SIZE_MAX;
    uint64_t selected_occurrence = 0;
    for (size_t index = 0; index < schedule->count; ++index) {
        const struct ra_runtime_schedule_event *event = &schedule->events[index];
        if (!event->pending && !event->ready) {
            continue;
        }
        uint64_t occurrence = event->pending ? event->pending_occurrence : event->ready_occurrence;
        /* The first equal occurrence retains configuration order.  An earlier uncompleted minute
         * always wins over a later minute, even when the later event appears first in config. */
        if (selected == SIZE_MAX || occurrence < selected_occurrence) {
            selected = index;
            selected_occurrence = occurrence;
        }
    }
    if (selected != SIZE_MAX) {
        struct ra_runtime_schedule_event *event = &schedule->events[selected];
        bool pending = event->pending;
        uint64_t occurrence = pending ? event->pending_occurrence : event->ready_occurrence;
        const struct tm *event_local = pending ? &event->pending_local : &event->ready_local;
        if (!scheduled_dispatch_fill(schedule, selected, event_local, occurrence, dispatch)) {
            /* Configuration was validated before startup; do not let an impossible render stall
             * every subsequent event forever when a runtime bound is exceeded. */
            event->pending = false;
            event->message_queued = false;
            event->ready = false;
            event->completed_occurrence = occurrence;
            return -1;
        }
        if (!pending) {
            event->pending = true;
            event->pending_occurrence = occurrence;
            event->pending_local = *event_local;
            event->message_queued = false;
            event->ready = false;
        }
        return 1;
    }
    return 0;
}

int ra_runtime_queue_scheduled_message(struct ra_runtime *runtime,
                                       const struct ra_scheduled_dispatch *dispatch) {
    struct ra_runtime_schedule_event *event = scheduled_dispatch_event(runtime, dispatch);
    if (!event) {
        return -1;
    }
    if (!dispatch->has_message) {
        return 0;
    }
    if (event->message_queued) {
        return 0;
    }
    if (queue_status(event->node, dispatch->speech, dispatch->morse)) {
        return -1;
    }
    event->message_queued = true;
    return 0;
}

bool ra_runtime_complete_scheduled_dispatch(struct ra_runtime *runtime,
                                            const struct ra_scheduled_dispatch *dispatch) {
    struct ra_runtime_schedule_event *event = scheduled_dispatch_event(runtime, dispatch);
    if (!event) {
        return false;
    }
    if (dispatch->has_message && !event->message_queued) {
        return false;
    }
    event->completed_occurrence = event->pending_occurrence;
    event->pending = false;
    event->message_queued = false;
    return true;
}

/** @brief Append one complete word to a bounded telemetry report.
 * @param text Existing null-terminated report with fixed maximum capacity.
 * @param word Null-terminated word that must fit completely.
 * @return True when the complete word was copied without truncation.
 */
static bool status_text_append(char text[RA_CONTROLLER_STATUS_TEXT_MAX], const char *word) {
    size_t used = strlen(text);
    size_t available = RA_CONTROLLER_STATUS_TEXT_MAX - used;
    if (strnlen(word, available) == available) {
        return false;
    }
    ast_copy_string(text + used, word, available);
    return true;
}

/** @brief Build bounded link-lifecycle speech from one to four known-safe words.
 * @param text Empty bounded destination.
 * @param first First word to append.
 * @param second Second word to append.
 * @param third Optional third word, or null for a two-word report.
 * @param fourth Optional final word, or null for a shorter report.
 * @return True only when every word fits with a separating space.
 */
static bool status_text_words(char text[RA_CONTROLLER_STATUS_TEXT_MAX], const char *first,
                              const char *second, const char *third, const char *fourth) {
    const char *const words[] = {first, second, third, fourth};
    text[0] = '\0';
    for (size_t index = 0; index < sizeof(words) / sizeof(*words) && words[index]; ++index) {
        if (index && !status_text_append(text, " ")) {
            return false;
        }
        if (!status_text_append(text, words[index])) {
            return false;
        }
    }
    return true;
}

/** @brief Format one node's perspective on a changed direct link.
 * @param local Configured local node identity.
 * @param first One changed-link endpoint.
 * @param second Other changed-link endpoint.
 * @param connected True selects the connected wording, false disconnected wording.
 * @param text Bounded destination for the speech and Morse-safe report.
 * @return True only when the complete report fits.
 */
static bool link_event_text(const char *local, const char *first, const char *second,
                            bool connected, char text[RA_CONTROLLER_STATUS_TEXT_MAX]) {
    const char *verb = connected ? "CONNECTED" : "DISCONNECTED";
    if (!strcmp(local, first)) {
        return status_text_words(text, second, verb, NULL, NULL);
    } else if (!strcmp(local, second)) {
        return status_text_words(text, first, verb, NULL, NULL);
    }
    return status_text_words(text, first, verb, connected ? "TO" : "FROM", second);
}

int ra_runtime_queue_link_event(struct ra_runtime *runtime, const char *first, const char *second,
                                bool connected) {
    int result = 0;
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        char text[RA_CONTROLLER_STATUS_TEXT_MAX];
        if (!link_event_text(node->name, first, second, connected, text) ||
            queue_status_speech(node, text, first, second)) {
            result = -1;
        }
    }
    return result;
}

bool ra_runtime_link_snapshot(struct ra_runtime *runtime, const char *local,
                              struct ra_link_peer_status *entries, size_t capacity, size_t *count) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            size_t found = ra_link_hub_snapshot(&node->links, entries, capacity);
            if (count) {
                *count = found;
            }
            return true;
        }
    }
    return false;
}

char *ra_runtime_link_topology(struct ra_runtime *runtime, const char *local) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            return ra_link_hub_topology(&node->links);
        }
    }
    return NULL;
}

int ra_runtime_remote_command(struct ra_runtime *runtime, const char *local, const char *remote,
                              char digit) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (strcmp(node->name, local)) {
            continue;
        }
        /* Keep a selected peer from retaining command authority after a policy reload. */
        if (!ra_link_access_allowed(node->settings.link_allow_nodes, node->settings.link_deny_nodes,
                                    remote, true)) {
            node->remote_node[0] = '\0';
            return -1;
        }
        if (!digit) {
            /* Selection also proves the peer's current directory identity before forwarding. */
            char *verified = resolve_link_node(node, remote, NULL);
            if (verified && ra_link_hub_connected(&node->links, remote)) {
                ast_free(verified);
                ast_copy_string(node->remote_node, remote, sizeof(node->remote_node));
                return 0;
            }
            ast_free(verified);
            return -1;
        }
        if (!ra_link_hub_send_digit(&node->links, remote, digit)) {
            return 0;
        }
        node->remote_node[0] = '\0';
        return -1;
    }
    return -1;
}

bool ra_runtime_authorize(struct ra_runtime *runtime, const char *local, const char *remote,
                          const char *peer_ip) {
    for (struct ra_runtime_node *node = runtime->nodes; node; node = node->next) {
        if (!strcmp(node->name, local)) {
            char *verified = resolve_link_node(node, remote, peer_ip);
            bool allowed =
                ra_link_access_allowed(node->settings.link_allow_nodes,
                                       node->settings.link_deny_nodes, remote, verified != NULL);
            ast_free(verified);
            return allowed;
        }
    }
    return false;
}
