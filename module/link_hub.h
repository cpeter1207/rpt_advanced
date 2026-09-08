/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Per-node peer ownership and hardware-paced mix-minus routing.
 */
#ifndef RPT_ADVANCED_LINK_HUB_H
#define RPT_ADVANCED_LINK_HUB_H
#include "controller.h"
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
struct ast_channel;
struct ast_format;
struct ra_link_port;

/** @brief Maximum direct-peer identity length including the terminating null byte. */
#define RA_LINK_PEER_NAME_MAX 64

/** @brief One caller-owned snapshot of an attached or retained direct peer's routing state. */
struct ra_link_peer_status {
    char name[RA_LINK_PEER_NAME_MAX]; /**< Verified remote node identity. */
    bool transmit;                    /**< Program audio is sent to this peer. */
    bool forward;                     /**< Peer audio is forwarded to other peers. */
    bool permanent;                   /**< Unexpected loss is eligible for recovery. */
    bool retrying; /**< Link is retained while its transport is absent or reconnecting. */
    bool paused;   /**< Reconnect awaits the operator's reconnect-all command. */
    uint64_t receive_missing;        /**< Cumulative missing incoming PCM samples. */
    uint64_t consecutive_underruns;  /**< Current consecutive incoming missing PCM samples. */
    uint64_t underrun_average_milli; /**< Ten-second EWMA of missing samples, in millisamples. */
    unsigned int receive_reserve_ms; /**< Current elastic incoming PCM reserve in milliseconds. */
};
/** @brief Redial callback invoked by the hub manager for a retained peer.
 * @param context Runtime-owned callback context.
 * @param remote Decimal remote node identity.
 * @param transmit Preserve outbound audio mode.
 * @param forward Preserve forwarding mode.
 * @param permanent Preserve automatic-recovery status after reconnect-all.
 * @param cancelled Set when an explicit permanent disconnect supersedes this attempt.
 * @param paused Set when disconnect-all retains this attempt for reconnect-all.
 * @return Zero when a replacement peer was attached.
 */
typedef int (*ra_link_reconnect_fn)(void *context, const char *remote, bool transmit, bool forward,
                                    bool permanent, const atomic_bool *cancelled,
                                    const atomic_bool *paused);

/** @brief Deliver one peer-identified IAX DTMF event to node control handling.
 * @param context Borrowed runtime callback context.
 * @param remote Verified direct-peer identity that emitted the event.
 * @param digit Completed conventional DTMF character.
 * @param now_ms Peer-reader monotonic timestamp.
 *
 * The receiver evaluates current node policy after receiving this identity. This permits explicit
 * outbound audio links while preventing a denied peer from controlling the local node.
 */
typedef void (*ra_link_hub_digit_fn)(void *context, const char *remote, char digit,
                                     uint64_t now_ms);

/** @brief Report a direct-link lifecycle event outside audio routing.
 * @param context Borrowed runtime-owned callback context.
 * @param remote Stable direct-peer identity.
 * @param connected True after attach, false after detach.
 */
typedef void (*ra_link_hub_event_fn)(void *context, const char *remote, bool connected);

/** @brief Opaque retained-link recovery record owned by a routing hub. */
struct ra_link_retry;
/** @brief Node-owned routing state initialized with ra_link_hub_init(). */
struct ra_link_hub {
    _Atomic(struct ra_link_port *) ports; /**< Atomically published immutable peer list. */
    atomic_uint readers;                  /**< Audio traversals that may retain detached ports. */
    int16_t *local;                       /**< Original local receive block. */
    int16_t *remote;                      /**< Summed incoming audio for the transmitter. */
    int16_t *outgoing;                    /**< Per-peer mix-minus scratch block. */
    size_t capacity;                      /**< Allocated samples per scratch block. */
    unsigned int rate;                    /**< Local radio PCM sample rate fixed at first attach. */
    pthread_t manager;                    /**< Reaps disconnected peers outside the audio thread. */
    atomic_bool stop;                     /**< Requests manager shutdown. */
    bool manager_started;                 /**< Manager must be joined before releasing the hub. */
    const char *local_name;               /**< Borrowed local node name used for loop prevention. */
    ra_link_reconnect_fn reconnect;       /**< Runtime callback for retained peers. */
    void *reconnect_context;              /**< Borrowed runtime callback context. */
    ra_link_hub_digit_fn digit;           /**< Runtime callback for peer-identified IAX DTMF. */
    void *digit_context;                  /**< Borrowed context paired with digit. */
    ra_link_hub_event_fn event;           /**< Runtime callback for direct-link lifecycle events. */
    void *event_context;                  /**< Borrowed context paired with lifecycle events. */
    struct ra_link_retry *retries;        /**< Manager-owned retained recovery records. */
    atomic_uint topology_generation; /**< Control-plane topology changes pending advertisement. */
    atomic_uint last_keyed_sequence; /**< Lock-free coherent-copy generation for last_keyed. */
    atomic_uchar last_keyed[RA_LINK_PEER_NAME_MAX]; /**< Last direct peer to become active. */
};

