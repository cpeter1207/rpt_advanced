/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Resolved node and scheduled-media settings with shared-default inheritance.
 */
#ifndef RPT_ADVANCED_SETTINGS_H
#define RPT_ADVANCED_SETTINGS_H
#include "config.h"
#include "link_command.h"
#include "scheduled_action.h"
#include "scheduled_event.h"

/** @brief Maximum local or remote node-identity bytes, including the terminating null byte.
 *
 * This matches the fixed direct-peer transport identity used by the scheduler and link layer.
 * Configuration therefore permits at most 63 identity bytes and never relies on truncation.
 */
#define RA_NODE_NAME_MAX 64U

/** @brief Validate one option against the same schema used for resolution.
 * @param identifier True selects ID settings; false selects node settings.
 * @param key Option name.
 * @param value Trimmed option value.
 * @return Null on success, or a stable diagnostic for unknown names or invalid values.
 */
const char *ra_settings_validate(bool identifier, const char *key, const char *value);

/** @brief Validate an option for one documented configuration-section category.
 */
enum ra_settings_kind {
    RA_SETTINGS_NODE,
    RA_SETTINGS_IDENTIFIER,
    RA_SETTINGS_ANNOUNCEMENT,
    RA_SETTINGS_COURTESY,
    RA_SETTINGS_COURTESY_SET,
    RA_SETTINGS_MORSE,
    RA_SETTINGS_SPEECH,
    RA_SETTINGS_TIME,
    RA_SETTINGS_TEMPLATE,
    RA_SETTINGS_MACRO,
    RA_SETTINGS_EVENT
};

/** @brief Select the network directory sources checked after a local static override. */
enum ra_link_lookup_method {
    RA_LINK_LOOKUP_BOTH, /**< Check ASL DNS first, then the configured external file. */
    RA_LINK_LOOKUP_DNS,  /**< Check ASL DNS only. */
    RA_LINK_LOOKUP_FILE  /**< Check the configured external file only. */
};
/** @brief Validate one option for a specific documented section category.
 * @param kind Section category that owns @p key.
 * @param key Exact option name.
 * @param value Trimmed option value.
 * @return Null on success, or a stable diagnostic for an unknown or invalid option.
 */
const char *ra_settings_validate_kind(enum ra_settings_kind kind, const char *key,
                                      const char *value);

/** @brief One node's controller and media settings. Strings are borrowed. */
struct ra_node_settings {
    bool enabled;                 /**< Start this node's controller. */
    bool full_duplex;             /**< Permit simultaneous receive and transmit. */
    bool dtmf_muting;             /**< Silence completed local DTMF frames before routing. */
    uint64_t hang_ms;             /**< Transmit hang time in milliseconds. */
    uint64_t transmit_timeout_ms; /**< Continuous-PTT watchdog duration; zero disables it. */
    uint64_t timeout_lockout_ms;  /**< Post-timeout PTT lockout duration; zero releases on unkey. */
    uint64_t
        kerchunk_max_ms; /**< Maximum receive duration treated as a kerchunk; zero disables it. */
    int64_t telemetry_duck_db;  /**< Receive-active identifier and telemetry attenuation in dB. */
    uint64_t courtesy_delay_ms; /**< Receiver/link unkey-to-courtesy delay. */
    uint64_t sample_rate; /**< Requested rate; zero selects hardware-bounded automatic mode. */
    const char *channel;  /**< USBRadioPlus channel identifier, without the technology prefix. */
    const char
        *callsign;     /**< Optional station callsign, up to 63 bytes, used by message templates. */
    const char *codec; /**< Asterisk codec name; empty selects signed linear automatically. */
    const char *link_allow_nodes; /**< Incoming allowlist; empty accepts all verified nodes. */
    const char *link_deny_nodes;  /**< Incoming denylist, overriding allowlist membership. */
    const char *link_static_directory_file; /**< Optional local-priority static node directory. */
    const char *link_directory_file;        /**< Optional ASL external-node directory. */
    enum ra_link_lookup_method link_lookup_method; /**< DNS/file selection after static lookup. */
    struct ra_link_command_mapping link_commands[RA_LINK_ACTION_COUNT]; /**< Inherited prefixes. */
};

