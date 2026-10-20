/** @file rptadv_gpiod_adapter.h
 * @brief Serialized, control-only digital line access through Linux GPIO.
 *
 * Records require their complete size and ABI 1. Borrowed records and UTF-8
 * device bytes remain readable for each call. Open copies configuration and
 * selects only the given device path and line offset, never another device.
 * The caller serializes each context and retains this DSO through destruction.
 * No operation is real-time safe. Functions contain panics and return 0 on
 * success, -1 for invalid input, 1 for device failure, 2 for unsupported access.
 * A submitted output level is not independent electrical readback.
 */
#ifndef RPTADV_GPIOD_ADAPTER_H
#define RPTADV_GPIOD_ADAPTER_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/** Digital input acquisition capability. */
#define RPTADV_GPIOD_INPUT 1u
/** Digital output command capability; neither PWM nor physical readback. */
#define RPTADV_GPIOD_OUTPUT 2u

/** One exclusively requested line, addressed by a configured stable path. */
typedef struct rptadv_gpiod_endpoint_v1 {
    uint32_t struct_size;    /**< Complete record size. */
    uint32_t abi_version;    /**< 1. */
    const uint8_t *device;   /**< Borrowed absolute device path, without required NUL. */
    size_t device_length;    /**< Device byte count. */
    uint32_t line;           /**< Kernel line offset. */
    uint32_t output;         /**< 0 input, 1 output. */
    uint32_t active_low;     /**< 1 inverts at the GPIO boundary, exactly once. */
    uint32_t startup_active; /**< Logical initial output value, 0 or 1. */
    uint32_t safe_active;    /**< Logical best-effort retirement output value, 0 or 1. */
} rptadv_gpiod_endpoint_v1;

/** One physical input observation; callers timestamp successful acquisition. */
typedef struct rptadv_gpiod_observation_v1 {
    uint32_t struct_size; /**< Complete record size. */
    uint32_t abi_version; /**< 1. */
    uint32_t quality;     /**< 0 unavailable, 1 valid, 2 device fault. */
    uint32_t active;      /**< Logical value; meaningful only when quality is valid. */
} rptadv_gpiod_observation_v1;

/** One steady logical output request; pulse timing belongs to station control. */
typedef struct rptadv_gpiod_output_v1 {
    uint32_t struct_size; /**< Complete record size. */
    uint32_t abi_version; /**< 1. */
    uint32_t active;      /**< 0 inactive, 1 active. */
} rptadv_gpiod_output_v1;

/** Complete ABI 1 table; all operations are required. */
typedef struct rptadv_gpiod_descriptor_v1 {
    uint32_t struct_size;   /**< Complete table size. */
    uint32_t abi_version;   /**< 1. */
    uint8_t capability[16]; /**< NUL-padded rptadv.gpiod. */
    /** Acquire the exact line with startup level set as part of acquisition. */
    int32_t (*open)(const rptadv_gpiod_endpoint_v1 *endpoint, void **context);
    /** Read an input, never a cached value; output readback is unsupported. */
    int32_t (*read)(void *context, rptadv_gpiod_observation_v1 *observation);
    /** Submit a logical output without reconfiguring direction or polarity. */
    int32_t (*write)(void *context, const rptadv_gpiod_output_v1 *output);
    /** Return only this context's actual input or output capability. */
    int32_t (*capabilities)(void *context, uint32_t *capabilities);
    /** Attempt safe state and release synchronously; NULL is harmless.
     * Call write with safe state first when confirmed safe handoff is required;
     * on failure retain the context instead of calling destroy. Physical safety
     * after device loss cannot be guaranteed by software.
     */
    void (*destroy)(void *context);
    /** Replace only the retirement level, without writing/reopening the line.
     * Serialized with all context calls. Returns 0 for a valid live output and
     * logical level 0 or 1, -1 for invalid arguments, 2 for an input context.
     */
    int32_t (*set_safe)(void *context, uint32_t active);
} rptadv_gpiod_descriptor_v1;

/** Return the immutable descriptor, retained until this DSO unloads.
 * @return Immutable ABI 1 table, never NULL; the caller must not free it.
 */
const rptadv_gpiod_descriptor_v1 *rptadv_gpiod_adapter_descriptor_v1(void);
#ifdef __cplusplus
}
#endif
#endif
