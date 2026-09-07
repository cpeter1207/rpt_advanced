/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Reservation, format negotiation, converter allocation, and failure ownership.
 */
#include <asterisk.h>

#include "connection.h"
#include <assert.h>
#include <asterisk/astobj2.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/format_cap.h>
#include <asterisk/translate.h>
#include <stdio.h>
#include <string.h>

/** @brief Opaque format fixture with explicit owned-reference accounting. */
struct ast_format {
    unsigned int rate; /**< PCM or codec clock rate. */
    int references;    /**< Owned references held by production code. */
};
/** @brief Native hardware format. */
static struct ast_format native = {.rate = 48000};
/** @brief Controller PCM format for the compressed-codec test. */
static struct ast_format pcm = {.rate = 16000};
/** @brief Selected compressed format. */
static struct ast_format compressed = {.rate = 16000};
/** @brief Native interface descriptor, whose public fields are borrowed. */
static const struct ast_channel_tech technology = {.type = "RadioPlusAdvanced"};
/** @brief Decode-path identity. */
static struct ast_trans_pvt decode;
/** @brief Encode-path identity. */
static struct ast_trans_pvt encode;
/** @brief Inject one of the eight fallible operation results. */
static int failure;
/** @brief Select compressed rather than native linear transport. */
static bool use_codec;
/** @brief Count owned converter paths. */
static int paths;
/** @brief Count reserved channels. */
static int channels;

/** @brief Look up the requested native interface.
 * @param name Requested technology.
 * @return Descriptor or injected absence.
 */
const struct ast_channel_tech *ast_get_channel_tech(const char *name) {
    assert(!strcmp(name, "RadioPlusAdvanced"));
    return failure == 1 ? NULL : &technology;
}

/** @brief Reference the backend's native format.
 * @param cap Borrowed capability set.
 * @param position Native format position.
 * @return Owned format or injected absence.
 */
struct ast_format *ast_format_cap_get_format(const struct ast_format_cap *cap, int position) {
    assert(cap == technology.capabilities && position == 0);
    if (failure == 2) {
        return NULL;
    }
    ++native.references;
    return &native;
}

/** @brief Supply the separately tested registry selector's chosen format.
 * @param radio Native hardware format.
 * @param rate Explicit or automatic policy.
 * @param name Codec name.
 * @return Owned chosen format or injected failure.
 */
struct ast_format *__wrap_ra_media_select(struct ast_format *radio, unsigned int rate,
                                          const char *name) {
    assert(radio == &native && rate == 0 && !strcmp(name, "requested"));
    if (failure == 3) {
        return NULL;
    }
    struct ast_format *selected = use_codec ? &compressed : &native;
    ++selected->references;
    return selected;
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

/** @brief Return a format's sample rate.
 * @param format Fixture format.
 * @return Samples per second.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) { return format->rate; }

/** @brief Return borrowed PCM without acquiring a reference.
 * @param rate Requested PCM rate.
 * @return Corresponding linear format.
 */
struct ast_format *ast_format_cache_get_slin_by_rate(unsigned int rate) {
    assert(rate == 48000 || rate == 16000);
    return rate == 48000 ? &native : &pcm;
}

/** @brief Compare format identity.
 * @param first First format.
 * @param second Second format.
 * @return Equality result.
 */
enum ast_format_cmp_res ast_format_cmp(const struct ast_format *first,
                                       const struct ast_format *second) {
    return first == second ? AST_FORMAT_CMP_EQUAL : AST_FORMAT_CMP_NOT_EQUAL;
}

/** @brief Allocate one required codec direction.
 * @param dest Destination format.
 * @param source Source format.
 * @return Owned path or injected allocation failure.
 */
struct ast_trans_pvt *ast_translator_build_path(struct ast_format *dest,
                                                struct ast_format *source) {
    bool decoding = dest == &pcm;
    assert(decoding ? source == &compressed : dest == &compressed && source == &pcm);
    if (failure == (decoding ? 4 : 5)) {
        return NULL;
    }
    ++paths;
    return decoding ? &decode : &encode;
}

/** @brief Release a converter.
 * @param path Owned fixture path.
 */
void ast_translator_free_path(struct ast_trans_pvt *path) {
    assert((path == &decode || path == &encode) && paths > 0);
    --paths;
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
    if (failure == 6) {
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

/** @brief Set the actual requested read codec, not just its sample rate.
 * @param channel Reserved channel.
 * @param format Negotiated codec.
 * @return Injected status.
 */
int ast_set_read_format(struct ast_channel *channel, struct ast_format *format) {
    assert(channel && format == (use_codec ? &compressed : &native));
    return failure == 7 ? -1 : 0;
}

/** @brief Set the actual requested write codec.
 * @param channel Reserved channel.
 * @param format Negotiated codec.
 * @return Injected status.
 */
int ast_set_write_format(struct ast_channel *channel, struct ast_format *format) {
    assert(channel && format == (use_codec ? &compressed : &native));
    return failure == 8 ? -1 : 0;
}

/** @brief Cover successful native/compressed reservations and every cleanup path.
 * @return Zero after all ownership assertions.
 */
int main(void) {
    struct ra_connection connection = {0};
    use_codec = true;
    for (failure = 1; failure <= 8; ++failure) {
        assert(ra_connection_open(&connection, "usb", 0, "requested"));
        assert(!connection.channel && !paths && !channels);
        assert(!native.references && !compressed.references && !pcm.references);
    }
    failure = 0;
    assert(!ra_connection_open(&connection, "usb", 0, "requested"));
    assert(paths == 2 && channels == 1 && compressed.references == 1);
    assert(connection.radio.linear == &pcm && connection.radio.codec == &compressed);
    ra_connection_close(&connection);
    assert(!paths && !channels && !compressed.references);
    use_codec = false;
    assert(!ra_connection_open(&connection, "usb", 0, "requested"));
    assert(!paths && channels == 1 && native.references == 1);
    /* Simulate the worker releasing transferred channel ownership before media cleanup. */
    ast_hangup(connection.channel);
    connection.channel = NULL;
    ra_connection_close(&connection);
    ra_connection_close(&connection);
    assert(!paths && !channels && !native.references);
    puts("radio reservation and negotiated converter ownership tests passed");
    return 0;
}
