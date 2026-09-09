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
#include <asterisk/format_cap.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>

/* The fixture implements Asterisk's allocation hooks with libc storage. */
#undef calloc
#undef free

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
    {.name = "other", .type = AST_MEDIA_TYPE_AUDIO, .sample_rate = 16000},
};
/** @brief Cache objects parallel to the codec registry. */
static struct ast_format formats[sizeof(codecs) / sizeof(*codecs)];
/** @brief Count codec references separately from format references. */
static int codec_references;
/** @brief Optional destination whose translation path is unavailable. */
static struct ast_format *blocked_destination;
/** @brief Optional source whose translation path is unavailable. */
static struct ast_format *blocked_source;
/** @brief Make the final registry entry share the cached 16 kHz linear format. */
static bool duplicate_linear;
/** @brief Signed-linear identifier format for compressed-codec testing. */
static struct ast_format linear = {.rate = 48000};
/** @brief Simulate a missing same-rate signed-linear cache entry. */
static bool missing_linear;
/** @brief Requested rate whose cached linear result is explicitly overridden. */
static unsigned int overridden_linear_rate;
/** @brief Borrowed override result for overridden_linear_rate. */
static struct ast_format *overridden_linear;
/** @brief Opaque capability ownership fixture. */
struct ast_format_cap {
    int references;     /**< Outstanding ownership. */
    unsigned int count; /**< Appended formats. */
    bool offered_8000;  /**< Records the legacy narrowband offer. */
    struct ast_format *formats[sizeof(codecs) / sizeof(*codecs)]; /**< Borrowed entries. */
};
/** @brief Single capability returned by the allocator fixture. */
static struct ast_format_cap capability;
/** @brief Inject capability allocation failure. */
static bool allocation_error;
/** @brief Inject capability append failure. */
static bool append_error;
/** @brief Candidate-allocation call selected for failure, or zero when disabled. */
static unsigned int failed_candidate_allocation;
/** @brief Candidate-allocation calls observed by the allocator fixture. */
static unsigned int candidate_allocations;
/** @brief Optional replacement codec-registry extent. */
static int codec_maximum;

/** @brief Allocate a tracked capability or inject failure.
 * @param flags Expected default flags.
 * @param tag Debug tag.
 * @param file Caller file.
 * @param line Caller line.
 * @param func Caller function.
 * @return Tracked capability or null.
 */
struct ast_format_cap *__ast_format_cap_alloc(enum ast_format_cap_flags flags, const char *tag,
                                              const char *file, int line, const char *func) {
    (void)tag;
    (void)file;
    (void)line;
    (void)func;
    assert(flags == AST_FORMAT_CAP_FLAG_DEFAULT);
    if (allocation_error) {
        return NULL;
    }
    assert(!capability.references);
    capability = (struct ast_format_cap){.references = 1};
    return &capability;
}

/** @brief Allocate candidate storage or inject an allocation failure.
 * @param count Number of elements.
 * @param size Bytes per element.
 * @param file Caller source file.
 * @param line Caller source line.
 * @param function Caller function name.
 * @return Zeroed allocation or null.
 */
void *__ast_calloc(size_t count, size_t size, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    ++candidate_allocations;
    return allocation_error || (failed_candidate_allocation &&
                                candidate_allocations == failed_candidate_allocation)
               ? NULL
               : calloc(count, size);
}

/** @brief Release candidate storage owned by the media implementation.
 * @param pointer Allocation to release.
 * @param file Caller source file.
 * @param line Caller source line.
 * @param function Caller function name.
 */
void __ast_free(void *pointer, const char *file, int line, const char *function) {
    (void)file;
    (void)line;
    (void)function;
    free(pointer);
}

/** @brief Count offered formats without retaining fixture references.
 * @param cap Tracked capability.
 * @param format Borrowed format.
 * @param framing Default framing.
 * @param tag Debug tag.
 * @param file Caller file.
 * @param line Caller line.
 * @param func Caller function.
 * @return Injected append status.
 */
int __ast_format_cap_append(struct ast_format_cap *cap, struct ast_format *format,
                            unsigned int framing, const char *tag, const char *file, int line,
                            const char *func) {
    (void)tag;
    (void)file;
    (void)line;
    (void)func;
    assert(cap && cap->references == 1 &&
           cap->count < sizeof(cap->formats) / sizeof(*cap->formats) && format && !framing);
    cap->formats[cap->count] = format;
    ++cap->count;
    if (cap == &capability) {
        capability.offered_8000 |= format == &formats[3];
    }
    return append_error ? -1 : 0;
}

/** @brief Return the number of entries in one fixture capability.
 * @param cap Fixture capability.
 * @return Entry count, or zero for a null capability.
 */
size_t ast_format_cap_count(const struct ast_format_cap *cap) { return cap ? cap->count : 0; }

/** @brief Return the registry extent, including one vacant identifier.
 * @return Maximum identifier.
 */
int ast_codec_get_max(void) {
    return codec_maximum ? codec_maximum : (int)(sizeof(codecs) / sizeof(*codecs)) + 1;
}

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
    struct ast_format *format =
        duplicate_linear && codec == &codecs[9] ? &formats[5] : &formats[codec - codecs];
    ++format->references;
    return format;
}

/** @brief Simulate a cache that rounds an unsupported linear rate.
 * @param rate Requested rate.
 * @return Borrowed linear format.
 */
