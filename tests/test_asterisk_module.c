/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exercise the real shared module through Asterisk's public lifecycle ABI.
 */
#include <asterisk.h>

#include "link_hub.h"
#include "runtime.h"
#include "worker.h"
#include <assert.h>
#include <asterisk/channel.h>
#include <asterisk/cli.h>
#include <asterisk/module.h>
#include <asterisk/pbx.h>
#include <asterisk/taskprocessor.h>
#include <dlfcn.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/** @brief Number of runtime starts or replacements to reject in sequence. */
static unsigned int runtime_failures;
/** @brief Captured hardware digit delivery callback. */
static ra_digit_handler digit_sink;
/** @brief Captured direct-link lifecycle callback supplied by the module runtime. */
static ra_link_event_handler event_sink;
/** @brief Inject a DTMF event while module reload deliberately suppresses producers. */
static bool emit_digit_during_reload;
/** @brief Inject one lifecycle report while reload temporarily rejects new producer events. */
static bool emit_event_during_reload;
/** @brief Number of complete runtime replacements requested by the module. */
static unsigned int runtime_reloads;
/** @brief Number of full runtime shutdowns requested by the module. */
static unsigned int runtime_stops;
/** @brief Whether the fixture still retains an active runtime after a failed replacement. */
static bool runtime_active;
/** @brief Runtime lock ownership. */
static bool runtime_locked;
/** @brief Number of current-runtime DTMF events parsed by the fixture. */
static unsigned int runtime_digit_calls;
/** @brief Lifecycle reports queued through the module's serialized control path. */
static unsigned int runtime_link_event_calls;

/** @cond TEST_FIXTURE */
/** @brief Start an initially empty runtime fixture. */
const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document) {
    digit_sink = runtime->digit;
    event_sink = runtime->event;
    (void)document;
    if (runtime_failures) {
        --runtime_failures;
        return "fixture radio unavailable";
    }
    runtime_active = true;
    return NULL;
}

/** @brief Replace the fixture runtime while preserving it after an injected startup failure. */
const char *ra_runtime_reload(struct ra_runtime *runtime, const struct ra_document *current,
                              const struct ra_document *replacement) {
    (void)current;
    (void)replacement;
    assert(runtime_locked && runtime_active);
    ++runtime_reloads;
    if (emit_digit_during_reload) {
        assert(digit_sink);
        digit_sink("usb", '1', 100);
    }
    if (emit_event_during_reload) {
        assert(event_sink);
        event_sink("usb", "123", true);
    }
    if (runtime_failures) {
        --runtime_failures;
        return "fixture radio unavailable";
    }
    digit_sink = runtime->digit;
    return NULL;
}

/** @brief Release the fixture runtime only during module teardown. */
void ra_runtime_stop(struct ra_runtime *runtime) {
    (void)runtime;
    ++runtime_stops;
    runtime_active = false;
}
/** @endcond */

/** @brief Asterisk configuration-directory symbol supplied by this test host. */
const char *ast_config_AST_CONFIG_DIR;
/** @brief Module registered by its shared-library constructor. */
static const struct ast_module_info *registered;
/** @brief Number of diagnostics observed. */
static unsigned int errors;
/** @brief Inject one configuration-path allocation failure. */
static bool fail_allocation;
/** @brief Selected admission or registration failure. */
static unsigned int link_failure;
/** @brief Permanent dial or attachment failures retained for automatic recovery. */
static unsigned int retained_permanent_links;
/** @brief Reject recording a permanent retry intent in the fixture runtime. */
static bool retain_permanent_failure;
/** @brief Captured incoming application callback. */
static int (*application)(struct ast_channel *, const char *);
/** @brief Captured CLI command. */
static struct ast_cli_entry *command_entry;
/** @brief Opaque channel storage; never dereferenced by the module. */
static int channel_identity;
/** @brief Reported channel technology. */
static struct ast_channel_tech technology = {.type = "IAX2"};
/** @brief Unsupported incoming technology fixture. */
static struct ast_channel_tech other_technology = {.type = "Local"};
/** @brief Select unsupported incoming technology. */
static bool wrong_technology;
/** @brief Reported remote caller identity. */
static struct ast_party_caller caller;
/** @brief Inject control-queue acquisition, enqueue, or allocation failure. */
static unsigned int queue_failure;
/** @brief Cause a reload between collection and operation execution. */
static bool reload_on_unlock;
/** @brief Action produced by the collector fixture. */
static enum ra_link_action digit_action;
/** @brief Number of partial-command resets requested after queue loss. */
static unsigned int digit_resets;
/** @brief Direct-peer records returned by the administrative status fixture. */
static struct ra_link_peer_status cli_peers[2];
/** @brief Current number of direct peers reported by the status fixture. */
static size_t cli_peer_count;
/** @brief Reject the selected local node from the status fixture. */
static bool cli_node_unknown;
/** @brief Add one peer between count and copy snapshots. */
static bool cli_snapshot_grows;
/** @brief Remove the selected node between count and copy snapshots. */
static bool cli_snapshot_disappears;
/** @brief Status request failure injection. */
static bool status_queue_failure;
/** @brief RF status requests received by the fixture runtime. */
static unsigned int status_queue_calls;
/** @brief Reconnect-all requests received by the fixture runtime. */
static unsigned int reconnect_all_calls;
/** @brief Topology text returned by the administrative status fixture. */
static const char *cli_topology = "";
/** @brief Reject topology allocation in the administrative status fixture. */
static bool cli_topology_failure;
/** @brief Topology snapshots requested by CLI or RF full status. */
static unsigned int topology_calls;
/** @brief RF full-status topology notices emitted by the module. */
static unsigned int topology_notices;
/** @brief Captured administrative output. */
static char cli_output[1024];
/** @brief Used length of captured administrative output. */
static size_t cli_output_length;
/** @brief Deferred control task. */
struct queued_task {
    int (*execute)(void *); /**< Captured task callback. */
    void *data;             /**< Owned task payload. */
};
/** @brief Bounded deferred tasks; callbacks run only when explicitly drained. */
static struct queued_task queued[256];
/** @brief Pending fixture task count. */
static size_t queued_count;

