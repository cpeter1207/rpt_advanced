/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief One node's identifier, audio, and transmit-ownership integration.
 */
#ifndef RPT_ADVANCED_CONTROLLER_H
#define RPT_ADVANCED_CONTROLLER_H
#include "duplex.h"
#include "identifier.h"
#include "playback.h"
#include <stdatomic.h>

/** @brief Maximum printable RF-status text including its terminating null byte. */
#define RA_CONTROLLER_STATUS_TEXT_MAX 128
/** @brief Bounded control-to-radio status queue depth. */
#define RA_CONTROLLER_STATUS_QUEUE_DEPTH 4
/** @brief Silence required after local receiver unkey before RF status playback begins. */
#define RA_CONTROLLER_STATUS_UNKEY_DELAY_MS 250U
/** @brief Maximum receiver and link courtesy announcements awaiting playback. */
#define RA_CONTROLLER_COURTESY_QUEUE_DEPTH 2

/** @brief Source that caused a pending courtesy announcement. */
enum ra_courtesy_source {
    RA_COURTESY_RECEIVER, /**< Local receiver unkeyed. */
    RA_COURTESY_LINK      /**< Linked receiver unkeyed. */
};

/** @brief Prepared identifier media and its resolved configuration. */
struct ra_controller_id {
    struct ra_identifier_settings settings; /**< Borrowed immutable configuration strings. */
    const int16_t *audio;                   /**< Borrowed file/speech PCM, or null. */
    size_t samples;                         /**< Prepared samples at the controller rate. */
};

/** @brief One control-prepared RF status announcement. */
struct ra_controller_status {
    char text[RA_CONTROLLER_STATUS_TEXT_MAX]; /**< Immutable speech and Morse fallback text. */
    int16_t *audio;                           /**< Owned prepared speech PCM, or null for Morse. */
    size_t samples;                           /**< Prepared PCM samples at the controller rate. */
};

/** @brief Single-threaded node state; arrays remain owned by the node lifecycle. */
struct ra_controller {
    const struct ra_controller_id *ids; /**< Immutable prepared identifiers. */
    struct ra_id_rule *rules;           /**< Writable scheduling rules, parallel to ids. */
    struct ra_id_state *states;         /**< Writable scheduling state, parallel to ids. */
    size_t count;                       /**< Number of IDs; zero permits null arrays. */
    unsigned int rate;                  /**< Negotiated PCM sample rate. */
    bool full_duplex;                   /**< Whether local reception may transmit. */
    bool link_active; /**< Current linked-receiver activity; it interrupts prepared identifiers. */
    const int16_t *link_audio;  /**< Borrowed link mix for the current block, or null. */
    uint64_t hang_ms;           /**< Transmitter hang time. */
    int telemetry_duck_db;      /**< Receive-active telemetry attenuation, from -60 through 0 dB. */
    double telemetry_duck_gain; /**< Precomputed receive-active telemetry gain. */
    double telemetry_gain;      /**< Audio-thread smoothed telemetry gain. */
    struct ra_duplex_state duplex;    /**< Current PTT request and hang state. */
    struct ra_playback playback;      /**< Currently selected identifier playback. */
    size_t playing;                   /**< Active identifier index, or SIZE_MAX. */
    bool receiving;                   /**< Previous qualified receiver indication. */
    uint64_t last_activity_ms;        /**< Last receive activity, initially startup. */
    uint64_t key_idle_ms;             /**< Idle period preceding the current conversation. */
    unsigned int status_speed_wpm;    /**< Per-node Morse speed used for RF-status fallback. */
    unsigned int status_frequency_hz; /**< Per-node Morse frequency used for RF-status fallback. */
    int status_level_db;              /**< Per-node Morse level used for RF-status fallback. */
    struct ra_controller_status
        status_queue[RA_CONTROLLER_STATUS_QUEUE_DEPTH]; /**< Fixed SPSC status slots. */
    atomic_uint status_write;      /**< Next slot produced by the control executor. */
    atomic_uint status_read;       /**< Current slot retained until radio playback completes. */
    unsigned int status_reclaimed; /**< Next consumed slot whose PCM control must release. */
    struct ra_playback
        status_playback;        /**< Worker-owned prepared-speech/Morse-fallback playback. */
    bool status_playing;        /**< A queued status is currently rendering. */
    uint64_t receiver_unkey_ms; /**< Local receiver's most recent falling-edge timestamp. */
    struct ra_controller_id
        courtesy[RA_CONTROLLER_COURTESY_QUEUE_DEPTH]; /**< Prepared courtesy media. */
    struct ra_playback courtesy_playback;             /**< Worker-owned active courtesy playback. */
    enum ra_courtesy_source courtesy_pending[RA_CONTROLLER_COURTESY_QUEUE_DEPTH]; /**< Due order. */
    size_t courtesy_pending_count; /**< Pending courtesy announcements. */
    bool courtesy_playing;         /**< A courtesy announcement is currently rendering. */
    uint64_t courtesy_due_ms;      /**< Earliest allowed courtesy playback time. */
    uint64_t courtesy_delay_ms;    /**< Configured unkey-to-courtesy delay. */
    bool link_was_active;          /**< Previous linked-receiver activity. */
};

/** @brief Validate media and initialize a controller whose array bindings are set.
 * @param state Controller with ids, rules, states, count, rate, duplex and hang configured.
 * @param now_ms Monotonic startup time.
 * @return False for an unrenderable Morse configuration; no state changes on failure.
 */
bool ra_controller_start(struct ra_controller *state, uint64_t now_ms);

/** @brief Queue prepared spoken RF status with Morse fallback without touching the radio callback.
 * @param state Started controller retained until its worker stops.
 * @param text ASCII status text accepted by the Morse encoder and Piper.
 * @param audio Owned prepared speech PCM, or null when synthesis was unavailable.
 * @param samples Prepared PCM sample count; zero requires a null audio pointer.
 * @return True when queued; false for invalid text, invalid audio, or a full queue.
 *
 * The serial control executor is the sole producer and the radio worker is the sole consumer.
 * Release/acquire publication keeps text and prepared PCM immutable during playback. On false the
 * caller remains responsible for @p audio; on true ownership transfers to the controller.
 */
bool ra_controller_queue_status(struct ra_controller *state, const char *text, int16_t *audio,
                                size_t samples);

/** @brief Return finished prepared-status PCM to the control executor for release.
 * @param state Started controller with its single control producer.
 * @param audio Output array for completed owned PCM pointers, or null when @p capacity is zero.
 * @param capacity Number of output entries.
 * @return Number of completed PCM allocations returned.
 *
 * This is never called from the hardware-paced worker, so releasing allocations cannot affect
 * real-time audio. A returned pointer is no longer reachable by that worker.
 */
size_t ra_controller_reclaim_status(struct ra_controller *state, int16_t **audio, size_t capacity);

/** @brief Process one hardware-paced PCM block or a sample-free carrier event.
 * @param state Successfully started controller with unchanged configuration/media.
 * @param receiving Qualified receiver indication.
 * @param audio Receive PCM replaced by transmit PCM; null only when samples is zero.
 * @param samples Number of PCM samples. Zero never advances identifier playback.
 * @param now_ms Monotonic event time, never decreasing.
 * @return Requested PTT state. Half-duplex receive always overrides transmission.
 */
bool ra_controller_process(struct ra_controller *state, bool receiving, int16_t *audio,
                           size_t samples, uint64_t now_ms);
#endif
