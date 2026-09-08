/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Select media from Asterisk's live codec registry and translation graph.
 */
#include <asterisk.h>

#include "media.h"
#include <asterisk/astobj2.h>
#include <asterisk/codec.h>
#include <asterisk/format.h>
#include <asterisk/format_cache.h>
#include <asterisk/format_cap.h>
#include <asterisk/translate.h>
#include <stdbool.h>
#include <string.h>

/** @brief Check both directions, including identity without a translator.
 * @param first First media format.
 * @param second Second media format.
 * @return Nonzero when each format can be converted to the other.
 */
static int bidirectional(struct ast_format *first, struct ast_format *second) {
    if (ast_format_cmp(first, second) == AST_FORMAT_CMP_EQUAL) {
        return 1;
    }
    return ast_translate_path_steps(first, second) != UINT_MAX &&
           ast_translate_path_steps(second, first) != UINT_MAX;
}

struct ast_format *ra_media_select(struct ast_format *radio, unsigned int rate, const char *name) {
    struct ast_format *selected = NULL;
    unsigned int selected_rate = 0;
    unsigned int native_rate = ast_format_get_sample_rate(radio);
    const char *requested = *name ? name : "slin";
    int maximum = ast_codec_get_max();
    for (int index = 0; index < maximum; ++index) {
        struct ast_codec *codec = ast_codec_get_by_id(index + 1);
        if (!codec) {
            continue;
        }
        unsigned int candidate_rate = codec->sample_rate;
        if (codec->type != AST_MEDIA_TYPE_AUDIO || strcmp(codec->name, requested) ||
            candidate_rate <= selected_rate ||
            (rate ? candidate_rate != rate : candidate_rate > native_rate)) {
            ao2_cleanup(codec);
            continue;
        }
        struct ast_format *candidate = ast_format_cache_get_by_codec(codec);
        ao2_cleanup(codec);
        if (!candidate) {
            continue;
        }
        struct ast_format *linear = ast_format_cache_get_slin_by_rate(candidate_rate);
        /* The cache may round to a different rate: never silently accept that. */
        if (linear && ast_format_get_sample_rate(linear) == candidate_rate &&
            bidirectional(candidate, radio) && bidirectional(candidate, linear)) {
            ao2_cleanup(selected);
            selected = candidate;
            selected_rate = candidate_rate;
        } else {
            ao2_cleanup(candidate);
        }
    }
    return selected;
}

/** @brief One owned registry format eligible for an ordered IAX attempt. */
struct ra_media_candidate {
    struct ast_format *format; /**< Owned concrete wire format. */
    unsigned int rate;         /**< Concrete sample rate. */
    bool linear;               /**< Prefer direct PCM to compression at the same rate. */
};

/** @brief Decide whether one candidate precedes another in the dial sequence.
 * @param left First candidate.
 * @param right Second candidate.
 * @return True when left has higher dial priority.
 */
static bool candidate_precedes(const struct ra_media_candidate *left,
                               const struct ra_media_candidate *right) {
    if (left->rate != right->rate) {
        return left->rate > right->rate;
    }
    return left->linear && !right->linear;
}

/** @brief Stably sort wire candidates without discarding registry-order tie breaking.
 * @param candidates Candidate array to sort in place.
 * @param count Number of initialized candidates.
 *
 * The live codec registry is already deterministically ordered.  Insertion sorting is adequate
 * for its small bounded set and keeps equivalent candidates in that order without an unreachable
 * synthetic tie-break condition.
 */
static void sort_candidates(struct ra_media_candidate *candidates, size_t count) {
    for (size_t index = 1; index < count; ++index) {
        struct ra_media_candidate current = candidates[index];
        size_t cursor = index;
        while (cursor && candidate_precedes(&current, &candidates[cursor - 1])) {
            candidates[cursor] = candidates[cursor - 1];
            --cursor;
        }
        candidates[cursor] = current;
    }
}

void ra_media_candidates_release(struct ast_format **formats, size_t count) {
    for (size_t index = 0; index < count; ++index) {
        ao2_cleanup(formats[index]);
    }
    ast_free(formats);
}

int ra_media_candidates_collect(struct ast_format *radio, struct ast_format ***formats,
                                size_t *count) {
    if (!radio || !formats || !count) {
        return -1;
    }
    *formats = NULL;
    *count = 0;
    unsigned int maximum_rate = ast_format_get_sample_rate(radio);
    int maximum = ast_codec_get_max();
    if (!maximum_rate || maximum <= 0) {
        return -1;
    }
    struct ra_media_candidate *candidates = ast_calloc((size_t)maximum, sizeof(*candidates));
    if (!candidates) {
        return -1;
    }
    size_t candidate_count = 0;
    for (int index = 0; index < maximum; ++index) {
        struct ast_codec *codec = ast_codec_get_by_id(index + 1);
        if (!codec) {
            continue;
        }
        unsigned int rate = codec->sample_rate;
        if (codec->type != AST_MEDIA_TYPE_AUDIO || !rate || rate > maximum_rate) {
            ao2_cleanup(codec);
            continue;
        }
        struct ast_format *format = ast_format_cache_get_by_codec(codec);
        ao2_cleanup(codec);
        if (!format) {
            continue;
        }
        struct ast_format *linear = ast_format_cache_get_slin_by_rate(rate);
        if (!linear || ast_format_get_sample_rate(linear) != rate ||
            ast_format_get_sample_rate(format) != rate || !bidirectional(format, linear)) {
            ao2_cleanup(format);
            continue;
        }
        candidates[candidate_count++] = (struct ra_media_candidate){
            .format = format,
            .rate = rate,
            .linear = ast_format_cmp(format, linear) == AST_FORMAT_CMP_EQUAL};
    }
    if (!candidate_count) {
        ast_free(candidates);
        return -1;
    }
    sort_candidates(candidates, candidate_count);
    struct ast_format **result = ast_calloc(candidate_count, sizeof(*result));
    if (!result) {
        for (size_t index = 0; index < candidate_count; ++index) {
            ao2_cleanup(candidates[index].format);
        }
        ast_free(candidates);
        return -1;
    }
    for (size_t index = 0; index < candidate_count; ++index) {
        result[index] = candidates[index].format;
    }
    ast_free(candidates);
    *formats = result;
    *count = candidate_count;
    return 0;
}

struct ast_format_cap *ra_media_offer_create(struct ast_format *format) {
    if (!format) {
        return NULL;
    }
    struct ast_format_cap *offer = ast_format_cap_alloc(AST_FORMAT_CAP_FLAG_DEFAULT);
    if (!offer) {
        return NULL;
    }
    if (ast_format_cap_append(offer, format, 0)) {
        ao2_cleanup(offer);
        return NULL;
    }
    return offer;
}
