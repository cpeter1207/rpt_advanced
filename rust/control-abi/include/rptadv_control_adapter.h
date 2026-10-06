/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file rptadv_control_adapter.h
 * @brief Shared versioned control-executor ABI.
 */
#ifndef RPTADV_CONTROL_ADAPTER_H
#define RPTADV_CONTROL_ADAPTER_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** One caller-owned task; success transfers its context to the provider.
 * The provider invokes run once and then release once. Both callbacks must
 * return normally and must not unwind across the ABI boundary.
 */
struct rptadv_control_task_v1 {
    void *context;                  /**< Opaque caller payload. */
    void (*run)(void *context);     /**< Execute on the serialized owner. */
    void (*release)(void *context); /**< Release after execution. */
};

/** Version 1 serialized control-execution capability.
 * Validate the readable version/size prefix, capability and callbacks before
 * use. Open creates one bounded executor with capacity from 1 through 65536.
 * Submission returns 0 for acceptance,
 * 1 for stopped admission, 2 for a full queue, and 3 for backend failure;
 * rejected payloads remain caller-owned. Drain gates admission and waits for
 * accepted work. Drain/close from the executor return -1 without consuming the
 * handle; the external lifecycle owner must retry. Successful close joins the
 * worker and is the provider-code unload barrier.
 */
struct rptadv_control_descriptor_v1 {
    uint32_t abi_version;                             /**< Exact incompatible ABI version. */
    size_t struct_size;                               /**< Complete descriptor size in bytes. */
    const char *capability;                           /**< Static NUL-terminated capability name. */
    void *(*open)(const char *name, size_t capacity); /**< Open bounded executor. */
    int (*submit)(void *context, struct rptadv_control_task_v1 task); /**< Transfer one task. */
    int (*stop_and_drain)(void *context); /**< Stop admission and drain. */
    int (*close)(void *context);          /**< Drain, join, and release. */
};

#ifdef __cplusplus
}
#endif

#endif
