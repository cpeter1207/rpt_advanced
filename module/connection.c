/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Reserve the native radio interface and construct negotiated codec paths.
 */
#include <asterisk.h>

#include "connection.h"
#include "media.h"
#include <asterisk/astobj2.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/format_cache.h>
#include <asterisk/format_cap.h>
#include <asterisk/translate.h>

void ra_connection_close(struct ra_connection *connection) {
    if (connection->channel) {
        ast_hangup(connection->channel);
    }
    if (connection->radio.decode) {
        ast_translator_free_path(connection->radio.decode);
    }
    if (connection->radio.encode) {
        ast_translator_free_path(connection->radio.encode);
    }
    ao2_cleanup(connection->radio.codec);
    *connection = (struct ra_connection){0};
}

const char *ra_connection_open(struct ra_connection *connection, const char *name,
                               unsigned int rate, const char *codec) {
    const struct ast_channel_tech *technology = ast_get_channel_tech("RadioPlusAdvanced");
    if (!technology) {
        return "RadioPlusAdvanced is not loaded";
    }
    struct ast_format *native = ast_format_cap_get_format(technology->capabilities, 0);
    if (!native) {
        return "radio has no native audio format";
    }
    struct ra_connection candidate = {0};
    /* Legacy AllStarLink peers advertise only 8 kHz codecs.  An unspecified
     * rate therefore starts at the interoperable network rate; callers that
     * explicitly request a rate retain the full negotiated-rate behavior. */
    unsigned int automatic_rate = !rate && !*codec ? 8000 : rate;
    candidate.radio.codec = ra_media_select(native, automatic_rate, codec);
    /* GCOVR_EXCL_START: fallback is exercised by live codec registries. */
    if (!candidate.radio.codec && !rate && !*codec) {
        candidate.radio.codec = ra_media_select(native, 0, codec);
    }
    /* GCOVR_EXCL_STOP */
    ao2_cleanup(native);
    if (!candidate.radio.codec) {
        return "requested codec/rate has no supported conversion path";
    }
    candidate.radio.linear =
        ast_format_cache_get_slin_by_rate(ast_format_get_sample_rate(candidate.radio.codec));
    if (ast_format_cmp(candidate.radio.codec, candidate.radio.linear) != AST_FORMAT_CMP_EQUAL) {
        candidate.radio.decode =
            ast_translator_build_path(candidate.radio.linear, candidate.radio.codec);
        candidate.radio.encode =
            ast_translator_build_path(candidate.radio.codec, candidate.radio.linear);
        if (!candidate.radio.decode || !candidate.radio.encode) {
            ra_connection_close(&candidate);
            return "cannot allocate codec converters";
        }
    }
    int cause = 0;
    candidate.channel =
        ast_request("RadioPlusAdvanced", technology->capabilities, NULL, NULL, name, &cause);
    if (!candidate.channel) {
        ra_connection_close(&candidate);
        return "cannot reserve radio channel";
    }
    if (ast_set_read_format(candidate.channel, candidate.radio.codec) ||
        ast_set_write_format(candidate.channel, candidate.radio.codec)) {
        ra_connection_close(&candidate);
        return "cannot set negotiated channel formats";
    }
    *connection = candidate;
    return NULL;
}
