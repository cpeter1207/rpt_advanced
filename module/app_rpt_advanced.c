/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Asterisk module lifecycle and validated configuration ownership.
 */
#include <asterisk.h>

#include "document.h"
#include "link_directory.h"
#include "runtime.h"
#include "schema.h"
#include <asterisk/buildopts.h>
#include <asterisk/channel.h>
#include <asterisk/cli.h>
#include <asterisk/logger.h>
#include <asterisk/module.h>
#include <asterisk/paths.h>
#include <asterisk/pbx.h>
#include <asterisk/taskprocessor.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>

/** @brief Configuration owned by the loaded module. */
static struct ra_document configuration;
/** @brief Radio resources whose strings belong to configuration. */
static void submit_digit(const char *node, char digit, uint64_t now_ms);
/** @brief Radio runtime delivers only decoded events to the control queue. */
static struct ra_runtime runtime = {.digit = submit_digit};
/** @brief Serialize admission and runtime replacement; never acquired by audio workers. */
AST_MUTEX_DEFINE_STATIC(runtime_lock);
/** @brief Descriptor is populated by Asterisk before application registration. */
static struct ast_module_info descriptor;
/** @brief Invalidates calls prepared before runtime replacement. Protected by runtime_lock. */
static atomic_uint_fast64_t runtime_revision;
/** @brief Serial control executor, retained until radio workers stop and tasks drain. */
static struct ast_taskprocessor *control_queue;
/** @brief Bounded number of submitted digit tasks. */
static atomic_uint pending_digits;
/** @brief Invalidates partial commands when any digit cannot be delivered. */
static atomic_uint_fast64_t lost_digits;
/** @brief Last reset applied to node collectors, protected by runtime_lock. */
static uint64_t applied_loss;
/** @brief One independently owned digit event. */
struct digit_task {
    uint64_t revision; /**< Runtime instance that produced the event. */
    uint64_t loss;     /**< Delivery-loss generation at enqueue time. */
    uint64_t now_ms;   /**< Monotonic detection timestamp. */
    char digit;        /**< Decoded digit, or zero for timeout. */
    char node[];       /**< Owned local node name. */
};

/** @brief Dial without preventing same-server incoming admission or configuration reload.
 * @param local Local node name.
 * @param remote Remote node name.
 * @param transmit Send program audio to the peer.
 * @param forward Relay peer audio to other links.
 * @param permanent Reconnect automatically after an unexpected transport failure.
 * @param expected Required runtime revision, or zero for a fresh administrative command.
 * @return Zero on connection, minus one on failure or intervening reload.
 */
static int connect_link(const char *local, const char *remote, bool transmit, bool forward,
                        bool permanent, uint64_t expected) {
    struct ra_link_dial dial = {0};
    ast_mutex_lock(&runtime_lock);
    uint64_t revision = atomic_load(&runtime_revision);
    int result = expected && expected != revision
                     ? -1
                     : ra_runtime_prepare_link(&runtime, local, remote, &dial);
    ast_mutex_unlock(&runtime_lock);
    if (result) {
        return -1;
    }
    struct ast_channel *channel = ra_link_dial_run(&dial, local);
    if (!channel) {
        return -1;
    }
    ast_mutex_lock(&runtime_lock);
    result =
        revision == atomic_load(&runtime_revision)
            ? ra_runtime_attach_link(&runtime, local, remote, channel, transmit, forward, permanent)
            : -1;
    ast_mutex_unlock(&runtime_lock);
    if (result) {
        ast_hangup(channel);
    }
    return result;
}

/** @brief Execute a collected operation outside the hardware worker.
 * @param local Local node name retained by the caller.
 * @param operation Parsed linking action.
 * @param revision Runtime that collected the command.
 * @return Zero on completion, minus one on failure.
 */
