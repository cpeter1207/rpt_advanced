/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Exclusive radio reservation and negotiated converter ownership.
 */
#ifndef RPT_ADVANCED_CONNECTION_H
#define RPT_ADVANCED_CONNECTION_H
#include "radio.h"

/** @brief Reserved channel and media resources retained until its worker stops. */
struct ra_connection {
    struct ast_channel
        *channel;          /**< Reserved, not yet called channel; null after ownership transfer. */
    struct ra_radio radio; /**< Codec reference and converter paths owned by this connection. */
};

/** @brief Reserve RadioPlusAdvanced and configure actual Asterisk codec conversion.
 * @param connection Receives ownership on success only; initially empty.
 * @param name Configured USBRadioPlus device name, without a technology prefix.
 * @param rate Explicit codec sample rate or zero for hardware-bounded automatic selection.
 * @param codec Requested codec name; empty selects native signed linear.
 * @return Null on success or a stable diagnostic. Does not call or key the radio.
 * The loaded backend advertises its hardware-native PCM format. The module
 * lifecycle must retain the backend while reserving a channel.
 */
const char *ra_connection_open(struct ra_connection *connection, const char *name,
                               unsigned int rate, const char *codec);

/** @brief Release remaining channel ownership and negotiated media resources.
 * @param connection Connection to clear, safe when empty.
 * After transferring channel ownership to a worker, set channel to null and
 * join that worker before releasing its borrowed converter paths here.
 */
void ra_connection_close(struct ra_connection *connection);
#endif
