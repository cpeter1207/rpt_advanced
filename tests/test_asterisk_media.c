/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Deterministic live-registry and bidirectional translator fixtures.
 */
#include <asterisk.h>

#include "media.h"
#include <assert.h>
#include <asterisk/astobj2.h>
#include <asterisk/codec.h>
#include <asterisk/format.h>
#include <limits.h>
#include <stdio.h>

/** @brief Minimal opaque format representation owned by this test host. */
struct ast_format {
    unsigned int rate; /**< Sample rate returned by the public API. */
    int references;    /**< Number of owned references. */
};
/** @brief Sparse registry, deliberately not ordered by sample rate. */
static struct ast_codec codecs[] = {
    {.name = "slin", .type = AST_MEDIA_TYPE_VIDEO, .sample_rate = 48000},
    {.name = "other", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 48000},
    {.name = "slin", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 0},
    {.name = "slin", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 8000},
    {.name = "slin", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 48000},
    {.name = "slin", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 16000},
    {.name = "slin", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 96000},
    {.name = "slin", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 44100},
    {.name = "slin", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 24000},
};
/** @brief Cache objects parallel to the codec registry. */
static struct ast_format formats[sizeof(codecs) / sizeof(*codecs)];
/** @brief Count codec references separately from format references. */
static int codec_references;
/** @brief Optional destination whose translation path is unavailable. */
static struct ast_format *blocked_destination;
/** @brief Optional source whose translation path is unavailable. */
static struct ast_format *blocked_source;
/** @brief Signed-linear identifier format for compressed-codec testing. */
static struct ast_format linear = {.rate = 48000};

/** @brief Return the registry extent, including one vacant identifier.
 * @return Maximum identifier.
 */
int ast_codec_get_max(void) { return (int)(sizeof(codecs) / sizeof(*codecs)) + 1; }

/** @brief Return an owned registry entry or the intentional hole.
 * @param id One-based registry identifier.
 * @return Referenced codec, or null.
 */
struct ast_codec *ast_codec_get_by_id(int id) {
    if (id == ast_codec_get_max()) {
        return NULL;
    }
    ++codec_references;
    return &codecs[id - 1];
}

/** @brief Return a cache reference except for the absent 24 kHz format.
 * @param codec Registry entry.
 * @return Owned format, or null.
 */
struct ast_format *ast_format_cache_get_by_codec(const struct ast_codec *codec) {
    if (codec->sample_rate == 24000) {
        return NULL;
    }
    struct ast_format *format = &formats[codec - codecs];
    ++format->references;
    return format;
}

/** @brief Simulate a cache that rounds an unsupported linear rate.
 * @param rate Requested rate.
 * @return Borrowed linear format.
 */
struct ast_format *ast_format_cache_get_slin_by_rate(unsigned int rate) {
    for (size_t i = 3; i < sizeof(formats) / sizeof(*formats); ++i) {
        if (formats[i].rate == rate && rate != 44100) {
            return &formats[i];
        }
    }
    return &linear;
}

/** @brief Read a fixture's sample rate.
 * @param format Fixture object.
 * @return Rate in samples per second.
 */
unsigned int ast_format_get_sample_rate(const struct ast_format *format) { return format->rate; }

/** @brief Compare fixture identity without invoking translators.
 * @param first First format.
 * @param second Second format.
 * @return Public Asterisk comparison result.
 */
enum ast_format_cmp_res ast_format_cmp(const struct ast_format *first,
                                       const struct ast_format *second) {
    return first == second ? AST_FORMAT_CMP_EQUAL : AST_FORMAT_CMP_NOT_EQUAL;
}

/** @brief Supply directed translation edges with injectable failure.
 * @param destination Destination format.
 * @param source Source format.
 * @return One step or UINT_MAX for unavailable conversion.
 */
unsigned int ast_translate_path_steps(struct ast_format *destination, struct ast_format *source) {
    return destination == blocked_destination || source == blocked_source ? UINT_MAX : 1;
}

/** @brief Release exactly one fixture reference, including nullable cleanup.
 * @param object Codec or format reference.
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
    if (!object) {
        return;
    }
    for (size_t i = 0; i < sizeof(codecs) / sizeof(*codecs); ++i) {
        if (object == &codecs[i]) {
            assert(codec_references > 0);
            --codec_references;
            return;
        }
        if (object == &formats[i]) {
            assert(formats[i].references > 0);
            --formats[i].references;
            return;
        }
    }
    assert(0 && "unexpected reference");
}

/** @brief Check selection and that all temporary references were released.
 * @param rate Requested rate, or zero.
 * @param name Requested codec name.
 * @param expected Expected cache object, or null.
 */
static void expect(unsigned int rate, const char *name, struct ast_format *expected) {
    struct ast_format *result = ra_media_select(&formats[4], rate, name);
    assert(result == expected);
    ao2_cleanup(result);
    assert(codec_references == 0);
    for (size_t i = 0; i < sizeof(formats) / sizeof(*formats); ++i) {
        assert(formats[i].references == 0);
    }
}

/** @brief Exercise rate policy, registry holes, cache gaps, and both path directions.
 * @return Zero after all checks.
 */
int main(void) {
    for (size_t i = 0; i < sizeof(formats) / sizeof(*formats); ++i) {
        formats[i].rate = codecs[i].sample_rate;
    }
    expect(0, "", &formats[4]);
    expect(96000, "slin", &formats[6]);
    expect(8000, "slin", &formats[3]);
    expect(44100, "slin", NULL);
    expect(24000, "slin", NULL);
    expect(12345, "slin", NULL);
    expect(0, "missing", NULL);
    expect(0, "other", &formats[1]);
    blocked_destination = &formats[3];
    expect(8000, "slin", NULL);
    blocked_destination = &formats[4];
    expect(8000, "slin", NULL);
    blocked_destination = NULL;
    blocked_source = &formats[3];
    expect(8000, "slin", NULL);
    blocked_source = NULL;
    /* Codec/radio conversion can work while codec/linear conversion fails. */
    formats[1].rate = codecs[1].sample_rate = 16000;
    blocked_destination = &formats[5];
    expect(16000, "other", NULL);
    blocked_destination = NULL;
    expect(16000, "other", &formats[1]);
    puts("Asterisk runtime media selection tests passed");
    return 0;
}
