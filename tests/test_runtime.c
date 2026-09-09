/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Node startup, ownership transfer, and complete partial-failure cleanup.
 */
#include "assets.h"
#include "connection.h"
#include "link_directory.h"
#include "link_hub.h"
#include "media.h"
#include "runtime.h"
#include "schema.h"
#include "worker.h"
#include <assert.h>
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/localtime.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/** @brief Allocation call selected for failure, zero disables injection. */
static unsigned int fail_allocation;
/** @brief Current allocation sequence. */
static unsigned int allocations;
/** @brief Active channels. */
static unsigned int channels;
/** @brief Workers that still require joining. */
static unsigned int workers;
/** @brief Reservation failure injection. */
static bool fail_open;
/** @brief Clock failure injection. */
static bool fail_clock;
/** @brief Clock-read call selected for failure, or zero when disabled. */
static unsigned int fail_clock_call;
/** @brief Clock reads observed by the fixture. */
static unsigned int clock_calls;
/** @brief Optional ordered monotonic times for one dial-budget test. */
static struct timespec clock_sequence[2];
/** @brief Number of configured ordered monotonic times. */
static size_t clock_sequence_count;
/** @brief Next configured monotonic time. */
static size_t clock_sequence_index;
/** @brief Call number selected for failure. */
static unsigned int fail_call;
/** @brief Worker start number selected for failure. */
static unsigned int fail_worker;
/** @brief First worker start rejected by the consecutive rollback-failure injection. */
static unsigned int fail_worker_from;
/** @brief Number of consecutive worker starts rejected for rollback-failure coverage. */
static unsigned int fail_worker_count;
/** @brief Reject stable callback-lock initialization for one runtime startup. */
static bool fail_callback_lock;
/** @brief Call counter. */
static unsigned int calls;
/** @brief Worker start counter. */
static unsigned int starts;
/** @brief Negotiated linear rate. */
static unsigned int rate = 16000;
/** @brief One replacement startup rate returned before the fixture resumes its prior rate. */
static unsigned int next_sample_rate;
/** @brief Provide prepared media even when no Morse fallback exists. */
static bool prepared;
/** @brief Count usable identifier sets bound to successful worker starts. */
static size_t seen_ids;
/** @brief Selected link failure: lookup, offer, dial, answer, or attachment. */
static unsigned int link_error;
/** @brief Opaque channel and capability identity. */
static int link_identity;
/** @brief IAX request attempts observed by the fixture. */
static unsigned int link_dials;
/** @brief Number of link channels released after a failed attachment. */
static unsigned int link_hangups;
/** @brief Whether the routing fixture retains an attached peer or reconnect request. */
static bool retained_link_state;
/** @brief Initial permanent-link intents accepted by the routing fixture. */
static unsigned int retained_permanent_links;
/** @brief Routing hubs closed only when their owning runtime node is released. */
static unsigned int hub_closes;
/** @brief Direct-peer records returned by the status fixture. */
static struct ra_link_peer_status status_peers[2];
/** @brief Number of reportable direct peers, which may exceed the copied records. */
static size_t status_peer_count;
/** @brief Most recently active direct peer, or an empty string when none exists. */
static char status_last_keyed[RA_LINK_PEER_NAME_MAX];
/** @brief Owned-topology source returned by the routing fixture. */
static const char *status_topology = "";
/** @brief Inject topology allocation failure. */
static bool status_topology_failure;
/** @brief Most recent status text submitted through the live controller interface. */
static char queued_status[RA_CONTROLLER_STATUS_TEXT_MAX];
/** @brief Most recent Piper text prepared by the runtime fixture. */
static char prepared_speech[RA_CONTROLLER_STATUS_TEXT_MAX * 2];
/** @brief Number of status requests accepted by the controller fixture. */
static size_t queued_status_count;
/** @brief Maximum status requests accepted before the fixture reports a full controller queue. */
static size_t queued_status_limit = SIZE_MAX;
/** @brief Make the next control-plane status reaper return one owned PCM buffer. */
static bool release_status_audio;
/** @brief Stages at which the recovery fixture changes a callback-owned cancellation flag. */
enum reconnect_race_point {
    RECONNECT_RACE_NONE,
    RECONNECT_RACE_AFTER_LOOKUP,
    RECONNECT_RACE_AFTER_OFFER,
    RECONNECT_RACE_AFTER_DIAL,
    RECONNECT_RACE_AFTER_ATTACH,
};
/** @brief Most recently registered hub recovery callback. */
static ra_link_reconnect_fn reconnect_callback;
/** @brief Context paired with the captured recovery callback. */
static void *reconnect_context;
/** @brief True only while a test invokes the captured recovery callback. */
static bool reconnect_invoking;
/** @brief Configured point at which the fixture simulates a concurrent control action. */
static enum reconnect_race_point reconnect_race;
/** @brief Callback-owned cancellation flag modified by the selected race. */
static atomic_bool *reconnect_cancelled;
/** @brief Callback-owned pause flag modified by the selected race. */
static atomic_bool *reconnect_paused;
/** @brief Select pause rather than cancellation at the configured race point. */
static bool reconnect_race_paused;
/** @brief Number of recovery-only hub attachments observed by the fixture. */
static unsigned int reconnect_attachments;
/** @brief Number of recovery-only permanent detaches observed by the fixture. */
static unsigned int reconnect_cancellations;
/** @brief Number of recovery-only pause detaches observed by the fixture. */
static unsigned int reconnect_pauses;
/** @brief Reader-to-runtime handlers installed by each active node hub. */
static ra_link_hub_digit_fn inbound_callbacks[2];
/** @brief Contexts paired with the captured inbound handlers. */
static void *inbound_callback_contexts[2];
/** @brief Hubs paired with captured inbound handlers for lifecycle callback injection. */
static struct ra_link_hub *inbound_callback_hubs[2];
/** @brief Number of inbound handlers captured after the current runtime start. */
static size_t inbound_callback_count;
/** @brief Reader-to-runtime lifecycle handlers installed by each active node hub. */
static ra_link_hub_event_fn inbound_event_callbacks[2];
/** @brief Contexts paired with the captured lifecycle handlers. */
static void *inbound_event_contexts[2];
/** @brief Number of lifecycle handlers captured after the current runtime start. */
static size_t inbound_event_callback_count;
/** @brief Force the next local civil-time conversion to fail. */
static bool fail_localtime;
/** @brief Force the next wall-clock read to fail. */
static bool fail_wall_clock;
/** @brief Return an invalid civil hour to exercise announcement validation. */
static bool invalid_localtime;

/** @brief Provide a deterministic wall clock or its documented failure value.
 * @param output Optional destination for the selected epoch.
 * @return Fixed valid epoch, or minus one when failure is injected.
 */
time_t __wrap_time(time_t *output) {
    time_t value = fail_wall_clock ? (time_t)-1 : (time_t)0;
    if (output) {
        *output = value;
    }
    return value;
}

/** @brief Supply a fixed valid local civil time for local-time telemetry tests.
 * @param when Current wall-clock input.
 * @param output Local civil-time destination.
 * @param zone Unused system-timezone selector.
 * @return @p output after filling a deterministic afternoon time.
 */
struct ast_tm *ast_localtime(const struct timeval *when, struct ast_tm *output, const char *zone) {
    assert(when && output && !zone);
    if (fail_localtime) {
        return NULL;
    }
    output->tm_hour = invalid_localtime ? 24 : 13;
    output->tm_min = 7;
    return output;
}
/** @brief Invoke the closing hub's reader callback after the runtime marks its worker stopped. */
static bool invoke_closed_digit;
/** @brief Number of IAX DTMF events delivered through the runtime bridge. */
static unsigned int inbound_digits;
/** @brief Local node selected by the last bridge-delivered IAX DTMF event. */
static char inbound_node[64];
/** @brief Most recent bridge-delivered IAX DTMF character. */
static char inbound_digit;
/** @brief Most recent bridge-delivered IAX DTMF timestamp. */
static uint64_t inbound_now_ms;
/** @brief Link lifecycle reports forwarded through the runtime bridge. */
static unsigned int inbound_events;
/** @brief Local endpoint from the most recent lifecycle report. */
static char inbound_event_local[64];
/** @brief Remote endpoint from the most recent lifecycle report. */
static char inbound_event_remote[64];
/** @brief True for the most recent attach report. */
static bool inbound_event_connected;

