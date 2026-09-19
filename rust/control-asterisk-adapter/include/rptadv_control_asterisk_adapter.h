/* SPDX-License-Identifier: GPL-2.0-only */
/**
 * @file rptadv_control_asterisk_adapter.h
 * @brief Versioned Asterisk taskprocessor capability.
 */
#ifndef RPTADV_CONTROL_ASTERISK_ADAPTER_H
#define RPTADV_CONTROL_ASTERISK_ADAPTER_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** One caller-owned task; successful submission transfers its context.
 * Both callbacks are required and must not unwind. Run does not destroy context;
 * release follows run exactly once, including a caller-contained task failure.
 */
struct rptadv_control_task_v1 {
    void *context;                  /**< Opaque caller payload. */
    void (*run)(void *context);     /**< Invoke the payload exactly once. */
    void (*release)(void *context); /**< Release the payload exactly once. */
};

/** ABI version 1 control-execution function table.
 * Validate the readable version/size prefix, capability and all callbacks before
 * use. Open creates a uniquely named application executor; do not acquire other
 * taskprocessor references to it. Submission: 0 transfers ownership, 1 stopped,
 * 2 full, 3 backend failure; rejected payloads remain untouched. Gate external
 * producers before close. Drain waits for payload run/release, but only successful
 * close joins the default worker and permits unloading this provider's code.
 * Drain/close from the executor return -1, retaining the handle for external retry.
 * The loader retains the provider and callback code through successful close.
 */
struct rptadv_control_descriptor_v1 {
    uint32_t abi_version;                             /**< Exact incompatible ABI version. */
    size_t struct_size;                               /**< Complete descriptor size. */
    const char *capability;                           /**< Static NUL-terminated capability name. */
    void *(*open)(const char *name, size_t capacity); /**< Open one bounded executor. */
    int (*submit)(void *context, struct rptadv_control_task_v1 task); /**< Submit a task. */
    int (*stop_and_drain)(void *context); /**< Stop admission and drain accepted tasks. */
    int (*close)(void *context);          /**< Drain, release and join the unique executor. */
};

/** @brief Return the immutable ABI version 1 descriptor.
 * @return Process-lifetime function table; never modify or free it.
 */
const struct rptadv_control_descriptor_v1 *rptadv_control_descriptor_v1(void);

#ifdef __cplusplus
}
#endif

#endif