/** @brief One ID set after all inheritance has been applied. Strings are borrowed. */
struct ra_identifier_settings {
    uint64_t interval_ms;        /**< Positive scheduling interval. */
    uint64_t priority;           /**< Nonnegative priority, bounded by INT_MAX for the scheduler. */
    bool first_key_only;         /**< Identify only after a qualifying idle period. */
    bool regardless_of_activity; /**< Periodic identification during inactivity. */
    bool polite; /**< Defer a due ID while reception or queued telemetry is active. */
    uint64_t polite_maximum_wait_ms; /**< Bounded polite-defer interval after the ID becomes due. */
    const char *file;                /**< Explicit sound-file path; empty disables this medium. */
    const char *speech_text;         /**< Piper text; empty disables speech. */
    const char *speech_model;        /**< Local Piper model path. */
    uint64_t speech_speed_percent;   /**< Playback speaking rate relative to the model default. */
    int64_t speech_level_db;         /**< Gain applied only to synthesized speech. */
    const char *morse_text;          /**< Terminal fallback text; empty disables Morse. */
    uint64_t morse_speed_wpm;        /**< Morse speed in PARIS words per minute. */
    uint64_t morse_frequency_hz;     /**< Morse tone frequency; runtime also checks Nyquist. */
    int64_t morse_level_db;          /**< Morse tone level relative to full-scale PCM. */
};

/** @brief One announcement set after inherited interval and media defaults are resolved.
 *
 * @p media deliberately reuses the established file-to-speech-to-Morse representation so
 * preparation and playback follow precisely the same fallback rules as identifiers. Its
 * identifier-only scheduling members are not used for announcements.
 */
struct ra_announcement_settings {
    uint64_t interval_ms;                /**< Zero plays on every transmitter release. */
    struct ra_identifier_settings media; /**< Resolved reusable media settings. */
};

/** @brief Input selected by one named courtesy tone. */
enum ra_courtesy_input {
    RA_COURTESY_INPUT_NONE,     /**< No input was assigned. */
    RA_COURTESY_INPUT_RECEIVER, /**< Local receiver carrier transition. */
    RA_COURTESY_INPUT_LINK      /**< One direct linked peer transition. */
};

/** @brief One named courtesy tone after inherited media defaults are resolved. */
struct ra_courtesy_settings {
    struct ra_identifier_settings media; /**< Resolved file, speech, and Morse fallback media. */
    const char *tone_sequence;    /**< Optional generated-tone sequence after file and speech. */
    enum ra_courtesy_input input; /**< Required source assignment for this named tone. */
    const char *remote_node;      /**< Optional permanent direct peer for a link-specific tone. */
    int64_t
        level_db; /**< Uniform media level and default tone-segment level from -60 through 0 dB. */
};

/** @brief One node's local clock-announcement format. */
struct ra_time_settings {
    uint64_t format; /**< Clock format: 12 or 24 hours. */
};

/** @brief One resolved named message template. Strings are configuration-owned. */
struct ra_template_settings {
    const char *name; /**< Case-sensitive template label from its section header. */
    const char *text; /**< Strict message text using documented `${name}` substitutions. */
};

/** @brief One resolved named controller-operation macro. Strings are configuration-owned. */
struct ra_macro_settings {
    const char *name;                /**< Case-sensitive macro label from its section header. */
    enum ra_scheduled_action action; /**< Validated controller operation. */
    const char
        *target_node; /**< Required up-to-63-byte node identity for connect and disconnect. */
};