static int execute_link(const char *local, const struct ra_link_operation *operation,
                        uint64_t revision) {
    switch (operation->action) {
    case RA_LINK_TRANSCEIVE:
    case RA_LINK_MONITOR:
    case RA_LINK_LOCAL_MONITOR:
        return connect_link(local, operation->remote, operation->action == RA_LINK_TRANSCEIVE,
                            operation->action != RA_LINK_LOCAL_MONITOR, false, revision);
    case RA_LINK_PERMANENT_TRANSCEIVE:
    case RA_LINK_PERMANENT_MONITOR:
    case RA_LINK_PERMANENT_LOCAL_MONITOR:
        return connect_link(local, operation->remote,
                            operation->action == RA_LINK_PERMANENT_TRANSCEIVE,
                            operation->action != RA_LINK_PERMANENT_LOCAL_MONITOR, true, revision);
    case RA_LINK_DISCONNECT: {
        ast_mutex_lock(&runtime_lock);
        int result = revision == atomic_load(&runtime_revision)
                         ? !ra_runtime_disconnect(&runtime, local, operation->remote)
                         : -1;
        ast_mutex_unlock(&runtime_lock);
        return result;
    }
    case RA_LINK_DISCONNECT_ALL: {
        ast_mutex_lock(&runtime_lock);
        int result = revision == atomic_load(&runtime_revision)
                         ? (int)ra_runtime_disconnect_all(&runtime, local) >= 0
                         : -1;
        ast_mutex_unlock(&runtime_lock);
        return result == -1 ? -1 : 0;
    }
    case RA_LINK_STATUS:
    case RA_LINK_FULL_STATUS:
    case RA_LINK_LAST_KEYED: {
        ast_mutex_lock(&runtime_lock);
        bool current = revision == atomic_load(&runtime_revision);
        size_t count = current ? ra_runtime_link_count(&runtime, local) : 0;
        ast_mutex_unlock(&runtime_lock);
        if (!current) {
            return -1;
        }
        ast_log(LOG_NOTICE, "rpt_advanced: node %s has %zu active links\n", local, count);
        return 0;
    }
    case RA_LINK_DISCONNECT_PERMANENT:
        ast_mutex_lock(&runtime_lock);
        int result = revision == atomic_load(&runtime_revision)
                         ? !ra_runtime_disconnect(&runtime, local, operation->remote)
                         : -1;
        ast_mutex_unlock(&runtime_lock);
        return result;
    case RA_LINK_RECONNECT_ALL:
        /* Permanent peers reconnect themselves after transport failure. */
        return 0;
    case RA_LINK_COMMAND:
        ast_log(LOG_NOTICE, "rpt_advanced: remote command forwarding is not available\n");
        return -1;
    default:
        return -1;
    }
}

/** @brief Interpret a queued digit only while its originating runtime is current.
 * @param argument Owned digit task.
 * @return Zero after freeing task storage.
 */
static int process_digit(void *argument) {
    struct digit_task *task = argument;
    struct ra_link_operation operation;
    bool ready = false;
    ast_mutex_lock(&runtime_lock);
    if (task->revision == atomic_load(&runtime_revision) &&
        task->loss == atomic_load(&lost_digits)) {
        if (applied_loss != task->loss) {
            ra_runtime_reset_digits(&runtime);
            applied_loss = task->loss;
        }
        ready = ra_runtime_digit(&runtime, task->node, task->digit, task->now_ms, &operation);
    }
    ast_mutex_unlock(&runtime_lock);
    if (ready) {
        int result = execute_link(task->node, &operation, task->revision);
        ast_log(LOG_NOTICE, "rpt_advanced: node %s link command %s\n", task->node,
                result ? "failed" : "completed");
    }
    ast_free(task);
    atomic_fetch_sub(&pending_digits, 1);
    return 0;
}

/** @brief Enqueue a decoded event without performing any network or command work.
 * @param node Borrowed node name.
 * @param digit Completed digit or timeout marker.
 * @param now_ms Detection time.
 */
static void submit_digit(const char *node, char digit, uint64_t now_ms) {
    unsigned int pending = atomic_fetch_add(&pending_digits, 1);
    struct digit_task *task =
        pending < 256 ? ast_calloc(1, sizeof(*task) + strlen(node) + 1) : NULL;
    if (task) {
        task->revision = atomic_load(&runtime_revision);
        task->loss = atomic_load(&lost_digits);
        task->now_ms = now_ms;
        task->digit = digit;
        for (size_t i = 0; node[i] != '\0'; ++i) {
            task->node[i] = node[i];
        }
        task->node[strlen(node)] = '\0';
        if (!ast_taskprocessor_push(control_queue, process_digit, task)) {
            return;
        }
        ast_free(task);
    }
    atomic_fetch_add(&lost_digits, 1);
    atomic_fetch_sub(&pending_digits, 1);
    ast_log(LOG_WARNING, "rpt_advanced: dropped DTMF event; partial commands invalidated\n");
}

