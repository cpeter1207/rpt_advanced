/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exclusive radio reservation and native PCM ownership.
 */
#ifndef RPT_ADVANCED_CONNECTION_H
#define RPT_ADVANCED_CONNECTION_H
#include "radio.h"

/** @brief Reserved channel and media resources retained until its worker stops. */
struct ra_connection {
    struct ast_channel
        *channel;          /**< Reserved, not yet called channel; null after ownership transfer. */
    struct ra_radio radio; /**< Native PCM format reference owned by this connection. */
};

/** @brief Reserve RadioPlusAdvanced with its required 48 kHz signed-linear format.
 * @param connection Receives ownership on success only; initially empty.
 * @param name Configured USBRadioPlus device name, without a technology prefix.
 * @return Null on success or a stable diagnostic. Does not call or key the radio.
 * The loaded backend must advertise 48 kHz signed-linear PCM. The module lifecycle
 * retains the backend while reserving a channel.
 */
const char *ra_connection_open(struct ra_connection *connection, const char *name);

/** @brief Release remaining channel ownership and native media resources.
 * @param connection Connection to clear, safe when empty.
 * After transferring channel ownership to a worker, set channel to null and
 * join that worker before releasing its owned format reference here.
 */
void ra_connection_close(struct ra_connection *connection);
#endif