/** @cond TEST_FIXTURE */
/** @brief Initialize one runtime callback mutex with deterministic failure injection. */
int __ast_pthread_mutex_init(int tracking, const char *file, int line, const char *function,
                             const char *name, ast_mutex_t *mutex) {
    (void)tracking;
    (void)file;
    (void)line;
    (void)function;
    (void)name;
    (void)mutex;
    return fail_callback_lock ? -1 : 0;
}

/** @brief Destroy one runtime callback mutex. */
int __ast_pthread_mutex_destroy(const char *file, int line, const char *function, const char *name,
                                ast_mutex_t *mutex) {
    (void)file;
    (void)line;
    (void)function;
    (void)name;
    (void)mutex;
    return 0;
}

/** @brief Lock one runtime callback mutex. */
int __ast_pthread_mutex_lock(const char *file, int line, const char *function, const char *name,
                             ast_mutex_t *mutex) {
    (void)file;
    (void)line;
    (void)function;
    (void)name;
    (void)mutex;
    return 0;
}

/** @brief Unlock one runtime callback mutex. */
int __ast_pthread_mutex_unlock(const char *file, int line, const char *function, const char *name,
                               ast_mutex_t *mutex) {
    (void)file;
    (void)line;
    (void)function;
    (void)name;
    (void)mutex;
    return 0;
}
/** @endcond */

/** @brief Capture the module control-queue submission requested by an IAX reader.
 * @param node Local configured node name.
 * @param digit Completed IAX DTMF character.
 * @param now_ms Reader monotonic timestamp.
 */
static void receive_inbound_digit(const char *node, char digit, uint64_t now_ms) {
    size_t length = strlen(node);
    assert(length < sizeof(inbound_node));
    ++inbound_digits;
    memcpy(inbound_node, node, length + 1);
    inbound_digit = digit;
    inbound_now_ms = now_ms;
}

/** @brief Capture the module control-queue submission requested by a hub lifecycle event.
 * @param local Runtime node selected by the hub.
 * @param remote Direct peer whose lifecycle changed.
 * @param connected True for attachment, false for detachment.
 */
static void receive_inbound_event(const char *local, const char *remote, bool connected) {
    assert(strlen(local) < sizeof(inbound_event_local));
    assert(strlen(remote) < sizeof(inbound_event_remote));
    ++inbound_events;
    strcpy(inbound_event_local, local);
    strcpy(inbound_event_remote, remote);
    inbound_event_connected = connected;
}

/** @brief Apply a configured recovery race after one owned control-plane operation.
 * @param point Completed operation boundary.
 *
 * The real callback may race disconnect or disconnect-all at every allocation/dial boundary.
 * This fixture makes each boundary deterministic without placing a test hook in production code.
 */
static void inject_reconnect_race(enum reconnect_race_point point) {
    if (reconnect_invoking && reconnect_race == point) {
        atomic_store(reconnect_race_paused ? reconnect_paused : reconnect_cancelled, true);
    }
}

char *ra_link_directory_lookup(const char *node, const char *peer_ip,
                               const struct ra_link_directory_policy *policy) {
    assert(!strcmp(node, "123") && policy && !*policy->static_file && !*policy->external_file &&
           policy->method == RA_LINK_LOOKUP_BOTH);
    assert(!peer_ip || !strcmp(peer_ip, "127.0.0.1"));
    char *destination = link_error == 1 ? NULL : strdup("radio@127.0.0.1/123");
    inject_reconnect_race(RECONNECT_RACE_AFTER_LOOKUP);
    return destination;
}

/** @cond RA_TEST_MEDIA_SHIMS
 * These linker shims stand in for the separately documented media interface.  Keeping them out of
 * the generated API prevents Doxygen from merging fixture parameter documentation with the real
 * declarations in media.h.
 */
int ra_media_candidates_collect(struct ast_format *radio, struct ast_format ***formats,
                                size_t *count) {
    (void)radio;
    if (link_error == 2) {
        return -1;
    }
    *count = link_error == 9 ? 2 : 1;
    *formats = calloc(*count, sizeof(**formats));
    if (!*formats) {
        *count = 0;
        return -1;
    }
    for (size_t index = 0; index < *count; ++index) {
        (*formats)[index] = (struct ast_format *)&link_identity;
    }
    inject_reconnect_race(RECONNECT_RACE_AFTER_OFFER);
    return 0;
}

void ra_media_candidates_release(struct ast_format **formats, size_t count) {
    (void)count;
    free(formats);
}

struct ast_format_cap *ra_media_offer_create(struct ast_format *format) {
    assert(format == (struct ast_format *)&link_identity);
    return link_error == 8 ? NULL : (struct ast_format_cap *)&link_identity;
}
/** @endcond */

/** @brief Verify offer ownership is released after dialing.
 * @param object Fixture capability.
 * @param tag Debug tag.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 */
void __ao2_cleanup_debug(void *object, const char *tag, const char *file, int line,
                         const char *function) {
    (void)tag;
    (void)file;
    (void)line;
    (void)function;
    assert(object == &link_identity);
}

/** @brief Supply an outbound channel or inject a failed call.
 * @param type IAX2 technology.
 * @param cap Offered capability.
 * @param assignedids Default channel identities.
 * @param requestor No source channel.
 * @param addr Resolved directory destination.
 * @param timeout Bounded dialing timeout.
 * @param reason Dial result storage.
 * @param cid_num Local node number.
 * @param cid_name Local node name.
 * @return Borrowed fixture identity or null.
 */
struct ast_channel *ast_request_and_dial(const char *type, struct ast_format_cap *cap,
                                         const struct ast_assigned_ids *assignedids,
                                         const struct ast_channel *requestor, const char *addr,
                                         int timeout, int *reason, const char *cid_num,
                                         const char *cid_name) {
    assert(!strcmp(type, "IAX2") && cap == (struct ast_format_cap *)&link_identity);
    assert(!assignedids && !requestor && !strcmp(addr, "radio@127.0.0.1/123"));
    assert(timeout > 0 && timeout <= 20000 && reason && !strcmp(cid_num, cid_name) &&
           (!strcmp(cid_num, "alpha") || !strcmp(cid_num, "beta") || !strcmp(cid_num, "reload")));
    ++link_dials;
    struct ast_channel *channel = link_error == 3 || (link_error == 9 && link_dials == 1)
                                      ? NULL
                                      : (struct ast_channel *)&link_identity;
    inject_reconnect_race(RECONNECT_RACE_AFTER_DIAL);
    return channel;
}

/** @brief Return answered or injected unanswered state.
 * @param channel Fixture channel.
 * @return Channel state.
 */
enum ast_channel_state ast_channel_state(const struct ast_channel *channel) {
    assert(channel == (struct ast_channel *)&link_identity);
    return link_error == 4 ? AST_STATE_DOWN : AST_STATE_UP;
}

/** @brief Count caller-owned channel cleanup.
 * @param channel Fixture channel.
 */
void ast_hangup(struct ast_channel *channel) {
    assert(channel == (struct ast_channel *)&link_identity);
    ++link_hangups;
}

/** @cond TEST_FIXTURE */
/* Verify that every runtime-owned hub is initialized before configuration starts. */
void ra_link_hub_init(struct ra_link_hub *hub) {
    assert(hub);
    hub->rate = 0;
}
/** @endcond */

int ra_link_hub_attach(struct ra_link_hub *hub, const char *name, struct ast_channel *channel,
                       struct ast_format *linear, bool transmit, bool forward, bool permanent) {
    (void)linear;
    assert(hub && !strcmp(name, "123") && channel == (struct ast_channel *)&link_identity);
    assert(transmit && forward);
    int result = link_error == 5 ? -1 : 0;
    if (reconnect_invoking) {
        ++reconnect_attachments;
        assert(permanent);
        inject_reconnect_race(RECONNECT_RACE_AFTER_ATTACH);
    }
    if (!result) {
        hub->rate = rate;
    }
    return result;
}