/** @brief Execute the queued tasks outside the runtime lock. */
static void drain_tasks(void) {
    assert(!runtime_locked);
    for (size_t i = 0; i < queued_count; ++i) {
        assert(!queued[i].execute(queued[i].data));
    }
    queued_count = 0;
}

/** @brief Supply Asterisk's serial control executor.
 * @param name Module-specific executor name.
 * @param create Default creation policy.
 * @return Fixture executor or injected failure.
 */
struct ast_taskprocessor *ast_taskprocessor_get(const char *name, enum ast_tps_options create) {
    assert(!strcmp(name, "rpt_advanced/links") && create == TPS_REF_DEFAULT);
    return queue_failure == 1 ? NULL : (struct ast_taskprocessor *)&queued_count;
}

/** @brief Verify executor shutdown drains callbacks outside the runtime lock.
 * @param processor Fixture executor.
 * @return Null after cleanup.
 */
void *ast_taskprocessor_unreference(struct ast_taskprocessor *processor) {
    assert(processor == (struct ast_taskprocessor *)&queued_count);
    drain_tasks();
    return NULL;
}

/** @brief Capture a task or inject queue submission failure.
 * @param processor Fixture executor.
 * @param execute Task callback.
 * @param data Owned payload on success.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Zero on enqueue, minus one on injected failure.
 */
int __ast_taskprocessor_push(struct ast_taskprocessor *processor, int (*execute)(void *),
                             void *data, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    assert(processor == (struct ast_taskprocessor *)&queued_count);
    if (queue_failure == 2) {
        return -1;
    }
    assert(queued_count < 256);
    queued[queued_count++] = (struct queued_task){execute, data};
    return 0;
}

/** @brief Supply digit-event allocation with injected failure.
 * @param count Element count.
 * @param size Element size.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Owned memory or null.
 */
void *__ast_calloc(size_t count, size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return queue_failure == 3 ? NULL : calloc(count, size);
}

bool ra_runtime_digit(struct ra_runtime *runtime, const char *local, char digit, uint64_t now_ms,
                      struct ra_link_operation *operation) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb") && now_ms == 100);
    ++runtime_digit_calls;
    operation->action = digit_action;
    memcpy(operation->remote, "123", 4);
    operation->digit = 0;
    return digit != '?';
}

void ra_runtime_reset_digits(struct ra_runtime *runtime) {
    (void)runtime;
    assert(runtime_locked);
    ++digit_resets;
}

/** @cond TEST_FIXTURE */
int ra_runtime_remote_command(struct ra_runtime *runtime, const char *local, const char *remote,
                              char digit) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb") && !strcmp(remote, "123"));
    assert(!digit || strchr("0123456789ABCD*", digit));
    return 0;
}
/** @endcond */

/** @brief Track serialized runtime access.
 * @param file Caller file.
 * @param line Caller line.
 * @param func Caller function.
 * @param name Lock name.
 * @param lock Opaque lock.
 * @return Zero.
 */
int __ast_pthread_mutex_lock(const char *file, int line, const char *func, const char *name,
                             ast_mutex_t *lock) {
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    assert(!runtime_locked);
    runtime_locked = true;
    return 0;
}

/** @brief Track balanced runtime unlock.
 * @param file Caller file.
 * @param line Caller line.
 * @param func Caller function.
 * @param name Lock name.
 * @param lock Opaque lock.
 * @return Zero.
 */
