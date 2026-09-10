/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file
 * @brief Validated control-plane actions used by scheduled events and macros.
 */
#ifndef RPT_ADVANCED_SCHEDULED_ACTION_H
#define RPT_ADVANCED_SCHEDULED_ACTION_H
#include <stdbool.h>

/** @brief Operations permitted in a named macro. */
enum ra_scheduled_action {
    RA_SCHEDULED_ACTION_CONNECT,        /**< Create one nonpermanent transceive direct link. */
    RA_SCHEDULED_ACTION_DISCONNECT,     /**< Detach one selected direct link. */
    RA_SCHEDULED_ACTION_DISCONNECT_ALL, /**< Detach every peer and pause retained retries. */
    RA_SCHEDULED_ACTION_RECONNECT_ALL   /**< Resume every retained retry. */
};

/** @brief Parse one macro operation without accepting shell or process syntax.
 * @param text Exact configured operation name.
 * @param action Receives the operation only on success.
 * @return True for a documented controller operation.
 */
bool ra_scheduled_action_parse(const char *text, enum ra_scheduled_action *action);
#endif