/** @cond TEST_FIXTURE */
/** @brief Record a permanent retry intent passed through the runtime bridge.
 * @param hub Selected node routing hub.
 * @param name Requested remote identity.
 * @param transmit Requested outbound-audio mode.
 * @param forward Requested peer-forwarding mode.
 * @return True unless the fixture selects retention failure.
 */
bool ra_link_hub_retain_permanent(struct ra_link_hub *hub, const char *name, bool transmit,
                                  bool forward) {
    assert(hub && !strcmp(name, "123") && transmit && forward);
    ++retained_permanent_links;
    return link_error != 10;
}
/** @endcond */

void ra_link_hub_set_reconnector(struct ra_link_hub *hub, ra_link_reconnect_fn callback,
                                 void *context) {
    assert(hub && callback && context);
    reconnect_callback = callback;
    reconnect_context = context;
}

/* Fixture stub: capture the node-specific reader-to-control DTMF handoff.
 * @param hub Node-owned routing hub.
 * @param callback Runtime bridge installed before any peer can attach.
 * @param context Runtime node paired with callback.
 */
void ra_link_hub_set_digit_handler(struct ra_link_hub *hub, ra_link_hub_digit_fn callback,
                                   void *context) {
    assert(hub && callback && context);
    if (inbound_callback_count < sizeof(inbound_callbacks) / sizeof(inbound_callbacks[0])) {
        inbound_callbacks[inbound_callback_count] = callback;
        inbound_callback_contexts[inbound_callback_count] = context;
        inbound_callback_hubs[inbound_callback_count] = hub;
        ++inbound_callback_count;
    }
}

/** @cond TEST_FIXTURE */
/** @brief Capture the node-specific lifecycle handoff installed before a peer can attach.
 * @param hub Node-owned routing hub.
 * @param callback Runtime bridge for lifecycle events.
 * @param context Runtime node paired with callback.
 */
void ra_link_hub_set_event_handler(struct ra_link_hub *hub, ra_link_hub_event_fn callback,
                                   void *context) {
    assert(hub && callback && context);
    if (inbound_event_callback_count <
        sizeof(inbound_event_callbacks) / sizeof(inbound_event_callbacks[0])) {
        inbound_event_callbacks[inbound_event_callback_count] = callback;
        inbound_event_contexts[inbound_event_callback_count] = context;
        ++inbound_event_callback_count;
    }
}
/** @endcond */

bool ra_link_hub_disconnect(struct ra_link_hub *hub, const char *name) {
    assert(hub && !strcmp(name, "123"));
    return link_error != 7;
}

/* Fixture stub: accept a permanent-link cancellation in the runtime fixture.
 * @param hub Routing hub owned by the fixture runtime.
 * @param name Exact remote node identity.
 * @return True after validating the requested peer.
 */
bool ra_link_hub_disconnect_permanent(struct ra_link_hub *hub, const char *name) {
    assert(hub && !strcmp(name, "123"));
    if (reconnect_invoking) {
        ++reconnect_cancellations;
    }
    return link_error != 7;
}

/* Fixture stub: accept a race-safe recovery detachment in the runtime fixture. */
bool ra_link_hub_detach_reconnect(struct ra_link_hub *hub, const char *name, bool permanent) {
    assert(hub && !strcmp(name, "123"));
    if (reconnect_invoking) {
        ++reconnect_pauses;
        assert(permanent);
    }
    return true;
}

size_t ra_link_hub_disconnect_all(struct ra_link_hub *hub) {
    assert(hub);
    return 0;
}

/* Accept nonpermanent disconnect-all requests in the runtime fixture. */
size_t ra_link_hub_disconnect_nonpermanent_all(struct ra_link_hub *hub) {
    assert(hub);
    return 0;
}

/* Fixture stub: accept retained-recovery resumption in the runtime fixture.
 * @param hub Routing hub owned by the fixture runtime.
 * @return Zero; the fixture has no retained peers.
 */
size_t ra_link_hub_reconnect_all(struct ra_link_hub *hub) {
    assert(hub);
    return 0;
}

/* Fixture stub: select whether the runtime must retain the hub over replacement.
 * @param hub Routing hub examined by the runtime lifecycle.
 * @return Selected attached/retry-state result.
 */
bool ra_link_hub_has_retained_state(struct ra_link_hub *hub) {
    assert(hub);
    return retained_link_state;
}

/* Fixture stub: return caller-owned direct-peer status records from the fixture.
 * @param hub Fixture routing hub.
 * @param entries Output array, or null when only counting.
 * @param capacity Output capacity.
 * @return Complete fixture count.
 */
size_t ra_link_hub_snapshot(struct ra_link_hub *hub, struct ra_link_peer_status *entries,
                            size_t capacity) {
    assert(hub);
    size_t copied = status_peer_count;
    if (copied > sizeof(status_peers) / sizeof(status_peers[0])) {
        copied = sizeof(status_peers) / sizeof(status_peers[0]);
    }
    if (copied > capacity) {
        copied = capacity;
    }
    for (size_t index = 0; entries && index < copied; ++index) {
        entries[index] = status_peers[index];
    }
    return status_peer_count;
}

/* Fixture stub: return the fixture's lock-free last-keyed snapshot.
 * @param hub Fixture routing hub.
 * @param name Output peer identity.
 * @param capacity Output capacity.
 * @return True when a complete identity is available.
 */
bool ra_link_hub_last_keyed(const struct ra_link_hub *hub, char *name, size_t capacity) {
    assert(hub);
    if (!status_last_keyed[0] || capacity < sizeof(status_last_keyed)) {
        return false;
    }
    for (size_t index = 0; index < sizeof(status_last_keyed); ++index) {
        name[index] = status_last_keyed[index];
        if (!name[index]) {
            break;
        }
    }
    return true;
}

/* Fixture stub: return the routing fixture's app_rpt-style topology report.
 * @param hub Fixture routing hub.
 * @return Owned topology text, or null for injected allocation failure.
 */
char *ra_link_hub_topology(struct ra_link_hub *hub) {
    assert(hub);
    return status_topology_failure ? NULL : strdup(status_topology);
}

/** @cond TEST_FIXTURE */
int ra_link_hub_send_digit(struct ra_link_hub *hub, const char *name, char digit) {
    assert(hub && !strcmp(name, "123") && strchr("0123456789ABCD*", digit));
    return link_error == 6 ? -1 : 0;
}

bool ra_link_hub_connected(struct ra_link_hub *hub, const char *name) {
    assert(hub && !strcmp(name, "123"));
    return link_error != 6;
}
/** @endcond */

void ra_link_hub_close(struct ra_link_hub *hub) {
    assert(hub);
    if (invoke_closed_digit) {
        for (size_t index = 0; index < inbound_callback_count; ++index) {
            if (inbound_callback_hubs[index] == hub) {
                inbound_callbacks[index](inbound_callback_contexts[index], "123", '7', 1400);
                break;
            }
        }
    }
    ++hub_closes;
}

void ra_identifier_prepare(const struct ra_identifier_settings *settings, unsigned int selected,
                           int16_t **audio, size_t *samples) {
    assert(settings->morse_text && selected == rate);
    assert(settings->speech_text);
    ast_copy_string(prepared_speech, settings->speech_text, sizeof(prepared_speech));
    *audio = prepared ? malloc(sizeof(**audio)) : NULL;
    *samples = prepared ? 1 : 0;
    if (*audio) {
        **audio = 1000;
    }
}

/** @brief Linker-provided allocation implementation.
 * @param count Element count.
 * @param size Element size.
 * @return Allocated memory.
 */
void *__real_calloc(size_t count, size_t size);
/** @brief Inject failure without changing successful allocation behavior.
 * @param count Element count.
 * @param size Element size.
 * @return Zeroed memory or null at the selected call.
 */
void *__wrap_calloc(size_t count, size_t size) {
    return ++allocations == fail_allocation ? NULL : __real_calloc(count, size);
}
/** @brief Asterisk allocation fixture.
 * @param count Element count.
 * @param size Element size.
 * @param file Caller file.
 * @param line Caller line.
 * @param function Caller function.
 * @return Zeroed memory or injected failure.
 */
