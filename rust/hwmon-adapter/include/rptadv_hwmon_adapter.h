/** @file rptadv_hwmon_adapter.h
 * @brief Serialized, non-audio Linux sensor and PWM capability.
 *
 * Records require their complete size and ABI 1. Device/channel are borrowed
 * UTF-8 byte extents for open only. Contexts retain the configured device; no
 * operation selects another sensor after failure. Each context is serialized
 * by its caller. Keep the DSO loaded until all contexts have been destroyed.
 * Functions contain panics: 0 success, -1 invalid input, 1 device error,
 * 2 unsupported capability. No operation is real-time safe.
 */
#ifndef RPTADV_HWMON_ADAPTER_H
#define RPTADV_HWMON_ADAPTER_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/** Measurement acquisition in source units. */
#define RPTADV_HWMON_INPUT 1u
/** Hardware PWM duty control, not electrical or tachometer readback. */
#define RPTADV_HWMON_PWM 2u

/** One exact hwmon directory and measurement or hardware PWM channel. */
typedef struct rptadv_hwmon_endpoint_v1 {
    uint32_t struct_size;   /**< Complete record size. */
    uint32_t abi_version;   /**< 1. */
    const uint8_t *device;  /**< Absolute stable directory path. */
    size_t device_length;   /**< Device byte count. */
    const uint8_t *channel; /**< Exact measurement name, or pwmN for output. */
    size_t channel_length;  /**< Channel byte count. */
    uint32_t output;        /**< 0 measurement, 1 hardware PWM. */
    double startup_percent; /**< Initial PWM duty, finite 0 through 100. */
    double safe_percent;    /**< Best-effort retirement PWM duty, same range. */
} rptadv_hwmon_endpoint_v1;

/** A successful measurement is already V, A, W, J, degrees C or RPM.
 * Calibration belongs to the caller. Host sensors.conf computations are not
 * applied. Timestamp only successful acquisition; failed values are not zero.
 */
typedef struct rptadv_hwmon_observation_v1 {
    uint32_t struct_size; /**< Complete record size. */
    uint32_t abi_version; /**< 1. */
    uint32_t quality;     /**< 0 unavailable, 1 valid, 2 device fault. */
    double value;         /**< Finite source value, meaningful only when valid. */
} rptadv_hwmon_observation_v1;

/** One steady PWM demand; timers and fan policy remain with the controller. */
typedef struct rptadv_hwmon_output_v1 {
    uint32_t struct_size; /**< Complete record size. */
    uint32_t abi_version; /**< 1. */
    double percent;       /**< Finite duty from 0 through 100. */
} rptadv_hwmon_output_v1;

/** Complete ABI-1 descriptor; all operations are required. */
typedef struct rptadv_hwmon_descriptor_v1 {
    uint32_t struct_size;   /**< Complete table size. */
    uint32_t abi_version;   /**< 1. */
    uint8_t capability[16]; /**< NUL-padded rptadv.hwmon. */
    /** Acquire only the selected device/channel; PWM requires existing manual mode. */
    int32_t (*open)(const rptadv_hwmon_endpoint_v1 *, void **);
    /** Acquire a finite measurement. PWM readback is unsupported; use a tachometer. */
    int32_t (*read)(void *, rptadv_hwmon_observation_v1 *);
    /** Submit duty through supported kernel hardware PWM, never GPIO bit-banging. */
    int32_t (*write)(void *, const rptadv_hwmon_output_v1 *);
    /** Report actual measurement or PWM capability for this context. */
    int32_t (*capabilities)(void *, uint32_t *);
    /** Attempt safe PWM duty, then release synchronously; NULL is harmless.
     * Confirm a safe write first when required for handoff. Retain the context
     * after failure instead of permitting a second owner.
     */
    void (*destroy)(void *);
} rptadv_hwmon_descriptor_v1;

/** Return the immutable descriptor for this DSO's lifetime. */
const rptadv_hwmon_descriptor_v1 *rptadv_hwmon_adapter_descriptor_v1(void);
#ifdef __cplusplus
}
#endif
#endif