struct ast_format *ast_format_cache_get_slin_by_rate(unsigned int rate) {
    if (missing_linear) {
        return NULL;
    }
    if (rate == overridden_linear_rate) {
        return overridden_linear;
    }
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
    if (object == &capability) {
        assert(capability.references == 1);
        --capability.references;
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

/** @brief Verify that candidate ownership has returned to the fixture. */
static void assert_released(void) {
    assert(!codec_references);
    for (size_t index = 0; index < sizeof(formats) / sizeof(*formats); ++index) {
        assert(!formats[index].references);
    }
}

/** @brief Collect and verify the ordered candidates for the 48 kHz fixture radio.
 * @param expected Ordered borrowed format expectations.
 * @param expected_count Number of expected entries.
 */
static void expect_candidates(struct ast_format *const *expected, size_t expected_count) {
    struct ast_format **result = NULL;
    size_t count = 0;
    assert(!ra_media_candidates_collect(&formats[4], &result, &count));
    assert(count == expected_count);
    for (size_t index = 0; index < count; ++index) {
        assert(result[index] == expected[index]);
    }
    ra_media_candidates_release(result, count);
    assert_released();
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
    missing_linear = true;
    expect(16000, "slin", NULL);
    missing_linear = false;
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
    /* Candidates prefer direct 48 kHz PCM and never exceed the local hardware rate. */
    struct ast_format *expected[] = {&formats[4], &formats[5], &formats[1], &formats[9],
                                     &formats[3]};
    expect_candidates(expected, sizeof(expected) / sizeof(*expected));
    /* Equal direct candidates retain their original registry order. */
    duplicate_linear = true;
    struct ast_format *with_duplicate_linear[] = {&formats[4], &formats[5], &formats[5],
                                                  &formats[1], &formats[3]};
    expect_candidates(with_duplicate_linear,
                      sizeof(with_duplicate_linear) / sizeof(*with_duplicate_linear));
    duplicate_linear = false;
    /* Direct PCM must move ahead of an earlier compressed format at the same rate. */
    enum ast_media_type saved_type = codecs[4].type;
    codecs[4].type = AST_MEDIA_TYPE_VIDEO;
    formats[3].rate = 0;
    struct ast_format *same_rate[] = {&formats[5], &formats[1], &formats[9]};
    expect_candidates(same_rate, sizeof(same_rate) / sizeof(*same_rate));
    formats[3].rate = codecs[3].sample_rate;
    codecs[4].type = saved_type;
    struct ast_format *sentinel[] = {&linear};
    struct ast_format **result = sentinel;
    size_t count = 99;
    assert(ra_media_candidates_collect(NULL, &result, &count) == -1 && result == sentinel &&
           count == 99);
    struct ast_format zero_rate_radio = {0};
    assert(ra_media_candidates_collect(&zero_rate_radio, &result, &count) == -1 && !result &&
           !count);
    assert(ra_media_candidates_collect(&formats[4], NULL, &count) == -1);
    assert(ra_media_candidates_collect(&formats[4], &result, NULL) == -1);
    codec_maximum = -1;
    assert(ra_media_candidates_collect(&formats[4], &result, &count) == -1 && !result && !count);
    codec_maximum = 0;
    allocation_error = true;
    result = NULL;
    count = 0;
    assert(ra_media_candidates_collect(&formats[4], &result, &count) == -1 && !result && !count);
    allocation_error = false;
    candidate_allocations = 0;
    failed_candidate_allocation = 2;
    assert(ra_media_candidates_collect(&formats[4], &result, &count) == -1 && !result && !count);
    failed_candidate_allocation = 0;
    formats[3].rate = 0;
    struct ast_format *without_8k[] = {&formats[4], &formats[5], &formats[1], &formats[9]};
    expect_candidates(without_8k, sizeof(without_8k) / sizeof(*without_8k));
    formats[3].rate = codecs[3].sample_rate;
    struct ast_format matching_linear = {.rate = 8000};
    overridden_linear_rate = 8000;
    overridden_linear = &matching_linear;
    formats[3].rate = 4000;
    expect_candidates(without_8k, sizeof(without_8k) / sizeof(*without_8k));
    formats[3].rate = codecs[3].sample_rate;
    overridden_linear = NULL;
    overridden_linear_rate = 0;
    blocked_destination = &formats[5];
    struct ast_format *without_compressed_16k[] = {&formats[4], &formats[5], &formats[3]};
    expect_candidates(without_compressed_16k,
                      sizeof(without_compressed_16k) / sizeof(*without_compressed_16k));
    blocked_destination = NULL;
    /* A cache result at the wrong rate is never accepted as wire PCM. */
    overridden_linear_rate = 8000;
    overridden_linear = &linear;
    expect_candidates(without_8k, sizeof(without_8k) / sizeof(*without_8k));
    overridden_linear = &matching_linear;
    expect_candidates(expected, sizeof(expected) / sizeof(*expected));
    overridden_linear = NULL;
    overridden_linear_rate = 0;
    /* Codec/radio conversion is deliberately irrelevant: link_hub resamples peer PCM. */
    blocked_source = &formats[5];
    struct ast_format *no_other[] = {&formats[4], &formats[5], &formats[3]};
    expect_candidates(no_other, sizeof(no_other) / sizeof(*no_other));
    blocked_source = NULL;
    missing_linear = true;
    result = NULL;
    count = 0;
    assert(ra_media_candidates_collect(&formats[4], &result, &count) == -1 && !result && !count);
    missing_linear = false;
    struct ast_format_cap *offer = ra_media_offer_create(&formats[4]);
    assert(offer == &capability && capability.count == 1 && capability.formats[0] == &formats[4]);
    ao2_cleanup(offer);
    assert(!ra_media_offer_create(NULL));
    allocation_error = true;
    assert(!ra_media_offer_create(&formats[4]));
    allocation_error = false;
    append_error = true;
    assert(!ra_media_offer_create(&formats[4]) && !capability.references);
    append_error = false;
    ra_media_candidates_release(NULL, 0);
    assert_released();
    puts("Asterisk runtime media selection tests passed");
    return 0;
}