int __ast_pthread_mutex_unlock(const char *file, int line, const char *func, const char *name,
                               ast_mutex_t *lock) {
    (void)file;
    (void)line;
    (void)func;
    (void)name;
    (void)lock;
    assert(runtime_locked);
    runtime_locked = false;
    if (reload_on_unlock) {
        reload_on_unlock = false;
        assert(!registered->reload());
    }
    return 0;
}

/** @brief Capture incoming application registration.
 * @param app Application name.
 * @param execute Application callback.
 * @param synopsis Short description.
 * @param description Full description.
 * @param mod Module identity.
 * @return Injected registration status.
 */
int ast_register_application2(const char *app, int (*execute)(struct ast_channel *, const char *),
                              const char *synopsis, const char *description, void *mod) {
    (void)mod;
    assert(!strcmp(app, "RptAdvanced") && synopsis && description);
    application = execute;
    return link_failure == 8 ? -1 : 0;
}

/** @brief Observe application removal.
 * @param app Application name.
 * @return Zero.
 */
int ast_unregister_application(const char *app) {
    assert(!strcmp(app, "RptAdvanced"));
    application = NULL;
    return 0;
}

/** @brief Capture administrative command registration.
 * @param entries Command array.
 * @param count Command count.
 * @param mod Module identity.
 * @return Injected registration status.
 */
int __ast_cli_register_multiple(struct ast_cli_entry *entries, int count, struct ast_module *mod) {
    (void)mod;
    assert(count == 2);
    command_entry = entries;
    assert(!entries->handler(entries, CLI_INIT, NULL));
    assert(!entries[1].handler(&entries[1], CLI_INIT, NULL));
    assert(!entries[1].handler(&entries[1], CLI_GENERATE, NULL));
    return link_failure == 9 ? -1 : 0;
}

/** @brief Observe administrative command removal.
 * @param entries Registered commands.
 * @param count Command count.
 * @return Zero.
 */
int ast_cli_unregister_multiple(struct ast_cli_entry *entries, int count) {
    assert(entries == command_entry && count == 2);
    command_entry = NULL;
    return 0;
}

/** @brief Capture CLI text for status-listing assertions.
 * @param fd CLI descriptor.
 * @param format Message format.
 * @param ... Message arguments.
 */
void ast_cli(int fd, const char *format, ...) {
    (void)fd;
    va_list arguments;
    va_start(arguments, format);
    int written = vsnprintf(cli_output + cli_output_length, sizeof(cli_output) - cli_output_length,
                            format, arguments);
    va_end(arguments);
    assert(written >= 0 && (size_t)written < sizeof(cli_output) - cli_output_length);
    cli_output_length += (size_t)written;
}

/** @brief Clear captured administrative output before one command assertion. */
static void clear_cli_output(void) {
    cli_output[0] = '\0';
    cli_output_length = 0;
}

/** @brief Return fixture technology.
 * @param channel Opaque channel.
 * @return Technology descriptor.
 */
const struct ast_channel_tech *ast_channel_tech(const struct ast_channel *channel) {
    (void)channel;
    return wrong_technology ? &other_technology : &technology;
}

/** @brief Return mutable caller identity.
 * @param channel Opaque channel.
 * @return Caller descriptor.
 */
struct ast_party_caller *ast_channel_caller(struct ast_channel *channel) {
    (void)channel;
    return &caller;
}

/** @brief Supply channel identity for the moved-channel name.
 * @param channel Opaque channel.
 * @return Stable identifier.
 */
const char *ast_channel_uniqueid(const struct ast_channel *channel) {
    (void)channel;
    return "fixture";
}

/** @brief Duplicate caller identity or inject allocation failure.
 * @param text Caller identity.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Owned duplicate or null.
 */
char *__ast_strdup(const char *text, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return link_failure == 1 ? NULL : strdup(text);
}

/** @brief Supply the incoming network source address.
 * @param channel Opaque channel.
 * @param function Requested dialplan function.
 * @param buffer Output buffer.
 * @param length Buffer length.
 * @return Injected lookup status.
 */
int ast_func_read(struct ast_channel *channel, const char *function, char *buffer, size_t length) {
    (void)channel;
    assert(!strcmp(function, "CHANNEL(peerip)"));
    snprintf(buffer, length, "127.0.0.1");
    return link_failure == 2 ? -1 : 0;
}

bool ra_runtime_authorize(struct ra_runtime *runtime, const char *local, const char *remote,
                          const char *peer_ip) {
    (void)runtime;
    assert(runtime_locked && local && remote && peer_ip);
    return link_failure != 3;
}

/** @brief Allocate a fixture handoff channel.
 * @param needqueue Required queued channel.
 * @param state Initial channel state.
 * @param cid_num Caller number.
 * @param cid_name Optional caller name.
 * @param acctcode Optional account.
 * @param exten Optional extension.
 * @param context Optional context.
 * @param assignedids Optional identities.
 * @param requestor Source channel.
 * @param amaflag Accounting policy.
 * @param endpoint Optional endpoint.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @param name_fmt Channel name format.
 * @param ... Name arguments.
 * @return Opaque channel or injected failure.
 */
