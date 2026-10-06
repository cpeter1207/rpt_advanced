/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file rptadv_control_asterisk_adapter.h
 * @brief Asterisk taskprocessor control-executor provider.
 */
#ifndef RPTADV_CONTROL_ASTERISK_ADAPTER_H
#define RPTADV_CONTROL_ASTERISK_ADAPTER_H

#include "rptadv_control_adapter.h"

#ifdef __cplusplus
extern "C" {
#endif

/** Return the immutable Asterisk control-executor descriptor.
 * @return The provider's versioned descriptor.
 */
const struct rptadv_control_descriptor_v1 *rptadv_control_descriptor_v1(void);

#ifdef __cplusplus
}
#endif

#endif