/** @brief Control peer connections from Asterisk's CLI without blocking the audio worker.
 * @param entry CLI registration metadata.
 * @param command Asterisk initialization, completion, or execution request.
 * @param arguments Parsed CLI arguments.
 * @return Asterisk CLI status or null for completion without candidates.
 */
static char *link_cli(struct ast_cli_entry *entry, int command, struct ast_cli_args *arguments) {
    if (command == CLI_INIT) {
        entry->command = "rpt_advanced link";
        entry->usage = "Usage: rpt_advanced link {connect|monitor|local-monitor|disconnect} "
                       "<local-node> <remote-node>\n";
        return NULL;
    }
    if (command == CLI_GENERATE) {
        return NULL;
    }
    if (arguments->argc != 5) {
        return CLI_SHOWUSAGE;
    }
    const char *operation = arguments->argv[2];
    bool disconnect = !strcmp(operation, "disconnect");
    bool transmit = !strcmp(operation, "connect");
    bool local_monitor = !strcmp(operation, "local-monitor");
    if (!disconnect && !transmit && !local_monitor && strcmp(operation, "monitor")) {
        return CLI_SHOWUSAGE;
    }
    int result;
    if (disconnect) {
        ast_mutex_lock(&runtime_lock);
        result = !ra_runtime_disconnect(&runtime, arguments->argv[3], arguments->argv[4]);
        ast_mutex_unlock(&runtime_lock);
    } else {
        result = connect_link(arguments->argv[3], arguments->argv[4], transmit, !local_monitor,
                              false, 0);
    }
    ast_cli(arguments->fd, "rpt_advanced: link %s %s\n", operation,
            result ? "failed" : "completed");
    return result ? CLI_FAILURE : CLI_SUCCESS;
}

/** @brief Administrative linking command; radio DTMF collection is a separate input path. */
static struct ast_cli_entry commands[] = {AST_CLI_DEFINE(link_cli, "Control rpt_advanced links")};

/** @brief Validate and transfer an incoming IAX channel out of its dialplan thread.
 * @param channel Incoming channel owned by the dialplan.
 * @param data Destination node name.
 * @return Zero after successful handoff, minus one on rejection.
 */
static int incoming_link(struct ast_channel *channel, const char *data) {
    if (!data || strcmp(ast_channel_tech(channel)->type, "IAX2")) {
        return -1;
    }
    struct ast_party_number *number = &ast_channel_caller(channel)->id.number;
    if (!number->valid || !number->str) {
        return -1;
    }
    char *remote = ast_strdup(number->str);
    if (!remote) {
        return -1;
    }
    char address[256];
    if (ast_func_read(channel, "CHANNEL(peerip)", address, sizeof(address))) {
        ast_free(remote);
        return -1;
    }
    ast_mutex_lock(&runtime_lock);
    bool authorized = ra_runtime_authorize(&runtime, data, remote, address);
    if (!authorized) {
        ast_mutex_unlock(&runtime_lock);
        ast_free(remote);
        return -1;
    }
    struct ast_channel *owned =
        ast_channel_alloc(1, AST_STATE_DOWN, remote, NULL, NULL, NULL, NULL, NULL, channel,
                          AST_AMA_NONE, "RptAdvanced/%s-%s", remote, ast_channel_uniqueid(channel));
    if (!owned) {
        ast_mutex_unlock(&runtime_lock);
        ast_free(remote);
        return -1;
    }
    ast_channel_unlock(owned);
    int result = ast_channel_move(owned, channel);
    if (!result) {
        result = ast_answer(owned);
    }
    if (!result) {
        result = ra_runtime_accept(&runtime, data, remote, owned, true, false);
    }
    if (result) {
        ast_hangup(owned);
    }
    ast_mutex_unlock(&runtime_lock);
    ast_free(remote);
    return result ? -1 : 0;
}

/** @brief Load a complete replacement without discarding working settings on failure.
 * @return Zero on success or minus one on file, syntax, or schema errors.
 */