struct ast_channel *__ast_channel_alloc(int needqueue, int state, const char *cid_num,
                                        const char *cid_name, const char *acctcode,
                                        const char *exten, const char *context,
                                        const struct ast_assigned_ids *assignedids,
                                        const struct ast_channel *requestor, enum ama_flags amaflag,
                                        struct ast_endpoint *endpoint, const char *file, int line,
                                        const char *function, const char *name_fmt, ...) {
    (void)cid_name;
    (void)acctcode;
    (void)exten;
    (void)context;
    (void)assignedids;
    (void)requestor;
    (void)amaflag;
    (void)endpoint;
    (void)file;
    (void)line;
    (void)function;
    assert(runtime_locked && needqueue && state == AST_STATE_DOWN && cid_num && name_fmt);
    return link_failure == 4 ? NULL : (struct ast_channel *)&channel_identity;
}

/** @brief Observe channel unlock before handoff.
 * @param object Channel identity.
 * @param file Caller file.
 * @param func Caller function.
 * @param line Caller line.
 * @param var Debug name.
 * @return Zero.
 */
int __ao2_unlock(void *object, const char *file, const char *func, int line, const char *var) {
    (void)file;
    (void)func;
    (void)line;
    (void)var;
    assert(object == &channel_identity);
    return 0;
}

/** @brief Inject channel-move failure.
 * @param destination Handoff channel.
 * @param source Incoming channel.
 * @return Injected status.
 */
int ast_channel_move(struct ast_channel *destination, struct ast_channel *source) {
    (void)source;
    assert(destination == (struct ast_channel *)&channel_identity);
    return link_failure == 5 ? -1 : 0;
}

/** @brief Inject answer failure.
 * @param channel Handoff channel.
 * @return Injected status.
 */
int ast_answer(struct ast_channel *channel) {
    assert(channel == (struct ast_channel *)&channel_identity);
    return link_failure == 6 ? -1 : 0;
}

/** @brief Observe failed handoff cleanup.
 * @param channel Handoff channel.
 */
void ast_hangup(struct ast_channel *channel) {
    assert(channel == (struct ast_channel *)&channel_identity);
}

int ra_runtime_accept(struct ra_runtime *runtime, const char *local, const char *remote,
                      struct ast_channel *channel, bool verified) {
    (void)runtime;
    assert(runtime_locked && local && remote && channel && verified);
    return link_failure == 7 ? -1 : 0;
}

int ra_runtime_prepare_link(struct ra_runtime *runtime, const char *local, const char *remote,
                            struct ra_link_dial *dial) {
    (void)runtime;
    (void)dial;
    assert(runtime_locked && local && remote);
    return link_failure == 1 ? -1 : 0;
}

struct ast_channel *ra_link_dial_run(struct ra_link_dial *dial, const char *local) {
    assert(!runtime_locked && dial && local);
    if (link_failure == 10) {
        assert(!registered->reload());
    }
    return link_failure == 2 ? NULL : (struct ast_channel *)&channel_identity;
}

int ra_runtime_attach_link(struct ra_runtime *runtime, const char *local, const char *remote,
                           struct ast_channel *channel, bool transmit, bool forward,
                           bool permanent) {
    (void)permanent;
    (void)runtime;
    (void)transmit;
    (void)forward;
    assert(runtime_locked && local && remote && channel);
    return link_failure == 3 ? -1 : 0;
}

/** @cond TEST_FIXTURE */
/** @brief Record a permanent initial-failure retry requested by the module.
 * @param runtime Active fixture runtime.
 * @param local Requested local node.
 * @param remote Requested remote node.
 * @param transmit Requested outbound-audio mode.
 * @param forward Requested peer-forwarding mode.
 * @return True unless retention is injected as unavailable.
 */
bool ra_runtime_retain_permanent_link(struct ra_runtime *runtime, const char *local,
                                      const char *remote, bool transmit, bool forward) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb") && !strcmp(remote, "123"));
    ++retained_permanent_links;
    return !retain_permanent_failure && transmit && forward;
}
/** @endcond */

bool ra_runtime_disconnect(struct ra_runtime *runtime, const char *local, const char *remote) {
    (void)runtime;
    assert(runtime_locked && local && remote);
    return !link_failure;
}

/* Observe a permanent-link disconnect that also cancels pending recovery.
 * @param runtime Active fixture runtime.
 * @param local Local node name.
 * @param remote Remote node name.
 * @return True unless the fixture injects a command failure.
 */
bool ra_runtime_disconnect_permanent(struct ra_runtime *runtime, const char *local,
                                     const char *remote) {
    (void)runtime;
    assert(runtime_locked && local && remote);
    return !link_failure;
}