void *__ast_calloc(size_t count, size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    return __wrap_calloc(count, size);
}
/** @brief Asterisk deallocation fixture.
 * @param pointer Owned memory.
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

/** @brief Capture prepared RF status submitted through the retained controller queue API.
 * @param controller Started node controller selected by the runtime.
 * @param text Complete speech and Morse-safe status text.
 * @param audio Owned prepared status PCM, if speech was available.
 * @param samples Prepared status PCM sample count.
 * @return True until the configured fixture queue limit is reached.
 */
bool __wrap_ra_controller_queue_status(struct ra_controller *controller, const char *text,
                                       int16_t *audio, size_t samples) {
    assert(controller && text);
    assert((audio == NULL) == (samples == 0));
    if (queued_status_count >= queued_status_limit) {
        return false;
    }
    size_t length = strlen(text);
    assert(length < sizeof(queued_status));
    memcpy(queued_status, text, length + 1);
    ++queued_status_count;
    free(audio);
    return true;
}

/** @brief Return one control-owned speech buffer to exercise runtime reaping.
 * @param controller Controller passed through the runtime reaper.
 * @param audio Destination for at most capacity owned buffers.
 * @param capacity Available audio-pointer slots.
 * @return One when the fixture requests a release, otherwise zero.
 */
size_t __wrap_ra_controller_reclaim_status(struct ra_controller *controller, int16_t **audio,
                                           size_t capacity) {
    assert(controller && audio && capacity);
    if (!release_status_audio) {
        return 0;
    }
    release_status_audio = false;
    audio[0] = malloc(sizeof(*audio[0]));
    assert(audio[0]);
    return 1;
}
/** @brief Deterministic startup clock.
 * @param clock Requested clock identifier.
 * @param value Receives startup time.
 * @return Zero or an injected failure.
 */
int __wrap_clock_gettime(clockid_t clock, struct timespec *value) {
    assert(clock == CLOCK_MONOTONIC);
    ++clock_calls;
    if (fail_clock || (fail_clock_call && clock_calls == fail_clock_call)) {
        return -1;
    }
    if (clock_sequence_index < clock_sequence_count) {
        *value = clock_sequence[clock_sequence_index++];
        return 0;
    }
    *value = (struct timespec){.tv_sec = 10};
    return 0;
}

const char *ra_connection_open(struct ra_connection *connection, const char *name,
                               unsigned int requested, const char *codec) {
    assert(*name && !requested && !*codec);
    if (fail_open) {
        return "fixture unavailable";
    }
    ++channels;
    connection->channel = (struct ast_channel *)connection;
    return NULL;
}
void ra_connection_close(struct ra_connection *connection) {
    if (connection->channel) {
        assert(channels);
        --channels;
    }
    *connection = (struct ra_connection){0};
}
/** @brief Supply negotiated rate without requiring hardware.
 * @param format Unused fixture identity.
 * @return Test-selected rate.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) {
    (void)format;
    if (next_sample_rate) {
        unsigned int selected = next_sample_rate;
        next_sample_rate = 0;
        return selected;
    }
    return rate;
}
/** @brief Observe starting a reserved channel.
 * @param channel Reserved radio.
 * @param address Configured radio name.
 * @param timeout No independent dialing timer.
 * @return Zero or selected failure.
 */
int ast_call(struct ast_channel *channel, const char *address, int timeout) {
    assert(channel && *address && !timeout);
    return ++calls == fail_call ? -1 : 0;
}
int ra_worker_start(struct ra_worker *worker) {
    assert(worker->channel && worker->controller->rate == rate);
    assert(worker->dtmf_muting);
    if (!strcmp(worker->controller->courtesy[RA_COURTESY_RECEIVER].settings.morse_text, "R")) {
        assert(worker->controller->courtesy[RA_COURTESY_RECEIVER].settings.morse_frequency_hz ==
               500);
    }
    if (!strcmp(worker->controller->courtesy[RA_COURTESY_LINK].settings.morse_text, "L")) {
        assert(worker->controller->courtesy[RA_COURTESY_LINK].settings.morse_frequency_hz == 1000);
    }
    ++starts;
    if (fail_worker_count && starts >= fail_worker_from) {
        --fail_worker_count;
        return -1;
    }
    if (starts == fail_worker) {
        return -1;
    }
    ++workers;
    seen_ids += worker->controller->count;
    return 0;
}
void ra_worker_stop(struct ra_worker *worker) {
    assert(worker->channel && workers && channels);
    --workers;
    --channels;
}

/** @brief Verify a failed startup returns an empty runtime with no owned radios.
 * @param document Test configuration.
 */
static void rejected(const struct ra_document *document) {
    struct ra_runtime runtime = {0};
    allocations = calls = starts = 0;
    assert(ra_runtime_start(&runtime, document));
    assert(!runtime.nodes && !channels && !workers);
    ra_runtime_stop(&runtime);
}

/** @brief Exercise the call preparation, transport, and attachment boundaries.
 * @param runtime Test runtime.
 * @param local Selected local node.
 * @return Zero on attachment, minus one on failure.
 */
static int connect_fixture(struct ra_runtime *runtime, const char *local) {
    struct ra_link_dial dial = {0};
    if (ra_runtime_prepare_link(runtime, local, "123", &dial)) {
        return -1;
    }
    struct ast_channel *channel = ra_link_dial_run(&dial, local);
    assert(!dial.destination && !dial.candidates && !dial.candidate_count);
    if (!channel) {
        return -1;
    }
    int result = ra_runtime_attach_link(runtime, local, "123", channel, true, true, false);
    if (result) {
        ast_hangup(channel);
    }
    return result;
}

/** @brief Invoke the runtime-installed recovery callback without a live IAX transport.
 * @param cancelled Callback-owned permanent-disconnect flag.
 * @param paused Callback-owned disconnect-all flag.
 * @return Recovery callback result.
 */
static int reconnect_fixture(atomic_bool *cancelled, atomic_bool *paused) {
    assert(reconnect_callback && reconnect_context);
    reconnect_invoking = true;
    int result = reconnect_callback(reconnect_context, "123", true, true, true, cancelled, paused);
    reconnect_invoking = false;
    reconnect_race = RECONNECT_RACE_NONE;
    return result;
}

/** @brief Feed a DTMF string into a node's real collector.
 * @param runtime Started runtime.
 * @param digits Complete test sequence.
 * @param operation Receives any completed operation.
 * @return Result of the last digit.
 */
static bool digits_fixture(struct ra_runtime *runtime, const char *digits,
                           struct ra_link_operation *operation) {
    bool ready = false;
    for (size_t i = 0; digits[i]; ++i) {
        ready = ra_runtime_digit(runtime, "alpha", digits[i], 100, operation);
    }
    return ready;
}

/** @brief Exercise multiple nodes, disabled nodes, ID state, and each failure boundary.
 * @return Zero after assertions.
 */
