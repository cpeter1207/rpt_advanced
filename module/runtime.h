/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Ownership of configured radio controllers and their running workers.
 */
#ifndef RPT_ADVANCED_RUNTIME_H
#define RPT_ADVANCED_RUNTIME_H
#include "document.h"
#include "link_command.h"
#include <stddef.h>

struct ra_runtime_node;
struct ast_channel;
struct ast_format;
struct ast_format_cap;
struct ra_link_peer_status;
/** @brief Nonblocking delivery of decoded digits to the module's control queue.
 * @param node Borrowed node name; copy before returning if retained.
 * @param digit Completed digit, or zero for the interdigit timeout.
 * @param now_ms Monotonic detection time.
 */
typedef void (*ra_digit_handler)(const char *node, char digit, uint64_t now_ms);
/** @brief Nonblocking delivery of a direct-link lifecycle event to module control.
 * @param local Local endpoint that observed the event.
 * @param remote Direct peer endpoint.
 * @param connected True after attach, false after detach.
 */
typedef void (*ra_link_event_handler)(const char *local, const char *remote, bool connected);
/** @brief Complete operation copied out of node-owned command state. */
struct ra_link_operation {
    enum ra_link_action action; /**< Requested linking action. */
    char remote[64];            /**< Decimal destination, resolved from zero shorthand if needed. */
    char digit;                 /**< Remote-mode digit, or zero when selecting the remote peer. */
};
/** @brief Owned call preparation, independent of runtime configuration lifetime. */
struct ra_link_dial {
    char *destination;              /**< Owned resolved dial string. */
    struct ast_format **candidates; /**< Owned ordered wire-format references. */
    size_t candidate_count;         /**< Number of entries in candidates. */
};
/** @brief Module-owned nodes; configuration strings must outlive this runtime. */
struct ra_runtime {
    struct ra_runtime_node *nodes; /**< Private list, initially null. */
    ra_digit_handler digit;        /**< Control-queue submission callback retained across reload. */
    ra_link_event_handler event;   /**< Control-queue submission callback for link lifecycle. */
};

/** @brief Start all enabled nodes from an already validated configuration.
 * @param runtime Empty destination; unchanged on failure.
 * @param document Immutable configuration retained until stop completes.
 * @return Null on success or a diagnostic after releasing all partial resources.
 */
const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document);

/** @brief Reconfigure a running runtime without replacing matching nodes' link hubs.
 * @param runtime Active runtime whose caller serializes all lifecycle operations.
 * @param current Valid configuration currently backing runtime-owned borrowed strings.
 * @param replacement Valid candidate configuration retained by the caller on failure.
 * @return Null after replacing enabled nodes and preserving their attached/retry link state, or a
 * diagnostic after restoring the current runtime.
 *
 * Matching node objects keep their stable hub address, peer readers, manager, retry records, and
 * rate-bound routing buffers. Removing or disabling a node ends its direct peers and cancels its
 * retries; a candidate cannot change the local PCM rate of a hub that has allocated link routing
 * state. The caller destroys `current` only after success and destroys `replacement` on failure.
 */
const char *ra_runtime_reload(struct ra_runtime *runtime, const struct ra_document *current,
                              const struct ra_document *replacement);

/** @brief Stop, unkey, and join all workers before releasing their media state.
 * @param runtime Owned runtime, safe when empty.
 */
void ra_runtime_stop(struct ra_runtime *runtime);

/** @brief Collect one decoded digit and resolve an inherited linking command.
 * @param runtime Active runtime, protected by the module lock.
 * @param local Local node name.
 * @param digit DTMF character or zero for timeout.
 * @param now_ms Detection timestamp.
 * @param operation Written only when a complete valid command is ready.
 * @return True for an executable operation.
 */
bool ra_runtime_digit(struct ra_runtime *runtime, const char *local, char digit, uint64_t now_ms,
                      struct ra_link_operation *operation);

/** @brief Abandon partial commands after a control-queue delivery failure.
 * @param runtime Active runtime protected by the module lock.
 */
void ra_runtime_reset_digits(struct ra_runtime *runtime);

/** @brief Admit an independently authenticated incoming peer to a running node.
 * @param runtime Active runtime; caller serializes admission with reload/stop.
 * @param local Exact destination node.
 * @param remote Verified decimal calling node identity.
 * @param channel Answered channel; ownership transfers on success only.
 * @param verified Result of independent directory/address identity verification.
 * @return Zero on successful ownership transfer; minus one on rejection or setup failure.
 */
int ra_runtime_accept(struct ra_runtime *runtime, const char *local, const char *remote,
                      struct ast_channel *channel, bool verified);

/** @brief Prepare a directory-verified call while the runtime is protected.
 * @param runtime Active runtime; caller serializes with reload.
 * @param local Local node name.
 * @param remote Decimal destination node.
 * @param dial Empty destination; owns resources on success.
 * @return Zero on preparation, minus one on lookup or media failure.
 */
int ra_runtime_prepare_link(struct ra_runtime *runtime, const char *local, const char *remote,
                            struct ra_link_dial *dial);

/** @brief Dial without holding the runtime lock, consuming prepared resources.
 * @param dial Prepared call, consumed even on failure.
 * @param local Local caller identity, retained throughout the call.
 * @return Owned answered channel, or null on failure.
 */
struct ast_channel *ra_link_dial_run(struct ra_link_dial *dial, const char *local);

