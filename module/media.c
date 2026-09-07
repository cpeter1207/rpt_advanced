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
#include <asterisk/translate.h>
#include <limits.h>
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
        if (ast_format_get_sample_rate(linear) == candidate_rate &&
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
