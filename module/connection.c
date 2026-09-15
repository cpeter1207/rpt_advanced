/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Reserve the fixed-rate native radio interface.
 */
#include <asterisk.h>

#include "connection.h"
#include <asterisk/astobj2.h>
#include <asterisk/channel.h>
#include <asterisk/format.h>
#include <asterisk/format_cache.h>
#include <asterisk/format_cap.h>

void ra_connection_close(struct ra_connection *connection) {
    if (connection->channel) {
        ast_hangup(connection->channel);
    }
    ao2_cleanup(connection->radio.linear);
    *connection = (struct ra_connection){0};
}

const char *ra_connection_open(struct ra_connection *connection, const char *name) {
    const struct ast_channel_tech *technology = ast_get_channel_tech("RadioPlusAdvanced");
    if (!technology) {
        return "RadioPlusAdvanced is not loaded";
    }
    struct ast_format *native = ast_format_cap_get_format(technology->capabilities, 0);
    if (!native) {
        return "radio has no native audio format";
    }
    struct ast_format *linear = ast_format_cache_get_slin_by_rate(48000);
    if (!linear || ast_format_get_sample_rate(native) != 48000 ||
        ast_format_cmp(native, linear) != AST_FORMAT_CMP_EQUAL) {
        ao2_cleanup(native);
        return "RadioPlusAdvanced must provide 48 kHz signed-linear PCM";
    }
    struct ra_connection candidate = {.radio.linear = native};
    int cause = 0;
    candidate.channel =
        ast_request("RadioPlusAdvanced", technology->capabilities, NULL, NULL, name, &cause);
    if (!candidate.channel) {
        ra_connection_close(&candidate);
        return "cannot reserve radio channel";
    }
    if (ast_set_read_format(candidate.channel, candidate.radio.linear) ||
        ast_set_write_format(candidate.channel, candidate.radio.linear)) {
        ra_connection_close(&candidate);
        return "cannot set native radio channel formats";
    }
    *connection = candidate;
    return NULL;
}