/** @brief Attach an answered outgoing channel after revalidating runtime identity.
 * @param runtime Active runtime; caller serializes with reload.
 * @param local Local node name.
 * @param remote Remote node name.
 * @param channel Answered channel; ownership transfers on success only.
 * @param transmit Enable outbound audio; false selects monitor.
 * @param forward Forward received audio to other peers.
 * @param permanent Redial after an unexpected transport failure.
 * @return Zero on attachment, minus one on unknown node or attachment failure.
 */
int ra_runtime_attach_link(struct ra_runtime *runtime, const char *local, const char *remote,
                           struct ast_channel *channel, bool transmit, bool forward,
                           bool permanent);

/** @brief Retain a permanent link request whose first dial or attachment did not succeed.
 * @param runtime Active runtime; caller serializes with reload and other control commands.
 * @param local Local node name.
 * @param remote Remote node name.
 * @param transmit Enable outbound audio after recovery.
 * @param forward Forward recovered peer audio to other peers.
 * @return True after the node records automatic retry intent; false for an unknown node, an
 * existing route, unavailable recovery callback, or allocation/manager-start failure.
 *
 * This carries no channel ownership. It lets a permanent DTMF link request survive an initial
 * transient dial or attachment failure and uses the same hub recovery policy as an ended peer.
 */
bool ra_runtime_retain_permanent_link(struct ra_runtime *runtime, const char *local,
                                      const char *remote, bool transmit, bool forward);

/** @brief Disconnect an exact nonpermanent peer from a local node.
 * @param runtime Active runtime; caller serializes with reload and other commands.
 * @param local Local node name.
 * @param remote Remote node name.
 * @return True when an existing nonpermanent peer was disconnected.
 */
bool ra_runtime_disconnect(struct ra_runtime *runtime, const char *local, const char *remote);

/** @brief Disconnect a permanent peer and prevent any retained retry from redialing it.
 * @param runtime Active runtime; caller serializes with reload and other commands.
 * @param local Local node name.
 * @param remote Remote node name.
 * @return True when an attached peer or pending permanent recovery was removed.
 */
bool ra_runtime_disconnect_permanent(struct ra_runtime *runtime, const char *local,
                                     const char *remote);

/** @brief Disconnect every peer attached to a local node.
 * @param runtime Active runtime.
 * @param local Local node name.
 * @return Number of peers released, or zero for an unknown node.
 */
size_t ra_runtime_disconnect_all(struct ra_runtime *runtime, const char *local);

/** @brief Resume every link retained by a prior disconnect-all for a local node.
 * @param runtime Active runtime; caller serializes with reload and other commands.
 * @param local Local node name.
 * @return Number of retained links made immediately eligible, or zero when unknown.
 */
size_t ra_runtime_reconnect_all(struct ra_runtime *runtime, const char *local);

/** @brief Queue concise spoken RF link status with a Morse fallback.
 * @param runtime Active runtime; caller serializes it with reload and other controls.
 * @param local Local node name.
 * @param last_keyed Select the remembered direct peer instead of current-link status.
 * @return Zero when status is queued, minus one for an unknown node, invalid text, or a full queue.
 */
int ra_runtime_queue_link_status(struct ra_runtime *runtime, const char *local, bool last_keyed);

/** @brief Queue a spoken connect or disconnect report for every configured local node.
 * @param runtime Active runtime whose caller serializes configuration ownership.
 * @param first One endpoint of the changed direct link.
 * @param second Other endpoint of the changed direct link.
 * @param connected True for connection, false for disconnection.
 * @return Zero when every node accepted the bounded telemetry report, minus one otherwise.
 */
int ra_runtime_queue_link_event(struct ra_runtime *runtime, const char *first, const char *second,
                                bool connected);

/** @brief Snapshot directly attached peers for a complete control-plane status listing.
 * @param runtime Active runtime; caller serializes it with reload and other controls.
 * @param local Local node name.
 * @param entries Caller-owned output entries, or null when only counting.
 * @param capacity Number of output entries.
 * @param count Receives the complete direct-peer count when non-null.
 * @return True when the local node exists; false otherwise.
 */
bool ra_runtime_link_snapshot(struct ra_runtime *runtime, const char *local,
                              struct ra_link_peer_status *entries, size_t capacity, size_t *count);

/** @brief Return an owned app_rpt-style topology report for a local node.
 * @param runtime Active runtime; caller serializes it with reload and other controls.
 * @param local Local node name.
 * @return Asterisk-allocated topology text, or null for an unknown node or allocation failure.
 *
 * The caller releases a non-null result with ast_free. This control-plane report may include
 * direct peers and their validated remote topology advertisements; it is never used by audio.
 */
char *ra_runtime_link_topology(struct ra_runtime *runtime, const char *local);

/** @brief Select or feed one directly connected remote-command peer.
 * @param runtime Active runtime; caller serializes it with reload and control commands.
 * @param local Local node name.
 * @param remote Directly connected remote node that must pass this node's current deny-first
 * policy; selection additionally requires independent directory resolution.
 * @param digit Zero selects remote mode; otherwise queues one DTMF digit.
 * @return Zero on success, minus one if policy, directory proof, or direct-peer availability
 * rejects the request.
 */
int ra_runtime_remote_command(struct ra_runtime *runtime, const char *local, const char *remote,
                              char digit);

/** @brief Verify the incoming address and apply the destination node's access lists.
 * @param runtime Active runtime; caller serializes with reload.
 * @param local Destination node.
 * @param remote Claimed remote identity.
 * @param peer_ip Numeric IAX source address.
 * @return True only for an independently verified, permitted caller.
 */
bool ra_runtime_authorize(struct ra_runtime *runtime, const char *local, const char *remote,
                          const char *peer_ip);
#endif