size_t ra_runtime_disconnect_all(struct ra_runtime *runtime, const char *local) {
    (void)runtime;
    assert(runtime_locked && local);
    return 0;
}

/* Queue a local RF link-status response in the fixture runtime.
 * @param state Fixture runtime.
 * @param local Selected local node.
 * @param last_keyed Select last-keyed instead of current-status wording.
 * @return Zero or an injected failure.
 */
int ra_runtime_queue_link_status(struct ra_runtime *runtime, const char *local, bool last_keyed) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb"));
    (void)last_keyed;
    ++status_queue_calls;
    return status_queue_failure ? -1 : 0;
}

int ra_runtime_queue_link_event(struct ra_runtime *runtime, const char *first, const char *second,
                                bool connected) {
    (void)runtime;
    assert(runtime_locked && !strcmp(first, "usb") && !strcmp(second, "123") && connected);
    ++runtime_link_event_calls;
    return 0;
}

/* Snapshot direct peers for the administrative status fixture.
 * @param state Fixture runtime.
 * @param local Selected local node.
 * @param entries Caller output records, or null when only counting.
 * @param capacity Output record capacity.
 * @param count Receives the full fixture peer count.
 * @return True unless the selected node is injected as unknown.
 */
bool ra_runtime_link_snapshot(struct ra_runtime *runtime, const char *local,
                              struct ra_link_peer_status *entries, size_t capacity, size_t *count) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb"));
    if (cli_node_unknown) {
        return false;
    }
    if (entries && cli_snapshot_disappears) {
        cli_snapshot_disappears = false;
        cli_node_unknown = true;
        return false;
    }
    if (entries && cli_snapshot_grows) {
        cli_snapshot_grows = false;
        ++cli_peer_count;
    }
    size_t copied = cli_peer_count;
    if (copied > sizeof(cli_peers) / sizeof(cli_peers[0])) {
        copied = sizeof(cli_peers) / sizeof(cli_peers[0]);
    }
    if (copied > capacity) {
        copied = capacity;
    }
    for (size_t index = 0; entries && index < copied; ++index) {
        entries[index] = cli_peers[index];
    }
    if (count) {
        *count = cli_peer_count;
    }
    return true;
}

/* Observe reconnect-all dispatch in the fixture runtime.
 * @param state Fixture runtime.
 * @param local Selected local node.
 * @return Number of retry records resumed.
 */
size_t ra_runtime_reconnect_all(struct ra_runtime *runtime, const char *local) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb"));
    ++reconnect_all_calls;
    return 0;
}

/* Return an owned topology snapshot for CLI and RF full-status tests.
 * @param state Fixture runtime.
 * @param local Selected local node.
 * @return Owned fixture topology, or null when injected unavailable.
 */
char *ra_runtime_link_topology(struct ra_runtime *runtime, const char *local) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb"));
    ++topology_calls;
    return cli_topology_failure ? NULL : strdup(cli_topology);
}

/** @brief Supply Asterisk's allocating formatter and a deterministic failure case.
 * @param file Caller source file.
 * @param line Caller source line.
 * @param function Caller function.
 * @param result Receives allocated text.
 * @param format Formatting string.
 * @param ... Formatting arguments.
 * @return Formatted length or minus one on allocation failure.
 */
int __ast_asprintf(const char *file, int line, const char *function, char **result,
                   const char *format, ...) {
    (void)file;
    (void)line;
    (void)function;
    if (fail_allocation) {
        return -1;
    }
    va_list arguments;
    va_start(arguments, format);
    int length = vasprintf(result, format, arguments);
    va_end(arguments);
    return length;
}

/** @brief Supply Asterisk's deallocator to the standalone test host.
 * @param pointer Allocated memory.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 */
void __ast_free(void *pointer, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    free(pointer);
}

/** @brief Capture module registration as Asterisk would.
 * @param info Module descriptor.
 */
void ast_module_register(const struct ast_module_info *info) {
    assert(!registered);
    registered = info;
}

/** @brief Observe destructor registration cleanup.
 * @param info Registered module descriptor.
 */
void ast_module_unregister(const struct ast_module_info *info) {
    assert(registered == info);
    registered = NULL;
}

/** @brief Count diagnostics without relying on localized output.
 * @param level Asterisk logging level.
 * @param file Source file.
 * @param line Source line.
 * @param function Source function.
 * @param format Message format.
 * @param ... Message arguments.
 */
void ast_log(int level, const char *file, int line, const char *function, const char *format, ...) {
    (void)level;
    (void)file;
    (void)line;
    (void)function;
    if (!strcmp(format, "rpt_advanced: node %s network topology %s\n")) {
        ++topology_notices;
    }
    ++errors;
}

/** @brief Write a temporary configuration fixture.
 * @param path File in the test-owned temporary directory.
 * @param content Complete configuration text.
 */
