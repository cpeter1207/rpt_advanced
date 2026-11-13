/** @file
 * @brief Registration-only SIP/UDP capability. All calls are off audio.
 *
 * One context owns one account and one UDP socket, not a thread. Create prepares
 * only; register_account sends REGISTER after the host commits configuration.
 * The host serializes every
 * call on the same host-owned thread and joins it before releasing the DSO.
 * No adapter-created threads, media, SDP, incoming calls or implicit answer.
 * Credentials are borrowed at create and copied privately, never reported.
 * Digest realms come only from bounded native challenges, not configuration.
 * The host bounds initial authentication and the first observed reauthentication
 * challenge with its configured deadline. Repeated challenges never restart it.
 * Silent native refresh has no start event. The host independently expires
 * binding usability at its last success observation plus accepted_expires.
 * SIP authority and AOR remain distinct from the numeric next-hop endpoint.
 * Endpoint/URI spans use ASCII; URI user permits validated percent escapes.
 * Credentials use bounded UTF-8 without NUL or colon. Username also excludes
 * whitespace/control, quote and backslash; password is a literal secret. The host
 * must not enable native protocol debug/dump/capture while accounts are live.
 * 0 succeeds; 1 is pending/empty; -1 invalid input; -2 stale generation;
 * -3 invalid state; -4 native setup failure. Never retry an ambiguous create.
 * Native setup refuses protocol debug/capture environment overrides and requires
 * the embedding process to ignore SIGPIPE already; it preserves that disposition.
 * Shutdown is asynchronous and can take 30 seconds. Service until stopped.
 * Destroy consumes only a stopped context; otherwise it returns 1 and retains
 * the context and native resources. Keep the DSO lease after failed shutdown.
 */
#ifndef RPTADV_SIP_ADAPTER_H
#define RPTADV_SIP_ADAPTER_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/** Borrowed UTF-8 extent. */
typedef struct rptadv_sip_span_v1 {
    const uint8_t *data; /**< Valid for length bytes. */
    size_t length;       /**< Byte count, excluding terminator. */
} rptadv_sip_span_v1;
/** Account setup. This slice accepts SIP/UDP and numeric transport endpoints. */
typedef struct rptadv_sip_config_v1 {
    uint32_t struct_size;         /**< Complete record size. */
    uint32_t expires;             /**< Requested lifetime in seconds, 1..65535. */
    uint64_t generation;          /**< Nonzero immutable owner generation. */
    rptadv_sip_span_v1 bind;      /**< Numeric SocketAddr; zero port chooses ephemeral. */
    rptadv_sip_span_v1 next_hop;  /**< Numeric SocketAddr, nonzero port. */
    rptadv_sip_span_v1 registrar; /**< SIP registrar authority URI, <=264 bytes. */
    rptadv_sip_span_v1 aor;       /**< SIP address of record, <=1030 bytes. */
    rptadv_sip_span_v1 username;  /**< Literal UTF-8 Digest username, <=255 bytes. */
    rptadv_sip_span_v1 password;  /**< Literal UTF-8 Digest password, <=4096 bytes. */
} rptadv_sip_config_v1;
/** Registration state. */
enum rptadv_sip_state_v1 {
    RPTADV_SIP_PREPARED = 0,    /**< Local setup only; no REGISTER sent. */
    RPTADV_SIP_REGISTERING = 1, /**< Initial authentication or observed reauthentication pending. */
    RPTADV_SIP_REGISTERED = 2,  /**< Last REGISTER succeeded. */
    RPTADV_SIP_UNREGISTERING = 3,  /**< Explicit removal pending. */
    RPTADV_SIP_UNREGISTERED = 4,   /**< Removal succeeded. */
    RPTADV_SIP_FAILED = 5,         /**< Terminal registration failure; no new attempts. */
    RPTADV_SIP_STOPPING = 6,       /**< Native shutdown pending. */
    RPTADV_SIP_STOPPED = 7,        /**< Final native shutdown succeeded; destroy allowed. */
    RPTADV_SIP_SHUTDOWN_FAILED = 8 /**< Native shutdown failed; retain lease. */
};
/** Authoritative redacted snapshot, also used for bounded state-change events. */
typedef struct rptadv_sip_status_v1 {
    uint32_t struct_size; /**< Complete writable record size. */
    uint32_t state;       /**< rptadv_sip_state_v1. */
    uint64_t generation;  /**< Exact account generation. */
    uint64_t sequence;    /**< Monotonic state observation number. */
    uint32_t sip_status;  /**< Numeric response only; no reason/header text. */
    uint32_t
        accepted_expires;    /**< Last accepted Contact expiry; retained during reauthentication. */
    uint32_t registrations;  /**< Successful initial/refresh responses, saturating. */
    uint32_t events_dropped; /**< Oldest events replaced in fixed eight-slot queue. */
} rptadv_sip_status_v1;
/** Sized implementation-neutral ABI 1; every slot is required. */
typedef struct rptadv_sip_descriptor_v1 {
    uint32_t struct_size;   /**< Complete descriptor size. */
    uint32_t abi_version;   /**< 1. */
    uint8_t capability[16]; /**< NUL-padded rptadv.sip. */
    int32_t (*create)(const rptadv_sip_config_v1 *config, void **context); /**< Copy and prepare. */
    int32_t (*service)(void *context, uint64_t generation); /**< Nonblocking native turn. */
    int32_t (*next_event)(void *context, uint64_t generation,
                          rptadv_sip_status_v1 *event); /**< Pop oldest. */
    int32_t (*status)(void *context, uint64_t generation,
                      rptadv_sip_status_v1 *status); /**< Copy snapshot. */
    int32_t (*register_account)(void *context,
                                uint64_t generation); /**< Activate once after host commit. */
    int32_t (*unregister_account)(void *context, uint64_t generation); /**< Begin removal once. */
    int32_t (*shutdown)(void *context, uint64_t generation); /**< Stop admission and timers. */
    int32_t (*destroy)(void *context, uint64_t generation);  /**< Free only after stopped. */
} rptadv_sip_descriptor_v1;
/** Borrow immutable descriptor while the DSO lease is held.
 * @return Complete immutable registration capability descriptor.
 */
const rptadv_sip_descriptor_v1 *rptadv_sip_adapter_descriptor_v1(void);
#ifdef __cplusplus
}
#endif
#endif
