/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file rptadv_control_standalone_adapter.h
 * @brief Standalone lock-free control-executor provider.
 */
#ifndef RPTADV_CONTROL_STANDALONE_ADAPTER_H
#define RPTADV_CONTROL_STANDALONE_ADAPTER_H

#include "rptadv_control_adapter.h"

#ifdef __cplusplus
extern "C" {
#endif

/** Return the immutable standalone control-executor descriptor.
 * @return The provider's versioned descriptor.
 */
const struct rptadv_control_descriptor_v1 *rptadv_control_standalone_descriptor_v1(void);

#ifdef __cplusplus
}
#endif

#endif