static int read_configuration(void) {
    char *path = NULL;
    if (ast_asprintf(&path, "%s/rpt_advanced.conf", ast_config_AST_CONFIG_DIR) < 0) {
        ast_log(LOG_ERROR, "rpt_advanced: cannot allocate configuration path\n");
        return -1;
    }
    FILE *stream = fopen(path, "r");
    if (!stream) {
        ast_log(LOG_ERROR, "rpt_advanced: cannot open %s\n", path);
        ast_free(path);
        return -1;
    }
    struct ra_document replacement = {0};
    size_t line;
    const char *error = ra_document_read(stream, &replacement, &line);
    fclose(stream);
    if (error) {
        ast_log(LOG_ERROR, "rpt_advanced: %s:%zu: %s\n", path, line, error);
        ast_free(path);
        return -1;
    }
    const char *section;
    const char *key;
    error = ra_document_validate(&replacement, &section, &key);
    if (error) {
        ast_log(LOG_ERROR, "rpt_advanced: %s [%s] %s: %s\n", path, section, key ? key : "", error);
        ra_document_destroy(&replacement);
        ast_free(path);
        return -1;
    }
    ast_free(path);
    ast_mutex_lock(&runtime_lock);
    atomic_fetch_add(&runtime_revision, 1);
    ra_runtime_stop(&runtime);
    error = ra_runtime_start(&runtime, &replacement);
    if (error) {
        ast_log(LOG_ERROR, "rpt_advanced: %s\n", error);
        ra_document_destroy(&replacement);
        error = ra_runtime_start(&runtime, &configuration);
        if (error) {
            ast_log(LOG_ERROR, "rpt_advanced: restoring previous radios failed: %s\n", error);
        }
        ast_mutex_unlock(&runtime_lock);
        return -1;
    }
    ra_document_destroy(&configuration);
    configuration = replacement;
    ast_mutex_unlock(&runtime_lock);
    return 0;
}

/** @brief Stop producers before draining the control queue, without joining under its lock. */
static void stop_runtime(void) {
    ast_mutex_lock(&runtime_lock);
    atomic_fetch_add(&runtime_revision, 1);
    ra_runtime_stop(&runtime);
    ra_document_destroy(&configuration);
    ast_mutex_unlock(&runtime_lock);
    control_queue = ast_taskprocessor_unreference(control_queue);
}

/** @brief Validate configuration when Asterisk loads the module.
 * @return Asterisk success or decline status.
 */
static int load_module(void) {
    control_queue = ast_taskprocessor_get("rpt_advanced/links", TPS_REF_DEFAULT);
    if (!control_queue) {
        return AST_MODULE_LOAD_DECLINE;
    }
    if (read_configuration()) {
        control_queue = ast_taskprocessor_unreference(control_queue);
        return AST_MODULE_LOAD_DECLINE;
    }
    if (ast_register_application2(
            "RptAdvanced", incoming_link, "Connect an AllStarLink peer",
            "RptAdvanced(node): connect an authenticated IAX peer to a configured node.",
            descriptor.self)) {
        stop_runtime();
        return AST_MODULE_LOAD_DECLINE;
    }
    if (__ast_cli_register_multiple(commands, sizeof(commands) / sizeof(*commands),
                                    descriptor.self)) {
        ast_cli_unregister_multiple(commands, sizeof(commands) / sizeof(*commands));
        ast_unregister_application("RptAdvanced");
        stop_runtime();
        return AST_MODULE_LOAD_DECLINE;
    }
    return AST_MODULE_LOAD_SUCCESS;
}

/** @brief Replace validated configuration on an Asterisk module reload.
 * @return Zero on success or minus one while retaining the previous configuration.
 */
static int reload_module(void) { return read_configuration(); }

/** @brief Release module-owned configuration.
 * @return Zero after cleanup.
 */
static int unload_module(void) {
    ast_cli_unregister_multiple(commands, sizeof(commands) / sizeof(*commands));
    ast_unregister_application("RptAdvanced");
    stop_runtime();
    return 0;
}

/** @brief Public Asterisk module descriptor; lifecycle callbacks own configuration. */
static struct ast_module_info descriptor = {
    .name = "app_rpt_advanced",
    .description = "rpt_advanced radio controller",
    .key = ASTERISK_GPL_KEY,
    .buildopt_sum = AST_BUILDOPT_SUM,
    .flags = AST_MODFLAG_LOAD_ORDER,
    .load = load_module,
    .unload = unload_module,
    .reload = reload_module,
    .load_pri = AST_MODPRI_DEFAULT,
    /* Order the configured driver first; disabled configurations need no radio. */
    .optional_modules = "chan_usbradioplus",
    .support_level = AST_MODULE_SUPPORT_EXTENDED,
};

/** @brief Register the descriptor when the shared library is opened. */
static void __attribute__((constructor)) register_module(void) { ast_module_register(&descriptor); }

/** @brief Remove the descriptor when the shared library is closed. */
static void __attribute__((destructor)) unregister_module(void) {
    ast_module_unregister(&descriptor);
}
