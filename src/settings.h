/** @file
 * @brief Resolved node and identifier settings with shared-default inheritance.
 */
#ifndef RPT_ADVANCED_SETTINGS_H
#define RPT_ADVANCED_SETTINGS_H
#include "config.h"

/** @brief Validate one option against the same schema used for resolution.
 * @param identifier True selects ID settings; false selects node settings.
 * @param key Option name.
 * @param value Trimmed option value.
 * @return Null on success, or a stable diagnostic for unknown names or invalid values.
 */
const char *ra_settings_validate(bool identifier, const char *key, const char *value);

/** @brief One node's controller and media settings. Strings are borrowed. */
struct ra_node_settings {
    bool enabled;         /**< Start this node's controller. */
    bool full_duplex;     /**< Permit simultaneous receive and transmit. */
    uint64_t hang_ms;     /**< Transmit hang time in milliseconds. */
    uint64_t sample_rate; /**< Requested rate; zero selects hardware-bounded automatic mode. */
    const char *channel;  /**< USBRadioPlus channel identifier, without the technology prefix. */
    const char *codec;    /**< Asterisk codec name; empty selects signed linear automatically. */
};

/** @brief One ID set after all inheritance has been applied. Strings are borrowed. */
struct ra_identifier_settings {
    uint64_t interval_ms;        /**< Positive scheduling interval. */
    uint64_t priority;           /**< Nonnegative priority, bounded by INT_MAX for the scheduler. */
    bool first_key_only;         /**< Identify only after a qualifying idle period. */
    bool regardless_of_activity; /**< Periodic identification during inactivity. */
    const char *file;            /**< Explicit sound-file path; empty disables this medium. */
    const char *speech_text;     /**< Piper text; empty disables speech. */
    const char *speech_model;    /**< Local Piper model path. */
    uint64_t speech_speed_percent; /**< Playback speaking rate relative to the model default. */
    const char *morse_text;        /**< Terminal fallback text; empty disables Morse. */
    uint64_t morse_speed_wpm;      /**< Morse speed in PARIS words per minute. */
    uint64_t morse_frequency_hz;   /**< Morse tone frequency; runtime also checks Nyquist. */
};

/** @brief Resolve node settings without modifying the output on failure.
 * @param entries Parsed entries with non-null strings.
 * @param count Entry count; zero permits null entries.
 * @param node Named node section, inheriting from general.
 * @param result Receives settings on success, borrowing entry strings.
 * @return Invalid option name, or null on success. Unknown options are checked by the loader.
 */
const char *ra_node_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                     const char *node, struct ra_node_settings *result);

/** @brief Resolve ID settings from identifier, node defaults, then the ID set.
 * @param entries Parsed entries with non-null strings.
 * @param count Entry count; zero permits null entries.
 * @param node Scoped identifier-default section, or null.
 * @param set Scoped identifier-set section, or null.
 * @param result Receives settings on success, borrowing entry strings.
 * @return Invalid option name, or null on success; failure leaves result unchanged.
 */
const char *ra_identifier_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                           const char *node, const char *set,
                                           struct ra_identifier_settings *result);
#endif