/** @brief One resolved zero-time event. Strings are configuration-owned. */
struct ra_event_settings {
    const char *name; /**< Case-sensitive event label from its section header. */
    const char *at;   /**< Original strict local-time trigger text. */
    struct ra_scheduled_event_time trigger; /**< Parsed local-time trigger. */
    const char *template_name;              /**< Optional named template label. */
    const char *message;                    /**< Optional direct message-template text. */
    const char *macro_name;                 /**< Optional named macro label. */
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
 * @param node Node name whose speech and Morse defaults apply, or null.
 * @param set Scoped identifier-set section, or null.
 * @param result Receives settings on success, borrowing entry strings.
 * @return Invalid option name, or null on success; failure leaves result unchanged.
 */
const char *ra_identifier_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                           const char *node, const char *set,
                                           struct ra_identifier_settings *result);

/** @brief Resolve announcement settings from announcement, node defaults, then the set.
 * @param entries Parsed entries with non-null strings.
 * @param count Entry count; zero permits null entries.
 * @param node Node name whose speech and Morse defaults apply, or null.
 * @param set Scoped announcement-set section, or null.
 * @param result Receives settings on success, borrowing entry strings.
 * @return Invalid option name, or null on success; failure leaves result unchanged.
 *
 * Announcement intervals accept zero, which requests playback on each completed transmitter
 * release. Positive intervals are scheduled by the controller after successful playback.
 */
const char *ra_announcement_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                             const char *node, const char *set,
                                             struct ra_announcement_settings *result);

/** @brief Resolve a named courtesy tone from shared, node, and set configuration.
 * @param entries Parsed entries with non-null strings.
 * @param count Entry count; zero permits null entries.
 * @param node Node name whose speech and Morse defaults apply.
 * @param set Scoped courtesy-tone section, which must be non-null.
 * @param result Receives resolved media and assignment on success.
 * @return Invalid option name, incomplete assignment, or null on success.
 *
 * A named tone must assign `input = receiver` or `input = link`. `remote_node` is allowed only
 * for a link tone and identifies a permanent direct peer. Prepared media uses file, speech,
 * generated tone sequence, then Morse fallback order.
 */
const char *ra_courtesy_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                         const char *node, const char *set,
                                         struct ra_courtesy_settings *result);

/** @brief Resolve flat and node-scoped time settings without altering output on failure.
 * @param entries Parsed configuration entries, or null when @p count is zero.
 * @param count Entry count.
 * @param node Node name whose time section applies, or null for flat defaults only.
 * @param result Receives the resolved settings on success.
 * @return Invalid option name, or null on success.
 */
const char *ra_time_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                     const char *node, struct ra_time_settings *result);

/** @brief Resolve a named template from global defaults and a same-label node override.
 * @param entries Parsed configuration entries, or null when @p count is zero.
 * @param count Entry count.
 * @param node Node whose same-label override applies, or null for a global template.
 * @param set Complete discovered template section.
 * @param result Receives immutable, configuration-owned strings on success.
 * @return Invalid option name, incomplete template, or null on success.
 */
const char *ra_template_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                         const char *node, const char *set,
                                         struct ra_template_settings *result);

/** @brief Resolve a named macro from global defaults and a same-label node override.
 * @param entries Parsed configuration entries, or null when @p count is zero.
 * @param count Entry count.
 * @param node Node whose same-label override applies, or null for a global macro.
 * @param set Complete discovered macro section.
 * @param result Receives immutable, validated operation settings on success.
 * @return Invalid option name, incomplete macro, or null on success.
 */
const char *ra_macro_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                      const char *node, const char *set,
                                      struct ra_macro_settings *result);

/** @brief Resolve one node-scoped zero-time event without changing output on failure.
 * @param entries Parsed configuration entries, or null when @p count is zero.
 * @param count Entry count.
 * @param set Complete discovered event section.
 * @param result Receives immutable parsed event settings on success.
 * @return Invalid option name, incomplete event, or null on success.
 */
const char *ra_event_settings_resolve(const struct ra_config_entry *entries, size_t count,
                                      const char *set, struct ra_event_settings *result);
#endif
