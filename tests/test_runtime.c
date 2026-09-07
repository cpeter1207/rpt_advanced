/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Node startup, ownership transfer, and complete partial-failure cleanup.
 */
#include "assets.h"
#include "connection.h"
#include "runtime.h"
#include "schema.h"
#include "worker.h"
#include <assert.h>
#include <asterisk.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <stdlib.h>
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