static void write_config(const char *path, const char *content) {
    FILE *stream = fopen(path, "w");
    assert(stream);
    assert(fputs(content, stream) >= 0);
    assert(fclose(stream) == 0);
}

/** @brief Test loading, failure paths, reload, and unloading through the real descriptor.
 * @return Zero after assertions and temporary-file cleanup.
 */
int main(void) {
    char directory[] = "/tmp/rpt-advanced-module-XXXXXX";
    assert(mkdtemp(directory));
    ast_config_AST_CONFIG_DIR = directory;
    char path[PATH_MAX];
    assert(snprintf(path, sizeof(path), "%s/rpt_advanced.conf", directory) > 0);
    void *handle = dlopen("build/module-coverage/app_rpt_advanced.so", RTLD_NOW);
    if (!handle) {
        fprintf(stderr, "%s\n", dlerror());
    }
    assert(handle && registered);
    assert(registered->optional_modules &&
           !strcmp(registered->optional_modules, "chan_usbradioplus"));
    queue_failure = 1;
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    queue_failure = 0;
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    fail_allocation = true;
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    fail_allocation = false;
    write_config(path, "[broken\n");
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    write_config(path, "[identifier missing welcome]\n");
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    write_config(path, "[usb]\nfull_duplex=maybe\n");
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    write_config(path, "[usb]\nfull_duplex=yes\n[identifier usb welcome]\nmorse_text=KG0BP\n");
    link_failure = 8;
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    link_failure = 9;
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    link_failure = 0;
    assert(registered->load() == AST_MODULE_LOAD_SUCCESS);
    assert(application && command_entry);
    assert(event_sink);
    event_sink("usb", "123", true);
    drain_tasks();
    assert(runtime_link_event_calls == 1);
    queue_failure = 2;
    event_sink("usb", "123", true);
    queue_failure = 3;
    event_sink("usb", "123", true);
    queue_failure = 0;
    char oversized_endpoint[RA_LINK_PEER_NAME_MAX + 1];
    memset(oversized_endpoint, '1', sizeof(oversized_endpoint) - 1);
    oversized_endpoint[sizeof(oversized_endpoint) - 1] = '\0';
    event_sink(oversized_endpoint, "123", true);
    event_sink("usb", oversized_endpoint, true);
    assert(!queued_count);
    assert(application(NULL, NULL) == -1);
    wrong_technology = true;
    assert(application(NULL, "usb") == -1);
    wrong_technology = false;
    assert(application(NULL, "usb") == -1);
    caller.id.number.valid = 1;
    assert(application(NULL, "usb") == -1);
    caller.id.number.str = "123";
    for (link_failure = 1; link_failure <= 7; ++link_failure) {
        assert(application(NULL, "usb") == -1 && !runtime_locked);
    }
    link_failure = 0;
    assert(!application(NULL, "usb") && !runtime_locked);
    assert(!command_entry->handler(command_entry, CLI_GENERATE, NULL));
    struct ast_cli_args empty_arguments = {0};
    assert(command_entry->handler(command_entry, CLI_HANDLER, &empty_arguments) == CLI_SHOWUSAGE);
    const char *argv[] = {"rpt_advanced", "link", "invalid", "usb", "123"};
    struct ast_cli_args arguments = {.argc = 5, .argv = argv};
    assert(command_entry->handler(command_entry, CLI_HANDLER, &arguments) == CLI_SHOWUSAGE);
    const char *operations[] = {"connect", "monitor", "local-monitor", "disconnect"};
    for (size_t i = 0; i < sizeof(operations) / sizeof(*operations); ++i) {
        argv[2] = operations[i];
        assert(command_entry->handler(command_entry, CLI_HANDLER, &arguments) == CLI_SUCCESS);
        link_failure = 1;
        assert(command_entry->handler(command_entry, CLI_HANDLER, &arguments) == CLI_FAILURE);
        link_failure = 0;
    }
    const char *status_argv[] = {"rpt_advanced", "link", "status", "usb"};
    struct ast_cli_args status_arguments = {.argc = 4, .argv = status_argv};
    const char *short_argv[] = {"rpt_advanced", "link", "connect", "usb"};
    struct ast_cli_args short_arguments = {.argc = 4, .argv = short_argv};
    assert(command_entry->handler(command_entry, CLI_HANDLER, &short_arguments) == CLI_SHOWUSAGE);
    cli_node_unknown = true;
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: unknown node usb\n"));
    cli_node_unknown = false;
    cli_peer_count = 0;
    cli_topology = "T123";
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_SUCCESS);
    assert(!strcmp(cli_output, "rpt_advanced: usb has no active links\n  topology: T123\n"));
    cli_topology = "";
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_SUCCESS);
    assert(!strcmp(cli_output, "rpt_advanced: usb has no active links\n  topology: none\n"));
    cli_topology_failure = true;
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: unable to list topology\n"));
    cli_topology_failure = false;
    cli_peers[0] = (struct ra_link_peer_status){.name = "123",
                                                .transmit = true,
                                                .forward = true,
                                                .permanent = true,
                                                .receive_missing = 160,
                                                .consecutive_underruns = 2,
                                                .underrun_average_milli = 1250,
                                                .receive_reserve_ms = 40};
    cli_peer_count = 1;
    cli_topology = "T123,R456";
    queue_failure = 3;
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: unable to list links\n"));
    queue_failure = 0;
    cli_snapshot_disappears = true;
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: unknown node usb\n"));
    cli_node_unknown = false;
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_SUCCESS);
    assert(!strcmp(cli_output,
                   "rpt_advanced: usb has 1 link\n"
                   "  123: transceive (permanent) rx-missing=160 "
                   "current-underrun-samples=2 10s-underrun-samples=1.250 reserve=40ms\n"
                   "  topology: T123,R456\n"));
    cli_topology = "";
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_SUCCESS);
    assert(!strcmp(cli_output,
                   "rpt_advanced: usb has 1 link\n"
                   "  123: transceive (permanent) rx-missing=160 "
                   "current-underrun-samples=2 10s-underrun-samples=1.250 reserve=40ms\n"
                   "  topology: none\n"));
    cli_topology = "T123,R456";
    cli_topology_failure = true;
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: unable to list topology\n"));
    cli_topology_failure = false;
    cli_peers[0] = (struct ra_link_peer_status){.name = "234", .forward = true};
    cli_peers[1] = (struct ra_link_peer_status){.name = "345"};
    cli_peer_count = 1;
    cli_snapshot_grows = true;
    cli_topology = "R234,R345";
    const char *compat_status_argv[] = {"rpt", "link", "status", "usb"};
    struct ast_cli_args compat_status_arguments = {.argc = 4, .argv = compat_status_argv};
    clear_cli_output();
    assert(command_entry[1].handler(&command_entry[1], CLI_HANDLER, &compat_status_arguments) ==
           CLI_SUCCESS);
    assert(!strcmp(cli_output, "rpt_advanced: usb has 2 links\n"
                               "  234: monitor rx-missing=0 current-underrun-samples=0 "
                               "10s-underrun-samples=0.000 reserve=0ms\n"
                               "  345: local-monitor rx-missing=0 current-underrun-samples=0 "
                               "10s-underrun-samples=0.000 reserve=0ms\n"
                               "  topology: R234,R345\n"));
    cli_peers[0] = (struct ra_link_peer_status){
        .name = "456", .forward = true, .permanent = true, .retrying = true};
    cli_peers[1] = (struct ra_link_peer_status){.name = "567", .retrying = true, .paused = true};
    cli_peer_count = 2;
    cli_topology = "";
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_SUCCESS);
    assert(!strcmp(cli_output,
                   "rpt_advanced: usb has 2 links\n"
                   "  456: monitor (permanent) (retrying) rx-missing=0 "
                   "current-underrun-samples=0 10s-underrun-samples=0.000 reserve=0ms\n"
                   "  567: local-monitor (paused) rx-missing=0 current-underrun-samples=0 "
                   "10s-underrun-samples=0.000 reserve=0ms\n"
                   "  topology: none\n"));
    argv[2] = "connect";
    const unsigned int outgoing_failures[] = {2, 3, 10};
    for (size_t i = 0; i < sizeof(outgoing_failures) / sizeof(*outgoing_failures); ++i) {
        link_failure = outgoing_failures[i];
        assert(command_entry->handler(command_entry, CLI_HANDLER, &arguments) == CLI_FAILURE);
    }
    link_failure = 0;
    unsigned int reloads_before = runtime_reloads;
    unsigned int stops_before = runtime_stops;
    write_config(path, "[usb]\nunknown=yes\n");
    assert(registered->reload() == -1);
    assert(runtime_reloads == reloads_before && runtime_stops == stops_before && runtime_active);
    write_config(path, "[usb]\n");
    runtime_failures = 1;
    assert(registered->reload() == -1);
    assert(runtime_reloads == reloads_before + 1 && runtime_stops == stops_before &&
           runtime_active);
    runtime_failures = 1;
    assert(registered->reload() == -1);
    assert(runtime_reloads == reloads_before + 2 && runtime_stops == stops_before &&
           runtime_active);
    write_config(path, "[usb]\n");
    assert(registered->reload() == 0);
    assert(runtime_reloads == reloads_before + 3 && runtime_stops == stops_before &&
           runtime_active);
    assert(errors == 8);
    assert(digit_sink);
    unsigned int parsed_before_stop = runtime_digit_calls;
    emit_digit_during_reload = true;
    assert(!registered->reload());
    emit_digit_during_reload = false;
    assert(queued_count == 0 && runtime_stops == stops_before && runtime_active);
    assert(runtime_digit_calls == parsed_before_stop);
    unsigned int event_before_stale_reload = runtime_link_event_calls;
    event_sink("usb", "123", true);
    emit_event_during_reload = true;
    assert(!registered->reload());
    emit_event_during_reload = false;
    drain_tasks();
    assert(runtime_link_event_calls == event_before_stale_reload);
    digit_sink("usb", '1', 100);
    assert(!registered->reload());
    drain_tasks();
    digit_sink("usb", '1', 100);
    queue_failure = 2;
    digit_sink("usb", '2', 100);
    queue_failure = 0;
    drain_tasks();
    digit_sink("usb", '?', 100);
    drain_tasks();
    assert(digit_resets == 1);
    digit_sink("usb", RA_WORKER_DIGIT_DROPPED, 0);
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(digit_resets == 2);
    queue_failure = 3;
    digit_sink("usb", '1', 100);
    queue_failure = 0;
    for (size_t i = 0; i < 257; ++i) {
        digit_sink("usb", '1', 100);
    }
    assert(queued_count == 256);
    drain_tasks();
    const enum ra_link_action actions[] = {RA_LINK_TRANSCEIVE,
                                           RA_LINK_MONITOR,
                                           RA_LINK_LOCAL_MONITOR,
                                           RA_LINK_DISCONNECT,
                                           RA_LINK_STATUS,
                                           RA_LINK_DISCONNECT_ALL,
                                           RA_LINK_LAST_KEYED,
                                           RA_LINK_DISCONNECT_PERMANENT,
                                           RA_LINK_PERMANENT_MONITOR,
                                           RA_LINK_PERMANENT_TRANSCEIVE,
                                           RA_LINK_FULL_STATUS,
                                           RA_LINK_RECONNECT_ALL,
                                           RA_LINK_PERMANENT_LOCAL_MONITOR,
                                           RA_LINK_COMMAND};
    unsigned int queued_before = status_queue_calls;
    unsigned int topology_before = topology_calls;
    unsigned int reconnect_before = reconnect_all_calls;
    for (size_t i = 0; i < sizeof(actions) / sizeof(*actions); ++i) {
        digit_action = actions[i];
        digit_sink("usb", '1', 100);
        drain_tasks();
    }
    assert(status_queue_calls == queued_before + 3);
    assert(topology_calls == topology_before + 1);
    assert(reconnect_all_calls == reconnect_before + 1);
    unsigned int retained_before = retained_permanent_links;
    digit_action = RA_LINK_PERMANENT_TRANSCEIVE;
    link_failure = 2;
    digit_sink("usb", '1', 100);
    drain_tasks();
    link_failure = 3;
    digit_sink("usb", '1', 100);
    drain_tasks();
    retain_permanent_failure = true;
    link_failure = 2;
    digit_sink("usb", '1', 100);
    drain_tasks();
    retain_permanent_failure = false;
    link_failure = 10;
    digit_sink("usb", '1', 100);
    drain_tasks();
    link_failure = 0;
    assert(retained_permanent_links == retained_before + 3);
    status_queue_failure = true;
    digit_action = RA_LINK_STATUS;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(status_queue_calls == queued_before + 4);
    status_queue_failure = false;
    cli_topology_failure = true;
    digit_action = RA_LINK_FULL_STATUS;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(topology_calls == topology_before + 2);
    assert(status_queue_calls == queued_before + 4);
    cli_topology_failure = false;
    cli_topology = "T123,R456";
    status_queue_failure = true;
    digit_action = RA_LINK_FULL_STATUS;
    digit_sink("usb", '1', 100);
    drain_tasks();
    status_queue_failure = false;
    unsigned int notices_before = topology_notices;
    digit_action = RA_LINK_FULL_STATUS;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(topology_notices == notices_before + 1);
    cli_topology = "";
    digit_action = RA_LINK_FULL_STATUS;
    digit_sink("usb", '1', 100);
    drain_tasks();
    cli_topology = "R234,R345";
    digit_action = (enum ra_link_action)99;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_action = RA_LINK_DISCONNECT_ALL;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_action = RA_LINK_DISCONNECT_PERMANENT;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_action = RA_LINK_TRANSCEIVE;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_action = RA_LINK_DISCONNECT;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_action = RA_LINK_STATUS;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(status_queue_calls == queued_before + 7);
    digit_action = RA_LINK_FULL_STATUS;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(topology_calls == topology_before + 5);
    digit_action = RA_LINK_RECONNECT_ALL;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(reconnect_all_calls == reconnect_before + 1);
    digit_action = RA_LINK_COMMAND;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_sink("usb", '1', 100);
    assert(registered->unload() == 0);
    assert(runtime_stops == stops_before + 1 && !runtime_active);
    assert(dlclose(handle) == 0 && !registered);
    assert(unlink(path) == 0);
    assert(rmdir(directory) == 0);
    puts("Asterisk shared-module lifecycle tests passed");
    return 0;
}