/** @brief Initialize an unused routing hub before its first API call. */
void ra_link_hub_init(struct ra_link_hub *hub);

/** @brief Attach an authenticated answered peer without changing a live list on failure.
 * @param hub Node-owned hub.
 * @param name Verified remote node name.
 * @param channel Answered channel, ownership transfers on success only.
 * @param linear Cached local radio PCM format.
 * @param transmit Send audio to this peer; false selects monitor mode.
 * @param forward Forward received audio to other peers; false selects local-monitor mode.
 * @param permanent Redial after an unexpected transport failure.
 * @return Zero on success, minus one on allocation, duplicate, or transport failure.
 */
int ra_link_hub_attach(struct ra_link_hub *hub, const char *name, struct ast_channel *channel,
                       struct ast_format *linear, bool transmit, bool forward, bool permanent);

/** @brief Set the callback used to restore retained peers after recovery events.
 * @param hub Node-owned hub.
 * @param callback Borrowed callback, or null to disable recovery.
 * @param context Borrowed callback context.
 */
void ra_link_hub_set_reconnector(struct ra_link_hub *hub, ra_link_reconnect_fn callback,
                                 void *context);

/** @brief Retain an initial permanent-link request after its first dial or attachment fails.
 * @param hub Node-owned routing hub.
 * @param name Verified remote node name.
 * @param transmit Preserve outbound audio mode for the retry.
 * @param forward Preserve peer-forwarding mode for the retry.
 * @return True after recording an immediately eligible automatic retry; false when recovery is
 * unavailable, that name is already attached or retained, or resources cannot be allocated.
 *
 * The hub starts its control-plane manager when necessary, so this works before any peer has
 * attached. The first manager attempt is due immediately; later failed automatic attempts use
 * the normal bounded exponential delay. An explicit permanent disconnect removes this intent.
 */
bool ra_link_hub_retain_permanent(struct ra_link_hub *hub, const char *name, bool transmit,
                                  bool forward);

/** @brief Set the control-plane recipient for peer-identified IAX DTMF end events.
 * @param hub Node-owned hub.
 * @param callback Borrowed callback, or null to discard incoming peer DTMF.
 * @param context Borrowed callback context.
 *
 * The peer reader invokes this callback without the routing lock and never from the
 * hardware-paced audio callback. Set it before attaching a peer and keep it valid until close;
 * the callback receives the stable identity of the emitting attached peer.
 */
void ra_link_hub_set_digit_handler(struct ra_link_hub *hub, ra_link_hub_digit_fn callback,
                                   void *context);

/** @brief Set the control-plane recipient for direct peer connect/disconnect events.
 * @param hub Initialized routing hub.
 * @param callback Borrowed callback, or null to suppress lifecycle reporting.
 * @param context Borrowed callback context.
 *
 * The hub calls this only after the lifecycle lock has been released and never from the
 * hardware-paced routing callback.
 */
void ra_link_hub_set_event_handler(struct ra_link_hub *hub, ra_link_hub_event_fn callback,
                                   void *context);

/** @brief Disconnect one nonpermanent peer after removing it from the hardware-visible list.
 * @param hub Node-owned hub.
 * @param name Exact remote node name.
 * @return True if the nonpermanent peer existed and was released.
 */
bool ra_link_hub_disconnect(struct ra_link_hub *hub, const char *name);

/** @brief Disconnect one permanent peer and cancel any pending automatic recovery.
 * @param hub Node-owned routing hub.
 * @param name Exact remote node name.
 * @return True when an attached peer or pending retry was removed.
 */
bool ra_link_hub_disconnect_permanent(struct ra_link_hub *hub, const char *name);

/** @brief Release a just-attached recovery peer without cancelling its retained retry intent.
 * @param hub Node-owned routing hub.
 * @param name Exact remote node name.
 * @param permanent Exact permanence of the recovery attempt.
 * @return True when the matching peer was detached.
 */
bool ra_link_hub_detach_reconnect(struct ra_link_hub *hub, const char *name, bool permanent);

