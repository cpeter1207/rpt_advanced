/* SPDX-License-Identifier: GPL-2.0-only */
/** @file
 * @brief Ownership of configured radio controllers and their running workers.
 */
#ifndef RPT_ADVANCED_RUNTIME_H
#define RPT_ADVANCED_RUNTIME_H
#include "document.h"
#include "link_command.h"

struct ra_runtime_node;
struct ast_channel;
struct ast_format_cap;
/** @brief Nonblocking delivery of decoded digits to the module's control queue.
 * @param node Borrowed node name; copy before returning if retained.
 * @param digit Completed digit, or zero for the interdigit timeout.
 * @param now_ms Monotonic detection time.
 */
typedef void (*ra_digit_handler)(const char *node, char digit, uint64_t now_ms);
/** @brief Complete operation copied out of node-owned command state. */
struct ra_link_operation {
    enum ra_link_action action; /**< Requested linking action. */
    char remote[64];            /**< Decimal destination, resolved from zero shorthand if needed. */
};
/** @brief Owned call preparation, independent of runtime configuration lifetime. */
struct ra_link_dial {
    char *destination;            /**< Owned resolved dial string. */
    struct ast_format_cap *offer; /**< Owned codec offer. */
};
/** @brief Module-owned nodes; configuration strings must outlive this runtime. */
struct ra_runtime {
    struct ra_runtime_node *nodes; /**< Private list, initially null. */
    ra_digit_handler digit;        /**< Control-queue submission callback retained across reload. */
};

/** @brief Start all enabled nodes from an already validated configuration.
 * @param runtime Empty destination; unchanged on failure.
 * @param document Immutable configuration retained until stop completes.
 * @return Null on success or a diagnostic after releasing all partial resources.
 */
const char *ra_runtime_start(struct ra_runtime *runtime, const struct ra_document *document);

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
 * @param same_server Independently verified same-server origin.
 * @return Zero on successful ownership transfer; minus one on rejection or setup failure.
 */
int ra_runtime_accept(struct ra_runtime *runtime, const char *local, const char *remote,
                      struct ast_channel *channel, bool verified, bool same_server);

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

/** @brief Disconnect an exact peer from a local node.
 * @param runtime Active runtime; caller serializes with reload and other commands.
 * @param local Local node name.
 * @param remote Remote node name.
 * @return True when an existing peer was disconnected.
 */
bool ra_runtime_disconnect(struct ra_runtime *runtime, const char *local, const char *remote);

/** @brief Disconnect every peer attached to a local node.
 * @param runtime Active runtime.
 * @param local Local node name.
 * @return Number of peers released, or zero for an unknown node.
 */
size_t ra_runtime_disconnect_all(struct ra_runtime *runtime, const char *local);

/** @brief Count peers attached to a local node.
 * @param runtime Active runtime.
 * @param local Local node name.
 * @return Number of attached peers, or zero for an unknown node.
 */
size_t ra_runtime_link_count(struct ra_runtime *runtime, const char *local);

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
