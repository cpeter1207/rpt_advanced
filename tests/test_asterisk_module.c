/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exercise the real shared module through Asterisk's public lifecycle ABI.
 */
#include <asterisk.h>

#include "runtime.h"
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

/** @brief Number of runtime starts to reject in sequence. */
static unsigned int runtime_failures;
/** @brief Captured hardware digit delivery callback. */
static ra_digit_handler digit_sink;

const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document) {
    digit_sink = runtime->digit;
    (void)document;
    if (runtime_failures) {
        --runtime_failures;
        return "fixture radio unavailable";
    }
    return NULL;
}

void ra_runtime_stop(struct ra_runtime *runtime) { (void)runtime; }

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
/** @brief Runtime lock ownership. */
static bool runtime_locked;
/** @brief Inject control-queue acquisition, enqueue, or allocation failure. */
static unsigned int queue_failure;
/** @brief Cause a reload between collection and operation execution. */
static bool reload_on_unlock;
/** @brief Action produced by the collector fixture. */
static enum ra_link_action digit_action;
/** @brief Number of partial-command resets requested after queue loss. */
static unsigned int digit_resets;
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

bool ra_runtime_digit(struct ra_runtime *state, const char *local, char digit, uint64_t now_ms,
                      struct ra_link_operation *operation) {
    (void)state;
    assert(runtime_locked && !strcmp(local, "usb") && now_ms == 100);
    operation->action = digit_action;
    memcpy(operation->remote, "123", 4);
    return digit != '?';
}

void ra_runtime_reset_digits(struct ra_runtime *state) {
    (void)state;
    assert(runtime_locked);
    ++digit_resets;
}

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
    assert(count == 1);
    command_entry = entries;
    assert(!entries->handler(entries, CLI_INIT, NULL));
    return link_failure == 9 ? -1 : 0;
}

/** @brief Observe administrative command removal.
 * @param entries Registered commands.
 * @param count Command count.
 * @return Zero.
 */
int ast_cli_unregister_multiple(struct ast_cli_entry *entries, int count) {
    assert(entries == command_entry && count == 1);
    command_entry = NULL;
    return 0;
}

/** @brief Discard CLI text; callback return values are asserted separately.
 * @param fd CLI descriptor.
 * @param format Message format.
 * @param ... Message arguments.
 */
void ast_cli(int fd, const char *format, ...) {
    (void)fd;
    (void)format;
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

bool ra_runtime_authorize(struct ra_runtime *state, const char *local, const char *remote,
                          const char *peer_ip) {
    (void)state;
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

int ra_runtime_accept(struct ra_runtime *state, const char *local, const char *remote,
                      struct ast_channel *channel, bool verified, bool same_server) {
    (void)state;
    assert(runtime_locked && local && remote && channel && verified && !same_server);
    return link_failure == 7 ? -1 : 0;
}

int ra_runtime_prepare_link(struct ra_runtime *state, const char *local, const char *remote,
                            struct ra_link_dial *dial) {
    (void)state;
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

int ra_runtime_attach_link(struct ra_runtime *state, const char *local, const char *remote,
                           struct ast_channel *channel, bool transmit, bool forward) {
    (void)state;
    (void)transmit;
    (void)forward;
    assert(runtime_locked && local && remote && channel);
    return link_failure == 3 ? -1 : 0;
}

bool ra_runtime_disconnect(struct ra_runtime *state, const char *local, const char *remote) {
    (void)state;
    assert(runtime_locked && local && remote);
    return !link_failure;
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
    (void)format;
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
    argv[2] = "connect";
    const unsigned int outgoing_failures[] = {2, 3, 10};
    for (size_t i = 0; i < sizeof(outgoing_failures) / sizeof(*outgoing_failures); ++i) {
        link_failure = outgoing_failures[i];
        assert(command_entry->handler(command_entry, CLI_HANDLER, &arguments) == CLI_FAILURE);
    }
    link_failure = 0;
    write_config(path, "[usb]\nunknown=yes\n");
    assert(registered->reload() == -1);
    write_config(path, "[usb]\n");
    runtime_failures = 1;
    assert(registered->reload() == -1);
    runtime_failures = 2;
    assert(registered->reload() == -1);
    write_config(path, "");
    assert(registered->reload() == 0);
    assert(errors == 9);
    assert(digit_sink);
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
    queue_failure = 3;
    digit_sink("usb", '1', 100);
    queue_failure = 0;
    for (size_t i = 0; i < 257; ++i) {
        digit_sink("usb", '1', 100);
    }
    assert(queued_count == 256);
    drain_tasks();
    const enum ra_link_action actions[] = {RA_LINK_TRANSCEIVE, RA_LINK_MONITOR,
                                           RA_LINK_LOCAL_MONITOR, RA_LINK_DISCONNECT,
                                           RA_LINK_STATUS};
    for (size_t i = 0; i < sizeof(actions) / sizeof(*actions); ++i) {
        digit_action = actions[i];
        digit_sink("usb", '1', 100);
        drain_tasks();
    }
    digit_action = RA_LINK_TRANSCEIVE;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_action = RA_LINK_DISCONNECT;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    digit_sink("usb", '1', 100);
    assert(registered->unload() == 0);
    assert(dlclose(handle) == 0 && !registered);
    assert(unlink(path) == 0);
    assert(rmdir(directory) == 0);
    puts("Asterisk shared-module lifecycle tests passed");
    return 0;
}