/** @brief Disconnect every current peer without stopping the hub manager.
 * @param hub Node-owned routing hub.
 * @return Number of peers released. Their routing modes remain paused until reconnect-all.
 */
size_t ra_link_hub_disconnect_all(struct ra_link_hub *hub);

/** @brief Disconnect every attached nonpermanent peer without retaining it for reconnect-all.
 * @param hub Node-owned routing hub.
 * @return Number of detached temporary peers.
 */
size_t ra_link_hub_disconnect_nonpermanent_all(struct ra_link_hub *hub);

/** @brief Immediately retry every link retained by disconnect-all or a failed permanent dial.
 * @param hub Node-owned routing hub.
 * @return Number of recovery records resumed. Temporary links require this explicit action;
 * permanent links alone retry unexpected transport failures automatically.
 */
size_t ra_link_hub_reconnect_all(struct ra_link_hub *hub);

/** @brief Check whether a reload must preserve peer or retry ownership for a hub.
 * @param hub Node-owned routing hub.
 * @return True when an attached port or retained reconnect request is still owned.
 *
 * This lifecycle query is not a status report. It prevents a rate-changing runtime replacement
 * from closing buffers, peer readers, or delayed reconnect intent that remain live.
 */
bool ra_link_hub_has_retained_state(struct ra_link_hub *hub);

/** @brief Copy attached and retained peer identities and routing modes for a control-plane report.
 * @param hub Node-owned routing hub.
 * @param entries Caller-owned output array, or null when only counting peers.
 * @param capacity Number of available output entries.
 * @return Total non-ended direct-peer and retained-retry count, which may exceed capacity.
 *
 * A retry that is concurrently reconnecting a newly attached peer is not duplicated. This takes
 * the lifecycle lock outside audio processing and never returns borrowed peer data.
 */
size_t ra_link_hub_snapshot(struct ra_link_hub *hub, struct ra_link_peer_status *entries,
                            size_t capacity);

/** @brief Build an app_rpt-compatible system-wide linked-node list for control-plane status.
 * @param hub Node-owned routing hub.
 * @return Caller-owned comma-separated mode/name list, or null on allocation failure.
 *
 * The returned string is allocated with Asterisk's allocator and must be released with ast_free.
 * It is empty when no reportable direct peer exists. Local-monitor peers are omitted; each direct
 * peer contributes its current route and its latest validated `L` advertisement.
 */
char *ra_link_hub_topology(struct ra_link_hub *hub);

/** @brief Copy the most recently active direct peer identity without locking the radio worker.
 * @param hub Node-owned routing hub.
 * @param name Destination with room for RA_LINK_PEER_NAME_MAX bytes.
 * @param capacity Destination capacity.
 * @return True when a complete recorded peer identity was copied.
 *
 * The radio worker writes the identity through lock-free atomics only when a peer newly keys.
 */
bool ra_link_hub_last_keyed(const struct ra_link_hub *hub, char *name, size_t capacity);

/** @brief Calculate the next permanent-link retry delay.
 * @param previous_ms Prior delay, or zero for the first delayed retry.
 * @return One second initially, doubling through a five-minute maximum.
 */
uint64_t ra_link_hub_retry_delay(uint64_t previous_ms);

/** @brief Queue a DTMF digit for one directly connected peer.
 * @param hub Node-owned routing hub.
 * @param name Exact connected peer identity.
 * @param digit DTMF digit to forward.
 * @return Zero when queued, minus one when that peer is absent or unavailable.
 */
int ra_link_hub_send_digit(struct ra_link_hub *hub, const char *name, char digit);

/** @brief Check whether an exact peer is still directly attached.
 * @param hub Node-owned routing hub.
 * @param name Exact remote identity.
 * @return True when the peer is attached and has not ended.
 */
bool ra_link_hub_connected(struct ra_link_hub *hub, const char *name);

/** @brief Disconnect all peers and release routing buffers after the radio worker stops.
 * @param hub Owned hub, safe when empty.
 */
void ra_link_hub_close(struct ra_link_hub *hub);

/** @brief Route one hardware block and render the local transmitter.
 * @param hub Node-owned hub.
 * @param controller Started radio controller.
 * @param receiving Qualified local reception.
 * @param audio Local receive PCM replaced by transmitter PCM.
 * @param samples Hardware block length; zero handles control events only.
 * @param now_ms Monotonic controller time.
 * @return Requested transmitter key state.
 */
bool ra_link_hub_process(struct ra_link_hub *hub, struct ra_controller *controller, bool receiving,
                         int16_t *audio, size_t samples, uint64_t now_ms);
#endif
