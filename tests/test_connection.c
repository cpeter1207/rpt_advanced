/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Verify fixed 48 kHz native radio reservation and ownership cleanup.
 */
#include <asterisk.h>

#include "connection.h"
#include <assert.h>
#include <asterisk/astobj2.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/format_cap.h>
#include <stdio.h>
#include <string.h>

/** @brief Opaque format fixture with explicit owned-reference accounting. */
struct ast_format {
    unsigned int rate; /**< Sample clock reported by the public API. */
    int references;    /**< References held by production code. */
};
/** @brief Required native signed-linear format. */
static struct ast_format native = {.rate = 48000};
/** @brief Unsupported native-rate fixture. */
static struct ast_format wrong_rate = {.rate = 16000};
/** @brief Unsupported non-linear fixture at the otherwise valid rate. */
static struct ast_format wrong_format = {.rate = 48000};
/** @brief Format presently advertised by the backend capability. */
static struct ast_format *advertised = &native;
/** @brief Cached 48 kHz signed-linear format. */
static struct ast_format *cached_linear = &native;
/** @brief Native interface descriptor, whose public fields are borrowed. */
static const struct ast_channel_tech technology = {.type = "RadioPlusAdvanced"};
/** @brief Inject one of the six fallible operation results. */
static int failure;
/** @brief Count reserved channels. */
static int channels;

/** @brief Look up the required native interface.
 * @param name Requested technology.
 * @return Descriptor or injected absence.
 */
const struct ast_channel_tech *ast_get_channel_tech(const char *name) {
    assert(!strcmp(name, "RadioPlusAdvanced"));
    return failure == 1 ? NULL : &technology;
}

/** @brief Reference the backend's advertised format.
 * @param cap Borrowed capability set.
 * @param position Native format position.
 * @return Owned format or injected absence.
 */
struct ast_format *ast_format_cap_get_format(const struct ast_format_cap *cap, int position) {
    assert(cap == technology.capabilities && position == 0);
    if (failure == 2) {
        return NULL;
    }
    ++advertised->references;
    return advertised;
}

/** @brief Release a fixture format reference.
 * @param object Format or null.
 * @param tag Unused diagnostic tag.
 * @param file Unused source filename.
 * @param line Unused source line.
 * @param function Unused function name.
 */
void __ao2_cleanup_debug(void *object, const char *tag, const char *file, int line,
                         const char *function) {
    (void)tag;
    (void)file;
    (void)line;
    (void)function;
    if (object) {
        struct ast_format *format = object;
        assert(format->references > 0);
        --format->references;
    }
}

/** @brief Return a format's native sample rate.
 * @param format Fixture format.
 * @return Samples per second.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) { return format->rate; }

/** @brief Return the registered signed-linear format without acquiring a reference.
 * @param rate Requested PCM rate.
 * @return Cached format or null for the injected absence.
 */
struct ast_format *ast_format_cache_get_slin_by_rate(unsigned int rate) {
    assert(rate == 48000);
    return failure == 3 ? NULL : cached_linear;
}

/** @brief Compare fixture format identity.
 * @param first First format.
 * @param second Second format.
 * @return Equality result.
 */
enum ast_format_cmp_res ast_format_cmp(const struct ast_format *first,
                                       const struct ast_format *second) {
    return first == second ? AST_FORMAT_CMP_EQUAL : AST_FORMAT_CMP_NOT_EQUAL;
}

/** @brief Reserve the native hardware channel without calling it.
 * @param type Requested technology.
 * @param request_cap Backend native capabilities.
 * @param assignedids No assigned identifiers.
 * @param requestor No requesting channel.
 * @param addr Configured radio name.
 * @param cause Backend failure cause output.
 * @return Owned channel or injected failure.
 */
struct ast_channel *ast_request(const char *type, struct ast_format_cap *request_cap,
                                const struct ast_assigned_ids *assignedids,
                                const struct ast_channel *requestor, const char *addr, int *cause) {
    assert(!strcmp(type, "RadioPlusAdvanced") && request_cap == technology.capabilities);
    assert(!assignedids && !requestor && !strcmp(addr, "usb"));
    *cause = 0;
    if (failure == 4) {
        return NULL;
    }
    ++channels;
    return (struct ast_channel *)&channels;
}

/** @brief Release a reserved channel.
 * @param channel Owned fixture channel.
 */
void ast_hangup(struct ast_channel *channel) {
    assert(channel == (struct ast_channel *)&channels && channels == 1);
    --channels;
}

/** @brief Set the fixed native read format.
 * @param channel Reserved channel.
 * @param format Required 48 kHz signed-linear format.
 * @return Injected status.
 */
int ast_set_read_format(struct ast_channel *channel, struct ast_format *format) {
    assert(channel && format == &native);
    return failure == 5 ? -1 : 0;
}

/** @brief Set the fixed native write format.
 * @param channel Reserved channel.
 * @param format Required 48 kHz signed-linear format.
 * @return Injected status.
 */
int ast_set_write_format(struct ast_channel *channel, struct ast_format *format) {
    assert(channel && format == &native);
    return failure == 6 ? -1 : 0;
}

/** @brief Assert that no fixture retains a production-owned reference. */
static void clean(void) {
    assert(!channels && !native.references && !wrong_rate.references && !wrong_format.references);
}

/** @brief Cover fixed-native validation and every reservation cleanup path.
 * @return Zero after all ownership assertions.
 */
int main(void) {
    struct ra_connection connection = {0};
    for (failure = 1; failure <= 6; ++failure) {
        assert(ra_connection_open(&connection, "usb"));
        assert(!connection.channel);
        clean();
    }
    failure = 0;
    advertised = &wrong_rate;
    assert(ra_connection_open(&connection, "usb"));
    clean();
    advertised = &wrong_format;
    assert(ra_connection_open(&connection, "usb"));
    clean();
    advertised = &native;
    assert(!ra_connection_open(&connection, "usb"));
    assert(channels == 1 && native.references == 1 && connection.radio.linear == &native);
    ra_connection_close(&connection);
    clean();
    assert(!ra_connection_open(&connection, "usb"));
    /* Simulate the worker releasing transferred channel ownership before media cleanup. */
    ast_hangup(connection.channel);
    connection.channel = NULL;
    ra_connection_close(&connection);
    ra_connection_close(&connection);
    clean();
    puts("fixed 48 kHz radio reservation tests passed");
    return 0;
}