int main(void) {
    char speech_text[64];
    assert(ra_runtime_telemetry_speech_text("temperature 75", NULL, NULL, speech_text,
                                            sizeof(speech_text)));
    assert(!strcmp(speech_text, "temperature 75"));
    assert(ra_runtime_telemetry_speech_text("123 N0CALL", "123", NULL, speech_text,
                                            sizeof(speech_text)));
    assert(!strcmp(speech_text, "node,1,2,3 N,0,C,A,L,L"));
    assert(
        ra_runtime_telemetry_speech_text("  123", "123", "456", speech_text, sizeof(speech_text)));
    assert(!strcmp(speech_text, "  node,1,2,3"));
    assert(ra_runtime_telemetry_speech_text("456 abc", "123", "456", speech_text,
                                            sizeof(speech_text)));
    assert(!strcmp(speech_text, "node,4,5,6 abc"));
    assert(ra_runtime_telemetry_speech_text("abc", "xyz", "456", speech_text, sizeof(speech_text)));
    assert(!strcmp(speech_text, "abc"));
    assert(!ra_runtime_telemetry_speech_text(" ", NULL, NULL, speech_text, 1));
    assert(!ra_runtime_telemetry_speech_text("123", "123", NULL, speech_text, 5));
    assert(!ra_runtime_telemetry_speech_text("A", NULL, NULL, speech_text, 1));
    assert(!ra_runtime_telemetry_speech_text("", NULL, NULL, speech_text, 0));
    char *sections[] = {"alpha", "disabled", "beta", "identifier alpha periodic"};
    struct ra_config_entry entries[] = {
        {"disabled", "node_enabled", "no"},
        {"beta", "link_deny_nodes", "123"},
        {"general", "receiver_courtesy_morse_text", "R"},
        {"general", "receiver_courtesy_morse_frequency_hz", "500"},
        {"general", "link_courtesy_morse_text", "L"},
        {"general", "link_courtesy_morse_frequency_hz", "1000"},
        {"identifier alpha periodic", "morse_text", "TEST"},
    };
    struct ra_document document = {.sections = sections,
                                   .section_count = 4,
                                   .entries = entries,
                                   .count = sizeof(entries) / sizeof(*entries)};
    struct ra_runtime runtime = {.digit = receive_inbound_digit, .event = receive_inbound_event};
    struct ra_document empty = {0};
    const char *section;
    const char *key;
    assert(!ra_document_validate(&document, &section, &key));
    assert(!ra_runtime_start(&runtime, &empty));
    ra_runtime_stop(&runtime);
    for (fail_allocation = 1; fail_allocation <= 5; ++fail_allocation) {
        rejected(&document);
    }
    fail_allocation = 0;
    fail_callback_lock = true;
    rejected(&document);
    fail_callback_lock = false;
    fail_open = true;
    rejected(&document);
    fail_open = false;
    fail_clock = true;
    rejected(&document);
    fail_clock = false;
    rate = 1000;
    rejected(&document);
    rate = 16000;
    fail_call = 2;
    rejected(&document);
    fail_call = 0;
    fail_worker = 2;
    rejected(&document);
    fail_worker = 0;

    /* A matching replacement retains its hub, including attached-peer and retry ownership. */
    char *reload_sections[] = {"reload"};
    struct ra_config_entry reload_current_entries[] = {
        {"reload", "transmit_hang_ms", "100"},
    };
    struct ra_config_entry reload_replacement_entries[] = {
        {"reload", "transmit_hang_ms", "200"},
    };
    struct ra_config_entry reload_candidate_entries[] = {
        {"reload", "transmit_hang_ms", "300"},
    };
    struct ra_config_entry reload_disabled_entries[] = {
        {"reload", "node_enabled", "no"},
    };
    struct ra_document reload_current = {.sections = reload_sections,
                                         .section_count = 1,
                                         .entries = reload_current_entries,
                                         .count = 1};
    struct ra_document reload_replacement = {.sections = reload_sections,
                                             .section_count = 1,
                                             .entries = reload_replacement_entries,
                                             .count = 1};
    struct ra_document reload_candidate = {.sections = reload_sections,
                                           .section_count = 1,
                                           .entries = reload_candidate_entries,
                                           .count = 1};
    struct ra_document reload_disabled = {.sections = reload_sections,
                                          .section_count = 1,
                                          .entries = reload_disabled_entries,
                                          .count = 1};
    struct ra_runtime reload_runtime = {.digit = receive_inbound_digit};
    allocations = calls = starts = hub_closes = 0;
    inbound_callback_count = inbound_digits = 0;
    retained_link_state = false;
    assert(!ra_runtime_start(&reload_runtime, &reload_current));
    assert(workers == 1 && channels == 1 && inbound_callback_count == 1);
    assert(!connect_fixture(&reload_runtime, "reload"));
    retained_link_state = true;
    unsigned int reload_workers = workers;
    unsigned int reload_channels = channels;
    unsigned int reload_hub_closes = hub_closes;
    assert(!ra_runtime_reload(&reload_runtime, &reload_current, &reload_replacement));
    assert(workers == reload_workers && channels == reload_channels &&
           hub_closes == reload_hub_closes && inbound_callback_count == 1);
    inbound_callbacks[0](inbound_callback_contexts[0], "123", '6', 1300);
    assert(inbound_digits == 1 && !strcmp(inbound_node, "reload") && inbound_digit == '6' &&
           inbound_now_ms == 1300);

    /* A candidate that cannot start restores the radio without closing retained links. */
    fail_worker = starts + 1;
    assert(ra_runtime_reload(&reload_runtime, &reload_replacement, &reload_candidate));
    fail_worker = 0;
    assert(workers == reload_workers && channels == reload_channels &&
           hub_closes == reload_hub_closes && inbound_callback_count == 1);

    /* Rate-bound hub buffers reject a candidate rate while retaining both routing and radio. */
    next_sample_rate = 48000;
    assert(ra_runtime_reload(&reload_runtime, &reload_replacement, &reload_candidate));
    assert(!next_sample_rate && workers == reload_workers && channels == reload_channels &&
           hub_closes == reload_hub_closes && inbound_callback_count == 1);

    /* An empty former hub can change rate by being closed and initialized at the new rate. */
    retained_link_state = false;
    rate = 32000;
    assert(!ra_runtime_reload(&reload_runtime, &reload_replacement, &reload_candidate));
    assert(workers == reload_workers && channels == reload_channels &&
           hub_closes == reload_hub_closes + 1 && inbound_callback_count == 2);
    rate = 16000;

    /* Removing a retained node ends its peers and cancels its recovery state at commit. */
    retained_link_state = true;
    assert(!ra_runtime_reload(&reload_runtime, &reload_candidate, &reload_disabled));
    assert(!reload_runtime.nodes && !workers && !channels && hub_closes == reload_hub_closes + 2);

    /* An omitted retained node follows the same committed release path. */
    inbound_callback_count = 0;
    struct ra_runtime omitted_runtime = {.digit = receive_inbound_digit};
    assert(!ra_runtime_start(&omitted_runtime, &reload_current));
    unsigned int omitted_hub_closes = hub_closes;
    assert(!ra_runtime_reload(&omitted_runtime, &reload_current, &empty));
    assert(!omitted_runtime.nodes && !workers && !channels && hub_closes == omitted_hub_closes + 1);

    /* Rollback leaves unaffected nodes running while it restores only the failed replacement. */
    char *two_node_sections[] = {"one", "untouched"};
    char *one_node_sections[] = {"one"};
    char *added_node_sections[] = {"one", "added"};
    char *disabled_node_sections[] = {"one", "added", "disabled"};
    char *failed_add_sections[] = {"one", "added", "failed"};
    struct ra_config_entry disabled_node_entries[] = {
        {"added", "node_enabled", "no"},
        {"disabled", "node_enabled", "no"},
    };
    struct ra_document two_node_current = {.sections = two_node_sections, .section_count = 2};
    struct ra_document one_node_replacement = {.sections = one_node_sections, .section_count = 1};
    struct ra_document added_node_replacement = {.sections = added_node_sections,
                                                 .section_count = 2};
    struct ra_document disabled_node_replacement = {.sections = disabled_node_sections,
                                                    .section_count = 3,
                                                    .entries = disabled_node_entries,
                                                    .count = 2};
    struct ra_document failed_add_replacement = {.sections = failed_add_sections,
                                                 .section_count = 3};
    struct ra_runtime two_node_runtime = {.digit = receive_inbound_digit};
    allocations = calls = starts = 0;
    inbound_callback_count = inbound_digits = 0;
    assert(!ra_runtime_start(&two_node_runtime, &two_node_current));
    assert(workers == 2 && channels == 2 && inbound_callback_count == 2);
    fail_worker = starts + 1;
    assert(ra_runtime_reload(&two_node_runtime, &two_node_current, &one_node_replacement));
    fail_worker = 0;
    assert(workers == 2 && channels == 2);
    ra_runtime_stop(&two_node_runtime);
    assert(!two_node_runtime.nodes && !workers && !channels);

    /* Rollback attempts every changed node even when restoring the first one also fails. */
    struct ra_runtime multi_restore_runtime = {.digit = receive_inbound_digit};
    assert(!ra_runtime_start(&multi_restore_runtime, &two_node_current));
    fail_worker_from = starts + 2;
    fail_worker_count = 3;
    assert(ra_runtime_reload(&multi_restore_runtime, &two_node_current, &two_node_current));
    assert(!fail_worker_count && !workers && channels == 2);
    fail_worker_from = 0;
    ra_runtime_stop(&multi_restore_runtime);
    assert(!multi_restore_runtime.nodes && !workers && !channels);

    /* New nodes commit only after startup, and omitted empty nodes release after commit. */
    struct ra_runtime added_node_runtime = {.digit = receive_inbound_digit};
    allocations = calls = starts = 0;
    inbound_callback_count = inbound_digits = 0;
    unsigned int added_hub_closes = hub_closes;
    assert(!ra_runtime_start(&added_node_runtime, &one_node_replacement));
    assert(!ra_runtime_reload(&added_node_runtime, &one_node_replacement, &added_node_replacement));
    assert(workers == 2 && channels == 2 && inbound_callback_count == 2);
    assert(!ra_runtime_reload(&added_node_runtime, &added_node_replacement,
                              &disabled_node_replacement));
    assert(workers == 1 && channels == 1 && hub_closes == added_hub_closes + 1);

    /* A missing section follows the same safe post-commit release path as a disabled section. */
    assert(!ra_runtime_reload(&added_node_runtime, &disabled_node_replacement,
                              &added_node_replacement));
    assert(workers == 2 && channels == 2);
    assert(!ra_runtime_reload(&added_node_runtime, &added_node_replacement, &one_node_replacement));
    assert(workers == 1 && channels == 1 && hub_closes == added_hub_closes + 2);

    /* A later new-node failure releases earlier tentative nodes before restoring the original. */
    fail_worker = starts + 3;
    assert(ra_runtime_reload(&added_node_runtime, &one_node_replacement, &failed_add_replacement));
    fail_worker = 0;
    assert(workers == 1 && channels == 1 && hub_closes == added_hub_closes + 4);
    ra_runtime_stop(&added_node_runtime);
    assert(!added_node_runtime.nodes && !workers && !channels &&
           hub_closes == added_hub_closes + 5);

    /* An empty runtime rejects an allocation failure before it can acquire any link ownership. */
    struct ra_runtime allocation_runtime = {.digit = receive_inbound_digit};
    fail_allocation = allocations + 1;
    assert(ra_runtime_reload(&allocation_runtime, &empty, &one_node_replacement));
    fail_allocation = 0;
    assert(!allocation_runtime.nodes && !workers && !channels);

    /* If the former radio cannot restart either, return that diagnostic and release it cleanly. */
    struct ra_runtime restoration_runtime = {.digit = receive_inbound_digit};
    assert(!ra_runtime_start(&restoration_runtime, &one_node_replacement));
    fail_worker_from = starts + 1;
    fail_worker_count = 2;
    assert(ra_runtime_reload(&restoration_runtime, &one_node_replacement, &added_node_replacement));
    assert(!fail_worker_count && !workers && channels == 1);
    fail_worker_from = 0;
    ra_runtime_stop(&restoration_runtime);
    assert(!restoration_runtime.nodes && !workers && !channels);

    /* Failed tentative allocation reports a failed restoration when the old radio also fails. */
    struct ra_runtime allocation_restore_runtime = {.digit = receive_inbound_digit};
    assert(!ra_runtime_start(&allocation_restore_runtime, &one_node_replacement));
    fail_callback_lock = true;
    fail_worker_from = starts + 2;
    fail_worker_count = 1;
    assert(ra_runtime_reload(&allocation_restore_runtime, &one_node_replacement,
                             &added_node_replacement));
    assert(!fail_worker_count && !workers && channels == 1);
    fail_callback_lock = false;
    fail_worker_from = 0;
    ra_runtime_stop(&allocation_restore_runtime);
    assert(!allocation_restore_runtime.nodes && !workers && !channels);

    /* Failed tentative startup likewise returns a restoration error without retaining new hubs. */
    struct ra_runtime startup_restore_runtime = {.digit = receive_inbound_digit};
    assert(!ra_runtime_start(&startup_restore_runtime, &one_node_replacement));
    fail_worker_from = starts + 2;
    fail_worker_count = 2;
    assert(ra_runtime_reload(&startup_restore_runtime, &one_node_replacement,
                             &added_node_replacement));
    assert(!fail_worker_count && !workers && channels == 1);
    fail_worker_from = 0;
    ra_runtime_stop(&startup_restore_runtime);
    assert(!startup_restore_runtime.nodes && !workers && !channels);

    /* A reader callback sees the worker stopped before hub close joins and releases it. */
    struct ra_runtime closing_callback_runtime = {.digit = receive_inbound_digit};
    inbound_callback_count = inbound_digits = 0;
    assert(!ra_runtime_start(&closing_callback_runtime, &one_node_replacement));
    invoke_closed_digit = true;
    ra_runtime_stop(&closing_callback_runtime);
    invoke_closed_digit = false;
    assert(!closing_callback_runtime.nodes && !workers && !channels && !inbound_digits);

    /* A node without a module DTMF sink safely discards authenticated reader events. */
    struct ra_runtime no_digit_runtime = {0};
    inbound_callback_count = inbound_event_callback_count = inbound_digits = 0;
    assert(!ra_runtime_start(&no_digit_runtime, &document));
    assert(inbound_callback_count == 2);
    assert(inbound_event_callback_count == 2);
    inbound_callbacks[0](inbound_callback_contexts[0], "123", '4', 1200);
    inbound_event_callbacks[0](inbound_event_contexts[0], "123", true);
    assert(!inbound_digits);
    ra_runtime_stop(&no_digit_runtime);

    inbound_callback_count = inbound_event_callback_count = inbound_events = 0;
    assert(!ra_runtime_start(&runtime, &document));
    assert(workers == 2 && channels == 2);
    assert(inbound_callback_count == 2);
    assert(inbound_event_callback_count == 2);
    assert(inbound_callbacks[0] && inbound_callback_contexts[0]);
    inbound_callbacks[0](inbound_callback_contexts[0], "123", '5', 1234);
    assert(inbound_digits == 1 && !strcmp(inbound_node, "alpha") && inbound_digit == '5' &&
           inbound_now_ms == 1234);
    /* A denied outbound audio peer cannot inject local link-control DTMF. */
    inbound_callbacks[1](inbound_callback_contexts[1], "123", '6', 1235);
    assert(inbound_digits == 1);
    inbound_event_callbacks[0](inbound_event_contexts[0], "123", true);
    assert(inbound_events == 1);
    assert(!strcmp(inbound_event_remote, "123"));
    assert(inbound_event_connected);

    /* Exercise recovery through the hub-registered callback at every ownership boundary. */
    atomic_bool cancelled;
    atomic_bool paused;
    atomic_init(&cancelled, false);
    atomic_init(&paused, false);
    reconnect_cancelled = &cancelled;
    reconnect_paused = &paused;
    reconnect_attachments = 0;
    reconnect_cancellations = 0;
    reconnect_pauses = 0;
    link_hangups = 0;

    atomic_store(&cancelled, true);
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    atomic_store(&cancelled, false);
    atomic_store(&paused, true);
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    atomic_store(&paused, false);

    link_error = 1;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    link_error = 2;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    link_error = 3;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    link_error = 4;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(link_hangups == 1);
    link_error = 5;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(link_hangups == 2);

    link_error = 0;
    reconnect_race = RECONNECT_RACE_AFTER_LOOKUP;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(atomic_load(&cancelled));
    atomic_store(&cancelled, false);
    reconnect_race = RECONNECT_RACE_AFTER_OFFER;
    reconnect_race_paused = true;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(atomic_load(&paused));
    atomic_store(&paused, false);
    reconnect_race_paused = false;

    reconnect_race = RECONNECT_RACE_AFTER_DIAL;
    unsigned int hangups_before = link_hangups;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(atomic_load(&cancelled) && link_hangups == hangups_before + 1);
    atomic_store(&cancelled, false);
    reconnect_race = RECONNECT_RACE_AFTER_DIAL;
    reconnect_race_paused = true;
    hangups_before = link_hangups;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(atomic_load(&paused) && link_hangups == hangups_before + 1);
    atomic_store(&paused, false);
    reconnect_race_paused = false;

    reconnect_race = RECONNECT_RACE_AFTER_ATTACH;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(atomic_load(&cancelled) && reconnect_cancellations == 1);
    atomic_store(&cancelled, false);
    reconnect_race = RECONNECT_RACE_AFTER_ATTACH;
    reconnect_race_paused = true;
    assert(reconnect_fixture(&cancelled, &paused) == -1);
    assert(atomic_load(&paused) && reconnect_pauses == 1);
    atomic_store(&paused, false);
    reconnect_race_paused = false;
    assert(!reconnect_fixture(&cancelled, &paused));
    assert(reconnect_attachments == 4);
    reconnect_cancelled = NULL;
    reconnect_paused = NULL;
    link_hangups = 0;

    struct ra_link_operation operation;
    struct ra_link_peer_status listed[1] = {0};
    size_t listed_count = 99;
    assert(!ra_runtime_link_snapshot(&runtime, "missing", listed, 1, &listed_count));
    assert(listed_count == 99);
    assert(!ra_runtime_link_topology(&runtime, "missing"));
    status_topology_failure = true;
    assert(!ra_runtime_link_topology(&runtime, "alpha"));
    status_topology_failure = false;
    status_topology = "T123,R456";
    char *topology = ra_runtime_link_topology(&runtime, "alpha");
    assert(topology && !strcmp(topology, "T123,R456"));
    ast_free(topology);
    assert(ra_runtime_link_snapshot(&runtime, "alpha", NULL, 0, NULL));
    assert(ra_runtime_link_snapshot(&runtime, "alpha", NULL, 0, &listed_count));
    assert(!listed_count);
    queued_status_count = 0;
    queued_status_limit = SIZE_MAX;
    release_status_audio = true;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "NO LINKS"));
    assert(!ra_runtime_queue_time(&runtime, "alpha"));
    assert(!strcmp(queued_status, "1:07 PM"));
    assert(!strcmp(prepared_speech, "Good Afternoon. The time is 1:07 PM."));
    assert(ra_runtime_queue_time(&runtime, "missing") == -1);
    fail_wall_clock = true;
    assert(ra_runtime_queue_time(&runtime, "alpha") == -1);
    fail_wall_clock = false;
    fail_localtime = true;
    assert(ra_runtime_queue_time(&runtime, "alpha") == -1);
    fail_localtime = false;
    invalid_localtime = true;
    assert(ra_runtime_queue_time(&runtime, "alpha") == -1);
    invalid_localtime = false;
    queued_status_count = 0;
    assert(!ra_runtime_queue_link_event(&runtime, "alpha", "123", true));
    assert(queued_status_count == 2 && !strcmp(queued_status, "123 CONNECTED"));
    assert(!strcmp(prepared_speech, "node,1,2,3 CONNECTED"));
    assert(!ra_runtime_queue_link_event(&runtime, "alpha", "beta", false));
    assert(!strcmp(queued_status, "beta DISCONNECTED"));
    char oversized_event[RA_CONTROLLER_STATUS_TEXT_MAX + 1];
    memset(oversized_event, '1', sizeof(oversized_event) - 1);
    oversized_event[sizeof(oversized_event) - 1] = '\0';
    assert(ra_runtime_queue_link_event(&runtime, oversized_event, "123", true) == -1);
    assert(ra_runtime_queue_link_event(&runtime, "alpha", oversized_event, true) == -1);
    char nearly_full_event[RA_CONTROLLER_STATUS_TEXT_MAX];
    memset(nearly_full_event, '1', sizeof(nearly_full_event) - 1);
    nearly_full_event[sizeof(nearly_full_event) - 1] = '\0';
    size_t event_status_count = queued_status_count;
    assert(ra_runtime_queue_link_event(&runtime, nearly_full_event, "123", true) == -1);
    assert(queued_status_count == event_status_count);
    assert(ra_runtime_queue_link_event(&runtime, nearly_full_event, "foreign", true) == -1);
    assert(queued_status_count == event_status_count);
    assert(ra_runtime_queue_link_event(&runtime, "foreign", nearly_full_event, true) == -1);
    assert(queued_status_count == event_status_count);
    queued_status_limit = 0;
    assert(ra_runtime_queue_link_event(&runtime, "alpha", "123", true) == -1);
    queued_status_limit = SIZE_MAX;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", true));
    assert(!strcmp(queued_status, "NO LAST KEYED"));
    status_peers[0] = (struct ra_link_peer_status){
        .name = "123", .transmit = true, .forward = true, .permanent = true};
    status_peer_count = 1;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "LINK 123 TRANSCEIVE"));
    status_peers[0].retrying = true;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "LINK 123 TRANSCEIVE"));
    status_peers[0].paused = true;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "LINK 123 TRANSCEIVE"));
    status_last_keyed[0] = '1';
    status_last_keyed[1] = '2';
    status_last_keyed[2] = '3';
    status_last_keyed[3] = '\0';
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", true));
    assert(!strcmp(queued_status, "LAST KEYED 123"));
    memset(status_peers[0].name, '1', sizeof(status_peers[0].name));
    assert(ra_runtime_queue_link_status(&runtime, "alpha", false) == -1);
    status_peers[0] = (struct ra_link_peer_status){.name = "234", .forward = true};
    status_peer_count = 12;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "12 LINKS 234 MONITOR"));
    status_peers[0].permanent = true;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "12 LINKS 234 MONITOR"));
    status_peers[0].retrying = true;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "12 LINKS 234 MONITOR"));
    status_peers[0].paused = true;
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "12 LINKS 234 MONITOR"));
    memset(status_peers[0].name, '1', sizeof(status_peers[0].name));
    assert(ra_runtime_queue_link_status(&runtime, "alpha", false) == -1);
    status_peers[0] = (struct ra_link_peer_status){.name = "234", .forward = false};
    assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    assert(!strcmp(queued_status, "12 LINKS 234 LOCAL"));
    assert(ra_runtime_link_snapshot(&runtime, "alpha", listed, 1, &listed_count) &&
           listed_count == 12 && !strcmp(listed[0].name, "234"));
    status_peer_count = 1;
    queued_status_count = 0;
    queued_status_limit = RA_CONTROLLER_STATUS_QUEUE_DEPTH;
    for (size_t index = 0; index < RA_CONTROLLER_STATUS_QUEUE_DEPTH; ++index) {
        assert(!ra_runtime_queue_link_status(&runtime, "alpha", false));
    }
    assert(ra_runtime_queue_link_status(&runtime, "alpha", false) == -1);
    queued_status_limit = SIZE_MAX;
    assert(ra_runtime_queue_link_status(&runtime, "missing", false) == -1);
    assert(!ra_runtime_digit(&runtime, "missing", '*', 0, &operation));
    struct ra_runtime no_last_runtime = {0};
    assert(!ra_runtime_start(&no_last_runtime, &document));
    assert(!digits_fixture(&no_last_runtime, "*30#", &operation));
    ra_runtime_stop(&no_last_runtime);
    assert(!digits_fixture(&runtime, "*30#", &operation));
    assert(!digits_fixture(&runtime, "*99#", &operation));
    assert(digits_fixture(&runtime, "*3123#", &operation));
    assert(operation.action == RA_LINK_TRANSCEIVE && !strcmp(operation.remote, "123"));
    assert(digits_fixture(&runtime, "*30#", &operation));
    assert(operation.action == RA_LINK_TRANSCEIVE && !strcmp(operation.remote, "123"));
    assert(digits_fixture(&runtime, "*70", &operation));
    assert(operation.action == RA_LINK_STATUS && !*operation.remote);
    assert(digits_fixture(&runtime, "*10", &operation));
    assert(operation.action == RA_LINK_DISCONNECT_NONPERMANENT_ALL && !*operation.remote);
    assert(digits_fixture(&runtime, "*722", &operation));
    assert(operation.action == RA_LINK_TIME && !*operation.remote);
    assert(!digits_fixture(&runtime,
                           "*31234567890123456789012345678901234567890123456789012345678901234#",
                           &operation));
    assert(!digits_fixture(&runtime, "*3", &operation));
    ra_runtime_reset_digits(&runtime);
    assert(!digits_fixture(&runtime, "123#", &operation));
    assert(!ra_runtime_authorize(&runtime, "missing", "123", "127.0.0.1"));
    assert(ra_runtime_authorize(&runtime, "alpha", "123", "127.0.0.1"));
    assert(ra_runtime_accept(&runtime, "missing", "123", NULL, true) == -1);
    assert(ra_runtime_accept(&runtime, "alpha", "alpha", NULL, true) == -1);
    assert(ra_runtime_accept(&runtime, "alpha", "123", NULL, false) == -1);
    assert(
        !ra_runtime_accept(&runtime, "alpha", "123", (struct ast_channel *)&link_identity, true));
    assert(!ra_runtime_disconnect(&runtime, "missing", "123"));
    assert(!ra_runtime_disconnect_all(&runtime, "missing"));
    assert(!ra_runtime_disconnect_nonpermanent_all(&runtime, "missing"));
    assert(!ra_runtime_reconnect_all(&runtime, "missing"));
    assert(!ra_runtime_retain_permanent_link(&runtime, "missing", "123", true, true));
    assert(!ra_runtime_retain_permanent_link(&runtime, "alpha", "alpha", true, true));
    link_error = 10;
    assert(!ra_runtime_retain_permanent_link(&runtime, "alpha", "123", true, true));
    link_error = 0;
    assert(ra_runtime_retain_permanent_link(&runtime, "alpha", "123", true, true));
    assert(retained_permanent_links == 2);
    assert(!ra_runtime_disconnect_all(&runtime, "alpha"));
    assert(!ra_runtime_disconnect_nonpermanent_all(&runtime, "alpha"));
    assert(!ra_runtime_reconnect_all(&runtime, "alpha"));
    link_error = 7;
    assert(!ra_runtime_disconnect(&runtime, "alpha", "123"));
    assert(!ra_runtime_disconnect_permanent(&runtime, "alpha", "123"));
    link_error = 0;
    assert(ra_runtime_disconnect(&runtime, "alpha", "123"));
    assert(!ra_runtime_disconnect_permanent(&runtime, "missing", "123"));
    assert(ra_runtime_disconnect_permanent(&runtime, "alpha", "123"));
    assert(connect_fixture(&runtime, "missing") == -1);
    assert(ra_runtime_prepare_link(&runtime, "alpha", "123", NULL) == -1);
    assert(ra_runtime_prepare_link(&runtime, "alpha", "alpha", NULL) == -1);
    assert(ra_runtime_attach_link(&runtime, "missing", "123", NULL, true, true, false) == -1);
    assert(ra_runtime_attach_link(&runtime, "alpha", "alpha", NULL, true, true, false) == -1);
    link_dials = 0;
    assert(!connect_fixture(&runtime, "alpha"));
    assert(link_dials == 1);
    for (link_error = 1; link_error <= 5; ++link_error) {
        assert(connect_fixture(&runtime, "alpha") == -1);
    }
    assert(link_hangups == 2);
    link_error = 8;
    assert(connect_fixture(&runtime, "alpha") == -1);
    link_error = 9;
    link_dials = 0;
    assert(!connect_fixture(&runtime, "alpha") && link_dials == 2);
    link_error = 0;
    assert(!ra_link_dial_run(NULL, "alpha"));
    struct ra_link_dial empty_dial = {0};
    assert(!ra_link_dial_run(&empty_dial, "alpha"));
    struct ra_link_dial missing_candidates = {.destination = strdup("fixture")};
    assert(!ra_link_dial_run(&missing_candidates, "alpha"));
    struct ast_format **one_candidate = calloc(1, sizeof(*one_candidate));
    assert(one_candidate);
    one_candidate[0] = (struct ast_format *)&link_identity;
    struct ra_link_dial empty_candidates = {.destination = strdup("fixture"),
                                            .candidates = one_candidate};
    assert(!ra_link_dial_run(&empty_candidates, "alpha"));
    clock_calls = 0;
    fail_clock_call = 1;
    assert(!connect_fixture(&runtime, "alpha"));
    fail_clock_call = 0;
    clock_calls = 0;
    fail_clock_call = 2;
    assert(connect_fixture(&runtime, "alpha") == -1);
    fail_clock_call = 0;
    clock_calls = 0;
    clock_sequence[0] = (struct timespec){.tv_sec = 10};
    clock_sequence[1] = (struct timespec){.tv_sec = 9};
    clock_sequence_count = 2;
    clock_sequence_index = 0;
    assert(connect_fixture(&runtime, "alpha") == -1);
    clock_sequence[1] = (struct timespec){.tv_sec = 30};
    clock_sequence_index = 0;
    assert(connect_fixture(&runtime, "alpha") == -1);
    clock_sequence_count = 0;
    clock_sequence_index = 0;
    link_error = 1;
    assert(!ra_runtime_authorize(&runtime, "alpha", "123", "127.0.0.1"));
    link_error = 0;
    assert(!connect_fixture(&runtime, "alpha"));
    operation.digit = '9';
    assert(digits_fixture(&runtime, "*4123#", &operation));
    assert(operation.action == RA_LINK_COMMAND && !operation.digit &&
           !strcmp(operation.remote, "123"));
    assert(!ra_runtime_remote_command(&runtime, "alpha", operation.remote, operation.digit));
    assert(ra_runtime_disconnect(&runtime, "alpha", "123"));
    assert(!ra_runtime_digit(&runtime, "alpha", '5', 150, &operation));
    assert(!ra_runtime_remote_command(&runtime, "alpha", "123", 0));
    assert(!ra_runtime_remote_command(&runtime, "alpha", "123", 0));
    assert(ra_runtime_disconnect_permanent(&runtime, "alpha", "123"));
    assert(!ra_runtime_digit(&runtime, "alpha", '5', 175, &operation));
    assert(!ra_runtime_remote_command(&runtime, "alpha", "123", 0));
    assert(!ra_runtime_remote_command(&runtime, "alpha", "123", 0));
    assert(!ra_runtime_disconnect_all(&runtime, "alpha"));
    assert(!ra_runtime_digit(&runtime, "alpha", '5', 190, &operation));
    assert(!ra_runtime_remote_command(&runtime, "alpha", "123", 0));
    assert(ra_runtime_digit(&runtime, "alpha", '5', 200, &operation));
    assert(operation.action == RA_LINK_COMMAND && operation.digit == '5');
    assert(!ra_runtime_remote_command(&runtime, "alpha", operation.remote, operation.digit));
    assert(!ra_runtime_digit(&runtime, "alpha", '#', 300, &operation));
    assert(!ra_runtime_digit(&runtime, "alpha", '5', 400, &operation));
    link_error = 6;
    assert(ra_runtime_remote_command(&runtime, "alpha", "123", 0) == -1);
    assert(ra_runtime_remote_command(&runtime, "alpha", "123", '1') == -1);
    link_error = 0;
    assert(!ra_runtime_remote_command(&runtime, "alpha", "123", 0));
    assert(!ra_runtime_digit(&runtime, "alpha", 0, 500, &operation));
    assert(!ra_runtime_digit(&runtime, "alpha", 'Z', 550, &operation));
    link_error = 6;
    assert(ra_runtime_digit(&runtime, "alpha", '1', 600, &operation));
    assert(ra_runtime_remote_command(&runtime, "alpha", operation.remote, operation.digit) == -1);
    link_error = 0;
    link_error = 1;
    assert(ra_runtime_remote_command(&runtime, "alpha", "123", 0) == -1);
    link_error = 0;
    assert(!connect_fixture(&runtime, "beta"));
    assert(ra_runtime_remote_command(&runtime, "beta", "123", 0) == -1);
    assert(ra_runtime_remote_command(&runtime, "missing", "123", 0) == -1);
    ra_runtime_stop(&runtime);
    assert(!runtime.nodes && !workers && !channels);
    entries[6].value = "";
    seen_ids = 0;
    assert(!ra_runtime_start(&runtime, &document));
    assert(!seen_ids);
    ra_runtime_stop(&runtime);
    prepared = true;
    assert(!ra_runtime_start(&runtime, &document));
    assert(seen_ids == 1);
    ra_runtime_stop(&runtime);
    ra_runtime_stop(&runtime);
    puts("configured node startup and joined resource cleanup passed");
    return 0;
}
