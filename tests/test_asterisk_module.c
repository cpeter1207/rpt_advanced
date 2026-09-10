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
#include <asterisk/utils.h>
#include <dlfcn.h>
#include <errno.h>
#include <semaphore.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
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
/** @brief Interrupt the schedule ticker while a reload rejects control submissions. */
static bool emit_schedule_during_reload;
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
/** @brief Yielded scheduled dispatches awaiting the module control bridge. */
static unsigned int scheduled_dispatches;
/** @brief Inject a scheduler dispatch construction failure. */
static bool scheduled_dispatch_failure;
/** @brief Reject scheduled telemetry while retaining its dispatch for a later retry. */
static bool scheduled_message_failure;
/** @brief Reject completion of a queued scheduled dispatch. */
static bool scheduled_completion_failure;
/** @brief Include speech/Morse telemetry in the next scheduled fixture dispatch. */
static bool scheduled_has_message = true;
/** @brief Include a validated link operation in the next scheduled fixture dispatch. */
static bool scheduled_has_operation = true;
/** @brief Number of scheduled telemetry messages offered to the fixture runtime. */
static unsigned int scheduled_message_calls;
/** @brief Number of scheduled events completed before their optional macro action begins. */
static unsigned int scheduled_completions;
/** @brief Scheduled disconnect macros executed after their telemetry was accepted. */
static unsigned int scheduled_disconnects;
/** @brief Number of fixture scheduler tick threads Asterisk requested. */
static unsigned int scheduler_thread_starts;
/** @brief Reject one Asterisk background-thread request. */
static bool scheduler_thread_failure;
/** @brief Joinable scheduler thread created by the successful module-load fixture. */
static pthread_t scheduler_fixture_thread;
/** @brief True after the fixture has captured the running scheduler thread. */
static bool scheduler_fixture_thread_running;
/** @brief Test-only scheduler sleep entries announced by the ticker thread. */
static sem_t scheduler_sleep_entered;
/** @brief Test-only permissions for the ticker thread to complete one sleep. */
static sem_t scheduler_sleep_release;
/** @brief True after the deterministic ticker synchronization fixtures are initialized. */
static bool scheduler_sleep_synchronization_ready;
/** @brief Fixture wall clock advanced one epoch minute for every requested scheduler tick. */
static time_t scheduler_fixture_time = 7200;
/** @brief Pause the next ticker allocation after its first reload-state check. */
static bool block_scheduler_allocation;
/** @brief Pause a replacement runtime while its module reload state remains asserted. */
static bool block_runtime_reload;
/** @brief Announces the controlled ticker allocation to the reloading test thread. */
static sem_t scheduler_allocation_entered;
/** @brief Releases the controlled ticker allocation after reload state is asserted. */
static sem_t scheduler_allocation_release;
/** @brief Announces that the fixture reload is holding the module runtime lock. */
static sem_t runtime_reload_entered;
/** @brief Releases the fixture reload after the ticker has rechecked its reload state. */
static sem_t runtime_reload_release;
/** @brief Wait for one deterministic fixture synchronization event. */
static void wait_fixture_semaphore(sem_t *semaphore);
/** @brief Publish one deterministic fixture synchronization event. */
static void post_fixture_semaphore(sem_t *semaphore);
/** @brief Prompt the timer thread to submit one control task under test synchronization. */
static void trigger_schedule_tick(void);

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
    if (block_runtime_reload) {
        block_runtime_reload = false;
        post_fixture_semaphore(&runtime_reload_entered);
        wait_fixture_semaphore(&runtime_reload_release);
    }
    if (emit_digit_during_reload) {
        assert(digit_sink);
        digit_sink("usb", '1', 100);
    }
    if (emit_event_during_reload) {
        assert(event_sink);
        event_sink("usb", "123", true);
    }
    if (emit_schedule_during_reload) {
        trigger_schedule_tick();
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

/** @brief Yield one controllable schedule dispatch for the Asterisk bridge fixture.
 * @param runtime Active fixture runtime.
 * @param now Wall-clock value supplied by the ticker.
 * @param dispatch Output dispatch copied by the module before it releases its lock.
 * @return One when a fixture event is ready, zero when idle, or minus one when injected broken.
 */
int ra_runtime_next_scheduled_dispatch(struct ra_runtime *runtime, time_t now,
                                       struct ra_scheduled_dispatch *dispatch) {
    (void)runtime;
    (void)now;
    assert(runtime_locked && runtime_active && dispatch);
    if (scheduled_dispatch_failure) {
        return -1;
    }
    if (!scheduled_dispatches) {
        return 0;
    }
    --scheduled_dispatches;
    *dispatch = (struct ra_scheduled_dispatch){.generation = 1,
                                               .event_index = 0,
                                               .occurrence = 1,
                                               .has_message = scheduled_has_message,
                                               .has_operation = scheduled_has_operation,
                                               .operation = {.action = RA_LINK_DISCONNECT}};
    strcpy(dispatch->local, "usb");
    strcpy(dispatch->speech, "scheduled message");
    strcpy(dispatch->morse, "SCHEDULED MESSAGE");
    strcpy(dispatch->operation.remote, "123");
    return 1;
}

/** @brief Record scheduled telemetry admission through the fixture runtime.
 * @param runtime Active fixture runtime.
 * @param dispatch Current copied event.
 * @return Zero unless the test retains the event by simulating a full telemetry queue.
 */
int ra_runtime_queue_scheduled_message(struct ra_runtime *runtime,
                                       const struct ra_scheduled_dispatch *dispatch) {
    (void)runtime;
    assert(runtime_locked && dispatch && !strcmp(dispatch->local, "usb"));
    ++scheduled_message_calls;
    return scheduled_message_failure ? -1 : 0;
}

/** @brief Record a completed scheduled event before its macro is attempted.
 * @param runtime Active fixture runtime.
 * @param dispatch Current copied event.
 * @return True while the fixture runtime remains active.
 */
bool ra_runtime_complete_scheduled_dispatch(struct ra_runtime *runtime,
                                            const struct ra_scheduled_dispatch *dispatch) {
    (void)runtime;
    assert(runtime_locked && dispatch && !strcmp(dispatch->local, "usb"));
    ++scheduled_completions;
    return !scheduled_completion_failure;
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
/** @brief Expected CLI DTMF stream, or null while testing worker-delivered digits. */
static const char *cli_digit_stream;
/** @brief Next expected position in the CLI DTMF stream. */
static size_t cli_digit_position;
/** @brief One-based stream position that completes a CLI command, or zero when incomplete. */
static size_t cli_digit_complete_position;
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

/** @brief Reload the module while the scheduler-race fixture controls its runtime transition.
 * @param unused Unused POSIX thread argument.
 * @return Null after a successful replacement.
 */
static void *run_fixture_reload(void *unused) {
    (void)unused;
    assert(!registered->reload());
    return NULL;
}

/** @brief Wait for one deterministic fixture synchronization event.
 * @param semaphore Semaphore carrying the expected event.
 */
static void wait_fixture_semaphore(sem_t *semaphore) {
    int result;
    do {
        result = sem_wait(semaphore);
    } while (result && errno == EINTR);
    assert(!result);
}

/** @brief Publish one deterministic fixture synchronization event.
 * @param semaphore Semaphore receiving the event.
 */
static void post_fixture_semaphore(sem_t *semaphore) { assert(!sem_post(semaphore)); }

/** @brief Wait until the fixture ticker is blocked at its deterministic sleep point. */
static void wait_schedule_ticker_sleep(void) { wait_fixture_semaphore(&scheduler_sleep_entered); }

/** @brief Permit the fixture ticker to complete one deterministic sleep. */
static void release_schedule_ticker_sleep(void) {
    post_fixture_semaphore(&scheduler_sleep_release);
}

/** @brief Replace the ticker's production sleep with a test-controlled synchronization point.
 * @param requested Ignored production interval.
 * @param remaining Ignored remainder output.
 * @return Zero after the test releases this single sleep.
 *
 * The coverage-module link redirects its `nanosleep` reference here. This eliminates arbitrary
 * signal delivery and wall-clock waits while retaining the production ticker implementation
 * unchanged.
 */
int __wrap_nanosleep(const struct timespec *requested, struct timespec *remaining) {
    (void)requested;
    (void)remaining;
    assert(scheduler_sleep_synchronization_ready);
    post_fixture_semaphore(&scheduler_sleep_entered);
    wait_fixture_semaphore(&scheduler_sleep_release);
    return 0;
}

/** @brief Supply one deterministic epoch clock for ticker and immediate scheduling tests.
 * @param result Optional destination for the returned epoch time.
 * @return Current fixture epoch time.
 */
time_t __wrap_time(time_t *result) {
    if (result) {
        *result = scheduler_fixture_time;
    }
    return scheduler_fixture_time;
}

/** @brief Resolve the libc join implementation after waking the controlled ticker sleep.
 * @param thread Thread to join with the native libc implementation.
 * @param result Optional native thread-result destination.
 * @return Native POSIX join result.
 */
static int call_libc_pthread_join(pthread_t thread, void **result) {
    typedef int (*pthread_join_function)(pthread_t, void **);
    pthread_join_function join = (pthread_join_function)dlsym(RTLD_NEXT, "pthread_join");
    assert(join);
    return join(thread, result);
}

/** @brief Wake the test-controlled ticker only after module teardown requests its stop.
 * @param thread Thread the module is joining.
 * @param result Optional thread-result destination.
 * @return Native POSIX join result.
 *
 * `stop_schedule_ticker()` first clears its production run flag and then joins. Releasing the
 * blocked fixture sleep at that point makes the ticker observe the cleared flag and exit normally,
 * so unload is deterministic without changing production shutdown behavior.
 */
int __wrap_pthread_join(pthread_t thread, void **result) {
    if (scheduler_fixture_thread_running && pthread_equal(thread, scheduler_fixture_thread)) {
        release_schedule_ticker_sleep();
    }
    return call_libc_pthread_join(thread, result);
}

/** @brief Prompt the timer thread to submit a control task without a wall-clock test delay. */
static void trigger_schedule_tick(void) {
    assert(scheduler_fixture_thread_running);
    scheduler_fixture_time += 60;
    release_schedule_ticker_sleep();
    /* This rendezvous proves submission returned before the test drains its control task. */
    wait_schedule_ticker_sleep();
}

/** @brief Supply the Asterisk-selected background stack size to the module fixture.
 * @return Zero because the fixture leaves stack selection to pthread.
 */
int ast_background_stacksize(void) { return 0; }

/** @brief Create the module's control ticker with deterministic failure injection.
 * @param thread Receives the joinable ticker thread.
 * @param attributes Optional caller attributes.
 * @param start Routine supplied by the module.
 * @param argument Routine argument.
 * @param stacksize Requested Asterisk background stack size.
 * @param file Module source file.
 * @param caller Module function name.
 * @param line Module source line.
 * @param start_name Stringified start routine.
 * @return Zero on thread creation or a POSIX error selected by the fixture.
 */
int ast_pthread_create_stack(pthread_t *thread, pthread_attr_t *attributes, void *(*start)(void *),
                             void *argument, size_t stacksize, const char *file, const char *caller,
                             int line, const char *start_name) {
    (void)stacksize;
    (void)file;
    (void)caller;
    (void)line;
    assert(thread && start && start_name && !strcmp(start_name, "run_schedule_ticker"));
    ++scheduler_thread_starts;
    if (scheduler_thread_failure) {
        return EAGAIN;
    }
    int result = pthread_create(thread, attributes, start, argument);
    if (!result) {
        scheduler_fixture_thread = *thread;
        scheduler_fixture_thread_running = true;
    }
    return result;
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

/** @brief Supply task allocation with injected failure and a controlled reload race.
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
    if (block_scheduler_allocation) {
        block_scheduler_allocation = false;
        post_fixture_semaphore(&scheduler_allocation_entered);
        wait_fixture_semaphore(&scheduler_allocation_release);
    }
    return queue_failure == 3 ? NULL : calloc(count, size);
}

bool ra_runtime_digit(struct ra_runtime *runtime, const char *local, char digit, uint64_t now_ms,
                      struct ra_link_operation *operation) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb") && (now_ms == 100 || now_ms == 0));
    ++runtime_digit_calls;
    operation->action = digit_action;
    memcpy(operation->remote, "123", 4);
    operation->digit = 0;
    if (cli_digit_stream) {
        assert(digit == cli_digit_stream[cli_digit_position]);
        ++cli_digit_position;
        return cli_digit_position == cli_digit_complete_position;
    }
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
    assert(count == 3);
    command_entry = entries;
    assert(!entries->handler(entries, CLI_INIT, NULL));
    assert(!entries[1].handler(&entries[1], CLI_INIT, NULL));
    assert(!entries[1].handler(&entries[1], CLI_GENERATE, NULL));
    assert(!entries[2].handler(&entries[2], CLI_INIT, NULL));
    assert(!entries[2].handler(&entries[2], CLI_GENERATE, NULL));
    return link_failure == 9 ? -1 : 0;
}

/** @brief Observe administrative command removal.
 * @param entries Registered commands.
 * @param count Command count.
 * @return Zero.
 */
int ast_cli_unregister_multiple(struct ast_cli_entry *entries, int count) {
    assert(entries == command_entry && count == 3);
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
    if (!strcmp(local, "usb") && !strcmp(remote, "123")) {
        ++scheduled_disconnects;
    }
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

/* Accept nonpermanent disconnect-all requests in the module fixture. */
size_t ra_runtime_disconnect_nonpermanent_all(struct ra_runtime *runtime, const char *local) {
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

/* Fixture implementation for the public time-telemetry operation. */
int ra_runtime_queue_time(struct ra_runtime *runtime, const char *local) {
    (void)runtime;
    assert(runtime_locked && !strcmp(local, "usb"));
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
    assert(!sem_init(&scheduler_sleep_entered, 0, 0));
    assert(!sem_init(&scheduler_sleep_release, 0, 0));
    assert(!sem_init(&scheduler_allocation_entered, 0, 0));
    assert(!sem_init(&scheduler_allocation_release, 0, 0));
    assert(!sem_init(&runtime_reload_entered, 0, 0));
    assert(!sem_init(&runtime_reload_release, 0, 0));
    scheduler_sleep_synchronization_ready = true;
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
    scheduler_thread_failure = true;
    assert(registered->load() == AST_MODULE_LOAD_DECLINE);
    assert(scheduler_thread_starts == 1);
    scheduler_thread_failure = false;
    assert(registered->load() == AST_MODULE_LOAD_SUCCESS);
    assert(scheduler_thread_starts == 2);
    /* Establish the ticker's blocked state before every deterministic release-and-rendezvous. */
    wait_schedule_ticker_sleep();
    /* Both startup paths submit the current minute; duplicate occurrence keys are harmless. */
    drain_tasks();
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
    const char *command_argv[] = {"rpt_advanced", "command", "usb", "*722"};
    struct ast_cli_args command_arguments = {.argc = 4, .argv = command_argv};
    struct ast_cli_args incomplete_command_arguments = {.argc = 3, .argv = command_argv};
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER,
                                    &incomplete_command_arguments) == CLI_SHOWUSAGE);
    command_argv[2] = "";
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER, &command_arguments) ==
           CLI_SHOWUSAGE);
    command_argv[2] = "usb";
    command_argv[3] = "";
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER, &command_arguments) ==
           CLI_SHOWUSAGE);
    command_argv[3] = "*722";
    const unsigned int command_digits_before = runtime_digit_calls;
    const unsigned int command_status_before = status_queue_calls;
    digit_action = RA_LINK_TIME;
    cli_digit_stream = "*722#";
    cli_digit_position = 0;
    cli_digit_complete_position = 4;
    clear_cli_output();
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER, &command_arguments) ==
           CLI_SUCCESS);
    assert(cli_digit_position == strlen(cli_digit_stream));
    assert(runtime_digit_calls == command_digits_before + cli_digit_position);
    assert(status_queue_calls == command_status_before + 1);
    assert(!strcmp(cli_output, "rpt_advanced: DTMF command completed\n"));
    command_argv[3] = "*7#";
    cli_digit_stream = "*7#";
    cli_digit_position = 0;
    cli_digit_complete_position = 0;
    clear_cli_output();
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER, &command_arguments) ==
           CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: incomplete or unknown DTMF command\n"));
    command_argv[3] = "*7x";
    clear_cli_output();
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER, &command_arguments) ==
           CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: invalid DTMF digit x\n"));
    command_argv[3] = "*722";
    cli_digit_stream = "*722#";
    cli_digit_position = 0;
    cli_digit_complete_position = 4;
    status_queue_failure = true;
    clear_cli_output();
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER, &command_arguments) ==
           CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: DTMF command failed\n"));
    status_queue_failure = false;
    cli_digit_stream = "*722#";
    cli_digit_position = 0;
    cli_digit_complete_position = strlen(cli_digit_stream);
    status_queue_failure = true;
    clear_cli_output();
    assert(command_entry[2].handler(&command_entry[2], CLI_HANDLER, &command_arguments) ==
           CLI_FAILURE);
    assert(!strcmp(cli_output, "rpt_advanced: DTMF command failed\n"));
    status_queue_failure = false;
    cli_digit_stream = NULL;
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
                                                .receive_reserve_ms = 40,
                                                .receive_capacity_ms = 120,
                                                .receive_occupancy_ms = 80,
                                                .receive_filtered_occupancy_ms = 75,
                                                .receive_target_ms = 80,
                                                .receive_ratio_correction_ppm = -25};
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
    assert(!strcmp(cli_output, "rpt_advanced: usb has 1 link\n"
                               "  123: transceive (permanent) rx-missing=160 "
                               "current-underrun-samples=2 10s-underrun-samples=1.250 reserve=40ms "
                               "occupancy=80/120ms filtered=75ms target=80ms ratio=-25ppm\n"
                               "  topology: T123,R456\n"));
    cli_topology = "";
    clear_cli_output();
    assert(command_entry->handler(command_entry, CLI_HANDLER, &status_arguments) == CLI_SUCCESS);
    assert(!strcmp(cli_output, "rpt_advanced: usb has 1 link\n"
                               "  123: transceive (permanent) rx-missing=160 "
                               "current-underrun-samples=2 10s-underrun-samples=1.250 reserve=40ms "
                               "occupancy=80/120ms filtered=75ms target=80ms ratio=-25ppm\n"
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
                               "10s-underrun-samples=0.000 reserve=0ms occupancy=0/0ms "
                               "filtered=0ms target=0ms ratio=+0ppm\n"
                               "  345: local-monitor rx-missing=0 current-underrun-samples=0 "
                               "10s-underrun-samples=0.000 reserve=0ms occupancy=0/0ms "
                               "filtered=0ms target=0ms ratio=+0ppm\n"
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
                   "current-underrun-samples=0 10s-underrun-samples=0.000 reserve=0ms "
                   "occupancy=0/0ms filtered=0ms target=0ms ratio=+0ppm\n"
                   "  567: local-monitor (paused) rx-missing=0 current-underrun-samples=0 "
                   "10s-underrun-samples=0.000 reserve=0ms occupancy=0/0ms "
                   "filtered=0ms target=0ms ratio=+0ppm\n"
                   "  topology: none\n"));
    argv[2] = "connect";
    const unsigned int outgoing_failures[] = {2, 3, 10};
    for (size_t i = 0; i < sizeof(outgoing_failures) / sizeof(*outgoing_failures); ++i) {
        link_failure = outgoing_failures[i];
        assert(command_entry->handler(command_entry, CLI_HANDLER, &arguments) == CLI_FAILURE);
    }
    link_failure = 0;
    drain_tasks();
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
    drain_tasks();
    assert(runtime_reloads == reloads_before + 3 && runtime_stops == stops_before &&
           runtime_active);
    assert(errors == 8);
    assert(digit_sink);
    unsigned int parsed_before_stop = runtime_digit_calls;
    emit_digit_during_reload = true;
    assert(!registered->reload());
    emit_digit_during_reload = false;
    drain_tasks();
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
                                           RA_LINK_DISCONNECT_NONPERMANENT_ALL,
                                           RA_LINK_TIME,
                                           RA_LINK_COMMAND};
    unsigned int queued_before = status_queue_calls;
    unsigned int topology_before = topology_calls;
    unsigned int reconnect_before = reconnect_all_calls;
    for (size_t i = 0; i < sizeof(actions) / sizeof(*actions); ++i) {
        digit_action = actions[i];
        digit_sink("usb", '1', 100);
        drain_tasks();
    }
    assert(status_queue_calls == queued_before + 4);
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
    assert(status_queue_calls == queued_before + 5);
    status_queue_failure = false;
    cli_topology_failure = true;
    digit_action = RA_LINK_FULL_STATUS;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(topology_calls == topology_before + 2);
    assert(status_queue_calls == queued_before + 5);
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
    digit_action = RA_LINK_DISCONNECT_NONPERMANENT_ALL;
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
    assert(status_queue_calls == queued_before + 8);
    digit_action = RA_LINK_TIME;
    reload_on_unlock = true;
    digit_sink("usb", '1', 100);
    drain_tasks();
    assert(status_queue_calls == queued_before + 8);
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
    /* The background ticker does no audio work: it submits a revision-stamped control task. */
    trigger_schedule_tick();
    drain_tasks();
    /* A failed wall clock is reported once and does not cause one task per second. */
    scheduler_fixture_time = (time_t)-1;
    release_schedule_ticker_sleep();
    wait_schedule_ticker_sleep();
    assert(queued_count == 1);
    drain_tasks();
    release_schedule_ticker_sleep();
    wait_schedule_ticker_sleep();
    assert(!queued_count);
    scheduler_fixture_time = 7260;
    release_schedule_ticker_sleep();
    wait_schedule_ticker_sleep();
    drain_tasks();
    /* A reload beginning after allocation must invalidate the task before it reaches the queue. */
    block_scheduler_allocation = true;
    scheduler_fixture_time += 60;
    release_schedule_ticker_sleep();
    wait_fixture_semaphore(&scheduler_allocation_entered);
    block_runtime_reload = true;
    pthread_t reload_thread;
    assert(!pthread_create(&reload_thread, NULL, run_fixture_reload, NULL));
    wait_fixture_semaphore(&runtime_reload_entered);
    post_fixture_semaphore(&scheduler_allocation_release);
    wait_schedule_ticker_sleep();
    post_fixture_semaphore(&runtime_reload_release);
    assert(!pthread_join(reload_thread, NULL));
    drain_tasks();
    unsigned int scheduled_messages_before = scheduled_message_calls;
    unsigned int scheduled_completions_before = scheduled_completions;
    unsigned int scheduled_disconnects_before = scheduled_disconnects;
    /* One minute may contain any number of configuration-order scheduled events. */
    scheduled_dispatches = 2;
    trigger_schedule_tick();
    drain_tasks();
    assert(scheduled_message_calls == scheduled_messages_before + 2);
    assert(scheduled_completions == scheduled_completions_before + 2);
    assert(scheduled_disconnects == scheduled_disconnects_before + 2);
    scheduled_messages_before = scheduled_message_calls;
    scheduled_completions_before = scheduled_completions;
    scheduled_disconnects_before = scheduled_disconnects;
    scheduled_has_message = false;
    scheduled_has_operation = false;
    scheduled_dispatches = 1;
    trigger_schedule_tick();
    drain_tasks();
    scheduled_has_message = true;
    scheduled_has_operation = true;
    assert(scheduled_message_calls == scheduled_messages_before + 1);
    assert(scheduled_completions == scheduled_completions_before + 1);
    assert(scheduled_disconnects == scheduled_disconnects_before);
    scheduled_messages_before = scheduled_message_calls;
    scheduled_completions_before = scheduled_completions;
    scheduled_disconnects_before = scheduled_disconnects;
    scheduled_dispatches = 1;
    scheduled_message_failure = true;
    trigger_schedule_tick();
    drain_tasks();
    scheduled_message_failure = false;
    assert(scheduled_message_calls == scheduled_messages_before + 1);
    assert(scheduled_completions == scheduled_completions_before);
    assert(scheduled_disconnects == scheduled_disconnects_before);
    scheduled_messages_before = scheduled_message_calls;
    scheduled_completions_before = scheduled_completions;
    scheduled_disconnects_before = scheduled_disconnects;
    scheduled_dispatches = 1;
    scheduled_completion_failure = true;
    trigger_schedule_tick();
    drain_tasks();
    scheduled_completion_failure = false;
    assert(scheduled_message_calls == scheduled_messages_before + 1);
    assert(scheduled_completions == scheduled_completions_before + 1);
    assert(scheduled_disconnects == scheduled_disconnects_before);
    scheduled_messages_before = scheduled_message_calls;
    scheduled_dispatch_failure = true;
    trigger_schedule_tick();
    drain_tasks();
    scheduled_dispatch_failure = false;
    assert(scheduled_message_calls == scheduled_messages_before);
    scheduled_messages_before = scheduled_message_calls;
    queue_failure = 3;
    trigger_schedule_tick();
    queue_failure = 0;
    drain_tasks();
    assert(scheduled_message_calls == scheduled_messages_before);
    queue_failure = 2;
    trigger_schedule_tick();
    queue_failure = 0;
    drain_tasks();
    assert(scheduled_message_calls == scheduled_messages_before);
    scheduled_completions_before = scheduled_completions;
    scheduled_dispatches = 1;
    reload_on_unlock = true;
    trigger_schedule_tick();
    drain_tasks();
    assert(scheduled_completions == scheduled_completions_before + 1);
    emit_schedule_during_reload = true;
    assert(!registered->reload());
    emit_schedule_during_reload = false;
    assert(queued_count == 1);
    drain_tasks();
    scheduled_dispatches = 1;
    scheduled_messages_before = scheduled_message_calls;
    scheduled_completions_before = scheduled_completions;
    scheduled_disconnects_before = scheduled_disconnects;
    assert(!registered->reload());
    drain_tasks();
    assert(scheduled_message_calls == scheduled_messages_before + 1);
    assert(scheduled_completions == scheduled_completions_before + 1);
    assert(scheduled_disconnects == scheduled_disconnects_before + 1);
    scheduled_dispatches = 1;
    trigger_schedule_tick();
    assert(queued_count);
    /* A task retained from the previous revision must not execute after the reload. */
    scheduled_dispatches = 0;
    scheduled_messages_before = scheduled_message_calls;
    assert(!registered->reload());
    drain_tasks();
    assert(scheduled_message_calls == scheduled_messages_before);
    scheduled_dispatches = 0;
    digit_sink("usb", '1', 100);
    assert(registered->unload() == 0);
    scheduler_fixture_thread_running = false;
    assert(runtime_stops == stops_before + 1 && !runtime_active);
    assert(dlclose(handle) == 0 && !registered);
    scheduler_sleep_synchronization_ready = false;
    assert(!sem_destroy(&runtime_reload_release));
    assert(!sem_destroy(&runtime_reload_entered));
    assert(!sem_destroy(&scheduler_allocation_release));
    assert(!sem_destroy(&scheduler_allocation_entered));
    assert(!sem_destroy(&scheduler_sleep_release));
    assert(!sem_destroy(&scheduler_sleep_entered));
    assert(unlink(path) == 0);
    assert(rmdir(directory) == 0);
    puts("Asterisk shared-module lifecycle tests passed");
    return 0;
}
