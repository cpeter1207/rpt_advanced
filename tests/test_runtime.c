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
/** @brief Call number selected for failure. */
static unsigned int fail_call;
/** @brief Worker start number selected for failure. */
static unsigned int fail_worker;
/** @brief Call counter. */
static unsigned int calls;
/** @brief Worker start counter. */
static unsigned int starts;
/** @brief Negotiated linear rate. */
static unsigned int rate = 16000;
/** @brief Provide prepared media even when no Morse fallback exists. */
static bool prepared;
/** @brief Count usable identifier sets bound to successful worker starts. */
static size_t seen_ids;
/** @brief Selected link failure: lookup, offer, dial, answer, or attachment. */
static unsigned int link_error;
/** @brief Opaque channel and capability identity. */
static int link_identity;
/** @brief Number of link channels released after a failed attachment. */
static unsigned int link_hangups;

char *ra_link_directory_lookup(const char *node, const char *peer_ip, const char *directory_file) {
    assert(!strcmp(node, "123") && !*directory_file);
    assert(!peer_ip || !strcmp(peer_ip, "127.0.0.1"));
    return link_error == 1 ? NULL : strdup("radio@127.0.0.1/123");
}

struct ast_format_cap *ra_media_offer(struct ast_format *radio) {
    (void)radio;
    return link_error == 2 ? NULL : (struct ast_format_cap *)&link_identity;
}

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
    assert(timeout == 20000 && reason && !strcmp(cid_num, "alpha") && !strcmp(cid_name, "alpha"));
    return link_error == 3 ? NULL : (struct ast_channel *)&link_identity;
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

int ra_link_hub_attach(struct ra_link_hub *hub, const char *name, struct ast_channel *channel,
                       struct ast_format *linear, bool transmit, bool forward, bool permanent) {
    (void)permanent;
    (void)linear;
    assert(hub && !strcmp(name, "123") && channel == (struct ast_channel *)&link_identity);
    assert(transmit && forward);
    return link_error == 5 ? -1 : 0;
}

void ra_link_hub_set_reconnector(struct ra_link_hub *hub, ra_link_reconnect_fn callback,
                                 void *context) {
    (void)callback;
    (void)context;
    assert(hub);
}

bool ra_link_hub_disconnect(struct ra_link_hub *hub, const char *name) {
    assert(hub && !strcmp(name, "123"));
    return true;
}

void ra_link_hub_close(struct ra_link_hub *hub) { assert(hub); }

void ra_identifier_prepare(const struct ra_identifier_settings *settings, unsigned int selected,
                           int16_t **audio, size_t *samples) {
    assert(settings->morse_text && selected == rate);
    *audio = prepared ? malloc(sizeof(**audio)) : NULL;
    *samples = prepared ? 1 : 0;
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
/** @brief Deterministic startup clock.
 * @param clock Requested clock identifier.
 * @param value Receives startup time.
 * @return Zero or an injected failure.
 */
int __wrap_clock_gettime(clockid_t clock, struct timespec *value) {
    assert(clock == CLOCK_MONOTONIC);
    *value = (struct timespec){.tv_sec = 10};
    return fail_clock ? -1 : 0;
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
    if (++starts == fail_worker) {
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
    assert(!dial.destination && !dial.offer);
    if (!channel) {
        return -1;
    }
    int result = ra_runtime_attach_link(runtime, local, "123", channel, true, true, false);
    if (result) {
        ast_hangup(channel);
    }
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
    char *sections[] = {"alpha", "disabled", "beta", "identifier alpha periodic"};
    struct ra_config_entry entries[] = {
        {"disabled", "node_enabled", "no"},
        {"identifier alpha periodic", "morse_text", "TEST"},
    };
    struct ra_document document = {
        .sections = sections, .section_count = 4, .entries = entries, .count = 2};
    struct ra_runtime runtime = {0};
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
    assert(!ra_runtime_start(&runtime, &document));
    assert(workers == 2 && channels == 2);
    struct ra_link_operation operation;
    assert(!ra_runtime_digit(&runtime, "missing", '*', 0, &operation));
    assert(!digits_fixture(&runtime, "*30#", &operation));
    assert(!digits_fixture(&runtime, "*99#", &operation));
    assert(digits_fixture(&runtime, "*3123#", &operation));
    assert(operation.action == RA_LINK_TRANSCEIVE && !strcmp(operation.remote, "123"));
    assert(digits_fixture(&runtime, "*10#", &operation));
    assert(operation.action == RA_LINK_DISCONNECT && !strcmp(operation.remote, "123"));
    assert(digits_fixture(&runtime, "*70", &operation));
    assert(operation.action == RA_LINK_STATUS && !*operation.remote);
    assert(!digits_fixture(&runtime,
                           "*31234567890123456789012345678901234567890123456789012345678901234#",
                           &operation));
    assert(!digits_fixture(&runtime, "*3", &operation));
    ra_runtime_reset_digits(&runtime);
    assert(!digits_fixture(&runtime, "123#", &operation));
    assert(!ra_runtime_authorize(&runtime, "missing", "123", "127.0.0.1"));
    assert(ra_runtime_authorize(&runtime, "alpha", "123", "127.0.0.1"));
    assert(ra_runtime_accept(&runtime, "missing", "123", NULL, true, false) == -1);
    assert(ra_runtime_accept(&runtime, "alpha", "123", NULL, false, false) == -1);
    assert(!ra_runtime_accept(&runtime, "alpha", "123", (struct ast_channel *)&link_identity, true,
                              false));
    assert(!ra_runtime_disconnect(&runtime, "missing", "123"));
    assert(ra_runtime_disconnect(&runtime, "alpha", "123"));
    assert(connect_fixture(&runtime, "missing") == -1);
    assert(ra_runtime_attach_link(&runtime, "missing", "123", NULL, true, true, false) == -1);
    for (link_error = 1; link_error <= 5; ++link_error) {
        assert(connect_fixture(&runtime, "alpha") == -1);
    }
    assert(link_hangups == 2);
    link_error = 1;
    assert(!ra_runtime_authorize(&runtime, "alpha", "123", "127.0.0.1"));
    link_error = 0;
    assert(!connect_fixture(&runtime, "alpha"));
    ra_runtime_stop(&runtime);
    assert(!runtime.nodes && !workers && !channels);
    entries[1].value = "";
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
